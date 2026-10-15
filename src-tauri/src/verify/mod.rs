//! Staged connection verification (SPEC §9). A connection is reported as
//! verified only when a real request has gone through the real integration
//! path; writing a file or reaching an endpoint is never enough.

use crate::adapters::{claude_code, codex};
use crate::gateway::{BrokerStatus, Phase};
use crate::credentials::{account_key, CredentialStore};
use crate::diagnostics::{now_secs, redact_known};
use crate::discovery::{self, ProbeError};
use crate::providers::ProviderDefinition;
use crate::state::{ConnectionRecord, ConnectionStatus, StageOutcome, StageResult, VerificationReport};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

pub const E2E_TOKEN: &str = "PRISMFLOW_OK";
pub const TOOL_TOKEN: &str = "PRISMFLOW_TOOL_OK";

pub struct VerifyEnv<'a> {
    pub client: reqwest::Client,
    pub store: &'a dyn CredentialStore,
    pub codex_bin: Option<PathBuf>,
    pub claude_bin: Option<PathBuf>,
    /// Broker status for gateway connections; None for direct ones.
    pub gateway: Option<BrokerStatus>,
    /// Normally the provider's base URL; tests point it at a mock server.
    pub base_url: String,
    /// Working directory for agent runs (keeps project files out of scope).
    pub workdir: PathBuf,
    pub agent_timeout: Duration,
}

struct Run {
    stages: Vec<StageResult>,
}

impl Run {
    fn push(&mut self, stage: u8, name: &str, outcome: StageOutcome, detail: impl Into<String>) {
        self.stages.push(StageResult { stage, name: name.into(), outcome, detail: detail.into() });
    }
}

fn status_for_probe(e: &ProbeError) -> ConnectionStatus {
    match e {
        ProbeError::Unauthorized(_) => ConnectionStatus::AuthenticationFailed,
        _ => ConnectionStatus::ProviderUnavailable,
    }
}

