//! Application service: the operations the UI invokes, independent of Tauri
//! so they are testable with an in-memory credential store and temp dirs.

use crate::adapters::{self, codex, AppDetection};
use crate::config_engine;
use crate::credentials::{account_key, CredentialError, CredentialStore};
use crate::diagnostics::{now_secs, DiagnosticsLog, Event, Level};
use crate::discovery::{self, ModelInfo};
use crate::providers::{self, Protocol, ProviderDefinition};
use crate::state::{ConnectionRecord, ConnectionStatus, StateStore, VerificationReport};
use crate::verify::{self, VerifyEnv};
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct ProviderView {
    #[serde(flatten)]
    pub def: ProviderDefinition,
    pub has_credential: bool,
    /// Codex needs the Responses API and a known base URL (no gateway yet).
    pub codex_direct: bool,
}

#[derive(Debug, Serialize)]
pub struct ConnectPreview {
    pub file: String,
    pub before: String,
    pub after: String,
    pub summary: Vec<String>,
    pub warnings: Vec<String>,
    pub launch_command: String,
}

pub struct Service {
    pub store: Box<dyn CredentialStore>,
    pub state: StateStore,
    pub diag: DiagnosticsLog,
    pub client: reqwest::Client,
    pub codex_home: PathBuf,
    /// Absolute path of the binary that serves `credential print`.
    pub helper: PathBuf,
    pub agent_timeout: Duration,
}

type R<T> = Result<T, String>;

fn provider(id: &str) -> R<&'static ProviderDefinition> {
    providers::get(id).ok_or_else(|| format!("Unknown provider '{id}'."))
}

fn codex_direct(p: &ProviderDefinition) -> bool {
    p.protocols.contains(&Protocol::OpenAiResponses) && !p.base_url.is_empty()
}

fn connection_id(provider_id: &str) -> String {
    format!("{}:{provider_id}", codex::APP_ID)
}

fn validate_model_id(model: &str) -> R<&str> {
    let m = model.trim();
    if m.is_empty() || m.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"') {
        return Err("Enter a model ID without spaces or quotes, e.g. openai/gpt-oss-120b.".into());
    }
    Ok(m)
}

/// Shell- and AppleScript-safe launch line for a Terminal window.
pub fn launch_command(codex_bin: &str, profile: &str) -> String {
    format!("'{}' -p {profile}", codex_bin.replace('\'', r"'\''"))
}

impl Service {
    fn key(&self, provider_id: &str) -> Option<String> {
        self.store.get(&account_key(provider_id, "default")).ok()
    }

    pub fn providers(&self) -> Vec<ProviderView> {
        providers::registry()
            .iter()
            .map(|def| ProviderView {
                def: def.clone(),
                has_credential: self.key(def.id).is_some(),
                codex_direct: codex_direct(def),
            })
            .collect()
    }

    pub fn set_credential(&self, provider_id: &str, secret: &str) -> R<()> {
        let p = provider(provider_id)?;
        let secret = secret.trim();
        if secret.is_empty() || secret.chars().any(char::is_whitespace) {
            return Err("Paste the key exactly as issued: one token, no spaces.".into());
        }
        self.store
            .set(&account_key(p.id, "default"), secret)
            .map_err(|e| format!("Could not save to the secure store: {e}"))?;
        // A new key invalidates earlier verification results.
        let mut records = self.state.load().map_err(|e| e.to_string())?;
        for r in records.iter_mut().filter(|r| r.provider_id == p.id) {
            r.status = ConnectionStatus::Unverified;
        }
        self.state.save(&records).map_err(|e| e.to_string())?;
        self.diag.record(Level::Info, "credential.saved", &format!("Saved a {} key to the secure store.", p.display_name));
        Ok(())
    }

