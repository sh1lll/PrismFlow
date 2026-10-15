//! Application service: the operations the UI invokes, independent of Tauri
//! so they are testable with an in-memory credential store and temp dirs.

use crate::adapters::{self, agents, claude_code, claude_desktop, codex, AppDetection};
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
    pub key_optional: bool,
    #[serde(flatten)]
    pub def: ProviderDefinition,
    pub has_credential: bool,
    /// Codex can talk to this provider without the gateway.
    pub codex_direct: bool,
    /// How each app would reach this provider, or None when it cannot.
    pub routes: BTreeMap<&'static str, Option<Route>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UninstallOutcome {
    Planned,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct UninstallItem {
    pub what: String,
    pub outcome: UninstallOutcome,
    pub detail: String,
}

/// SPEC §23. `complete` is false when a step failed; nothing after a failed
/// restore is removed, so the user keeps keys and state to fix it.
#[derive(Debug, Clone, Serialize)]
pub struct UninstallReport {
    pub dry_run: bool,
    pub complete: bool,
    pub items: Vec<UninstallItem>,
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

/// Every application PrismFlow can connect.
fn app_ids() -> Vec<&'static str> {
    let mut v = vec![codex::APP_ID, codex::DESKTOP_APP_ID, claude_code::APP_ID, claude_desktop::APP_ID];
    v.extend(agents::AGENTS.iter().map(|a| a.id));
    v
}

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
        codex::APP_ID if codex_direct(p) && p.requires_key => Some(Route::Direct),
        codex::APP_ID => Some(Route::Gateway),
        codex::DESKTOP_APP_ID if codex_direct(p) && p.requires_key => Some(Route::Direct),
        codex::DESKTOP_APP_ID => Some(Route::Gateway),
        claude_code::APP_ID => Some(Route::Gateway),
        claude_desktop::APP_ID => Some(Route::Gateway),
        a if agents::spec(a).is_some() => Some(Route::Gateway),
        _ => None,
    }
}

pub fn connection_id(app_id: &str, provider_id: &str) -> String {
    format!("{app_id}:{provider_id}")
}

