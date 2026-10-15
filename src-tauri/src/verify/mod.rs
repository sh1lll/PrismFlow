//! Staged connection verification (SPEC §9). A connection is reported as
//! verified only when a real request has gone through the real integration
//! path; writing a file or reaching an endpoint is never enough.

use crate::adapters::codex;
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
pub fn classify_agent_failure(output: &str) -> ConnectionStatus {
    let o = output.to_lowercase();
    if o.contains("error loading config") {
        ConnectionStatus::ConfigurationInvalid
    } else if o.contains("401") || o.contains("unauthorized") || o.contains("invalid api key") || o.contains("no credential stored") {
        ConnectionStatus::AuthenticationFailed
    } else if o.contains("model_not_found")
        || o.contains("not a valid model")
        || o.contains("no endpoints found")
        || (o.contains("model") && o.contains("does not exist"))
    {
        ConnectionStatus::ModelUnavailable
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
    cmd.arg("exec").arg("-p").arg(profile);
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
    let stage1 = check_config(&record.managed_file, env.codex_bin.as_deref());
    match stage1 {
        Ok(detail) => run.push(1, "Configuration", StageOutcome::Pass, detail),
        Err(detail) => {
            run.push(1, "Configuration", StageOutcome::Fail, detail);
            return ConnectionStatus::ConfigurationInvalid;
        }
    }

    // Stage 2 — credential validation.
    let key = match env.store.get(&account_key(provider.id, "default")) {
        Ok(k) => k,
        Err(_) => {
            run.push(2, "Credential", StageOutcome::Fail, format!(
                "No {} key is stored. Add it under Providers.", provider.display_name
            ));
            return ConnectionStatus::NeedsCredentials;
        }
    };
    let secrets = [key.as_str()];
    match provider.auth_check_path {
        Some(path) => match discovery::check_credential(&env.client, &env.base_url, path, &key).await {
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
        Some(path) => match discovery::list_models(&env.client, &env.base_url, path, Some(&key)).await {
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

    // Stage 5 — end-to-end request through Codex itself.
    let Some(bin) = env.codex_bin.as_deref() else {
        run.push(5, "End-to-end", StageOutcome::Fail, "Codex CLI is not installed.");
        return ConnectionStatus::RequiresAttention;
    };
    let prompt = format!("Reply with exactly {E2E_TOKEN} and nothing else.");
    match run_codex(bin, &record.profile, &env.workdir, false, &prompt, env.agent_timeout).await {
        Ok((true, out)) if out.contains(E2E_TOKEN) => run.push(5, "End-to-end", StageOutcome::Pass,
            format!("Codex answered through the {} profile.", record.profile)),
        Ok((_, out)) => {
            let shown = redact_known(&tail(&out, 6), &secrets);
            run.push(5, "End-to-end", StageOutcome::Fail, format!("Codex did not return the expected reply:\n{shown}"));
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
        let plan = codex::plan_connect(home.path(), or, "a/model", &helper).unwrap();
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
            managed_file: plan.file,
            created_at: 0,
            status: ConnectionStatus::Unverified,
            last_report: None,
        };
        Fixture { _home: home, server, record, store }
    }

    async fn run(f: &Fixture, codex_body: Option<&str>) -> VerificationReport {
        let bin_dir = tempfile::tempdir().unwrap();
        let env = VerifyEnv {
            client: discovery::client(),
            store: &f.store,
            codex_bin: codex_body.map(|b| fake_codex(bin_dir.path(), b)),
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

    #[test]
    fn classifies_common_agent_errors() {
        assert_eq!(classify_agent_failure("No endpoints found for x/y"), ConnectionStatus::ModelUnavailable);
        assert_eq!(classify_agent_failure("Error loading config.toml: bad"), ConnectionStatus::ConfigurationInvalid);
        assert_eq!(classify_agent_failure("high demand"), ConnectionStatus::RequiresAttention);
    }
}