    pub fn delete_credential(&self, provider_id: &str) -> R<()> {
        let p = provider(provider_id)?;
        let users: Vec<String> = self
            .state
            .load()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|r| r.provider_id == p.id)
            .map(|r| r.profile)
            .collect();
        if !users.is_empty() {
            return Err(format!("Disconnect {} first; it uses this key.", users.join(", ")));
        }
        match self.store.delete(&account_key(p.id, "default")) {
            Ok(()) | Err(CredentialError::NotFound(_)) => {}
            Err(e) => return Err(format!("Could not remove the key: {e}")),
        }
        self.diag.record(Level::Info, "credential.deleted", &format!("Removed the {} key.", p.display_name));
        Ok(())
    }

    pub async fn discover_models(&self, provider_id: &str) -> R<Vec<ModelInfo>> {
        let p = provider(provider_id)?;
        let path = p.model_list_path.ok_or("This provider has no model list; enter a model ID manually.")?;
        if p.base_url.is_empty() {
            return Err("Custom providers are not supported in this build.".into());
        }
        let key = self.key(p.id);
        discovery::list_models(&self.client, p.base_url, path, key.as_deref())
            .await
            .map_err(|e| {
                let msg = format!("Model discovery for {} failed: {e}", p.display_name);
                self.diag.record(Level::Warn, "discovery.failed", &msg);
                msg
            })
    }

    pub fn applications(&self) -> Vec<AppDetection> {
        adapters::detect_all()
    }

    fn plan(&self, provider_id: &str, model: &str) -> R<(&'static ProviderDefinition, config_engine::EditPlan)> {
        let p = provider(provider_id)?;
        let model = validate_model_id(model)?;
        let plan = codex::plan_connect(&self.codex_home, p, model, &self.helper).map_err(|e| e.to_string())?;
        Ok((p, plan))
    }

    pub fn preview(&self, provider_id: &str, model: &str) -> R<ConnectPreview> {
        let (p, plan) = self.plan(provider_id, model)?;
        let codex_bin = codex::find_binary().map(|b| b.display().to_string()).unwrap_or_else(|| "codex".into());
        let mut warnings = vec![
            format!("Requests from this Codex profile are billed to your {} account, not to ChatGPT.", p.display_name),
            "Your own ~/.codex/config.toml is not modified; this connection only applies when Codex runs with the PrismFlow profile.".into(),
            "Codex asks PrismFlow's helper for the key at runtime. Any program running as your user can run that helper too; brokered tokens arrive with the gateway.".into(),
        ];
        if self.key(p.id).is_none() {
            warnings.insert(0, format!("No {} key is stored yet; add one before connecting.", p.display_name));
        }
        Ok(ConnectPreview {
            file: plan.file.display().to_string(),
            before: plan.before,
            after: plan.after,
            summary: plan.summary,
            warnings,
            launch_command: launch_command(&codex_bin, &codex::profile_name(p.id)),
        })
    }

    pub fn connect(&self, provider_id: &str, model: &str) -> R<ConnectionRecord> {
        let (p, plan) = self.plan(provider_id, model)?;
        if self.key(p.id).is_none() {
            return Err(format!("Add a {} key under Providers first.", p.display_name));
        }
        let applied = config_engine::apply(&plan).map_err(|e| {
            let msg = format!("Writing {} failed; nothing was changed: {e}", plan.file.display());
            self.diag.record(Level::Error, "connect.failed", &msg);
            msg
        })?;
        // The engine backs up our own previous profile next to it; move that
        // into the state dir so ~/.codex holds nothing but the live profile.
        if let Some(b) = applied.backup_path {
            let dest = self.state.backups_dir().join(b.file_name().unwrap_or_default());
            if std::fs::rename(&b, &dest).is_err() {
                let _ = std::fs::remove_file(&b);
            }
        }
        let record = ConnectionRecord {
            id: connection_id(p.id),
            provider_id: p.id.into(),
            app_id: codex::APP_ID.into(),
            model: validate_model_id(model)?.into(),
            profile: codex::profile_name(p.id),
            managed_file: plan.file.clone(),
            created_at: now_secs(),
            status: ConnectionStatus::Unverified,
            last_report: None,
        };
        self.state.upsert(record.clone()).map_err(|e| e.to_string())?;
        self.diag.record(Level::Info, "connect.applied", &format!(
            "Wrote {} for {} with model {}.", plan.file.display(), p.display_name, record.model
        ));
        Ok(record)
    }

    pub async fn verify(&self, id: &str) -> R<VerificationReport> {
        let mut record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let p = provider(&record.provider_id)?;
        let env = VerifyEnv {
            client: self.client.clone(),
            store: self.store.as_ref(),
            codex_bin: codex::find_binary(),
            base_url: p.base_url.into(),
            workdir: self.state.dir().to_path_buf(),
            agent_timeout: self.agent_timeout,
        };
        let report = verify::verify(&env, p, &record).await;
        record.status = report.status;
        record.last_report = Some(report.clone());
        self.state.upsert(record).map_err(|e| e.to_string())?;
        let level = match report.status {
            ConnectionStatus::ConnectedAndVerified => Level::Info,
            ConnectionStatus::PartiallyCompatible => Level::Warn,
            _ => Level::Error,
        };
        let failed = report.stages.iter().find(|s| s.outcome == crate::state::StageOutcome::Fail);
        self.diag.record(level, "verify.finished", &format!(
            "{id}: {:?}{}", report.status,
            failed.map(|s| format!(" (stage {} {}: {})", s.stage, s.name, s.detail)).unwrap_or_default()
        ));
        Ok(report)
    }

    pub fn disconnect(&self, id: &str) -> R<Option<String>> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let backup = codex::disconnect(&record.managed_file, &self.state.backups_dir()).map_err(|e| {
            let msg = format!("Disconnect stopped; nothing was removed: {e}");
            self.diag.record(Level::Error, "disconnect.failed", &msg);
            msg
        })?;
        self.state.remove(id).map_err(|e| e.to_string())?;
        self.diag.record(Level::Info, "disconnect.done", &format!(
            "Removed {}. Your own Codex config was never modified.", record.managed_file.display()
        ));
        Ok(backup.map(|b| b.display().to_string()))
    }

    pub fn connections(&self) -> R<Vec<ConnectionRecord>> {
        self.state.load().map_err(|e| e.to_string())
    }

    pub fn diagnostics(&self, limit: usize) -> Vec<Event> {
        self.diag.recent(limit)
    }

    /// Write a sanitized export (events + connection state; no secrets
    /// exist in either) to the state dir and return its path.
    pub fn export_diagnostics(&self) -> R<String> {
        let export = serde_json::json!({
            "app_version": env!("CARGO_PKG_VERSION"),
            "exported_at": now_secs(),
            "applications": self.applications(),
            "connections": self.connections()?,
            "events": self.diagnostics(500),
        });
        let path = self.state.dir().join(format!("diagnostics-export-{}.json", now_secs()));
        let body = crate::diagnostics::redact(&serde_json::to_string_pretty(&export).map_err(|e| e.to_string())?);
        config_engine::atomic_write(&path, &body).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    pub fn launch(&self, id: &str) -> R<()> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let bin = codex::find_binary().ok_or("Codex CLI is not installed.")?;
        let line = launch_command(&bin.display().to_string(), &record.profile);
        open_terminal(&line)?;
        self.diag.record(Level::Info, "launch", &format!("Opened Terminal with `{line}`."));
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn open_terminal(line: &str) -> R<()> {
    let script = line.replace('\\', "\\\\").replace('"', "\\\"");
    let status = std::process::Command::new("osascript")
        .args(["-e", &format!("tell application \"Terminal\" to do script \"{script}\"")])
        .args(["-e", "tell application \"Terminal\" to activate"])
        .status()
        .map_err(|e| format!("Could not open Terminal: {e}"))?;
    if status.success() { Ok(()) } else { Err("Terminal refused the launch request.".into()) }
}

#[cfg(not(target_os = "macos"))]
fn open_terminal(_line: &str) -> R<()> {
    Err("Launching a terminal is only implemented on macOS in this build; run the command shown instead.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::memory::InMemoryStore;

    fn service(dir: &std::path::Path) -> Service {
        let state_dir = dir.join("state");
        let codex_home = dir.join("codex");
        std::fs::create_dir_all(&codex_home).unwrap();
        Service {
            store: Box::new(InMemoryStore::new()),
            state: StateStore::open(&state_dir).unwrap(),
            diag: DiagnosticsLog::new(state_dir.join("diagnostics.jsonl")),
            client: discovery::client(),
            codex_home,
            helper: PathBuf::from("/bin/echo"),
            agent_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn connect_requires_a_key_then_records_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        assert!(s.connect("openrouter", "a/b").unwrap_err().contains("key"));
        s.set_credential("openrouter", "  sk-or-v1-abcdefabcdef  ").unwrap();
        let rec = s.connect("openrouter", "a/b").unwrap();
        assert_eq!(rec.status, ConnectionStatus::Unverified);
        assert!(rec.managed_file.exists());
        assert_eq!(s.connections().unwrap().len(), 1);
        assert!(s.providers().iter().any(|p| p.def.id == "openrouter" && p.has_credential));
    }

    #[test]
    fn key_never_reaches_disk_or_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-supersecretvalue1").unwrap();
        s.connect("openrouter", "a/b").unwrap();
        s.export_diagnostics().unwrap();
        for entry in walk(dir.path()) {
            let content = std::fs::read_to_string(&entry).unwrap_or_default();
            assert!(!content.contains("supersecretvalue1"), "secret found in {}", entry.display());
        }
    }

    fn walk(p: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(p).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() { out.extend(walk(&path)) } else { out.push(path) }
        }
        out
    }

    #[test]
    fn disconnect_restores_codex_home_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let user_cfg = s.codex_home.join("config.toml");
        std::fs::write(&user_cfg, "# hand edited\nmodel = \"x\"\n").unwrap();
        let before: Vec<_> = walk(&s.codex_home);
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        s.connect("openrouter", "a/b").unwrap();
        let rec = s.connect("openrouter", "c/d").unwrap(); // switch model
        let backup = s.disconnect(&rec.id).unwrap().unwrap();
        assert!(std::path::Path::new(&backup).exists());
        assert_eq!(walk(&s.codex_home), before);
        assert_eq!(std::fs::read_to_string(user_cfg).unwrap(), "# hand edited\nmodel = \"x\"\n");
        assert!(s.connections().unwrap().is_empty());
    }

    #[test]
    fn key_with_live_connection_cannot_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        let rec = s.connect("openrouter", "a/b").unwrap();
        assert!(s.delete_credential("openrouter").is_err());
        s.disconnect(&rec.id).unwrap();
        s.delete_credential("openrouter").unwrap();
        assert!(!s.providers().iter().any(|p| p.has_credential));
    }

    #[test]
    fn rejects_bad_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        assert!(s.set_credential("openrouter", "two words").is_err());
        assert!(s.set_credential("nope", "k").is_err());
        assert!(s.preview("openrouter", "has space").is_err());
        assert!(s.preview("nvidia_nim", "m").unwrap_err().contains("gateway"));
    }

    #[test]
    fn new_key_marks_connections_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        let mut rec = s.connect("openrouter", "a/b").unwrap();
        rec.status = ConnectionStatus::ConnectedAndVerified;
        s.state.upsert(rec.clone()).unwrap();
        s.set_credential("openrouter", "sk-or-v1-ghijklghijkl").unwrap();
        assert_eq!(s.state.get(&rec.id).unwrap().unwrap().status, ConnectionStatus::Unverified);
    }

    #[test]
    fn launch_command_quotes_paths() {
        assert_eq!(launch_command("/a b/codex", "prismflow-openrouter"), "'/a b/codex' -p prismflow-openrouter");
        assert_eq!(launch_command("/it's/codex", "p"), r"'/it'\''s/codex' -p p");
    }
}