/// Classify a failed agent run from its (already redacted) output.
fn number_after(text: &str, key: &str) -> Option<u64> {
    let i = text.find(key)? + key.len();
    let digits: String = text[i..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// A specific, actionable explanation for well-known failures, appended to
/// the stage detail (SPEC §9: clear errors with remediation).
pub fn remediation(output: &str, provider: &ProviderDefinition) -> Option<String> {
    let o = output.to_lowercase();
    if o.contains("exceed_context_size") || o.contains("exceeds the available context") || o.contains("context length")
        || o.contains("maximum context") || o.contains("context_length_exceeded")
    {
        let need = number_after(&o, "n_prompt_tokens").or_else(|| number_after(&o, "request ("));
        let have = number_after(&o, "n_ctx").or_else(|| number_after(&o, "context size ("));
        let sizes = match (need, have) {
            (Some(n), Some(h)) => format!(" The agent's prompt needs about {n} tokens; the model is running with {h}."),
            _ => String::new(),
        };
        let fix = match provider.id {
            "ollama" => "Restart Ollama with a larger window, e.g. `OLLAMA_CONTEXT_LENGTH=32768 ollama serve`, or set it in the Ollama app's settings.",
            "llamacpp" => "Restart llama-server with a larger `--ctx-size` (e.g. 32768).",
            "lmstudio" => "In LM Studio, reload the model with a larger context length (e.g. 32768).",
            _ => "Choose a model with a larger context window.",
        };
        return Some(format!("The model's context window is too small for this coding agent.{sizes} {fix}"));
    }
    None
}

pub fn classify_agent_failure(output: &str) -> ConnectionStatus {
    let o = output.to_lowercase();
    if o.contains("error loading config") || o.contains("invalid settings") {
        ConnectionStatus::ConfigurationInvalid
    } else if o.contains("401")
        || o.contains("unauthorized")
        || o.contains("invalid api key")
        || o.contains("authentication_error")
        || o.contains("no credential stored")
        || o.contains("no gateway token")
    {
        ConnectionStatus::AuthenticationFailed
    } else if o.contains("model_not_found")
        || o.contains("not a valid model")
        || o.contains("no endpoints found")
        || o.contains("not configured for target provider")
        || (o.contains("model") && o.contains("does not exist"))
    {
        ConnectionStatus::ModelUnavailable
    } else if o.contains("connection refused") || o.contains("unable to connect") || o.contains("gateway engine") {
        ConnectionStatus::ProviderUnavailable
    } else {
        ConnectionStatus::RequiresAttention
    }
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

async fn run_codex(
    bin: &Path,
    profile: &str,
    workdir: &Path,
    json: bool,
    prompt: &str,
    timeout: Duration,
) -> Result<(bool, String), String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("exec");
    if !profile.is_empty() {
        cmd.arg("-p").arg(profile);
    }
    cmd.args(["--skip-git-repo-check", "--ephemeral", "-s", "read-only", "-C"]);
    cmd.arg(workdir);
    if json {
        cmd.arg("--json");
    }
    cmd.arg(prompt)
        .env("PATH", codex::child_path(bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| format!("could not start codex: {e}"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => Err(format!("codex did not finish within {}s", timeout.as_secs())),
        Ok(Err(e)) => Err(format!("codex failed to run: {e}")),
        Ok(Ok(out)) => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            Ok((out.status.success(), text))
        }
    }
}

/// True when a `codex exec --json` stream shows a completed command
/// execution whose output contains the tool token.
pub fn saw_tool_execution(jsonl: &str) -> Option<bool> {
    let mut parsed_any = false;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        parsed_any = true;
        let item = &v["item"];
        if item["type"].as_str() == Some("command_execution")
            && item.to_string().contains(TOOL_TOKEN)
        {
            return Some(true);
        }
    }
    parsed_any.then_some(false)
}

/// Run `claude -p` against the connection's settings file. Only `plan` mode:
/// verification never grants write or bypass permissions (matrix §E).
/// Inherited `ANTHROPIC_*` / `CLAUDE_CODE_*` variables are removed so the
/// run sees exactly what the settings file configures.
async fn run_claude(bin: &Path, settings: &Path, workdir: &Path, prompt: &str, timeout: Duration) -> Result<(bool, String), String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("-p")
        .arg("--settings").arg(settings)
        .args(["--permission-mode", "plan", "--output-format", "stream-json", "--verbose", "--no-session-persistence"])
        .arg(prompt)
        .current_dir(workdir)
        .env("PATH", claude_code::child_path(bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("ANTHROPIC_") || k.starts_with("CLAUDE_CODE_") {
            cmd.env_remove(k.as_ref());
        }
    }
    let child = cmd.spawn().map_err(|e| format!("could not start claude: {e}"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => Err(format!("claude did not finish within {}s", timeout.as_secs())),
        Ok(Err(e)) => Err(format!("claude failed to run: {e}")),
        Ok(Ok(out)) => Ok((
            out.status.success(),
            format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )),
    }
}

/// The final `result` event of a stream-json run: (is_error, text).
pub fn claude_result(stream: &str) -> Option<(bool, String)> {
    stream.lines().rev().find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        (v["type"] == "result").then(|| {
            (v["is_error"].as_bool().unwrap_or(true), v["result"].as_str().unwrap_or_default().to_string())
        })
    })
}

/// True when the stream shows a `Read` tool_use whose tool_result carries
/// the probe token; Some(false) when it parsed but no round-trip happened.
pub fn claude_saw_tool_round_trip(stream: &str) -> Option<bool> {
    let mut parsed_any = false;
    let mut read_ids = Vec::new();
    for line in stream.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        parsed_any = true;
        let Some(content) = v["message"]["content"].as_array() else { continue };
        for c in content {
            match c["type"].as_str() {
                Some("tool_use") if c["name"] == "Read" => read_ids.push(c["id"].clone()),
                Some("tool_result") if read_ids.contains(&c["tool_use_id"]) && c["content"].to_string().contains(TOOL_TOKEN) => {
                    return Some(true)
                }
                _ => {}
            }
        }
    }
    parsed_any.then_some(false)
}

pub async fn verify(
    env: &VerifyEnv<'_>,
    provider: &ProviderDefinition,
    record: &ConnectionRecord,
) -> VerificationReport {
    let started_at = now_secs();
    let mut run = Run { stages: Vec::new() };
    let status = run_stages(env, provider, record, &mut run).await;
    VerificationReport { started_at, finished_at: now_secs(), stages: run.stages, status }
}

