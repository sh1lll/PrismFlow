//! Application service: the operations the UI invokes, independent of Tauri
//! so they are testable with an in-memory credential store and temp dirs.

use crate::adapters::{self, claude_code, codex, AppDetection};
use crate::config_engine;
use crate::credentials::{account_key, CredentialError, CredentialStore};
use crate::diagnostics::{now_secs, DiagnosticsLog, Event, Level};
use crate::discovery::{self, ModelInfo};
use crate::gateway::supervisor::{engine_type, GatewayRuntime, UpstreamSpec};
use crate::gateway::{tokens, Binding, Broker, BrokerStatus};
use crate::providers::{self, Protocol, ProviderDefinition};
use crate::state::{ConnectionRecord, ConnectionStatus, Route, StateStore, VerificationReport};
use crate::verify::{self, VerifyEnv};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct ProviderView {
    #[serde(flatten)]
    pub def: ProviderDefinition,
    pub has_credential: bool,
    /// Codex can talk to this provider without the gateway.
    pub codex_direct: bool,
    /// How each app would reach this provider, or None when it cannot.
    pub routes: BTreeMap<&'static str, Option<Route>>,
}

#[derive(Debug, Serialize)]
pub struct ConnectPreview {
    pub app_id: String,
    pub route: Route,
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
    pub diag: Arc<DiagnosticsLog>,
    pub client: reqwest::Client,
    pub codex_home: PathBuf,
    pub claude_dir: PathBuf,
    /// Absolute path of the binary that serves `credential print` / `token print`.
    pub helper: PathBuf,
    pub agent_timeout: Duration,
    pub broker: Broker,
    /// Test hook: upstream base URL per provider id (mock servers).
    pub upstream_overrides: HashMap<String, String>,
}

type R<T> = Result<T, String>;

const APPS: &[&str] = &[codex::APP_ID, claude_code::APP_ID];

fn provider(id: &str) -> R<&'static ProviderDefinition> {
    providers::get(id).ok_or_else(|| format!("Unknown provider '{id}'."))
}

fn codex_direct(p: &ProviderDefinition) -> bool {
    p.protocols.contains(&Protocol::OpenAiResponses) && !p.base_url.is_empty()
}

/// The matrix decision for an (app, provider) pair; None = unsupported.
pub fn route_for(app_id: &str, p: &ProviderDefinition) -> Option<Route> {
    if p.base_url.is_empty() {
        return None; // custom providers wait for URL validation + consent (SPEC §6.3)
    }
    match app_id {
        codex::APP_ID if codex_direct(p) => Some(Route::Direct),
        codex::APP_ID => Some(Route::Gateway),
        claude_code::APP_ID => Some(Route::Gateway),
        _ => None,
    }
}

pub fn connection_id(app_id: &str, provider_id: &str) -> String {
    format!("{app_id}:{provider_id}")
}

fn app_name(app_id: &str) -> &'static str {
    if app_id == claude_code::APP_ID { "Claude Code" } else { "Codex" }
}

fn validate_model_id(model: &str) -> R<&str> {
    let m = model.trim();
    if m.is_empty() || m.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"') {
        return Err("Enter a model ID without spaces or quotes, e.g. openai/gpt-oss-120b.".into());
    }
    Ok(m)
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Shell- and AppleScript-safe launch line for a Terminal window.
pub fn launch_command(codex_bin: &str, profile: &str) -> String {
    format!("{} -p {profile}", quote(codex_bin))
}

pub fn claude_launch_command(claude_bin: &str, settings: &str) -> String {
    format!("{} --settings {}", quote(claude_bin), quote(settings))
}

impl Service {
    fn key(&self, provider_id: &str) -> Option<String> {
        self.store.get(&account_key(provider_id, "default")).ok()
    }

    fn upstream_url(&self, p: &ProviderDefinition) -> String {
        self.upstream_overrides.get(p.id).cloned().unwrap_or_else(|| p.base_url.to_string())
    }