fn app_name(app_id: &str) -> &'static str {
    if let Some(a) = agents::spec(app_id) {
        return a.display_name;
    }
    match app_id {
        claude_code::APP_ID => "Claude Code",
        codex::DESKTOP_APP_ID => "Codex Desktop",
        claude_desktop::APP_ID => "Claude Desktop",
        _ => "Codex",
    }
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

    /// The key to use for `p`: the stored one, or a placeholder for
    /// providers that need none (local servers). None = missing and required.
    fn usable_key(&self, p: &ProviderDefinition) -> Option<String> {
        self.key(p.id).or_else(|| (!p.requires_key).then(|| "no-key-required".to_string()))
    }

    fn upstream_url(&self, p: &ProviderDefinition) -> String {
        self.upstream_overrides.get(p.id).cloned().unwrap_or_else(|| p.base_url.to_string())
    }

    pub fn providers(&self) -> Vec<ProviderView> {
        providers::registry()
            .into_iter()
            .map(|def| ProviderView {
                def: def.clone(),
                has_credential: self.key(def.id).is_some(),
                key_optional: !def.requires_key,
                codex_direct: codex_direct(def),
                routes: app_ids().into_iter().map(|a| (a, route_for(a, def))).collect(),
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
        discovery::list_models_as(&self.client, &self.upstream_url(p), path, key.as_deref(), p.auth_style)
            .await
            .map_err(|e| {
                let msg = format!("Model discovery for {} failed: {e}", p.display_name);
                self.diag.record(Level::Warn, "discovery.failed", &msg);
                msg
            })
    }

    pub fn settings(&self) -> crate::gateway::RuntimeSettings {
        self.broker.settings()
    }

    /// Save gateway limits and apply them (engine-side ones restart the engine).
    pub async fn set_settings(&self, settings: crate::gateway::RuntimeSettings) -> R<crate::gateway::RuntimeSettings> {
        if settings.rate_limit_per_minute > 10_000 || settings.max_concurrency_per_provider > 256 || settings.upstream_retries > 5 {
            return Err("Limits out of range: up to 10000 requests/min, 256 concurrent, 5 retries.".into());
        }
        self.state.save_settings(&settings).map_err(|e| e.to_string())?;
        self.broker.set_settings(settings.clone());
        self.diag.record(Level::Info, "settings.saved", &format!("Gateway limits: {settings:?}."));
        if self.broker.is_running().await {
            self.sync_gateway().await?;
        }
        Ok(settings)
    }

    /// Load saved custom providers into the registry (call at startup).
    pub fn load_custom_providers(&self) -> R<()> {
        let list = self.state.load_custom_providers().map_err(|e| e.to_string())?;
        providers::set_custom(&list);
        self.broker.set_settings(self.state.load_settings().map_err(|e| e.to_string())?);
        Ok(())
    }

    pub async fn check_custom_url(&self, url: &str) -> R<providers::UrlCheck> {
        let url = url.to_string();
        tokio::task::spawn_blocking(move || providers::check_custom_url(&url)).await.map_err(|e| e.to_string())?
    }

    /// Save a custom provider. `consent` must be true: the user has read the
    /// URL check's warnings and agreed to send data to that host (SPEC §6.3).
    pub async fn add_custom_provider(
        &self,
        name: &str,
        url: &str,
        protocol: Protocol,
        requires_key: bool,
        consent: bool,
    ) -> R<ProviderDefinition> {
        let name = name.trim();
        if name.is_empty() || name.len() > 60 {
            return Err("Give the provider a name (up to 60 characters).".into());
        }
        let check = self.check_custom_url(url).await?;
        if !consent {
            return Err(format!("Confirm that PrismFlow may send requests to {} before saving.", check.host));
        }
        let id = providers::custom_id(name);
        if providers::get(&id).is_some_and(|p| !p.custom) {
            return Err(format!("'{name}' clashes with a built-in provider; choose another name."));
        }
        let mut list = self.state.load_custom_providers().map_err(|e| e.to_string())?;
        if self.connections()?.iter().any(|r| r.provider_id == id) {
            return Err(format!("{name} has connections; disconnect them before changing it."));
        }
        list.retain(|c| c.id != id);
        list.push(providers::CustomProvider {
            id: id.clone(),
            display_name: name.into(),
            base_url: check.normalized.clone(),
            protocol,
            requires_key,
            consented_host: check.host.clone(),
        });
        self.state.save_custom_providers(&list).map_err(|e| e.to_string())?;
        providers::set_custom(&list);
        self.diag.record(Level::Info, "provider.custom_added", &format!(
            "Added custom provider {name} ({}) at {}; user consented to send requests to {}.", id, check.normalized, check.host
        ));
        Ok(provider(&id)?.clone())
    }

    pub fn remove_custom_provider(&self, id: &str) -> R<()> {
        let p = provider(id)?;
        if !p.custom {
            return Err("Built-in providers cannot be removed.".into());
        }
        if self.connections()?.iter().any(|r| r.provider_id == id) {
            return Err(format!("Disconnect everything using {} first.", p.display_name));
        }
        let _ = self.store.delete(&account_key(id, "default"));
        let mut list = self.state.load_custom_providers().map_err(|e| e.to_string())?;
        list.retain(|c| c.id != id);
        self.state.save_custom_providers(&list).map_err(|e| e.to_string())?;
        providers::set_custom(&list);
        self.diag.record(Level::Info, "provider.custom_removed", &format!("Removed custom provider {id} and its key."));
        Ok(())
    }

    pub fn applications(&self) -> Vec<AppDetection> {
        adapters::detect_all()
    }

    /// PrismFlow-owned Codex model catalog for a provider (kept in the state
    /// dir, not in ~/.codex, so the user's Codex home holds only the profile).
    pub fn codex_catalog_path(&self, provider_id: &str) -> PathBuf {
        self.state.dir().join("codex-catalogs").join(format!("{provider_id}.json"))
    }

    /// Write the catalog for `model`. Context window comes from the
    /// provider's own model list when it reports one; otherwise it is omitted.
    async fn write_codex_catalog(&self, p: &ProviderDefinition, model: &str) -> R<PathBuf> {
        let bin = codex::find_binary().ok_or("Codex CLI is not installed.")?;
        let template = tokio::task::spawn_blocking(move || codex::catalog_template(&bin))
            .await
            .map_err(|e| e.to_string())??;
        let listed = tokio::time::timeout(Duration::from_secs(15), self.discover_models(p.id)).await;
        let info = match listed {
            Ok(Ok(models)) => models.into_iter().find(|m| m.id == model),
            _ => None,
        };
        let entry = codex::CatalogEntry {
            slug: model.into(),
            context_window: info.as_ref().and_then(|m| m.context_length),
            images: false,
        };
        let path = self.codex_catalog_path(p.id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let body = serde_json::to_string_pretty(&codex::render_catalog(&template, &[entry])).map_err(|e| e.to_string())?;
        config_engine::atomic_write(&path, &body).map_err(|e| e.to_string())?;
        Ok(path)
    }

    fn plan(&self, app_id: &str, provider_id: &str, model: &str) -> R<(&'static ProviderDefinition, Route, config_engine::EditPlan)> {
        let catalog = (app_id == codex::APP_ID || app_id == codex::DESKTOP_APP_ID).then(|| self.codex_catalog_path(provider_id));
        self.plan_with(app_id, provider_id, model, catalog.as_deref())
    }

    fn plan_with(
        &self,
        app_id: &str,
        provider_id: &str,
        model: &str,
        catalog: Option<&std::path::Path>,
    ) -> R<(&'static ProviderDefinition, Route, config_engine::EditPlan)> {
        let p = provider(provider_id)?;
        let model = validate_model_id(model)?;
        let route = route_for(app_id, p).ok_or_else(|| match app_id {
            a if a == codex::APP_ID || a == codex::DESKTOP_APP_ID || a == claude_code::APP_ID || a == claude_desktop::APP_ID || agents::spec(a).is_some() => format!(
                "{} needs a base URL; custom providers are not supported in this build.", p.display_name
            ),
            other => format!("Unknown application '{other}'."),
        })?;
        let id = connection_id(app_id, p.id);
        let url = self.broker.url_for_config();
        if app_id == claude_desktop::APP_ID {
            let steps = claude_desktop::guided_setup(&self.broker.url_for_config(), "(shown after you connect)".into(), model).steps;
            return Ok((p, route, config_engine::EditPlan { file: PathBuf::new(), before: String::new(), after: String::new(), summary: steps }));
        }
        if let Some(spec) = agents::spec(app_id) {
            let files = self.agent_files(spec, p, model)?;
            let (script, body) = files[0].clone();
            let before = std::fs::read_to_string(&script).unwrap_or_default();
            let mut summary = vec![format!("Create {} (launches {} through PrismFlow's gateway)", script.display(), spec.display_name)];
            summary.extend(files[1..].iter().map(|(f, _)| format!("Create isolated config {}", f.display())));
            summary.push(format!("Your own {} configuration is not read or changed", spec.display_name));
            return Ok((p, route, config_engine::EditPlan { file: script, before, after: body, summary }));
        }
        let plan = match (app_id, route) {
            (codex::DESKTOP_APP_ID, _) => {
                let (base_url, args) = if route == Route::Direct {
                    (p.base_url.to_string(), vec!["credential".into(), "print".into(), "--provider".into(), p.id.into()])
                } else {
                    (format!("{url}/v1"), vec!["token".into(), "print".into(), "--connection".into(), id.clone()])
                };
                codex::plan_connect_base(&self.codex_home, p, model, &base_url, &self.helper, args, catalog).map_err(|e| e.to_string())?
            }
            (codex::APP_ID, Route::Direct) => codex::plan_connect(&self.codex_home, p, model, &self.helper, catalog).map_err(|e| e.to_string())?,
            (codex::APP_ID, Route::Gateway) => codex::plan_connect_gateway(&self.codex_home, p, model, &self.helper, &id, &url, catalog)
                .map_err(|e| e.to_string())?,
            _ => {
                let routing = self.state.get(&id).ok().flatten().map(|r| r.routing).unwrap_or_default();
                claude_code::plan_connect(&self.claude_dir, p, model, &self.helper, &id, &url, &routing).map_err(|e| e.to_string())?
            }
        };
        Ok((p, route, plan))
    }

    fn agent_files(&self, spec: &agents::AgentSpec, p: &ProviderDefinition, model: &str) -> R<Vec<(PathBuf, String)>> {
        let bin = agents::find_binary(spec).ok_or_else(|| format!("{} is not installed.", spec.display_name))?;
        let id = connection_id(spec.id, p.id);
        Ok(agents::plan_files(spec, &agents::agent_dir(self.state.dir(), &id), &bin, &self.helper, &id, &self.broker.url_for_config(), model))
    }

    fn profile_for(&self, app_id: &str, provider_id: &str) -> String {
        if agents::spec(app_id).is_some() {
            connection_id(app_id, provider_id).replace(':', "-")
        } else if app_id == codex::DESKTOP_APP_ID || app_id == claude_desktop::APP_ID {
            String::new() // desktop apps have no PrismFlow profile
        } else if app_id == claude_code::APP_ID {
            claude_code::profile_name(provider_id)
        } else {
            codex::profile_name(provider_id)
        }
    }

    fn launch_line(&self, app_id: &str, profile: &str, file: &std::path::Path) -> String {
        if app_id == claude_desktop::APP_ID {
            return format!("open -b {}", claude_desktop::BUNDLE_ID);
        }
        if app_id == codex::DESKTOP_APP_ID {
            return format!("open -b {}", codex::DESKTOP_BUNDLE_ID);
        }
        if agents::spec(app_id).is_some() {
            return quote(&file.display().to_string());
        }
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
        let profile = self.profile_for(app_id, p.id);
        let mut warnings = vec![format!(
            "Requests from this {} profile are billed to your {} account.", app_name(app_id), p.display_name
        )];
        if app_id == claude_desktop::APP_ID {
            warnings.push("PrismFlow does not change Claude Desktop's files. After connecting, it shows the gateway address and a token for you to enter in Claude Desktop's own Configure Third-Party Inference screen.".into());
            warnings.push("While Claude Desktop uses third-party inference, it is not signed in to your Claude account; switch back in the same screen.".into());
        } else if app_id == codex::DESKTOP_APP_ID {
            let priors = codex::read_base_priors(&self.codex_home).unwrap_or_default();
            match priors.get("model_provider").cloned().flatten() {
                Some(cur) if !cur.starts_with("prismflow_") => warnings.push(format!(
                    "The Codex app currently uses provider \"{cur}\" (set by you or another tool). PrismFlow switches it and puts \"{cur}\" back when you disconnect."
                )),
                _ => {}
            }
            warnings.push("This edits your ~/.codex/config.toml (the desktop app has no profiles): three top-level keys and one provider table. A backup is kept and the previous values are restored on disconnect. Restart the Codex app afterwards.".into());
        } else if let Some(a) = agents::spec(app_id) {
            warnings.push(format!("Your own {} configuration is not modified; PrismFlow launches it with isolated settings from its own folder.", a.display_name));
        } else if app_id == claude_code::APP_ID {
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
        if self.usable_key(p).is_none() {
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
        if app_id == codex::DESKTOP_APP_ID {
            if let Some(other) = self.connections()?.into_iter().find(|r| r.app_id == codex::DESKTOP_APP_ID && r.provider_id != provider_id) {
                return Err(format!("Codex Desktop is connected to {} already. Disconnect it first; the app has one configuration.", other.provider_id));
            }
        }
        // Desktop: keep the original priors across model switches.
        let desktop_priors = if app_id == codex::DESKTOP_APP_ID {
            match self.state.get(&connection_id(app_id, provider_id)).ok().flatten() {
                Some(existing) => existing.priors,
                None => codex::read_base_priors(&self.codex_home).map_err(|e| e.to_string())?,
            }
        } else {
            Default::default()
        };
        let (p, route, mut plan) = self.plan(app_id, provider_id, model)?;
        if (app_id == codex::APP_ID || app_id == codex::DESKTOP_APP_ID) && self.usable_key(p).is_some() {
            if let Err(e) = self.write_codex_catalog(p, validate_model_id(model)?).await {
                // Codex still works without a catalog (it falls back to defaults).
                self.diag.record(Level::Warn, "connect.catalog", &format!(
                    "No Codex model catalog for {}: {e}. Codex will use default model metadata.", p.display_name
                ));
                plan = self.plan_with(app_id, provider_id, model, None)?.2;
            }
        }
        if self.usable_key(p).is_none() {
            return Err(format!("Add a {} key under Providers first.", p.display_name));
        }
        if route == Route::Gateway {
            GatewayRuntime::discover().map_err(|e| format!("Nothing was changed: {e}"))?;
        }
        let id = connection_id(app_id, p.id);
        if route == Route::Gateway {
            tokens::ensure(self.store.as_ref(), &id).map_err(|e| format!("Could not create a local token: {e}"))?;
        }
        if let Some(spec) = agents::spec(app_id) {
            agents::write(&self.agent_files(spec, p, validate_model_id(model)?)?).map_err(|e| format!("Nothing usable was written: {e}"))?;
        }
        let applied = if agents::spec(app_id).is_some() || app_id == claude_desktop::APP_ID {
            config_engine::AppliedEdit { file: plan.file.clone(), backup_path: None }
        } else { config_engine::apply(&plan).map_err(|e| {
            let msg = format!("Writing {} failed; nothing was changed: {e}", plan.file.display());
            self.diag.record(Level::Error, "connect.failed", &msg);
            msg
        })? };
        // The engine backs up our own previous file next to it; move that
        // into the state dir so the app's config dir holds only the live file.
        if let Some(b) = applied.backup_path {
            let dest = self.state.backups_dir().join(b.file_name().unwrap_or_default());
            if std::fs::rename(&b, &dest).is_err() {
                let _ = std::fs::remove_file(&b);
            }
        }
        let profile = self.profile_for(app_id, p.id);
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
            // Switching model keeps the connection's routing choices.
            routing: self.state.get(&id).ok().flatten().map(|r| r.routing).unwrap_or_default(),
            priors: desktop_priors,
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

    /// Change a gateway connection's tiers, reasoning policy and fallbacks.
    pub async fn set_routing(&self, id: &str, routing: crate::routing::Routing) -> R<ConnectionRecord> {
        let mut record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        if record.route != Route::Gateway {
            return Err("Routing options need the gateway route. Direct Codex connections talk to the provider themselves.".into());
        }
        if record.app_id != claude_code::APP_ID && !routing.tiers.is_empty() {
            return Err("Per-tier models apply to Claude Code only; Codex uses one model per profile.".into());
        }
        if routing.fallbacks.len() > 5 {
            return Err("Use at most 5 fallback models.".into());
        }
        if !routing.fallbacks.is_empty() && !routing.fallback_consent {
            return Err("Fallbacks can send one request to several providers and bill each of them. Confirm that before saving.".into());
        }
        for r in routing.tiers.values().chain(routing.fallbacks.iter()) {
            let p = provider(&r.provider_id)?;
            validate_model_id(&r.model)?;
            if route_for(&record.app_id, p).is_none() {
                return Err(format!("{} cannot serve {}.", p.display_name, app_name(&record.app_id)));
            }
            if self.usable_key(p).is_none() {
                return Err(format!("Add a {} key under Providers before routing to it.", p.display_name));
            }
        }
        record.routing = routing;
        record.status = ConnectionStatus::Unverified;
        self.state.upsert(record.clone()).map_err(|e| e.to_string())?;
        if record.app_id == claude_code::APP_ID {
            // Model display names in the settings file follow the tiers.
            let (_, _, plan) = self.plan(&record.app_id, &record.provider_id, &record.model)?;
            config_engine::apply(&plan).map_err(|e| e.to_string())?;
        }
        self.diag.record(Level::Info, "routing.saved", &format!(
            "{id}: {} tier override(s), reasoning {:?}, {} fallback(s){}.",
            record.routing.tiers.len(), record.routing.reasoning, record.routing.fallbacks.len(),
            if record.routing.fallback_consent { " (multi-provider billing accepted)" } else { "" }
        ));
        self.sync_gateway().await?;
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
            bindings.push((token, Binding {
                connection_id: r.id.clone(),
                provider_id: r.provider_id.clone(),
                model: r.model.clone(),
                routing: std::sync::Arc::new(r.routing.clone()),
            }));
            let primary = crate::routing::ModelRef::new(&r.provider_id, &r.model);
            for target in r.routing.refs(&primary) {
                let m = models.entry(target.provider_id.clone()).or_default();
                if !m.contains(&target.model) {
                    m.push(target.model.clone());
                }
            }
        }
        let mut specs = Vec::new();
        for (provider_id, models) in models {
            let p = provider(&provider_id)?;
            let Some(key) = self.usable_key(p) else {
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
        let removed = if record.app_id == claude_desktop::APP_ID {
            Ok(None) // nothing was written; revoking the token below is the disconnect
        } else if record.app_id == codex::DESKTOP_APP_ID {
            codex::plan_disconnect_base(&self.codex_home, &record.provider_id, &record.priors)
                .map_err(|e| e.to_string())
                .and_then(|plans| {
                    let mut backup = None;
                    for plan in plans {
                        let applied = config_engine::apply(&plan).map_err(|e| e.to_string())?;
                        if let Some(b) = applied.backup_path {
                            let dest = self.state.backups_dir().join(b.file_name().unwrap_or_default());
                            if std::fs::rename(&b, &dest).is_err() {
                                let _ = std::fs::remove_file(&b);
                            }
                            backup = Some(dest);
                        }
                    }
                    Ok(backup)
                })
        } else if agents::spec(&record.app_id).is_some() {
            agents::remove(&agents::agent_dir(self.state.dir(), &record.id))
        } else if record.app_id == claude_code::APP_ID {
            claude_code::disconnect(&record.managed_file, &self.state.backups_dir()).map_err(|e| e.to_string())
        } else {
            codex::disconnect(&record.managed_file, &self.state.backups_dir()).map_err(|e| e.to_string())
        };
        let backup = removed.map_err(|e| {
            let msg = format!("Disconnect stopped; nothing was removed: {e}");
            self.diag.record(Level::Error, "disconnect.failed", &msg);
            msg
        })?;
        if record.app_id == codex::APP_ID || record.app_id == codex::DESKTOP_APP_ID {
            let _ = std::fs::remove_file(self.codex_catalog_path(&record.provider_id));
        }
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

    /// Interactive agent sessions still running on this connection's
    /// profile; they keep their old settings until restarted (SPEC §9).
    pub fn running_sessions(&self, id: &str) -> R<Vec<u32>> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let procs = adapters::processes::list();
        Ok(if record.app_id == claude_desktop::APP_ID || record.app_id == codex::DESKTOP_APP_ID {
            Vec::new() // desktop apps: restart is shown in the UI instead
        } else if agents::spec(&record.app_id).is_some() {
            let dir = agents::agent_dir(self.state.dir(), &record.id).display().to_string();
            procs.iter().filter(|p| p.command.contains(&dir)).map(|p| p.pid).collect()
        } else if record.app_id == claude_code::APP_ID {
            adapters::processes::claude_sessions(&procs, &record.managed_file.display().to_string())
        } else {
            adapters::processes::codex_sessions(&procs, &record.profile)
        })
    }

    /// Remove everything PrismFlow added (SPEC §23, acceptance §13.15):
    /// managed agent files (via disconnect, which also revokes tokens), every
    /// secure-store entry, the gateway, and the state directory.
    pub async fn uninstall(&self, dry_run: bool) -> R<UninstallReport> {
        use UninstallOutcome::*;
        let mut items = Vec::new();
        let mut push = |what: String, outcome, detail: String| items.push(UninstallItem { what, outcome, detail });

        // 1. Connections: restore each app's config and revoke its token.
        for r in self.connections()? {
            if dry_run {
                push(format!("Disconnect {}", r.id), Planned, format!("Delete {}", r.managed_file.display()));
                continue;
            }
            match self.disconnect(&r.id).await {
                Ok(_) => push(format!("Disconnect {}", r.id), Done, format!("Removed {}", r.managed_file.display())),
                Err(e) => {
                    push(format!("Disconnect {}", r.id), Failed, e);
                    return Ok(UninstallReport { dry_run, complete: false, items });
                }
            }
        }

        // 2. Stray PrismFlow-owned agent files (e.g. left by a crash).
        let strays = [
            (self.codex_home.clone(), ".config.toml", codex::MANAGED_MARKER),
            (self.claude_dir.clone(), ".settings.json", "\"_prismflow\""),
        ];
        for (dir, suffix, marker) in strays {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !(name.starts_with("prismflow-") && name.ends_with(suffix)) {
                    continue;
                }
                let ours = std::fs::read_to_string(e.path()).map(|c| c.contains(marker)).unwrap_or(false);
                if !ours {
                    push(format!("Keep {}", e.path().display()), Planned, "Not marked as PrismFlow-managed; left alone.".into());
                    continue;
                }
                if dry_run {
                    push(format!("Remove {}", e.path().display()), Planned, "PrismFlow-managed file with no connection".into());
                } else if let Err(err) = std::fs::remove_file(e.path()) {
                    push(format!("Remove {}", e.path().display()), Failed, err.to_string());
                    return Ok(UninstallReport { dry_run, complete: false, items });
                } else {
                    push(format!("Remove {}", e.path().display()), Done, String::new());
                }
            }
        }

        // 3. Secure store: every provider key and every possible token.
        let mut accounts: Vec<(String, String)> = providers::registry()
            .into_iter()
            .map(|p| (account_key(p.id, "default"), format!("{} key", p.display_name)))
            .collect();
        for app in app_ids() {
            for p in providers::registry() {
                let id = connection_id(app, p.id);
                accounts.push((tokens::account(&id), format!("gateway token for {id}")));
            }
        }
        for (acct, label) in accounts {
            if self.store.get(&acct).is_err() {
                continue;
            }
            if dry_run {
                push(format!("Delete {label}"), Planned, "From the system secure store".into());
                continue;
            }
            match self.store.delete(&acct) {
                Ok(()) | Err(CredentialError::NotFound(_)) => push(format!("Delete {label}"), Done, String::new()),
                Err(e) => {
                    push(format!("Delete {label}"), Failed, e.to_string());
                    return Ok(UninstallReport { dry_run, complete: false, items });
                }
            }
        }

        // 4. Gateway and state directory (backups, catalogs, logs, settings).
        let dir = self.state.dir().to_path_buf();
        if dry_run {
            push("Stop the gateway".into(), Planned, String::new());
            push(format!("Delete {}", dir.display()), Planned, "State, backups, diagnostics, settings, custom providers".into());
        } else {
            self.broker.stop().await;
            push("Stop the gateway".into(), Done, String::new());
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => push(format!("Delete {}", dir.display()), Done, String::new()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    push(format!("Delete {}", dir.display()), Failed, e.to_string());
                    return Ok(UninstallReport { dry_run, complete: false, items });
                }
            }
            providers::set_custom(&[]);
        }
        Ok(UninstallReport { dry_run, complete: true, items })
    }

    /// Values to enter in Claude Desktop's own third-party inference screen.
    /// The token is this connection's revocable, loopback-only local token.
    pub async fn guided_setup(&self, id: &str) -> R<claude_desktop::GuidedSetup> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        if record.app_id != claude_desktop::APP_ID {
            return Err("Guided setup applies to Claude Desktop connections only.".into());
        }
        if !self.broker.is_running().await {
            self.sync_gateway().await?;
        }
        let token = tokens::ensure(self.store.as_ref(), id).map_err(|e| format!("Secure store error: {e}"))?;
        self.diag.record(Level::Info, "guided.revealed", &format!("Showed the local token for {id} for entry in Claude Desktop."));
        Ok(claude_desktop::guided_setup(&self.broker.url_for_config(), token, &record.model))
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
        if record.app_id == claude_desktop::APP_ID {
            std::process::Command::new("open").args(["-b", claude_desktop::BUNDLE_ID]).status().map_err(|e| e.to_string())?;
            self.diag.record(Level::Info, "launch", "Opened Claude Desktop.");
            return Ok(());
        }
        if record.app_id == codex::DESKTOP_APP_ID {
            std::process::Command::new("open").args(["-b", codex::DESKTOP_BUNDLE_ID]).status().map_err(|e| e.to_string())?;
            self.diag.record(Level::Info, "launch", "Opened the Codex app.");
            return Ok(());
        }
        if let Some(a) = agents::spec(&record.app_id) {
            agents::find_binary(a).ok_or_else(|| format!("{} is not installed.", a.display_name))?;
        } else if record.app_id == claude_code::APP_ID {
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
            // Unit tests never reach a real provider.
            upstream_overrides: providers::registry().iter().map(|p| (p.id.to_string(), "http://127.0.0.1:9".to_string())).collect(),
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
        assert_eq!(route_for(CODEX, get("ollama")), Some(Route::Gateway), "keyless local servers go through the gateway");
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

    #[tokio::test]
    async fn custom_provider_needs_consent_and_persists() {
        let _g = providers::TEST_CUSTOM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let err = s.add_custom_provider("Box", "http://localhost:4000/v1", Protocol::OpenAiChat, false, false).await.unwrap_err();
        assert!(err.contains("Confirm"));
        let p = s.add_custom_provider("Box", "http://localhost:4000/v1/", Protocol::OpenAiChat, false, true).await.unwrap();
        assert_eq!(p.id, "custom_box");
        assert_eq!(p.base_url, "http://localhost:4000/v1");
        assert_eq!(route_for(CLAUDE, &p), Some(Route::Gateway));
        assert_eq!(s.state.load_custom_providers().unwrap().len(), 1);
        // reload from disk into a fresh registry view
        providers::set_custom(&[]);
        s.load_custom_providers().unwrap();
        assert!(providers::get("custom_box").is_some());
        assert!(s.add_custom_provider("Bad", "http://example.com/v1", Protocol::OpenAiChat, true, true).await.is_err());
        s.remove_custom_provider("custom_box").unwrap();
        assert!(providers::get("custom_box").is_none());
        assert!(s.remove_custom_provider("openrouter").is_err());
    }

    #[tokio::test]
    async fn uninstall_leaves_no_trace_and_dry_run_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        std::fs::write(s.codex_home.join("config.toml"), "# mine\n").unwrap();
        std::fs::write(s.codex_home.join("prismflow-mine.config.toml"), "model = \"user file, unmarked\"\n").unwrap();
        let before_codex: Vec<_> = walk(&s.codex_home);
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        std::fs::write(s.codex_home.join("prismflow-stray.config.toml"), format!("{}\nmodel = \"x\"\n", codex::MANAGED_MARKER)).unwrap();

        let plan = s.uninstall(true).await.unwrap();
        assert!(plan.complete && plan.dry_run);
        assert!(plan.items.iter().all(|i| i.outcome == UninstallOutcome::Planned));
        assert!(plan.items.iter().any(|i| i.what.contains("OpenRouter key")));
        assert_eq!(s.connections().unwrap().len(), 1, "dry run must not act");

        let done = s.uninstall(false).await.unwrap();
        assert!(done.complete, "{:#?}", done.items);
        assert!(s.key("openrouter").is_none());
        assert!(!s.state.dir().exists());
        assert!(!s.codex_home.join("prismflow-stray.config.toml").exists());
        assert_eq!(walk(&s.codex_home), before_codex, "user files, including the unmarked look-alike, survive");
    }

    #[tokio::test]
    async fn codex_desktop_switches_and_restores_the_base_config() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let base = s.codex_home.join("config.toml");
        let original = "# mine\nmodel = \"nvidia_nim/z-ai/glm\"\nmodel_provider = \"other\"\n\n[model_providers.other]\nbase_url = \"http://127.0.0.1:9090/v1\"\n";
        std::fs::write(&base, original).unwrap();
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        let preview = s.preview(codex::DESKTOP_APP_ID, "openrouter", "a/b").unwrap();
        assert!(preview.warnings.iter().any(|w| w.contains("\"other\"")), "{:?}", preview.warnings);
        let rec = s.connect(codex::DESKTOP_APP_ID, "openrouter", "a/b").await.unwrap();
        assert_eq!(rec.priors["model_provider"].as_deref(), Some("other"));
        assert!(std::fs::read_to_string(&base).unwrap().contains("model_provider = \"prismflow_openrouter\""));
        s.set_credential("groq", "gsk_abcdefabcdefabcdef").unwrap();
        assert!(s.connect(codex::DESKTOP_APP_ID, "groq", "x").await.unwrap_err().contains("Disconnect it first"));
        // A model switch keeps the original priors.
        let rec = s.connect(codex::DESKTOP_APP_ID, "openrouter", "c/d").await.unwrap();
        assert_eq!(rec.priors["model_provider"].as_deref(), Some("other"));
        s.disconnect(&rec.id).await.unwrap();
        let restored = std::fs::read_to_string(&base).unwrap();
        let (r, o): (toml_edit::DocumentMut, toml_edit::DocumentMut) = (restored.parse().unwrap(), original.parse().unwrap());
        assert_eq!(r["model_provider"].as_str(), o["model_provider"].as_str());
        assert_eq!(r["model"].as_str(), o["model"].as_str());
        assert!(r.get("model_catalog_json").is_none());
        assert!(r["model_providers"].get("prismflow_openrouter").is_none());
        assert!(restored.starts_with("# mine"));
        assert!(walk(&s.codex_home).iter().all(|f| !f.display().to_string().contains("prismflow-backup")), "backups live in the state dir");
    }

    #[test]
    fn rejects_bad_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        assert!(s.set_credential("openrouter", "two words").is_err());
        assert!(s.set_credential("nope", "k").is_err());
        assert!(s.preview(CODEX, "openrouter", "has space").is_err());
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