async fn run_stages(
    env: &VerifyEnv<'_>,
    provider: &ProviderDefinition,
    record: &ConnectionRecord,
    run: &mut Run,
) -> ConnectionStatus {
    // Stage 1 — configuration validation.
    let agent = crate::adapters::agents::spec(&record.app_id);
    let is_claude = record.app_id == claude_code::APP_ID;
    let stage1 = if let Some(a) = agent {
        check_agent_config(a, &record.managed_file)
    } else if record.app_id == codex::DESKTOP_APP_ID {
        check_desktop_config(&record.managed_file, &record.provider_id, env.codex_bin.as_deref())
    } else if is_claude {
        check_claude_config(&record.managed_file, env.claude_bin.as_deref())
    } else {
        check_config(&record.managed_file, env.codex_bin.as_deref())
    };
    match stage1 {
        Ok(detail) => run.push(1, "Configuration", StageOutcome::Pass, detail),
        Err(detail) => {
            run.push(1, "Configuration", StageOutcome::Fail, detail);
            return ConnectionStatus::ConfigurationInvalid;
        }
    }

    // Stage 2 — credential validation.
    let stored = env.store.get(&account_key(provider.id, "default")).ok();
    let key = match stored.or_else(|| (!provider.requires_key).then(String::new)) {
        Some(k) => k,
        None => {
            run.push(2, "Credential", StageOutcome::Fail, format!(
                "No {} key is stored. Add it under Providers.", provider.display_name
            ));
            return ConnectionStatus::NeedsCredentials;
        }
    };
    let secrets = [key.as_str()];
    match provider.auth_check_path {
        _ if key.is_empty() => run.push(2, "Credential", StageOutcome::Skipped,
            format!("{} needs no key.", provider.display_name)),
        Some(path) => match discovery::check_credential_as(&env.client, &env.base_url, path, &key, provider.auth_style).await {
            Ok(()) => run.push(2, "Credential", StageOutcome::Pass, "The provider accepted the stored key."),
            Err(e) => {
                let status = status_for_probe(&e);
                run.push(2, "Credential", StageOutcome::Fail, redact_known(&format!(
                    "{e}. {}", if status == ConnectionStatus::AuthenticationFailed {
                        "Replace the key under Providers."
                    } else {
                        "Check your network connection and the provider's status page."
                    }
                ), &secrets));
                return status;
            }
        },
        None => run.push(2, "Credential", StageOutcome::Skipped,
            "No documented side-effect-free key check for this provider; the end-to-end stage proves the key instead."),
    }

    // Stages 3 + 4 — endpoint and model.
    match provider.model_list_path {
        Some(path) => match discovery::list_models_as(&env.client, &env.base_url, path, (!key.is_empty()).then_some(key.as_str()), provider.auth_style).await {
            Ok(models) => {
                let transport = if env.base_url.starts_with("https://") { " over TLS" } else { "" };
                run.push(3, "Endpoint", StageOutcome::Pass, format!(
                    "Reached {}{transport} and it answered in the expected format.", env.base_url
                ));
                if models.iter().any(|m| m.id == record.model) {
                    run.push(4, "Model", StageOutcome::Pass, format!(
                        "{} is listed. Listing does not prove access; the next stage does.", record.model
                    ));
                } else {
                    run.push(4, "Model", StageOutcome::Warn, format!(
                        "{} is not in the provider's model list. It may be misspelled or unavailable to this key.", record.model
                    ));
                }
            }
            Err(e) => {
                run.push(3, "Endpoint", StageOutcome::Fail, redact_known(&e.to_string(), &secrets));
                return status_for_probe(&e);
            }
        },
        None => {
            run.push(3, "Endpoint", StageOutcome::Skipped, "This provider has no model-list endpoint.");
            run.push(4, "Model", StageOutcome::Skipped, "Cannot check the model without a model list.");
        }
    }

    // Gateway connections also need the local broker + engine up.
    if let Some(g) = &env.gateway {
        if g.phase == Phase::Ready {
            run.push(3, "Gateway", StageOutcome::Pass, format!(
                "PrismFlow's gateway is ready at {} ({}, loopback only).", g.url, g.engine
            ));
        } else {
            run.push(3, "Gateway", StageOutcome::Fail, format!(
                "PrismFlow's gateway is {:?}. {}", g.phase,
                g.detail.clone().unwrap_or_else(|| "Restart it from the Overview page.".into())
            ));
            return ConnectionStatus::RequiresAttention;
        }
    }

    if is_claude {
        return claude_agent_stages(env, provider, record, run, &secrets).await;
    }
    if let Some(a) = agent {
        return terminal_agent_stages(env, a, provider, record, run, &secrets).await;
    }

    // Stage 5 — end-to-end request through Codex itself.
    let Some(bin) = env.codex_bin.as_deref() else {
        run.push(5, "End-to-end", StageOutcome::Fail, "Codex CLI is not installed.");
        return ConnectionStatus::RequiresAttention;
    };
    let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
    match run_codex(bin, &record.profile, &env.workdir, false, &prompt, env.agent_timeout).await {
        Ok((true, out)) if out.contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass,
            if record.profile.is_empty() {
                "Codex answered using your base config, as the Codex app will (restart the app to pick it up).".to_string()
            } else {
                format!("Codex answered through the {} profile.", record.profile)
            }),
        Ok((_, out)) => {
            let shown = redact_known(&tail(&out, 6), &secrets);
            let hint = remediation(&out, provider).map(|h| format!("{h}\n\n")).unwrap_or_default();
            run.push(5, "End-to-end", StageOutcome::Fail, format!("{hint}Codex did not return the expected reply:\n{shown}"));
            return classify_agent_failure(&shown);
        }
        Err(e) => {
            run.push(5, "End-to-end", StageOutcome::Fail, e);
            return ConnectionStatus::ProviderUnavailable;
        }
    }

    // Stage 6 — agent compatibility: a real tool call.
    let prompt = format!("Use your shell tool to run `echo {TOOL_TOKEN}`, then reply DONE.");
    match run_codex(bin, &record.profile, &env.workdir, true, &prompt, env.agent_timeout).await {
        Ok((_, out)) => match saw_tool_execution(&out) {
            Some(true) => {
                run.push(6, "Tool calling", StageOutcome::Pass, "Codex ran a shell tool call with this model.");
                ConnectionStatus::ConnectedAndVerified
            }
            Some(false) => {
                run.push(6, "Tool calling", StageOutcome::Fail,
                    "The model answered but did not complete a tool call. Chat works; agent tasks may not. Prefer a tool-capable model.");
                ConnectionStatus::PartiallyCompatible
            }
            None => {
                run.push(6, "Tool calling", StageOutcome::Skipped,
                    "Could not read Codex's event stream, so tool calling is unverified.");
                ConnectionStatus::PartiallyCompatible
            }
        },
        Err(e) => {
            run.push(6, "Tool calling", StageOutcome::Fail, e);
            ConnectionStatus::PartiallyCompatible
        }
    }
}

