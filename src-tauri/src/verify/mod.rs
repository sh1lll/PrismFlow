//! Staged connection verification. A connection is reported as
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
    /// PrismFlow's data folder (connection-owned files live under it).
    pub state_dir: PathBuf,
}

/// Stages finished so far, per connection, while a test runs (the UI polls it).
static PROGRESS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Vec<StageResult>>>> =
    std::sync::LazyLock::new(Default::default);

/// The stages a running test has finished; None when no test is running.
pub fn progress(connection_id: &str) -> Option<Vec<StageResult>> {
    PROGRESS.lock().ok()?.get(connection_id).cloned()
}

struct Run {
    id: String,
    stages: Vec<StageResult>,
}

impl Run {
    fn push(&mut self, stage: u8, name: &str, outcome: StageOutcome, detail: impl Into<String>) {
        self.stages.push(StageResult { stage, name: name.into(), outcome, detail: detail.into() });
        if let Ok(mut p) = PROGRESS.lock() {
            p.insert(self.id.clone(), self.stages.clone());
        }
    }
}

impl Drop for Run {
    /// Also runs when the test is stopped part-way.
    fn drop(&mut self) {
        if let Ok(mut p) = PROGRESS.lock() {
            p.remove(&self.id);
        }
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
/// the stage detail.
pub fn remediation(output: &str, provider: &ProviderDefinition) -> Option<String> {
    let o = output.to_lowercase();
    if o.contains("exceed_context_size") || o.contains("exceeds the available context") || o.contains("context length")
        || o.contains("maximum context") || o.contains("context_length_exceeded")
    {
        let need = number_after(&o, "n_prompt_tokens").or_else(|| number_after(&o, "request ("));
        let have = number_after(&o, "n_ctx").or_else(|| number_after(&o, "context size ("));
        let sizes = match (need, have) {
            (Some(n), Some(h)) => format!(" It needs room for about {n} tokens but has {h}."),
            _ => String::new(),
        };
        let fix = match provider.id {
            "ollama" => "Fix: in the Ollama app's settings, raise the context length to 32k (or run `OLLAMA_CONTEXT_LENGTH=32768 ollama serve`).",
            "llamacpp" => "Fix: restart llama-server with `--ctx-size 32768`.",
            "lmstudio" => "Fix: in LM Studio, reload the model with a context length of 32768.",
            _ => "Fix: pick a model with a bigger context (memory).",
        };
        return Some(format!("This model can't hold enough text for a coding app.{sizes} {fix}"));
    }
    None
}

const TOO_SLOW: &str = "No answer within";

/// A test that ran out of time: the provider was reachable, the model slow.
fn too_slow(app: &str, timeout: std::time::Duration) -> String {
    let mins = timeout.as_secs().div_ceil(60);
    format!("{TOO_SLOW} {mins} minute{}. {app} reached the model, but it was too slow. Local models often are; try a smaller one, or test again.", if mins == 1 { "" } else { "s" })
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
    home: Option<&Path>,
    profile: &str,
    workdir: &Path,
    json: bool,
    prompt: &str,
    timeout: Duration,
) -> Result<(bool, String), String> {
    std::fs::create_dir_all(workdir).map_err(|e| format!("could not create {}: {e}", workdir.display()))?;
    let mut cmd = crate::spawn::async_command(bin);
    cmd.arg("exec");
    if !profile.is_empty() {
        cmd.arg("-p").arg(profile);
    }
    cmd.args(["--skip-git-repo-check", "--ephemeral", "-s", "read-only", "-C"]);
    cmd.arg(workdir);
    if json {
        cmd.arg("--json");
    }
    if let Some(h) = home {
        cmd.env("CODEX_HOME", h);
    }
    cmd.arg(prompt)
        .env("PATH", codex::child_path(bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::proctree::own_group(&mut cmd);
    let child = cmd.spawn().map_err(|e| format!("could not start codex: {e}"))?;
    let tree = crate::proctree::TreeGuard::new(child.id());
    let done = tokio::time::timeout(timeout, child.wait_with_output()).await;
    if done.is_ok() {
        tree.finished();
    }
    match done {
        Err(_) => Err(too_slow("Codex", timeout)),
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
/// Successful model requests this connection made through the gateway since
/// `since` (unix seconds). A tool call shows as a second request carrying the
/// tool's result.
fn model_turns_since(state_dir: &Path, connection: &str, since: u64) -> usize {
    crate::usage::Store::open(state_dir).recent(200).iter()
        .filter(|r| r.connection == connection && r.ts >= since && r.status == 200)
        .count()
}

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

/// The separate Claude Code's own config folder (its settings file lives there).
fn claude_dir_for(record: &ConnectionRecord) -> Option<&Path> {
    (record.app_id == claude_code::SEPARATE_APP_ID).then(|| record.managed_file.parent()).flatten()
}

/// Run `claude -p` against the connection's settings file. Only `plan` mode:
/// verification never grants write or bypass permissions.
/// Inherited `ANTHROPIC_*` / `CLAUDE_CODE_*` variables are removed so the
/// run sees exactly what the settings file configures.
async fn run_claude(bin: &Path, config_dir: Option<&Path>, settings: &Path, workdir: &Path, prompt: &str, tools: &str, timeout: Duration) -> Result<(bool, String), String> {
    let started = std::time::Instant::now();
    let first = run_claude_once(bin, config_dir, settings, workdir, prompt, Some(tools), timeout).await;
    // Older Claude Code without --tools / --strict-mcp-config: run as before.
    match first {
        Ok((false, out)) if out.contains("unknown option") => {
            run_claude_once(bin, config_dir, settings, workdir, prompt, None, timeout.saturating_sub(started.elapsed())).await
        }
        other => other,
    }
}

/// `tools`: only these built-in tools ("" = none) and none of the user's
/// MCP servers. A test needs one reply or one Read; offering every tool
/// makes the request several times larger, and small models wander off
/// using them before they answer.
async fn run_claude_once(bin: &Path, config_dir: Option<&Path>, settings: &Path, workdir: &Path, prompt: &str, tools: Option<&str>, timeout: Duration) -> Result<(bool, String), String> {
    let mut cmd = crate::spawn::async_command(bin);
    if let Some(d) = config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", d);
    }
    cmd.arg("-p")
        .arg("--settings").arg(settings)
        .args(["--permission-mode", "plan", "--output-format", "stream-json", "--verbose", "--no-session-persistence"]);
    if let Some(t) = tools {
        cmd.args(["--tools", t, "--strict-mcp-config"]);
    }
    cmd.arg(prompt)
        .current_dir(workdir)
        .env("PATH", claude_code::child_path(bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        // Everything a parent Claude Code session or SDK host sets, but keep
        // CLAUDE_CONFIG_DIR (the user's own choice of config folder).
        if k.starts_with("ANTHROPIC_") || k.starts_with("CLAUDE_CODE_") || k.starts_with("CLAUDE_AGENT_SDK_")
            || k == "CLAUDECODE" || k == "CLAUDE_EFFORT" || k == "CLAUDE_PID"
        {
            cmd.env_remove(k.as_ref());
        }
    }
    crate::proctree::own_group(&mut cmd);
    let child = cmd.spawn().map_err(|e| format!("could not start claude: {e}"))?;
    let tree = crate::proctree::TreeGuard::new(child.id());
    let done = tokio::time::timeout(timeout, child.wait_with_output()).await;
    if done.is_ok() {
        tree.finished();
    }
    match done {
        Err(_) => Err(too_slow("Claude Code", timeout)),
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
    let mut run = Run { id: record.id.clone(), stages: Vec::new() };
    if let Ok(mut p) = PROGRESS.lock() {
        p.insert(record.id.clone(), Vec::new());
    }
    let status = run_stages(env, provider, record, &mut run).await;
    VerificationReport { started_at, finished_at: now_secs(), stages: std::mem::take(&mut run.stages), status }
}

async fn run_stages(
    env: &VerifyEnv<'_>,
    provider: &ProviderDefinition,
    record: &ConnectionRecord,
    run: &mut Run,
) -> ConnectionStatus {
    // Stage 1 — configuration validation.
    let agent = crate::adapters::agents::spec(&record.app_id);
    let is_claude = claude_code::is_cli(&record.app_id);
    let stage1 = if record.app_id == crate::adapters::claude_desktop::SEPARATE_APP_ID {
        crate::adapters::claude_desktop::auto_active(&record.managed_file, &record.id)
            .then(|| "The separate Claude Desktop is set up for PrismFlow.".to_string())
            .ok_or_else(|| "The separate Claude Desktop is no longer set up for PrismFlow. Connect it again.".to_string())
    } else if record.app_id == crate::adapters::claude_desktop::APP_ID {
        Ok("Gateway connection created. Claude Desktop's own settings hold the address and token you entered; PrismFlow does not read them.".to_string())
    } else if record.app_id == crate::adapters::jetbrains::APP_ID {
        check_jetbrains_config(&record.id)
    } else if record.app_id == crate::adapters::vscode::CLAUDE_SHARED_APP_ID {
        crate::adapters::vscode::claude_shared_settings(&record.id)
            .filter(|f| f.is_file())
            .map(|_| "VS Code runs the Claude Code extension through PrismFlow.".to_string())
            .ok_or_else(|| format!("{} no longer points the Claude Code extension at PrismFlow.", crate::adapters::vscode::settings_file().display()))
    } else if record.app_id == crate::adapters::vscode::SEPARATE_APP_ID {
        // managed_file is the separate window's User/settings.json.
        let user = record.managed_file.parent().unwrap_or(Path::new("/"));
        crate::adapters::vscode::claude_config_dir_in(user, &record.id)
            .filter(|d| d.join("settings.json").is_file())
            .map(|_| "The separate VS Code is set up for PrismFlow.".to_string())
            .ok_or_else(|| "The separate VS Code is no longer set up for PrismFlow. Connect it again.".to_string())
    } else if record.app_id == crate::adapters::vscode::CLAUDE_APP_ID {
        crate::adapters::vscode::claude_config_dir(&record.id)
            .filter(|d| d.join("settings.json").is_file())
            .map(|_| "VS Code points the Claude Code extension at PrismFlow.".to_string())
            .ok_or_else(|| format!("{} no longer points the Claude Code extension at PrismFlow.", crate::adapters::vscode::settings_file().display()))
    } else if crate::adapters::editors::ALL.contains(&record.app_id.as_str()) {
        crate::adapters::editors::check(&record.app_id, &record.id, &env.state_dir)
    } else if record.app_id == crate::adapters::vscode::CHAT_APP_ID {
        crate::adapters::vscode::chat_model(&record.id)
            .map(|_| "VS Code Chat has the PrismFlow model.".to_string())
            .ok_or_else(|| format!("{} no longer has the PrismFlow model.", crate::adapters::vscode::chat_models_file().display()))
    } else if let Some(a) = agent {
        check_agent_config(a, &record.managed_file)
    } else if codex::is_desktop(&record.app_id) {
        check_desktop_config(&record.managed_file, &record.provider_id, env.codex_bin.as_deref())
    } else if is_claude {
        check_claude_config(&record.managed_file, env.claude_bin.as_deref())
    } else {
        check_config(&record.managed_file, env.codex_bin.as_deref())
    };
    match stage1 {
        Ok(_) => run.push(1, "Configuration", StageOutcome::Pass, "Setup looks right."),
        Err(detail) => {
            let plain = if detail.contains("isn't installed") || detail.starts_with("Your own") {
                detail
            } else {
                format!("PrismFlow's settings for this app are missing or were changed. Connect it again.\n\n{detail}")
            };
            run.push(1, "Configuration", StageOutcome::Fail, plain);
            return ConnectionStatus::ConfigurationInvalid;
        }
    }

    // Stage 2 — credential validation.
    let stored = env.store.get(&account_key(provider.id, "default")).ok();
    let key = match stored.or_else(|| (!provider.requires_key).then(String::new)) {
        Some(k) => k,
        None => {
            run.push(2, "Credential", StageOutcome::Fail, format!(
                "No {} key yet. Add one on the Providers page.", provider.display_name
            ));
            return ConnectionStatus::NeedsCredentials;
        }
    };
    let secrets = [key.as_str()];
    match provider.auth_check_path {
        _ if key.is_empty() => run.push(2, "Credential", StageOutcome::Skipped,
            format!("{} doesn't need a key.", provider.display_name)),
        Some(path) => match discovery::check_credential_as(&env.client, &env.base_url, path, &key, provider.auth_style).await {
            Ok(()) => run.push(2, "Credential", StageOutcome::Pass, "Key accepted."),
            Err(e) => {
                let status = status_for_probe(&e);
                run.push(2, "Credential", StageOutcome::Fail, redact_known(&format!(
                    "{}\n\n{e}", if status == ConnectionStatus::AuthenticationFailed {
                        "The key was rejected. Change it on the Providers page."
                    } else {
                        "Couldn't reach the provider. Check your internet connection."
                    }
                ), &secrets));
                return status;
            }
        },
        None => run.push(2, "Credential", StageOutcome::Skipped,
            "Checked during the reply step."),
    }

    // Stages 3 + 4 — endpoint and model.
    match provider.model_list_path {
        Some(path) => match discovery::list_models_as(&env.client, &env.base_url, path, (!key.is_empty()).then_some(key.as_str()), provider.auth_style).await {
            Ok(models) => {
                run.push(3, "Endpoint", StageOutcome::Pass, "The provider answered.");
                if models.iter().any(|m| m.id == record.model) {
                    run.push(4, "Model", StageOutcome::Pass, format!(
                        "{} is available.", record.model
                    ));
                } else {
                    run.push(4, "Model", StageOutcome::Warn, format!(
                        "{} isn't in this provider's list. Check the spelling.", record.model
                    ));
                }
            }
            Err(e) => {
                run.push(3, "Endpoint", StageOutcome::Fail, redact_known(&format!("Couldn't reach the provider. Check your internet connection, or that it's running.\n\n{e}"), &secrets));
                return status_for_probe(&e);
            }
        },
        None => {
            run.push(3, "Endpoint", StageOutcome::Skipped, "This provider doesn't share a model list.");
            run.push(4, "Model", StageOutcome::Skipped, "Checked during the reply step.");
        }
    }

    // Gateway connections also need the local broker + engine up.
    if let Some(g) = &env.gateway {
        if g.phase == Phase::Ready {
            let _ = (&g.url, &g.engine);
            // The app reads its PrismFlow key at run time; without it every
            // request is refused (and some apps retry for minutes first).
            match env.store.get(&crate::gateway::tokens::account(&record.id)) {
                Ok(token) => {
                    run.push(3, "Gateway", StageOutcome::Pass, "Running.");
                    // Claude-style apps also ask for token counts all the
                    // time; a test that only sends one reply missed that.
                    let a = record.app_id.as_str();
                    if a.starts_with("claude_") || a.starts_with("vscode_claude") || a == "vscode_separate" || a == "jetbrains" {
                        let body = serde_json::json!({ "model": record.model, "messages": [{ "role": "user", "content": "hi" }] });
                        let counted = env.client.post(format!("{}/v1/messages/count_tokens", g.url.trim_end_matches('/')))
                            .header("x-api-key", &token).header("anthropic-version", "2023-06-01").json(&body).send().await;
                        let ok = match counted {
                            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok().is_some_and(|v| v["input_tokens"].is_u64()),
                            _ => false,
                        };
                        if ok {
                            run.push(3, "App calls", StageOutcome::Pass, "Token counting works.");
                        } else {
                            run.push(3, "App calls", StageOutcome::Fail, "The app's token-count requests fail, so long chats will break in the app. Restart PrismFlow; if it stays, save a problem report in Settings.");
                            return ConnectionStatus::RequiresAttention;
                        }
                    }
                }
                Err(crate::credentials::CredentialError::NotFound(_)) => {
                    run.push(3, "Gateway", StageOutcome::Fail, "This app's PrismFlow key is missing. Remove the app and add it again.");
                    return ConnectionStatus::AuthenticationFailed;
                }
                Err(e) => {
                    run.push(3, "Gateway", StageOutcome::Fail, crate::credentials::store_problem(&e));
                    return ConnectionStatus::AuthenticationFailed;
                }
            }
        } else {
            run.push(3, "Gateway", StageOutcome::Fail, format!(
                "PrismFlow's background service isn't running. Restart it under More → Activity.{}",
                g.detail.clone().map(|d| format!("\n\n{d}")).unwrap_or_default()
            ));
            return ConnectionStatus::RequiresAttention;
        }
    }

    if is_claude {
        return claude_agent_stages(env, provider, record, run, &secrets).await;
    }
    if crate::adapters::claude_desktop::is_claude_desktop(&record.app_id) {
        return claude_desktop_stages(env, provider, record, run, &secrets).await;
    }
    if let Some(a) = agent {
        return terminal_agent_stages(env, a, provider, record, run, &secrets).await;
    }
    if record.app_id == crate::adapters::jetbrains::APP_ID {
        return jetbrains_stages(env, provider, record, run, &secrets).await;
    }
    if crate::adapters::vscode::is_claude(&record.app_id) || record.app_id == crate::adapters::vscode::SEPARATE_APP_ID {
        // The extension runs Claude Code with PrismFlow's settings; run it the same way.
        let mut r = record.clone();
        r.managed_file = if record.app_id == crate::adapters::vscode::CLAUDE_APP_ID {
            crate::adapters::vscode::claude_config_dir(&record.id).unwrap_or_default().join("settings.json")
        } else {
            crate::adapters::agents::agent_dir(&env.state_dir, &record.id).join("claude/settings.json")
        };
        return claude_agent_stages(env, provider, &r, run, &secrets).await;
    }
    if record.app_id == crate::adapters::vscode::CHAT_APP_ID {
        return vscode_chat_stages(env, record, run, &secrets, None).await;
    }
    if crate::adapters::editors::ALL.contains(&record.app_id.as_str()) {
        // The editor sends ordinary requests with this key; send the same.
        let Some(gw) = env.gateway.as_ref().filter(|g| !g.url.is_empty()) else {
            run.push(5, "End-to-end", StageOutcome::Fail, "PrismFlow's background service isn't running.");
            return ConnectionStatus::RequiresAttention;
        };
        let token = env.store.get(&crate::gateway::tokens::account(&record.id)).unwrap_or_default();
        let model = serde_json::json!({ "id": record.model, "url": format!("{}/v1/messages", gw.url), "requestHeaders": { "x-api-key": token } });
        return vscode_chat_stages(env, record, run, &secrets, Some(model)).await;
    }

    // Stage 5 — end-to-end request through Codex itself.
    let Some(bin) = env.codex_bin.as_deref() else {
        run.push(5, "End-to-end", StageOutcome::Fail, "Codex isn't installed.");
        return ConnectionStatus::RequiresAttention;
    };
    // Codex reads its config from the folder that holds the managed file.
    let home = (codex::is_desktop(&record.app_id) || record.app_id == codex::CLI_SEPARATE_APP_ID).then(|| record.managed_file.parent()).flatten();
    let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
    match run_codex(bin, home, &record.profile, &env.workdir, false, &prompt, env.agent_timeout).await {
        Ok((true, out)) if out.contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass,
            if record.profile.is_empty() {
                "Codex answered. Restart the Codex app to use it.".to_string()
            } else {
                "Codex answered.".to_string()
            }),
        Ok((_, out)) => {
            let shown = redact_known(&tail(&out, 6), &secrets);
            let hint = remediation(&out, provider).map(|h| format!("{h}\n\n")).unwrap_or_default();
            run.push(5, "End-to-end", StageOutcome::Fail, format!("{}\n\n{shown}", if hint.is_empty() { "Codex didn't answer as expected.".to_string() } else { hint.trim().to_string() }));
            return classify_agent_failure(&shown);
        }
        Err(e) => {
            let slow = e.starts_with(TOO_SLOW);
            run.push(5, "End-to-end", StageOutcome::Fail, e);
            return if slow { ConnectionStatus::RequiresAttention } else { ConnectionStatus::ProviderUnavailable };
        }
    }

    // Stage 6 — agent compatibility: a real tool call.
    let prompt = format!("Use your shell tool to run `echo {TOOL_TOKEN}`, then reply DONE.");
    let since = now_secs().saturating_sub(1);
    match run_codex(bin, home, &record.profile, &env.workdir, true, &prompt, env.agent_timeout).await {
        Ok((_, out)) => match saw_tool_execution(&out) {
            Some(true) => {
                run.push(6, "Tool calling", StageOutcome::Pass, "The model can use tools.");
                ConnectionStatus::ConnectedAndVerified
            }
            // The model asked to run the command and Codex sent the result
            // back, but Codex's own read-only sandbox didn't run it (Windows
            // blocks the shell there; some Linux systems can't start it).
            Some(false) if model_turns_since(&env.state_dir, &record.id, since) >= 2 => {
                run.push(6, "Tool calling", StageOutcome::Pass,
                    "The model can use tools. (Codex's own safety sandbox blocked the test command, which is normal for its read-only mode.)");
                ConnectionStatus::ConnectedAndVerified
            }
            Some(false) => {
                let shown = redact_known(&tail(&out, 12), &secrets);
                run.push(6, "Tool calling", StageOutcome::Fail,
                    format!("The model can chat but couldn't use tools, so coding tasks may not work. Try a different model.\n\n{shown}"));
                ConnectionStatus::PartiallyCompatible
            }
            None => {
                let shown = redact_known(&tail(&out, 12), &secrets);
                run.push(6, "Tool calling", StageOutcome::Skipped,
                    format!("Couldn't check whether the model can use tools.\n\n{shown}"));
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
        run.push(5, "End-to-end", StageOutcome::Fail, "Claude Code isn't installed.");
        return ConnectionStatus::RequiresAttention;
    };
    if let Err(e) = std::fs::create_dir_all(&env.workdir) {
        run.push(5, "End-to-end", StageOutcome::Fail, format!("Cannot create {}: {e}", env.workdir.display()));
        return ConnectionStatus::RequiresAttention;
    }

    // Stage 5 — a real Claude Code turn through the gateway.
    let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
    match run_claude(bin, claude_dir_for(record), &record.managed_file, &env.workdir, &prompt, "", env.agent_timeout).await {
        Ok((_, out)) => match claude_result(&out) {
            Some((false, text)) if text.contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass,
                "Claude Code answered.".to_string()),
            _ => {
                let shown = redact_known(&tail(&out, 6), secrets);
                let hint = remediation(&out, provider).map(|h| format!("{h}\n\n")).unwrap_or_default();
                run.push(5, "End-to-end", StageOutcome::Fail, format!("{}\n\n{shown}", if hint.is_empty() { "Claude Code didn't answer as expected.".to_string() } else { hint.trim().to_string() }));
                return classify_agent_failure(&shown);
            }
        },
        Err(e) => {
            let slow = e.starts_with(TOO_SLOW);
            run.push(5, "End-to-end", StageOutcome::Fail, e);
            return if slow { ConnectionStatus::RequiresAttention } else { ConnectionStatus::ProviderUnavailable };
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
    match run_claude(bin, claude_dir_for(record), &record.managed_file, &env.workdir, &prompt, "Read", env.agent_timeout).await {
        Ok((_, out)) => match claude_saw_tool_round_trip(&out) {
            Some(true) => {
                run.push(6, "Tool calling", StageOutcome::Pass, "The model can use tools.");
                ConnectionStatus::ConnectedAndVerified
            }
            Some(false) => {
                let shown = redact_known(&tail(&out, 12), &secrets);
                run.push(6, "Tool calling", StageOutcome::Fail,
                    format!("The model can chat but couldn't use tools, so coding tasks may not work. Try a different model.\n\n{shown}"));
                ConnectionStatus::PartiallyCompatible
            }
            None => {
                let shown = redact_known(&tail(&out, 12), &secrets);
                run.push(6, "Tool calling", StageOutcome::Skipped,
                    format!("Couldn't check whether the model can use tools.\n\n{shown}"));
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
    let bin = claude_code::helper_program(helper);
    match bin {
        Some(b) if Path::new(&b).is_file() => {}
        Some(b) => return Err(format!("The token helper {b} no longer exists (was PrismFlow moved?). Reconnect to repair.")),
        None => return Err("The settings file has no PrismFlow token helper.".into()),
    }
    if v["env"]["ANTHROPIC_BASE_URL"].as_str().is_none_or(|u| !u.starts_with("http://127.0.0.1:")) {
        return Err("The settings file does not point at PrismFlow's local gateway.".into());
    }
    if claude_bin.is_none() {
        return Err("Claude Code isn't installed.".into());
    }
    Ok("Settings file is valid, PrismFlow-managed, and its token helper exists.".into())
}

/// Claude Desktop: send the same request shape it sends (Anthropic Messages,
/// Bearer local token) through the gateway. The app itself cannot be driven,
/// so the result stays "configured, not verified" until the user tests it.
async fn claude_desktop_stages(
    env: &VerifyEnv<'_>,
    provider: &ProviderDefinition,
    record: &ConnectionRecord,
    run: &mut Run,
    secrets: &[&str],
) -> ConnectionStatus {
    let Some(gw) = env.gateway.as_ref().filter(|g| !g.url.is_empty()) else {
        run.push(5, "End-to-end", StageOutcome::Fail, "PrismFlow's background service isn't running.");
        return ConnectionStatus::RequiresAttention;
    };
    let Ok(token) = env.store.get(&crate::gateway::tokens::account(&record.id)) else {
        run.push(5, "End-to-end", StageOutcome::Fail, "This connection is missing its key. Connect it again.");
        return ConnectionStatus::NeedsCredentials;
    };
    // Automatic setup: test with exactly what Claude Desktop holds.
    // managed_file is the third-party settings folder this connection set up.
    let root = &record.managed_file;
    let auto = crate::adapters::claude_desktop::auto_active(root, &record.id);
    let (base, token) = match crate::adapters::claude_desktop::auto_profile(root).filter(|_| auto) {
        Some(p) => (
            p["inferenceGatewayBaseUrl"].as_str().unwrap_or(&gw.url).to_string(),
            p["inferenceGatewayApiKey"].as_str().unwrap_or(&token).to_string(),
        ),
        None => (gw.url.clone(), token),
    };
    if auto {
        // Claude Desktop lists models first (modelDiscoveryEnabled).
        let listed = env.client.get(format!("{base}/v1/models")).bearer_auth(&token).send().await;
        if !listed.as_ref().is_ok_and(|r| r.status().is_success()) {
            run.push(5, "End-to-end", StageOutcome::Fail, "Claude Desktop's settings point at PrismFlow, but its model list didn't load. Connect again.");
            return ConnectionStatus::ConfigurationInvalid;
        }
    }
    let gw_url = base;
    let body = serde_json::json!({
        "model": record.model, "max_tokens": 64, "stream": false,
        "messages": [{"role": "user", "content": format!("Reply with exactly {E2E_TOKEN} and nothing else.")}]
    });
    let sent = env.client.post(format!("{gw_url}/v1/messages")).bearer_auth(&token)
        .header("anthropic-version", "2023-06-01").json(&body)
        .timeout(env.agent_timeout).send().await;
    let mut all_secrets: Vec<&str> = secrets.to_vec();
    all_secrets.push(&token);
    match sent {
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            if status.is_success() && text.contains(E2E_TOKEN) {
                run.push(5, "End-to-end", StageOutcome::Pass, format!(
                    "The model answered{}.", ""));
            } else {
                let shown = redact_known(&tail(&text, 6), &all_secrets);
                let hint = remediation(&text, provider).map(|h| format!("{h}\n\n")).unwrap_or_default();
                run.push(5, "End-to-end", StageOutcome::Fail, format!("{}\n\nHTTP {status}: {shown}", if hint.is_empty() { "The model didn't answer as expected.".to_string() } else { hint.trim().to_string() }));
                return classify_agent_failure(&shown);
            }
        }
        Err(e) => {
            run.push(5, "End-to-end", StageOutcome::Fail, format!("PrismFlow's background service didn't respond.\n\n{}", e.without_url()));
            return ConnectionStatus::ProviderUnavailable;
        }
    }
    if auto {
        run.push(6, "In Claude Desktop", StageOutcome::Pass,
            "Claude Desktop is set to use PrismFlow. Quit and reopen it if it's open.");
        return ConnectionStatus::ConnectedAndVerified;
    }
    run.push(6, "In Claude Desktop", StageOutcome::Skipped,
        "Finish by pressing Test connection in Claude Desktop.");
    ConnectionStatus::Unverified
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
        return Err("Codex isn't installed. PrismFlow needs the Codex command-line tool to test the Codex app.".into());
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
    let (prog, lead) = crate::adapters::agents::script_runner(script);
    let mut cmd = crate::spawn::async_command(prog);
    cmd.args(lead).args(args).current_dir(workdir)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    crate::proctree::own_group(&mut cmd);
    let child = cmd.spawn().map_err(|e| format!("could not start the agent: {e}"))?;
    let tree = crate::proctree::TreeGuard::new(child.id());
    let done = tokio::time::timeout(timeout, child.wait_with_output()).await;
    if done.is_ok() {
        tree.finished();
    }
    match done {
        Err(_) => Err(too_slow("The app", timeout)),
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
            format!("{} answered.", a.display_name)),
        Ok((_, out)) => {
            let shown = redact_known(&tail(&out, 6), secrets);
            let hint = remediation(&out, provider).map(|h| format!("{h}\n\n")).unwrap_or_default();
            run.push(5, "End-to-end", StageOutcome::Fail, format!("{}\n\n{shown}", if hint.is_empty() { format!("{} didn't answer as expected.", a.display_name) } else { hint.trim().to_string() }));
            return classify_agent_failure(&shown);
        }
        Err(e) => {
            let slow = e.starts_with(TOO_SLOW);
            run.push(5, "End-to-end", StageOutcome::Fail, e);
            return if slow { ConnectionStatus::RequiresAttention } else { ConnectionStatus::ProviderUnavailable };
        }
    }
    if !a.tools {
        run.push(6, "Tool calling", StageOutcome::Skipped,
            format!("Not needed for {}.", a.display_name));
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
            run.push(6, "Tool calling", StageOutcome::Pass, "The model can use tools.".to_string());
            ConnectionStatus::ConnectedAndVerified
        }
        Ok(_) => {
            run.push(6, "Tool calling", StageOutcome::Fail,
                "The model can chat but couldn't use tools, so coding tasks may not work. Try a different model.");
            ConnectionStatus::PartiallyCompatible
        }
        Err(e) => {
            run.push(6, "Tool calling", StageOutcome::Fail, e);
            ConnectionStatus::PartiallyCompatible
        }
    }
}

/// VS Code Chat: send the requests VS Code sends, to the URL with the key it has.
async fn vscode_chat_stages(env: &VerifyEnv<'_>, record: &ConnectionRecord, run: &mut Run, secrets: &[&str], given: Option<serde_json::Value>) -> ConnectionStatus {
    use crate::adapters::vscode;
    let Some(model) = given.or_else(|| vscode::chat_model(&record.id)) else {
        run.push(5, "End-to-end", StageOutcome::Fail, "The PrismFlow model is missing from VS Code Chat. Connect again.");
        return ConnectionStatus::ConfigurationInvalid;
    };
    let ask = serde_json::json!([{ "role": "user", "content": format!("Reply with exactly {E2E_TOKEN} and nothing else.") }]);
    match vscode::chat_request(&env.client, &model, ask, false).await {
        Ok(v) if v.to_string().contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass, "The model answered."),
        Ok(v) => {
            run.push(5, "End-to-end", StageOutcome::Fail, format!("The model didn't answer as expected.\n\n{}", redact_known(&tail(&v.to_string(), 6), secrets)));
            return ConnectionStatus::RequiresAttention;
        }
        Err(e) => {
            let shown = redact_known(&e, secrets);
            run.push(5, "End-to-end", StageOutcome::Fail, shown.clone());
            return classify_agent_failure(&shown);
        }
    }
    let ask = serde_json::json!([{ "role": "user", "content": "Use the read_file tool to read notes.txt." }]);
    match vscode::chat_request(&env.client, &model, ask, true).await {
        Ok(v) if v["content"].as_array().is_some_and(|c| c.iter().any(|b| b["type"] == "tool_use" && b["name"] == "read_file")) => {
            run.push(6, "Tool calling", StageOutcome::Pass, "The model can use tools.");
            ConnectionStatus::ConnectedAndVerified
        }
        Ok(_) => {
            run.push(6, "Tool calling", StageOutcome::Fail,
                "The model can chat but couldn't use tools, so agent mode may not work. Try a different model.");
            ConnectionStatus::PartiallyCompatible
        }
        Err(e) => {
            run.push(6, "Tool calling", StageOutcome::Fail, redact_known(&e, secrets));
            ConnectionStatus::PartiallyCompatible
        }
    }
}

fn check_jetbrains_config(id: &str) -> Result<String, String> {
    use crate::adapters::jetbrains;
    for prefix in ["Claude Code (PrismFlow", "Codex (PrismFlow"] {
        let (cmd, _, env) = jetbrains::entry(id, prefix)
            .ok_or_else(|| format!("{} no longer has PrismFlow's agents.", jetbrains::acp_file().display()))?;
        if !Path::new(&cmd).is_file() {
            return Err(format!("{cmd} is missing. Install Node.js 22 or newer, then connect again."));
        }
        for dir in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"].iter().filter_map(|k| env.get(*k)) {
            if !Path::new(dir).is_dir() {
                return Err(format!("{dir} is missing. Connect again."));
            }
        }
    }
    Ok("acp.json has PrismFlow's agents.".into())
}

/// Stages 5–7 for JetBrains: talk to the agents exactly as written in
/// acp.json, the way the IDE does (ACP over stdio).
async fn jetbrains_stages(env: &VerifyEnv<'_>, provider: &ProviderDefinition, record: &ConnectionRecord, run: &mut Run, secrets: &[&str]) -> ConnectionStatus {
    use crate::adapters::{acp, jetbrains};
    let work = env.workdir.join(record.id.replace(':', "-"));
    if let Err(e) = std::fs::create_dir_all(&work) {
        run.push(5, "End-to-end", StageOutcome::Fail, e.to_string());
        return ConnectionStatus::RequiresAttention;
    }
    let Some((cmd, args, vars)) = jetbrains::entry(&record.id, "Claude Code (PrismFlow") else {
        run.push(5, "End-to-end", StageOutcome::Fail, "acp.json no longer has PrismFlow's agents. Connect again.");
        return ConnectionStatus::ConfigurationInvalid;
    };
    let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
    match acp::prompt(&cmd, &args, &vars, &work, &prompt, env.agent_timeout).await {
        Ok(r) if r.text.contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass, "The Claude Code agent answered."),
        Ok(r) => {
            let shown = redact_known(&tail(&r.text, 6), secrets);
            let hint = remediation(&r.text, provider).unwrap_or_else(|| "The Claude Code agent didn't answer as expected.".into());
            run.push(5, "End-to-end", StageOutcome::Fail, format!("{hint}\n\n{shown}"));
            return classify_agent_failure(&shown);
        }
        Err(e) => {
            let shown = redact_known(&e, secrets);
            run.push(5, "End-to-end", StageOutcome::Fail, shown.clone());
            return classify_agent_failure(&shown);
        }
    }
    let nonce = format!("PF{}", uuid::Uuid::new_v4().simple());
    let probe = work.join("prismflow_probe.txt");
    let _ = std::fs::write(&probe, format!("{nonce}\n"));
    let prompt = "Use your file-reading tool to read the file prismflow_probe.txt in the current directory, then reply with its exact contents and nothing else.";
    let tools = acp::prompt(&cmd, &args, &vars, &work, prompt, env.agent_timeout).await;
    let _ = std::fs::remove_file(&probe);
    let status = match tools {
        Ok(r) if r.text.contains(&nonce) => {
            run.push(6, "Tool calling", StageOutcome::Pass, "The model can use tools.");
            ConnectionStatus::ConnectedAndVerified
        }
        Ok(_) => {
            run.push(6, "Tool calling", StageOutcome::Fail,
                "The model can chat but couldn't use tools, so coding tasks may not work. Try a different model.");
            ConnectionStatus::PartiallyCompatible
        }
        Err(e) => {
            run.push(6, "Tool calling", StageOutcome::Fail, redact_known(&e, secrets));
            ConnectionStatus::PartiallyCompatible
        }
    };
    // The Codex agent shares the connection; check that it answers too.
    if let Some((cmd, args, vars)) = jetbrains::entry(&record.id, "Codex (PrismFlow") {
        let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
        match acp::prompt(&cmd, &args, &vars, &work, &prompt, env.agent_timeout).await {
            Ok(r) if r.text.contains(E2E_TOKEN) => run.push(7, "Codex agent", StageOutcome::Pass, "The Codex agent answered."),
            Ok(r) => run.push(7, "Codex agent", StageOutcome::Warn, format!("The Codex agent didn't answer as expected.\n\n{}", redact_known(&tail(&r.text, 6), secrets))),
            Err(e) => run.push(7, "Codex agent", StageOutcome::Warn, format!("The Codex agent didn't start.\n\n{}", redact_known(&e, secrets))),
        }
    }
    status
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
                return Err(format!("Your own Codex settings file has a mistake in it, so Codex won't start. Fix or remove it: {}", base.display()));
            }
        }
    }
    if codex_bin.is_none() {
        return Err("Codex isn't installed.".into());
    }
    Ok("Profile file is valid, PrismFlow-managed, and its credential helper exists.".into())
}

// The fixtures are POSIX shell scripts standing in for the agent CLIs.
#[cfg(all(test, unix))]
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
        let plan = codex::plan_connect_gateway(home.path(), or, "a/model", &helper, "c1", "http://127.0.0.1:1", None).unwrap();
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
            routing: Default::default(), priors: Default::default(), app_models: Vec::new(),
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
            state_dir: std::env::temp_dir(),
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

    /// The UI sees finished steps while the reply is slow, and Skip (dropping
    /// the test) stops the program it started and clears the progress.
    #[tokio::test]
    async fn slow_test_shows_progress_and_skip_stops_it() {
        let mut f = fixture(200, 200).await;
        f.record.id = "slow-progress".into();
        let pid_file = f._home.path().join("agent.pid");
        // The agent starts a program of its own (like an npm shim does).
        let body = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let mut fut = Box::pin(run(&f, Some(&body)));
        let seen = tokio::select! {
            _ = &mut fut => panic!("should still be waiting for the reply"),
            p = async { loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Some(p) = progress("slow-progress").filter(|p| p.len() >= 4 && pid_file.exists()) { break p; }
            } } => p,
        };
        assert!(seen.iter().any(|s| s.name == "Model"), "{seen:?}");
        drop(fut);
        assert!(progress("slow-progress").is_none());
        let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_string();
        let mut alive = true;
        for _ in 0..40 {
            alive = crate::spawn::command("kill").args(["-0", &pid]).status().unwrap().success();
            if !alive { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive, "the program the agent started ({pid}) was left running");
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
            restarts: 0, requests: 0, rejected: 0, upstream_errors: 0, shortcuts: 0, connections: 1, upstreams: vec!["nvidia_nim".into()],
        }
    }

    async fn run_claude_fixture(claude_body: &str, phase: Phase) -> VerificationReport {
        run_claude_fixture_with(claude_body, phase, true).await
    }

    async fn run_claude_fixture_with(claude_body: &str, phase: Phase, with_app_key: bool) -> VerificationReport {
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
        // The gateway's token counting, as the real broker answers it.
        Mock::given(method("POST")).and(path("/v1/messages/count_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "input_tokens": 12 })))
            .mount(&server).await;
        let store = InMemoryStore::new();
        store.set(&account_key("nvidia_nim", "default"), "nvapi-secretsecretsecret").unwrap();
        if with_app_key {
            store.set(&crate::gateway::tokens::account("claude_code:nvidia_nim"), "pft_fixture").unwrap();
        }
        let bin_dir = tempfile::tempdir().unwrap();
        let claude = bin_dir.path().join("claude");
        std::fs::write(&claude, format!("#!/bin/sh\n{claude_body}\n")).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let record = ConnectionRecord {
            id: "claude_code:nvidia_nim".into(), provider_id: "nvidia_nim".into(), app_id: claude_code::APP_ID.into(),
            model: "meta/llama-3.3-70b-instruct".into(), profile: claude_code::profile_name("nvidia_nim"),
            route: crate::state::Route::Gateway, managed_file: plan.file, created_at: 0,
            status: ConnectionStatus::Unverified, last_report: None, routing: Default::default(), priors: Default::default(), app_models: Vec::new(),
        };
        let work = tempfile::tempdir().unwrap();
        let env = VerifyEnv {
            client: discovery::client(), store: &store, codex_bin: None, claude_bin: Some(claude),
            gateway: Some(BrokerStatus { url: server.uri(), ..gateway_status(phase) }), base_url: server.uri(),
            workdir: work.path().join("verify"), agent_timeout: Duration::from_secs(10),
            state_dir: std::env::temp_dir(),
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
    async fn missing_app_key_is_named_before_running_the_app() {
        let r = run_claude_fixture_with(GOOD_CLAUDE, Phase::Ready, false).await;
        assert_eq!(r.status, ConnectionStatus::AuthenticationFailed, "{:#?}", r.stages);
        assert!(r.stages.last().unwrap().detail.contains("key is missing"));
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