    pub fn providers(&self) -> Vec<ProviderView> {
        providers::registry()
            .iter()
            .map(|def| ProviderView {
                def: def.clone(),
                has_credential: self.key(def.id).is_some(),
                codex_direct: codex_direct(def),
                routes: APPS.iter().map(|a| (*a, route_for(a, def))).collect(),
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

    /// True when a gateway connection uses this provider (its engine must
    /// restart to pick up a changed key).
    pub fn provider_in_gateway(&self, provider_id: &str) -> bool {
        self.state.load().unwrap_or_default().iter().any(|r| r.route == Route::Gateway && r.provider_id == provider_id)
    }

    pub fn delete_credential(&self, provider_id: &str) -> R<()> {
        let p = provider(provider_id)?;
        let users: Vec<String> = self
            .state
            .load()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|r| r.provider_id == p.id)
            .map(|r| format!("{} ({})", r.profile, app_name(&r.app_id)))
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
        discovery::list_models(&self.client, &self.upstream_url(p), path, key.as_deref())
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

    fn plan(&self, app_id: &str, provider_id: &str, model: &str) -> R<(&'static ProviderDefinition, Route, config_engine::EditPlan)> {
        let p = provider(provider_id)?;
        let model = validate_model_id(model)?;
        let route = route_for(app_id, p).ok_or_else(|| match app_id {
            codex::APP_ID | claude_code::APP_ID => format!(
                "{} needs a base URL; custom providers are not supported in this build.", p.display_name
            ),
            other => format!("Unknown application '{other}'."),
        })?;
        let id = connection_id(app_id, p.id);
        let url = self.broker.url_for_config();
        let plan = match (app_id, route) {
            (codex::APP_ID, Route::Direct) => codex::plan_connect(&self.codex_home, p, model, &self.helper).map_err(|e| e.to_string())?,
            (codex::APP_ID, Route::Gateway) => codex::plan_connect_gateway(&self.codex_home, p, model, &self.helper, &id, &url)
                .map_err(|e| e.to_string())?,
            _ => claude_code::plan_connect(&self.claude_dir, p, model, &self.helper, &id, &url).map_err(|e| e.to_string())?,
        };
        Ok((p, route, plan))
    }

    fn launch_line(&self, app_id: &str, profile: &str, file: &std::path::Path) -> String {
        if app_id == claude_code::APP_ID {
            let bin = claude_code::find_binary().map(|b| b.display().to_string()).unwrap_or_else(|| "claude".into());
            claude_launch_command(&bin, &file.display().to_string())
        } else {
            let bin = codex::find_binary().map(|b| b.display().to_string()).unwrap_or_else(|| "codex".into());
            launch_command(&bin, profile)
        }
    }

    pub fn preview(&self, app_id: &str, provider_id: &str, model: &str) -> R<ConnectPreview> {
        let (p, route, plan) = self.plan(app_id, provider_id, model)?;
        let profile = if app_id == claude_code::APP_ID { claude_code::profile_name(p.id) } else { codex::profile_name(p.id) };
        let mut warnings = vec![format!(
            "Requests from this {} profile are billed to your {} account.", app_name(app_id), p.display_name
        )];
        if app_id == claude_code::APP_ID {
            warnings.push("Your own ~/.claude/settings.json is not modified; this connection applies only when Claude Code runs with the PrismFlow settings file.".into());
        } else {
            warnings.push("Your own ~/.codex/config.toml is not modified; this connection only applies when Codex runs with the PrismFlow profile.".into());
        }
        match route {
            Route::Gateway => {
                warnings.push(format!(
                    "Requests go through PrismFlow's local gateway ({}), which translates between protocols. Keep PrismFlow running while you use this connection.",
                    self.broker.url_for_config()
                ));
                warnings.push(format!(
                    "{} receives only a revocable local token; your {} key never leaves PrismFlow.", app_name(app_id), p.display_name
                ));
                if let Err(e) = GatewayRuntime::discover() {
                    warnings.insert(0, format!("The gateway cannot start yet: {e}"));
                }
            }
            Route::Direct => warnings.push(
                "Codex asks PrismFlow's helper for the key at runtime. Any program running as your user can run that helper too.".into(),
            ),
        }
        if self.key(p.id).is_none() {
            warnings.insert(0, format!("No {} key is stored yet; add one before connecting.", p.display_name));
        }
        Ok(ConnectPreview {
            app_id: app_id.into(),
            route,
            launch_command: self.launch_line(app_id, &profile, &plan.file),
            file: plan.file.display().to_string(),
            before: plan.before,
            after: plan.after,
            summary: plan.summary,
            warnings,
        })
    }

    pub async fn connect(&self, app_id: &str, provider_id: &str, model: &str) -> R<ConnectionRecord> {
        let (p, route, plan) = self.plan(app_id, provider_id, model)?;
        if self.key(p.id).is_none() {
            return Err(format!("Add a {} key under Providers first.", p.display_name));
        }
        if route == Route::Gateway {
            GatewayRuntime::discover().map_err(|e| format!("Nothing was changed: {e}"))?;
        }
        let id = connection_id(app_id, p.id);
        if route == Route::Gateway {
            tokens::ensure(self.store.as_ref(), &id).map_err(|e| format!("Could not create a local token: {e}"))?;
        }
        let applied = config_engine::apply(&plan).map_err(|e| {
            let msg = format!("Writing {} failed; nothing was changed: {e}", plan.file.display());
            self.diag.record(Level::Error, "connect.failed", &msg);
            msg
        })?;
        // The engine backs up our own previous file next to it; move that
        // into the state dir so the app's config dir holds only the live file.
        if let Some(b) = applied.backup_path {
            let dest = self.state.backups_dir().join(b.file_name().unwrap_or_default());
            if std::fs::rename(&b, &dest).is_err() {
                let _ = std::fs::remove_file(&b);
            }
        }
        let profile = if app_id == claude_code::APP_ID { claude_code::profile_name(p.id) } else { codex::profile_name(p.id) };
        let record = ConnectionRecord {
            id: id.clone(),
            provider_id: p.id.into(),
            app_id: app_id.into(),
            model: validate_model_id(model)?.into(),
            profile,
            route,
            managed_file: plan.file.clone(),
            created_at: now_secs(),
            status: ConnectionStatus::Unverified,
            last_report: None,
        };
        self.state.upsert(record.clone()).map_err(|e| e.to_string())?;
        self.diag.record(Level::Info, "connect.applied", &format!(
            "Wrote {} for {} → {} with model {} ({:?} route).", plan.file.display(), app_name(app_id), p.display_name, record.model, route
        ));
        if route == Route::Gateway {
            if let Err(e) = self.sync_gateway().await {
                self.diag.record(Level::Error, "connect.gateway", &format!("{id} is configured, but the gateway is not running: {e}"));
            }
        }
        Ok(record)
    }

    /// Bring the broker in line with the gateway connections in state:
    /// start it, reconfigure it, or stop it when none remain (SPEC §13.6).
    pub async fn sync_gateway(&self) -> R<BrokerStatus> {
        let records: Vec<ConnectionRecord> = self
            .state
            .load()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|r| r.route == Route::Gateway)
            .collect();
        if records.is_empty() {
            self.broker.stop().await;
            return Ok(self.broker.status());
        }
        let mut models: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut bindings = Vec::new();
        for r in &records {
            let token = tokens::ensure(self.store.as_ref(), &r.id).map_err(|e| format!("Secure store error: {e}"))?;
            bindings.push((token, Binding { connection_id: r.id.clone(), provider_id: r.provider_id.clone() }));
            let m = models.entry(r.provider_id.clone()).or_default();
            if !m.contains(&r.model) {
                m.push(r.model.clone());
            }
        }
        let mut specs = Vec::new();
        for (provider_id, models) in models {
            let p = provider(&provider_id)?;
            let Some(key) = self.key(p.id) else {
                self.diag.record(Level::Warn, "gateway.no_key", &format!(
                    "{} has gateway connections but no stored key; its requests will fail until a key is added.", p.display_name
                ));
                continue;
            };
            specs.push(UpstreamSpec {
                provider_id: p.id.into(),
                engine_type: engine_type(p),
                base_url: self.upstream_url(p),
                models,
                key,
            });
        }
        self.broker.start(specs, bindings).await.map_err(|e| e.to_string())
    }

    pub fn gateway_status(&self) -> BrokerStatus {
        self.broker.status()
    }

    pub async fn restart_gateway(&self) -> R<BrokerStatus> {
        self.broker.stop().await;
        self.sync_gateway().await
    }

    pub async fn verify(&self, id: &str) -> R<VerificationReport> {
        let mut record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let p = provider(&record.provider_id)?;
        if record.route == Route::Gateway && !self.broker.is_running().await {
            // Verification must exercise the live path; bring it up first.
            let _ = self.sync_gateway().await;
        }
        let env = VerifyEnv {
            client: self.client.clone(),
            store: self.store.as_ref(),
            codex_bin: codex::find_binary(),
            claude_bin: claude_code::find_binary(),
            base_url: self.upstream_url(p),
            workdir: self.state.dir().join("verify"),
            agent_timeout: self.agent_timeout,
            gateway: (record.route == Route::Gateway).then(|| self.broker.status()),
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

    pub async fn disconnect(&self, id: &str) -> R<Option<String>> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let removed = if record.app_id == claude_code::APP_ID {
            claude_code::disconnect(&record.managed_file, &self.state.backups_dir()).map_err(|e| e.to_string())
        } else {
            codex::disconnect(&record.managed_file, &self.state.backups_dir()).map_err(|e| e.to_string())
        };
        let backup = removed.map_err(|e| {
            let msg = format!("Disconnect stopped; nothing was removed: {e}");
            self.diag.record(Level::Error, "disconnect.failed", &msg);
            msg
        })?;
        if record.route == Route::Gateway {
            tokens::revoke(self.store.as_ref(), id).map_err(|e| format!("Could not revoke the local token: {e}"))?;
        }
        self.state.remove(id).map_err(|e| e.to_string())?;
        self.diag.record(Level::Info, "disconnect.done", &format!(
            "Removed {}. Your own {} configuration was never modified.", record.managed_file.display(), app_name(&record.app_id)
        ));
        if record.route == Route::Gateway {
            if let Err(e) = self.sync_gateway().await {
                self.diag.record(Level::Warn, "gateway.sync", &e);
            }
        }
        Ok(backup.map(|b| b.display().to_string()))
    }

    pub fn connections(&self) -> R<Vec<ConnectionRecord>> {
        self.state.load().map_err(|e| e.to_string())
    }

    pub fn diagnostics(&self, limit: usize) -> Vec<Event> {
        self.diag.recent(limit)
    }

    /// Write a sanitized export (events + connection state + gateway status;
    /// no secrets exist in any of them) to the state dir and return its path.
    pub fn export_diagnostics(&self) -> R<String> {
        let export = serde_json::json!({
            "app_version": env!("CARGO_PKG_VERSION"),
            "exported_at": now_secs(),
            "applications": self.applications(),
            "connections": self.connections()?,
            "gateway": self.gateway_status(),
            "events": self.diagnostics(500),
        });
        let path = self.state.dir().join(format!("diagnostics-export-{}.json", now_secs()));
        let body = crate::diagnostics::redact(&serde_json::to_string_pretty(&export).map_err(|e| e.to_string())?);
        config_engine::atomic_write(&path, &body).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    pub fn launch(&self, id: &str) -> R<()> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        if record.app_id == claude_code::APP_ID {
            claude_code::find_binary().ok_or("Claude Code is not installed.")?;
        } else {
            codex::find_binary().ok_or("Codex CLI is not installed.")?;
        }
        let line = self.launch_line(&record.app_id, &record.profile, &record.managed_file);
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
        let claude_dir = dir.join("claude");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();
        let diag = Arc::new(DiagnosticsLog::new(state_dir.join("diagnostics.jsonl")));
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        Service {
            store: Box::new(InMemoryStore::new()),
            state: StateStore::open(&state_dir).unwrap(),
            broker: Broker::new(port, state_dir.clone(), Some(diag.clone())),
            diag,
            client: discovery::client(),
            codex_home,
            claude_dir,
            helper: PathBuf::from("/bin/echo"),
            agent_timeout: Duration::from_secs(5),
            upstream_overrides: HashMap::new(),
        }
    }

    const CODEX: &str = codex::APP_ID;
    const CLAUDE: &str = claude_code::APP_ID;

    #[tokio::test]
    async fn connect_requires_a_key_then_records_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        assert!(s.connect(CODEX, "openrouter", "a/b").await.unwrap_err().contains("key"));
        s.set_credential("openrouter", "  sk-or-v1-abcdefabcdef  ").unwrap();
        let rec = s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        assert_eq!(rec.status, ConnectionStatus::Unverified);
        assert_eq!(rec.route, Route::Direct);
        assert!(rec.managed_file.exists());
        assert_eq!(s.connections().unwrap().len(), 1);
        assert!(s.providers().iter().any(|p| p.def.id == "openrouter" && p.has_credential));
        assert!(!s.broker.is_running().await, "a direct connection must not start the gateway");
    }

    #[test]
    fn routing_matrix_matches_the_compatibility_matrix() {
        let get = |id| providers::get(id).unwrap();
        assert_eq!(route_for(CODEX, get("openrouter")), Some(Route::Direct));
        assert_eq!(route_for(CODEX, get("nvidia_nim")), Some(Route::Gateway));
        assert_eq!(route_for(CLAUDE, get("openrouter")), Some(Route::Gateway));
        assert_eq!(route_for(CLAUDE, get("nvidia_nim")), Some(Route::Gateway));
        assert_eq!(route_for(CLAUDE, get("custom_openai")), None);
        assert_eq!(route_for("vscode", get("openrouter")), None);
    }

    #[tokio::test]
    async fn key_never_reaches_disk_or_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-supersecretvalue1").unwrap();
        s.connect(CODEX, "openrouter", "a/b").await.unwrap();
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

    #[tokio::test]
    async fn disconnect_restores_codex_home_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let user_cfg = s.codex_home.join("config.toml");
        std::fs::write(&user_cfg, "# hand edited\nmodel = \"x\"\n").unwrap();
        let before: Vec<_> = walk(&s.codex_home);
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        let rec = s.connect(CODEX, "openrouter", "c/d").await.unwrap(); // switch model
        let backup = s.disconnect(&rec.id).await.unwrap().unwrap();
        assert!(std::path::Path::new(&backup).exists());
        assert_eq!(walk(&s.codex_home), before);
        assert_eq!(std::fs::read_to_string(user_cfg).unwrap(), "# hand edited\nmodel = \"x\"\n");
        assert!(s.connections().unwrap().is_empty());
    }

    /// Claude Code through the gateway, end to end at the service layer:
    /// connect writes only our settings file and mints a token, the broker
    /// comes up and serves that token, and disconnect revokes the token, stops the
    /// gateway and leaves the Claude config dir byte-identical.
    #[tokio::test]
    async fn claude_code_gateway_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let user = s.claude_dir.join("settings.json");
        std::fs::write(&user, "{\"theme\":\"dark\"}\n").unwrap();
        let before: Vec<(PathBuf, String)> = walk(&s.claude_dir).into_iter()
            .map(|p| { let c = std::fs::read_to_string(&p).unwrap(); (p, c) }).collect();
        s.set_credential("nvidia_nim", "nvapi-supersecretvalue2").unwrap();

        let rec = s.connect(CLAUDE, "nvidia_nim", "meta/llama-3.3-70b-instruct").await.unwrap();
        assert_eq!(rec.route, Route::Gateway);
        assert_eq!(rec.id, "claude_code:nvidia_nim");
        let token = s.store.get(&tokens::account(&rec.id)).expect("token minted");
        let status = s.gateway_status();
        assert_eq!(status.phase, crate::gateway::Phase::Ready, "{status:?}");
        assert_eq!(status.connections, 1);

        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&rec.managed_file).unwrap()).unwrap();
        assert_eq!(settings["env"]["ANTHROPIC_BASE_URL"], s.broker.url());
        // The broker accepts exactly that token.
        let ok = reqwest::Client::builder().no_proxy().build().unwrap()
            .get(format!("{}/v1/models", s.broker.url())).header("x-api-key", &token).send().await.unwrap();
        assert_eq!(ok.status(), 200);

        for f in walk(dir.path()) {
            let c = std::fs::read_to_string(&f).unwrap_or_default();
            assert!(!c.contains("supersecretvalue2"), "upstream key in {}", f.display());
            assert!(!c.contains(&token[4..]), "local token in {}", f.display());
        }

        s.disconnect(&rec.id).await.unwrap();
        assert!(s.store.get(&tokens::account(&rec.id)).is_err(), "token must be revoked");
        assert_eq!(s.gateway_status().phase, crate::gateway::Phase::Stopped, "last gateway connection stops the gateway");
        let after: Vec<(PathBuf, String)> = walk(&s.claude_dir).into_iter()
            .map(|p| { let c = std::fs::read_to_string(&p).unwrap(); (p, c) }).collect();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn codex_reaches_chat_only_provider_through_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("nvidia_nim", "nvapi-abcdefabcdef1234").unwrap();
        let preview = s.preview(CODEX, "nvidia_nim", "meta/llama-3.3-70b-instruct").unwrap();
        assert_eq!(preview.route, Route::Gateway);
        assert!(preview.warnings.iter().any(|w| w.contains("gateway")));
        let rec = s.connect(CODEX, "nvidia_nim", "meta/llama-3.3-70b-instruct").await.unwrap();
        assert_eq!(rec.route, Route::Gateway);
        let profile = std::fs::read_to_string(&rec.managed_file).unwrap();
        assert!(profile.contains(&format!("{}/v1", s.broker.url())));
        assert!(profile.contains("\"token\""));
        assert_eq!(s.gateway_status().phase, crate::gateway::Phase::Ready);
        s.disconnect(&rec.id).await.unwrap();
    }

    #[tokio::test]
    async fn key_with_live_connection_cannot_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        let rec = s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        assert!(s.delete_credential("openrouter").is_err());
        s.disconnect(&rec.id).await.unwrap();
        s.delete_credential("openrouter").unwrap();
        assert!(!s.providers().iter().any(|p| p.has_credential));
    }

    #[test]
    fn rejects_bad_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        assert!(s.set_credential("openrouter", "two words").is_err());
        assert!(s.set_credential("nope", "k").is_err());
        assert!(s.preview(CODEX, "openrouter", "has space").is_err());
        assert!(s.preview(CLAUDE, "custom_openai", "m").unwrap_err().contains("base URL"));
        assert!(s.preview("vscode", "openrouter", "m").is_err());
    }

    #[tokio::test]
    async fn new_key_marks_connections_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        let mut rec = s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        rec.status = ConnectionStatus::ConnectedAndVerified;
        s.state.upsert(rec.clone()).unwrap();
        s.set_credential("openrouter", "sk-or-v1-ghijklghijkl").unwrap();
        assert_eq!(s.state.get(&rec.id).unwrap().unwrap().status, ConnectionStatus::Unverified);
    }

    #[test]
    fn launch_commands_quote_paths() {
        assert_eq!(launch_command("/a b/codex", "prismflow-openrouter"), "'/a b/codex' -p prismflow-openrouter");
        assert_eq!(launch_command("/it's/codex", "p"), r"'/it'\''s/codex' -p p");
        assert_eq!(claude_launch_command("/x/claude", "/h/.claude/p.settings.json"), "'/x/claude' --settings '/h/.claude/p.settings.json'");
    }
}