async fn claude_agent_stages(env: &VerifyEnv<'_>, provider: &ProviderDefinition, record: &ConnectionRecord, run: &mut Run, secrets: &[&str]) -> ConnectionStatus {
    let Some(bin) = env.claude_bin.as_deref() else {
        run.push(5, "End-to-end", StageOutcome::Fail, "Claude Code is not installed.");
        return ConnectionStatus::RequiresAttention;
    };
    if let Err(e) = std::fs::create_dir_all(&env.workdir) {
        run.push(5, "End-to-end", StageOutcome::Fail, format!("Cannot create {}: {e}", env.workdir.display()));
        return ConnectionStatus::RequiresAttention;
    }

    // Stage 5 — a real Claude Code turn through the gateway.
    let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
    match run_claude(bin, &record.managed_file, &env.workdir, &prompt, env.agent_timeout).await {
        Ok((_, out)) => match claude_result(&out) {
            Some((false, text)) if text.contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass,
                format!("Claude Code answered through PrismFlow's gateway with {}.", record.model)),
            _ => {
                let shown = redact_known(&tail(&out, 6), secrets);
                let hint = remediation(&out, provider).map(|h| format!("{h}\n\n")).unwrap_or_default();
                run.push(5, "End-to-end", StageOutcome::Fail, format!("{hint}Claude Code did not return the expected reply:\n{shown}"));
                return classify_agent_failure(&shown);
            }
        },
        Err(e) => {
            run.push(5, "End-to-end", StageOutcome::Fail, e);
            return ConnectionStatus::ProviderUnavailable;
        }
    }

    // Stage 6 — a real tool call (Read is permitted in plan mode).
    let probe = env.workdir.join("prismflow-probe.txt");
    if let Err(e) = std::fs::write(&probe, format!("{TOOL_TOKEN}\n")) {
        run.push(6, "Tool calling", StageOutcome::Fail, format!("Cannot write the probe file: {e}"));
        return ConnectionStatus::PartiallyCompatible;
    }
    let prompt = format!(
        "Use the Read tool to read {} and reply with its exact contents. PRISMFLOW_TOOL_PROBE",
        probe.display()
    );
    match run_claude(bin, &record.managed_file, &env.workdir, &prompt, env.agent_timeout).await {
        Ok((_, out)) => match claude_saw_tool_round_trip(&out) {
            Some(true) => {
                run.push(6, "Tool calling", StageOutcome::Pass, "Claude Code completed a Read tool call with this model.");
                ConnectionStatus::ConnectedAndVerified
            }
            Some(false) => {
                run.push(6, "Tool calling", StageOutcome::Fail,
                    "The model answered but did not complete a tool call. Chat works; agent tasks may not. Prefer a tool-capable model.");
                ConnectionStatus::PartiallyCompatible
            }
            None => {
                run.push(6, "Tool calling", StageOutcome::Skipped,
                    "Could not read Claude Code's event stream, so tool calling is unverified.");
                ConnectionStatus::PartiallyCompatible
            }
        },
        Err(e) => {
            run.push(6, "Tool calling", StageOutcome::Fail, e);
            ConnectionStatus::PartiallyCompatible
        }
    }
}

fn check_claude_config(file: &Path, claude_bin: Option<&Path>) -> Result<String, String> {
    let content = std::fs::read_to_string(file)
        .map_err(|_| format!("{} is missing. Reconnect to recreate it.", file.display()))?;
    if !claude_code::is_ours(&content) {
        return Err(format!("{} is no longer PrismFlow-managed (or is not valid JSON).", file.display()));
    }
    let v: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let helper = v["apiKeyHelper"].as_str().unwrap_or_default();
    // The helper command starts with the single-quoted binary path.
    let bin = helper.strip_prefix('\'').and_then(|r| r.split_once("' ")).map(|(p, _)| p.replace(r"'\''", "'"));
    match bin {
        Some(b) if Path::new(&b).is_file() => {}
        Some(b) => return Err(format!("The token helper {b} no longer exists (was PrismFlow moved?). Reconnect to repair.")),
        None => return Err("The settings file has no PrismFlow token helper.".into()),
    }
    if v["env"]["ANTHROPIC_BASE_URL"].as_str().is_none_or(|u| !u.starts_with("http://127.0.0.1:")) {
        return Err("The settings file does not point at PrismFlow's local gateway.".into());
    }
    if claude_bin.is_none() {
        return Err("Claude Code is not installed.".into());
    }
    Ok("Settings file is valid, PrismFlow-managed, and its token helper exists.".into())
}

fn check_desktop_config(file: &Path, provider_id: &str, codex_bin: Option<&Path>) -> Result<String, String> {
    let text = std::fs::read_to_string(file).map_err(|_| format!("{} is missing.", file.display()))?;
    let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{} is not valid TOML: {e}", file.display()))?;
    let table = codex::provider_table(provider_id);
    if doc.get("model_provider").and_then(|v| v.as_str()) != Some(table.as_str()) {
        return Err(format!("Your Codex config no longer selects {table} (another tool or an edit changed model_provider). Reconnect to restore it."));
    }
    let helper = doc.get("model_providers").and_then(|t| t.get(&table)).and_then(|t| t.get("auth"))
        .and_then(|a| a.get("command")).and_then(|c| c.as_str()).ok_or("The PrismFlow provider table has no credential helper.")?;
    if !Path::new(helper).is_file() {
        return Err(format!("The credential helper {helper} no longer exists. Reconnect to repair."));
    }
    if codex_bin.is_none() {
        return Err("The Codex CLI is needed to verify the desktop app's configuration (they share it).".into());
    }
    Ok("Your Codex config selects PrismFlow's provider, and its credential helper exists.".into())
}

fn check_agent_config(a: &crate::adapters::agents::AgentSpec, script: &Path) -> Result<String, String> {
    let body = std::fs::read_to_string(script).map_err(|_| format!("{} is missing. Reconnect to recreate it.", script.display()))?;
    if !body.contains(crate::adapters::agents::MARKER) {
        return Err(format!("{} is no longer PrismFlow-managed.", script.display()));
    }
    if crate::adapters::agents::find_binary(a).is_none() {
        return Err(format!("{} is not installed.", a.display_name));
    }
    Ok(format!("Launch script is valid and {} is installed.", a.display_name))
}

async fn run_script(script: &Path, args: &[String], workdir: &Path, timeout: Duration) -> Result<(bool, String), String> {
    std::fs::create_dir_all(workdir).map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg(script).args(args).current_dir(workdir)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| format!("could not start the agent: {e}"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => Err(format!("the agent did not finish within {}s", timeout.as_secs())),
        Ok(Err(e)) => Err(format!("the agent failed to run: {e}")),
        Ok(Ok(out)) => Ok((out.status.success(), format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))),
    }
}

/// Stages 5–6 for terminal agents. Stage 6 plants a random nonce in a file
/// and asks the agent to read it: the model cannot produce the nonce
/// without a real file-read tool call.
async fn terminal_agent_stages(
    env: &VerifyEnv<'_>,
    a: &crate::adapters::agents::AgentSpec,
    provider: &ProviderDefinition,
    record: &ConnectionRecord,
    run: &mut Run,
    secrets: &[&str],
) -> ConnectionStatus {
    use crate::adapters::agents::oneshot_args;
    let work = env.workdir.join(record.id.replace(':', "-"));
    let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
    match run_script(&record.managed_file, &oneshot_args(a, &prompt), &work, env.agent_timeout).await {
        Ok((_, out)) if out.contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass,
            format!("{} answered through PrismFlow's gateway with {}.", a.display_name, record.model)),
        Ok((_, out)) => {
            let shown = redact_known(&tail(&out, 6), secrets);
            let hint = remediation(&out, provider).map(|h| format!("{h}\n\n")).unwrap_or_default();
            run.push(5, "End-to-end", StageOutcome::Fail, format!("{hint}{} did not return the expected reply:\n{shown}", a.display_name));
            return classify_agent_failure(&shown);
        }
        Err(e) => {
            run.push(5, "End-to-end", StageOutcome::Fail, e);
            return ConnectionStatus::ProviderUnavailable;
        }
    }
    if !a.tools {
        run.push(6, "Tool calling", StageOutcome::Skipped,
            format!("{} edits files through its own edit format, not tool calls; the end-to-end stage covers it.", a.display_name));
        return ConnectionStatus::ConnectedAndVerified;
    }
    let nonce = format!("PF{}", uuid::Uuid::new_v4().simple());
    if let Err(e) = std::fs::write(work.join("prismflow_probe.txt"), format!("{nonce}\n")) {
        run.push(6, "Tool calling", StageOutcome::Fail, format!("Could not prepare the probe file: {e}"));
        return ConnectionStatus::PartiallyCompatible;
    }
    let prompt = "Use your file-reading tool to read the file prismflow_probe.txt in the current directory, then reply with its exact contents and nothing else.";
    let result = run_script(&record.managed_file, &oneshot_args(a, prompt), &work, env.agent_timeout).await;
    let _ = std::fs::remove_file(work.join("prismflow_probe.txt"));
    match result {
        Ok((_, out)) if out.contains(&nonce) => {
            run.push(6, "Tool calling", StageOutcome::Pass, format!("{} read a file through a tool call with this model.", a.display_name));
            ConnectionStatus::ConnectedAndVerified
        }
        Ok(_) => {
            run.push(6, "Tool calling", StageOutcome::Fail,
                "The model answered but did not complete a file-read tool call. Chat works; agent tasks may not. Prefer a tool-capable model.");
            ConnectionStatus::PartiallyCompatible
        }
        Err(e) => {
            run.push(6, "Tool calling", StageOutcome::Fail, e);
            ConnectionStatus::PartiallyCompatible
        }
    }
}

fn check_config(file: &Path, codex_bin: Option<&Path>) -> Result<String, String> {
    let content = std::fs::read_to_string(file)
        .map_err(|_| format!("{} is missing. Reconnect to recreate it.", file.display()))?;
    if !content.starts_with(codex::MANAGED_MARKER) {
        return Err(format!("{} is no longer PrismFlow-managed.", file.display()));
    }
    let doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", file.display()))?;
    let helper = doc
        .get("model_providers")
        .and_then(|p| p.as_table_like())
        .and_then(|p| p.iter().find(|(k, _)| k.starts_with("prismflow_")).map(|(_, v)| v))
        .and_then(|v| v.get("auth"))
        .and_then(|a| a.get("command"))
        .and_then(|c| c.as_str())
        .ok_or("The profile has no PrismFlow credential helper.")?;
    if !Path::new(helper).is_file() {
        return Err(format!(
            "The credential helper {helper} no longer exists (was PrismFlow moved?). Reconnect to repair."
        ));
    }
    if let Some(base) = file.parent().map(|d| d.join("config.toml")) {
        if let Ok(user) = std::fs::read_to_string(&base) {
            if user.parse::<toml_edit::DocumentMut>().is_err() {
                return Err(format!("Your own {} has a TOML error; Codex will refuse to start.", base.display()));
            }
        }
    }
    if codex_bin.is_none() {
        return Err("Codex CLI is not installed.".into());
    }
    Ok("Profile file is valid, PrismFlow-managed, and its credential helper exists.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_engine;
    use crate::credentials::memory::InMemoryStore;
    use crate::providers;
    use std::os::unix::fs::PermissionsExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A fake `codex` that answers both stage-5 and stage-6 prompts.
    fn fake_codex(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("codex");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    const GOOD_CODEX: &str = r#"case "$*" in
  *--json*) printf '%s\n' '{"type":"item.completed","item":{"type":"command_execution","aggregated_output":"PRISMFLOW_TOOL_OK\n"}}' ;;
  *) echo PRISMFLOW_OK ;;
esac"#;

    struct Fixture {
        _home: tempfile::TempDir,
        server: MockServer,
        record: ConnectionRecord,
        store: InMemoryStore,
    }

    async fn fixture(models_status: u16, key_status: u16) -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let helper = fake_codex(home.path(), "echo helper"); // any existing file
        let or = providers::get("openrouter").unwrap();
        let plan = codex::plan_connect(home.path(), or, "a/model", &helper, None).unwrap();
        config_engine::apply(&plan).unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/key"))
            .respond_with(ResponseTemplate::new(key_status).set_body_json(serde_json::json!({"data":{}})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/models"))
            .respond_with(ResponseTemplate::new(models_status)
                .set_body_json(serde_json::json!({"data":[{"id":"a/model"}]})))
            .mount(&server).await;
        let store = InMemoryStore::new();
        store.set(&account_key("openrouter", "default"), "sk-or-v1-secretsecretsecret").unwrap();
        let record = ConnectionRecord {
            id: "c1".into(),
            provider_id: "openrouter".into(),
            app_id: codex::APP_ID.into(),
            model: "a/model".into(),
            profile: codex::profile_name("openrouter"),
            route: crate::state::Route::Direct,
            managed_file: plan.file,
            created_at: 0,
            status: ConnectionStatus::Unverified,
            last_report: None,
            routing: Default::default(), priors: Default::default(),
        };
        Fixture { _home: home, server, record, store }
    }

    async fn run(f: &Fixture, codex_body: Option<&str>) -> VerificationReport {
        let bin_dir = tempfile::tempdir().unwrap();
        let env = VerifyEnv {
            client: discovery::client(),
            store: &f.store,
            codex_bin: codex_body.map(|b| fake_codex(bin_dir.path(), b)),
            claude_bin: None,
            gateway: None,
            base_url: f.server.uri(),
            workdir: std::env::temp_dir(),
            agent_timeout: Duration::from_secs(10),
        };
        verify(&env, providers::get("openrouter").unwrap(), &f.record).await
    }

    fn outcomes(r: &VerificationReport) -> Vec<StageOutcome> {
        r.stages.iter().map(|s| s.outcome).collect()
    }

    #[tokio::test]
    async fn all_stages_pass_means_verified() {
        let f = fixture(200, 200).await;
        let r = run(&f, Some(GOOD_CODEX)).await;
        assert_eq!(r.status, ConnectionStatus::ConnectedAndVerified, "{:#?}", r.stages);
        assert_eq!(outcomes(&r), vec![StageOutcome::Pass; 6]);
    }

    #[tokio::test]
    async fn rejected_key_stops_at_stage_2() {
        let f = fixture(200, 401).await;
        let r = run(&f, Some(GOOD_CODEX)).await;
        assert_eq!(r.status, ConnectionStatus::AuthenticationFailed);
        assert_eq!(r.stages.len(), 2);
    }

    #[tokio::test]
    async fn missing_key_needs_credentials() {
        let f = fixture(200, 200).await;
        f.store.delete(&account_key("openrouter", "default")).unwrap();
        let r = run(&f, Some(GOOD_CODEX)).await;
        assert_eq!(r.status, ConnectionStatus::NeedsCredentials);
    }

    #[tokio::test]
    async fn endpoint_outage_is_provider_unavailable() {
        let f = fixture(503, 200).await;
        let r = run(&f, Some(GOOD_CODEX)).await;
        assert_eq!(r.status, ConnectionStatus::ProviderUnavailable);
    }

    #[tokio::test]
    async fn chat_without_tools_is_partial() {
        let f = fixture(200, 200).await;
        let r = run(&f, Some(r#"case "$*" in *--json*) echo '{"type":"item.completed","item":{"type":"agent_message","text":"DONE"}}' ;; *) echo PRISMFLOW_OK ;; esac"#)).await;
        assert_eq!(r.status, ConnectionStatus::PartiallyCompatible);
    }

    #[tokio::test]
    async fn agent_failure_is_classified_and_redacted() {
        let f = fixture(200, 200).await;
        let r = run(&f, Some("echo 'unexpected status 401 Unauthorized key=sk-or-v1-secretsecretsecret' >&2; exit 1")).await;
        assert_eq!(r.status, ConnectionStatus::AuthenticationFailed);
        let detail = &r.stages.last().unwrap().detail;
        assert!(!detail.contains("secretsecret"), "secret leaked: {detail}");
    }

    #[tokio::test]
    async fn missing_codex_fails_configuration() {
        let f = fixture(200, 200).await;
        let r = run(&f, None).await;
        assert_eq!(r.status, ConnectionStatus::ConfigurationInvalid);
    }

    #[tokio::test]
    async fn deleted_profile_is_configuration_invalid() {
        let f = fixture(200, 200).await;
        std::fs::remove_file(&f.record.managed_file).unwrap();
        let r = run(&f, Some(GOOD_CODEX)).await;
        assert_eq!(r.status, ConnectionStatus::ConfigurationInvalid);
        assert_eq!(r.stages.len(), 1);
    }

    // ---- Claude Code (gateway route) -------------------------------------

    const GOOD_CLAUDE: &str = r#"case "$*" in
  *PRISMFLOW_TOOL_PROBE*)
    echo '{"type":"system","subtype":"init"}'
    echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"p"}}]}}'
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"1 PRISMFLOW_TOOL_OK"}]}}'
    echo '{"type":"result","subtype":"success","is_error":false,"result":"PRISMFLOW_TOOL_OK"}' ;;
  *--permission-mode\ plan*)
    echo '{"type":"result","subtype":"success","is_error":false,"result":"PRISMFLOW_OK"}' ;;
  *) echo "verification must use plan mode: $*" >&2; exit 9 ;;
esac"#;

    fn gateway_status(phase: Phase) -> BrokerStatus {
        BrokerStatus {
            phase, url: "http://127.0.0.1:47821".into(), port: 47821,
            engine: "@the-next-ai/ai-gateway 1.0.21".into(), node: Some("v22".into()),
            detail: (phase != Phase::Ready).then(|| "Node.js 20 or newer is required".into()),
            restarts: 0, requests: 0, rejected: 0, upstream_errors: 0, connections: 1, upstreams: vec!["nvidia_nim".into()],
        }
    }

    async fn run_claude_fixture(claude_body: &str, phase: Phase) -> VerificationReport {
        let home = tempfile::tempdir().unwrap();
        let helper = fake_codex(home.path(), "echo helper");
        let nim = providers::get("nvidia_nim").unwrap();
        let plan = claude_code::plan_connect(home.path(), nim, "meta/llama-3.3-70b-instruct", &helper,
            "claude_code:nvidia_nim", "http://127.0.0.1:47821", &crate::routing::Routing::default()).unwrap();
        config_engine::apply(&plan).unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":[{"id":"meta/llama-3.3-70b-instruct"}]})))
            .mount(&server).await;
        let store = InMemoryStore::new();
        store.set(&account_key("nvidia_nim", "default"), "nvapi-secretsecretsecret").unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let claude = bin_dir.path().join("claude");
        std::fs::write(&claude, format!("#!/bin/sh\n{claude_body}\n")).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let record = ConnectionRecord {
            id: "claude_code:nvidia_nim".into(), provider_id: "nvidia_nim".into(), app_id: claude_code::APP_ID.into(),
            model: "meta/llama-3.3-70b-instruct".into(), profile: claude_code::profile_name("nvidia_nim"),
            route: crate::state::Route::Gateway, managed_file: plan.file, created_at: 0,
            status: ConnectionStatus::Unverified, last_report: None, routing: Default::default(), priors: Default::default(),
        };
        let work = tempfile::tempdir().unwrap();
        let env = VerifyEnv {
            client: discovery::client(), store: &store, codex_bin: None, claude_bin: Some(claude),
            gateway: Some(gateway_status(phase)), base_url: server.uri(),
            workdir: work.path().join("verify"), agent_timeout: Duration::from_secs(10),
        };
        verify(&env, nim, &record).await
    }

    #[tokio::test]
    async fn claude_code_all_stages_pass_in_plan_mode() {
        let r = run_claude_fixture(GOOD_CLAUDE, Phase::Ready).await;
        assert_eq!(r.status, ConnectionStatus::ConnectedAndVerified, "{:#?}", r.stages);
        assert!(r.stages.iter().any(|s| s.name == "Gateway" && s.outcome == StageOutcome::Pass));
        assert!(r.stages.iter().all(|s| s.outcome != StageOutcome::Fail));
    }

    #[tokio::test]
    async fn claude_code_stops_when_gateway_is_down() {
        let r = run_claude_fixture(GOOD_CLAUDE, Phase::Failed).await;
        assert_eq!(r.status, ConnectionStatus::RequiresAttention);
        let last = r.stages.last().unwrap();
        assert_eq!(last.name, "Gateway");
        assert!(last.detail.contains("Node.js"));
    }

    #[tokio::test]
    async fn claude_code_without_tool_round_trip_is_partial() {
        let body = r#"echo '{"type":"result","subtype":"success","is_error":false,"result":"PRISMFLOW_OK"}'"#;
        let r = run_claude_fixture(body, Phase::Ready).await;
        assert_eq!(r.status, ConnectionStatus::PartiallyCompatible, "{:#?}", r.stages);
    }

    #[tokio::test]
    async fn claude_code_auth_failure_is_classified_and_redacted() {
        let body = r#"echo '{"type":"result","subtype":"success","is_error":true,"result":"API Error: 401 {"type":"error","error":{"type":"authentication_error","message":"PrismFlow broker: revoked local token nvapi-secretsecretsecret"}}"}'"#;
        let r = run_claude_fixture(body, Phase::Ready).await;
        assert_eq!(r.status, ConnectionStatus::AuthenticationFailed, "{:#?}", r.stages);
        assert!(!r.stages.last().unwrap().detail.contains("secretsecret"));
    }

    #[test]
    fn stream_parsers_need_matching_tool_ids() {
        let ok = "{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"Read\"}]}}\n{\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"a\",\"content\":\"PRISMFLOW_TOOL_OK\"}]}}";
        assert_eq!(claude_saw_tool_round_trip(ok), Some(true));
        let forged = "{\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"PRISMFLOW_TOOL_OK\"}]}}";
        assert_eq!(claude_saw_tool_round_trip(forged), Some(false), "the model echoing the token is not a tool call");
        assert_eq!(claude_result("{\"type\":\"result\",\"is_error\":false,\"result\":\"x\"}"), Some((false, "x".into())));
    }

    #[test]
    fn classifies_common_agent_errors() {
        assert_eq!(classify_agent_failure("No endpoints found for x/y"), ConnectionStatus::ModelUnavailable);
        assert_eq!(classify_agent_failure("Error loading config.toml: bad"), ConnectionStatus::ConfigurationInvalid);
        assert_eq!(classify_agent_failure("high demand"), ConnectionStatus::RequiresAttention);
    }

    #[test]
    fn context_overflow_gets_a_specific_fix() {
        let raw = r#"{\"message\":\"request (11325 tokens) exceeds the available context size (4096 tokens)\",\"type\":\"exceed_context_size_error\",\"n_prompt_tokens\":11325,\"n_ctx\":4096}"#;
        let h = remediation(raw, crate::providers::get("ollama").unwrap()).unwrap();
        assert!(h.contains("11325") && h.contains("4096") && h.contains("OLLAMA_CONTEXT_LENGTH"), "{h}");
        assert!(remediation("all good", crate::providers::get("ollama").unwrap()).is_none());
    }
}
