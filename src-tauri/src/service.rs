//! Application service: the operations the UI invokes, independent of Tauri
//! so they are testable with an in-memory credential store and temp dirs.

use crate::adapters::{self, agents, claude_code, claude_desktop, codex, editors, jetbrains, vscode, AppDetection};
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
    pub signin_available: bool,
    #[serde(flatten)]
    pub def: ProviderDefinition,
    pub has_credential: bool,
    /// Keys saved for this provider (main + extras).
    pub key_count: usize,
    /// The address in use (differs from `base_url` when the user changed it).
    pub address: String,
    /// How each app would reach this provider, or None when it cannot.
    pub routes: BTreeMap<&'static str, Option<Route>>,
    /// Local providers only: whether something answers at its address now.
    pub running: Option<bool>,
}

/// Is anything listening at this URL's host and port? (A quick local check.)
fn listening(url: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    let (Some(host), Some(port)) = (u.host_str(), u.port_or_known_default()) else { return false };
    (host, port).to_socket_addrs().ok().into_iter().flatten()
        .any(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_millis(200)).is_ok())
}

#[derive(Debug, Serialize)]
pub struct ChatgptStatus {
    /// "prismflow" (PrismFlow's own sign-in), "codex" (Codex's), or none.
    pub source: Option<&'static str>,
    /// Unix seconds the current access token expires; 0 when unknown.
    pub expires_at: u64,
    pub codex_signed_in: bool,
    pub codex_installed: bool,
    pub in_use: bool,
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

/// What uninstall did. `complete` is false when a step failed; nothing after a failed
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

/// The models a ChatGPT plan offers, from the list Codex keeps after it
/// signs in (`models_cache.json`: no secrets). Hidden entries are skipped.
fn codex_plan_models(codex_home: &std::path::Path) -> R<Vec<ModelInfo>> {
    let text = std::fs::read_to_string(codex_home.join("models_cache.json"))
        .map_err(|_| "Open Codex once so it can fetch your plan's models, or type a model name.".to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| "Codex's model list couldn't be read. Type a model name.".to_string())?;
    let models: Vec<ModelInfo> = v["models"].as_array().into_iter().flatten()
        .filter(|m| m["visibility"].as_str() != Some("hide"))
        .filter_map(|m| Some(ModelInfo {
            id: m["slug"].as_str()?.to_string(),
            name: m["display_name"].as_str().map(String::from),
            context_length: m["context_window"].as_u64(),
            reports_tools: Some(true),
            price_in: None,
            price_out: None,
            coding: true,
        }))
        .collect();
    if models.is_empty() {
        return Err("Codex's model list is empty. Type a model name.".into());
    }
    Ok(models)
}

fn tempfile_dir_for_models(dir: &std::path::Path) -> R<std::path::PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    Ok(dir.to_path_buf())
}

/// The models an app shows in its own model menu.
pub struct AppMenu {
    pub models: Vec<ModelInfo>,
}

impl AppMenu {
    /// (model id, readable name) for apps that show names.
    pub fn labels(&self) -> Vec<(String, String)> {
        self.models.iter().map(|m| (m.id.clone(), m.name.clone().unwrap_or_else(|| pretty_model(&m.id)))).collect()
    }
}

/// "z-ai/glm-5.3-flash" → "glm-5.3-flash"; "qwen3:4b" stays.
fn pretty_model(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_string()
}

/// Embedding, speech and similar models can't chat.
fn not_for_chat(id: &str) -> bool {
    let l = id.to_lowercase();
    ["embed", "rerank", "whisper", "tts", "moderation", "-pf"].iter().any(|w| l.contains(w)) && !l.contains("chat")
}

/// Every application PrismFlow can connect.
fn app_ids() -> Vec<&'static str> {
    let mut v = vec![codex::APP_ID, codex::CLI_SEPARATE_APP_ID, codex::DESKTOP_APP_ID, codex::DESKTOP_SEPARATE_APP_ID, claude_code::APP_ID, claude_code::SEPARATE_APP_ID, claude_desktop::APP_ID, claude_desktop::SEPARATE_APP_ID, jetbrains::APP_ID, vscode::CLAUDE_APP_ID, vscode::CLAUDE_SHARED_APP_ID, vscode::CHAT_APP_ID, vscode::SEPARATE_APP_ID];
    v.extend(editors::ALL);
    v.extend(agents::AGENTS.iter().map(|a| a.id));
    v
}

fn provider(id: &str) -> R<&'static ProviderDefinition> {
    providers::get(id).ok_or_else(|| format!("Unknown provider '{id}'."))
}

/// The matrix decision for an (app, provider) pair; None = unsupported.
pub fn route_for(app_id: &str, p: &ProviderDefinition) -> Option<Route> {
    if p.base_url.is_empty() {
        return None; // custom providers wait for URL validation + consent
    }
    // Codex signs in to ChatGPT itself; routing it through PrismFlow adds nothing.
    if p.credential_source == providers::CredentialSource::CodexLogin && (codex::is_cli(app_id) || codex::is_desktop(app_id)) {
        return None;
    }
    match app_id {
        // Codex too goes through the gateway: an app is only ever given its
        // own revocable local token, never the provider key.
        a if codex::is_cli(a) || codex::is_desktop(a) => Some(Route::Gateway),
        a if claude_code::is_cli(a) => Some(Route::Gateway),
        a if claude_desktop::is_claude_desktop(a) => Some(Route::Gateway),
        jetbrains::APP_ID => Some(Route::Gateway),
        a if vscode::is_vscode(a) => Some(Route::Gateway),
        a if editors::ALL.contains(&a) => Some(Route::Gateway),
        a if agents::spec(a).is_some() => Some(Route::Gateway),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum Opens {
    Terminal,
    App,
    Nothing,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchInfo {
    pub line: String,
    pub opens: Opens,
}

/// Connections being tested now (from the app or the companion): one test
/// per connection at a time, so their progress views don't mix.
static RUNNING_TESTS: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

struct RunningTest(String);

impl RunningTest {
    /// `key` names the data folder and the connection.
    fn start(key: &str) -> R<Self> {
        let mut running = RUNNING_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        if !running.insert(key.to_string()) {
            return Err("A test is already running for this app.".into());
        }
        Ok(Self(key.to_string()))
    }
}

impl Drop for RunningTest {
    fn drop(&mut self) {
        RUNNING_TESTS.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

/// Apps that hold one PrismFlow setup at a time, so a second provider
/// can't be connected next to the first. (The Claude Code extension's two
/// modes share one setting; vscode.rs refuses that pairing itself.)
pub fn one_setup(app_id: &str) -> bool {
    codex::is_desktop(app_id) || claude_desktop::is_claude_desktop(app_id)
        || [editors::ZED, editors::ZED_SEPARATE, editors::ROO, editors::KILO].contains(&app_id)
        // The Claude Code extension keeps one connection in VS Code's settings.
        || [vscode::CLAUDE_APP_ID, vscode::CLAUDE_SHARED_APP_ID, vscode::SEPARATE_APP_ID].contains(&app_id)
}

pub fn connection_id(app_id: &str, provider_id: &str) -> String {
    format!("{app_id}:{provider_id}")
}

pub fn app_name(app_id: &str) -> &'static str {
    if let Some(a) = agents::spec(app_id) {
        return a.display_name;
    }
    if editors::ALL.contains(&app_id) {
        return editors::name(app_id);
    }
    match app_id {
        claude_code::APP_ID => "Claude Code",
        claude_code::SEPARATE_APP_ID => "Claude Code (separate)",
        codex::CLI_SEPARATE_APP_ID => "Codex CLI (separate)",
        codex::DESKTOP_APP_ID => "Codex app & VS Code",
        codex::DESKTOP_SEPARATE_APP_ID => "Codex app (separate)",
        claude_desktop::APP_ID => "Claude Desktop",
        claude_desktop::SEPARATE_APP_ID => "Claude Desktop (separate)",
        jetbrains::APP_ID => "JetBrains IDEs",
        vscode::CLAUDE_APP_ID => "Claude Code for VS Code (separate)",
        vscode::CLAUDE_SHARED_APP_ID => "Claude Code for VS Code (shared)",
        vscode::SEPARATE_APP_ID => "VS Code (separate)",
        vscode::CHAT_APP_ID => "VS Code Chat",
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

/// Quote a word for the shell that runs the launch line: POSIX single
/// quotes, or double quotes for cmd.exe (which has no escape for `"`).
fn quote(s: &str) -> String {
    if cfg!(windows) {
        return format!("\"{}\"", s.replace('"', ""));
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `VAR=value command` (POSIX) or `set "VAR=value" && command` (cmd.exe).
fn with_env(env: &[(&str, String)], line: String) -> String {
    let mut out = String::new();
    for (k, v) in env {
        if cfg!(windows) {
            out.push_str(&format!("set \"{k}={}\" && ", v.replace('"', "")));
        } else {
            out.push_str(&format!("{k}={} ", quote(v)));
        }
    }
    out + &line
}

/// Shell- and AppleScript-safe launch line for a Terminal window.
pub fn launch_command(codex_bin: &str, profile: &str) -> String {
    format!("{} -p {profile}", quote(codex_bin))
}

pub fn claude_launch_command(claude_bin: &str, settings: &str) -> String {
    format!("{} --settings {}", quote(claude_bin), quote(settings))
}

/// The "Try a model" chat's current target: (provider, model, in-memory token).
static PLAYGROUND: std::sync::Mutex<Option<(String, String, String)>> = std::sync::Mutex::new(None);

impl Service {
    fn key(&self, provider_id: &str) -> Option<String> {
        use providers::CredentialSource::*;
        match providers::get(provider_id).map(|p| p.credential_source).unwrap_or_default() {
            Keychain => self.store.get(&account_key(provider_id, "default")).ok(),
            GhCli if self.has_signin_consent(provider_id) => crate::credentials::signin::gh_token(),
            // PrismFlow's own ChatGPT sign-in first (it renews itself), else Codex's.
            CodexLogin if self.has_signin_consent(provider_id) => self.own_chatgpt().map(|t| t.access_token)
                .or_else(|| crate::credentials::signin::codex_access_token(&self.codex_home)),
            Gcloud if self.has_signin_consent(provider_id) => crate::credentials::signin::gcloud_token(),
            // The real token is fetched per model when the gateway starts.
            CopilotCli if self.has_signin_consent(provider_id) && crate::credentials::copilot::signed_in() => Some("copilot".into()),
            _ => None,
        }
    }

    fn has_signin_consent(&self, provider_id: &str) -> bool {
        self.state.load_signin_consent().iter().any(|id| id == provider_id)
    }

    /// Whether the vendor sign-in exists right now (independent of consent).
    fn signin_available(&self, p: &ProviderDefinition) -> bool {
        use providers::CredentialSource::*;
        match p.credential_source {
            Keychain => false,
            GhCli => crate::credentials::signin::gh_token().is_some(),
            CodexLogin => self.own_chatgpt().is_some() || crate::credentials::signin::codex_access_token(&self.codex_home).is_some(),
            Gcloud => crate::credentials::signin::gcloud_token().is_some(),
            CopilotCli => crate::credentials::copilot::signed_in(),
        }
    }

    /// Let PrismFlow use (or stop using) a vendor sign-in. The UI shows the
    /// terms-of-service risk before calling this with `consent = true`.
    pub async fn set_signin_consent(&self, provider_id: &str, consent: bool) -> R<()> {
        let p = provider(provider_id)?;
        if p.credential_source == providers::CredentialSource::Keychain {
            return Err(format!("{} uses an API key, not a sign-in.", p.display_name));
        }
        let mut ids = self.state.load_signin_consent();
        ids.retain(|i| i != provider_id);
        if consent {
            if !self.signin_available(p) {
                return Err(match p.credential_source {
                    providers::CredentialSource::GhCli => "Sign in with the GitHub CLI first: run `gh auth login`.".into(),
                    providers::CredentialSource::Gcloud => "Sign in with the Google Cloud CLI first: run `gcloud auth login`.".into(),
                    providers::CredentialSource::CopilotCli => "Install GitHub's Copilot CLI and sign in first: run `copilot login`.".into(),
                    _ => "Sign in to ChatGPT in Codex first: run `codex login`.".into(),
                });
            }
            ids.push(provider_id.into());
        } else if self.connections()?.iter().any(|r| r.provider_id == provider_id) {
            return Err(format!("Disconnect everything using {} first.", p.display_name));
        }
        self.state.save_signin_consent(&ids).map_err(|e| e.to_string())?;
        self.diag.record(Level::Info, "signin.consent", &format!("{} sign-in {}.", p.display_name, if consent { "allowed" } else { "withdrawn" }));
        if self.broker.is_running().await {
            let _ = self.sync_gateway().await;
        }
        Ok(())
    }

    /// The key to use for `p`: the stored one, or a placeholder for
    /// providers that need none (local servers). None = missing and required.
    pub(crate) fn usable_key(&self, p: &ProviderDefinition) -> Option<String> {
        self.key(p.id).or_else(|| (!p.requires_key).then(|| "no-key-required".to_string()))
    }

    /// PrismFlow's own ChatGPT sign-in, if there is one.
    fn own_chatgpt(&self) -> Option<crate::credentials::chatgpt::Tokens> {
        crate::credentials::chatgpt::load(self.store.as_ref()).ok().flatten()
    }

    fn chatgpt_account_id(&self) -> Option<String> {
        if let Some(t) = self.own_chatgpt() {
            return t.account_id.or_else(|| crate::credentials::chatgpt::account_id(&t.access_token));
        }
        let raw = std::fs::read_to_string(self.codex_home.join("auth.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        v["tokens"]["account_id"].as_str().map(String::from)
            .or_else(|| v["tokens"]["access_token"].as_str().and_then(crate::credentials::chatgpt::account_id))
    }

    /// Which ChatGPT sign-in is in use and until when.
    pub fn chatgpt_status(&self) -> ChatgptStatus {
        let own = self.own_chatgpt();
        let codex = crate::credentials::signin::codex_access_token(&self.codex_home);
        let (source, expires_at) = match (&own, &codex) {
            (Some(t), _) => (Some("prismflow"), t.expires_at),
            (None, Some(t)) => (Some("codex"), crate::credentials::chatgpt::expires_at(t)),
            (None, None) => (None, 0),
        };
        ChatgptStatus {
            source,
            expires_at,
            codex_signed_in: codex.is_some(),
            codex_installed: codex::find_binary().is_some(),
            in_use: self.has_signin_consent("chatgpt"),
        }
    }

    /// Sign in to ChatGPT in the browser, with PrismFlow keeping the sign-in.
    pub async fn chatgpt_sign_in(&self, open: impl FnOnce(&str) -> Result<(), String>) -> R<()> {
        use crate::credentials::chatgpt as c;
        // Tests point these at a stand-in sign-in server.
        let authorize = self.upstream_overrides.get("chatgpt_authorize").cloned().unwrap_or_else(|| c::AUTHORIZE_URL.to_string());
        let token_url = self.upstream_overrides.get("chatgpt_auth").cloned().unwrap_or_else(|| c::TOKEN_URL.to_string());
        let t = c::sign_in(&self.client, &authorize, &token_url, c::CALLBACK_PORT, open).await?;
        c::save(self.store.as_ref(), &t).map_err(|e| crate::credentials::store_problem(&e))?;
        self.diag.record(Level::Info, "chatgpt.signed_in", "Signed in to ChatGPT; PrismFlow keeps and renews the sign-in.");
        self.set_signin_consent("chatgpt", true).await?;
        if self.provider_in_gateway("chatgpt") {
            self.sync_gateway().await?;
        }
        Ok(())
    }

    /// Forget PrismFlow's own ChatGPT sign-in (Codex's is left alone).
    pub async fn chatgpt_sign_out(&self) -> R<()> {
        crate::credentials::chatgpt::forget(self.store.as_ref()).map_err(|e| crate::credentials::store_problem(&e))?;
        self.diag.record(Level::Info, "chatgpt.signed_out", "Removed PrismFlow's ChatGPT sign-in.");
        if crate::credentials::signin::codex_access_token(&self.codex_home).is_none() {
            let mut ids = self.state.load_signin_consent();
            ids.retain(|i| i != "chatgpt");
            self.state.save_signin_consent(&ids).map_err(|e| e.to_string())?;
        }
        if self.provider_in_gateway("chatgpt") {
            self.sync_gateway().await?;
        }
        Ok(())
    }

    /// Renews PrismFlow's own ChatGPT sign-in when it's close to expiring
    /// (or always, with `force`). Returns whether it renewed.
    pub async fn renew_chatgpt(&self, force: bool) -> R<bool> {
        use crate::credentials::chatgpt as c;
        let Some(t) = self.own_chatgpt() else { return Ok(false) };
        if !force && !c::needs_renewal(&t, now_secs()) {
            return Ok(false);
        }
        let token_url = self.upstream_overrides.get("chatgpt_auth").cloned().unwrap_or_else(|| c::TOKEN_URL.to_string());
        let new = c::renew(&self.client, &token_url, &t).await?;
        c::save(self.store.as_ref(), &new).map_err(|e| crate::credentials::store_problem(&e))?;
        self.diag.record(Level::Info, "chatgpt.renewed", "Renewed the ChatGPT sign-in.");
        Ok(true)
    }

    /// The plan's models straight from ChatGPT (the list Codex itself fetches).
    async fn chatgpt_live_models(&self, p: &ProviderDefinition) -> R<Vec<ModelInfo>> {
        let key = self.key(p.id).ok_or("Sign in to ChatGPT first.")?;
        let mut req = self.client.get(format!("{}/models?client_version=0.162.0", self.upstream_url(p).trim_end_matches('/'))).bearer_auth(key);
        if let Some(a) = self.chatgpt_account_id() {
            req = req.header("chatgpt-account-id", a);
        }
        let r = req.send().await.map_err(|e| discovery::unreachable_text(&e))?;
        if !r.status().is_success() {
            return Err(format!("ChatGPT didn't list its models (HTTP {}).", r.status().as_u16()));
        }
        let dir = tempfile_dir_for_models(&self.state.dir().join("chatgpt-models"))?;
        std::fs::write(dir.join("models_cache.json"), r.text().await.unwrap_or_default()).map_err(|e| e.to_string())?;
        codex_plan_models(&dir)
    }

    /// Runs Codex's own `codex login` (it opens the browser), then returns.
    pub async fn codex_login(&self) -> R<()> {
        let bin = codex::find_binary().ok_or("Codex isn't installed. Use “Sign in with ChatGPT” instead.")?;
        let mut cmd = crate::spawn::async_command(bin);
        cmd.arg("login").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|_| "Couldn't start Codex's sign-in.".to_string())?;
        match tokio::time::timeout(Duration::from_secs(10 * 60), child.wait()).await {
            Ok(Ok(s)) if s.success() => Ok(()),
            Ok(_) => Err("Codex's sign-in didn't finish. Try again.".into()),
            Err(_) => Err("Codex's sign-in timed out. Try again.".into()),
        }
    }

    pub fn upstream_url(&self, p: &ProviderDefinition) -> String {
        if let Some(u) = self.upstream_overrides.get(p.id) {
            return u.clone();
        }
        if p.local {
            if let Some(u) = self.provider_urls().get(p.id) {
                return u.clone();
            }
        }
        p.base_url.to_string()
    }

    /// Addresses the user set for servers on this computer (or their network).
    pub fn provider_urls(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.state.dir().join("provider_urls.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Sets (or with an empty `url`, resets) a local server's address.
    pub fn set_provider_url(&self, provider_id: &str, url: &str) -> R<String> {
        let p = provider(provider_id)?;
        if !p.local {
            return Err(format!("{}'s address can't be changed.", p.display_name));
        }
        let mut m = self.provider_urls();
        let url = url.trim().trim_end_matches('/');
        if url.is_empty() {
            m.remove(p.id);
        } else {
            let u = reqwest::Url::parse(url).map_err(|_| "Enter a full address, like http://127.0.0.1:11434".to_string())?;
            if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() || !u.username().is_empty() || u.query().is_some() {
                return Err("Enter a full address, like http://127.0.0.1:11434".into());
            }
            // Ollama's address is usually given without /v1.
            let v = if u.path().trim_end_matches('/').ends_with("/v1") { url.to_string() } else { format!("{url}/v1") };
            m.insert(p.id.into(), v);
        }
        crate::config_engine::atomic_write(&self.state.dir().join("provider_urls.json"), &serde_json::to_string_pretty(&m).unwrap_or_default())
            .map_err(|e| e.to_string())?;
        Ok(self.upstream_url(p))
    }

    pub fn providers(&self) -> Vec<ProviderView> {
        providers::registry()
            .into_iter()
            .map(|def| {
                let has_credential = if def.credential_source == providers::CredentialSource::Keychain {
                    self.key(def.id).is_some()
                } else {
                    self.has_signin_consent(def.id) && self.signin_available(def)
                };
                (def, has_credential)
            })
            .map(|(def, has_credential)| ProviderView {
                address: self.upstream_url(def),
                def: def.clone(),
                has_credential,
                // Extra keys only matter once there is a main one.
                key_count: if has_credential && def.credential_source == providers::CredentialSource::Keychain {
                    1 + self.extra_keys(def.id).len()
                } else {
                    usize::from(has_credential)
                },
                signin_available: def.credential_source != providers::CredentialSource::Keychain && self.signin_available(def),
                key_optional: !def.requires_key,
                routes: app_ids().into_iter().map(|a| (a, route_for(a, def))).collect(),
                running: def.local.then(|| listening(&self.upstream_url(def))),
            })
            .collect()
    }

    pub fn set_credential(&self, provider_id: &str, secret: &str) -> R<()> {
        let p = provider(provider_id)?;
        if p.credential_source != providers::CredentialSource::Keychain {
            return Err(format!("{} uses your existing sign-in; no key is entered here.", p.display_name));
        }
        let secret = secret.trim();
        if secret.is_empty() || secret.chars().any(char::is_whitespace) {
            return Err("Paste the key exactly as given, with no spaces.".into());
        }
        self.store
            .set(&account_key(p.id, "default"), secret)
            .map_err(|e| crate::credentials::store_problem(&e))?;
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
        let mut models = self.discover_models_raw(provider_id).await?;
        // PrismFlow's own larger-context Ollama copies aren't separate choices.
        if provider_id == "ollama" {
            models.retain(|m| !(m.id.contains("-pf") && m.id.ends_with('k')));
        }
        discovery::enrich(provider_id, &mut models);
        Ok(models)
    }

    async fn discover_models_raw(&self, provider_id: &str) -> R<Vec<ModelInfo>> {
        let p = provider(provider_id)?;
        if p.credential_source == providers::CredentialSource::CopilotCli {
            return self.copilot_models().await;
        }
        if p.credential_source == providers::CredentialSource::CodexLogin {
            // Codex's saved list first; without Codex, ask ChatGPT directly.
            return match codex_plan_models(&self.codex_home) {
                Ok(m) => Ok(m),
                Err(e) => self.chatgpt_live_models(p).await.map_err(|_| e),
            };
        }
        let path = p.model_list_path.ok_or("This provider doesn't list its models. Type the model name.")?;
        if p.base_url.is_empty() {
            return Err("Custom providers are not supported in this build.".into());
        }
        let key = self.key(p.id);
        discovery::list_models_as(&self.client, &self.upstream_url(p), path, key.as_deref(), p.auth_style)
            .await
            .map_err(|e| {
                // A local app that isn't running is the usual cause; say that.
                let msg = if p.local && matches!(e, discovery::ProbeError::Unavailable(_)) {
                    let name = p.display_name.trim_end_matches(" (local)");
                    format!("{name} isn't running. Open the {name} app, then try again.")
                } else {
                    format!("Couldn't get {}'s model list: {e}.", p.display_name)
                };
                self.diag.record(Level::Warn, "discovery.failed", &msg);
                msg
            })
    }

    /// Copilot's model list, from the endpoint its CLI hands out.
    async fn copilot_models(&self) -> R<Vec<ModelInfo>> {
        if !self.has_signin_consent("github_copilot") {
            return Err("Turn on GitHub Copilot on the Providers page first.".into());
        }
        let ep = tokio::task::spawn_blocking(|| crate::credentials::copilot::endpoint(None)).await.map_err(|e| e.to_string())??;
        let mut req = self.client.get(format!("{}/models", ep.base_url)).bearer_auth(&ep.api_key);
        for (k, v) in &ep.headers {
            req = req.header(k, v);
        }
        let v: serde_json::Value = req.send().await.map_err(|e| e.without_url().to_string())?.json().await.map_err(|e| e.to_string())?;
        Ok(v["data"].as_array().map(|a| a.iter().filter_map(|m| Some(ModelInfo {
            price_in: None,
            price_out: None,
            coding: false,
            id: m["id"].as_str()?.to_string(),
            name: m["name"].as_str().map(String::from),
            context_length: m["capabilities"]["limits"]["max_context_window_tokens"].as_u64(),
            reports_tools: m["capabilities"]["supports"]["tool_calls"].as_bool(),
        })).collect()).unwrap_or_default())
    }

    pub fn settings(&self) -> crate::gateway::RuntimeSettings {
        self.broker.settings()
    }

    /// Save gateway limits and apply them (engine-side ones restart the engine).
    pub async fn set_settings(&self, settings: crate::gateway::RuntimeSettings) -> R<crate::gateway::RuntimeSettings> {
        if settings.web_tools.backend == crate::webtools::SearchBackend::Searxng {
            let u = settings.web_tools.searxng_url.trim();
            if !(u.starts_with("https://") || u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost")) {
                return Err("The SearXNG URL must be https, or http on this machine.".into());
            }
        }
        if settings.rate_limit_per_minute > 10_000 || settings.max_concurrency_per_provider > 256 || settings.upstream_retries > 5 {
            return Err("Limits out of range: up to 10000 requests/min, 256 concurrent, 5 retries.".into());
        }
        if !(30..=3600).contains(&settings.provider_timeout_secs) {
            return Err("The provider wait must be between 30 and 3600 seconds.".into());
        }
        if !(2048..=1_048_576).contains(&settings.ollama_context) {
            return Err("The local model room must be between 2048 and 1048576 tokens.".into());
        }
        self.state.save_settings(&settings).map_err(|e| e.to_string())?;
        self.broker.set_settings(settings.clone());
        self.diag.record(Level::Info, "settings.saved", &format!("Gateway limits: {settings:?}."));
        if self.broker.is_running().await {
            self.sync_gateway().await?;
        }
        Ok(settings)
    }

    /// Store (or with an empty value, remove) the Brave Search API key.
    pub fn set_web_search_key(&self, key: &str) -> R<()> {
        let key = key.trim();
        if key.is_empty() {
            let _ = self.store.delete(crate::webtools::BRAVE_ACCOUNT);
            self.broker.set_web_key(None);
            return Ok(());
        }
        if key.chars().any(char::is_whitespace) {
            return Err("Paste the key exactly as issued.".into());
        }
        self.store.set(crate::webtools::BRAVE_ACCOUNT, key).map_err(|e| e.to_string())?;
        self.broker.set_web_key(Some(key.to_string()));
        self.diag.record(Level::Info, "webtools.key", "Saved a Brave Search API key to the secure store.");
        Ok(())
    }

    pub fn has_web_search_key(&self) -> bool {
        self.store.get(crate::webtools::BRAVE_ACCOUNT).is_ok()
    }

    /// Load saved custom providers into the registry (call at startup).
    pub fn load_custom_providers(&self) -> R<()> {
        let list = self.state.load_custom_providers().map_err(|e| e.to_string())?;
        providers::set_custom(&list);
        self.broker.set_settings(self.state.load_settings().map_err(|e| e.to_string())?);
        self.broker.set_web_key(self.store.get(crate::webtools::BRAVE_ACCOUNT).ok());
        Ok(())
    }

    pub async fn check_custom_url(&self, url: &str) -> R<providers::UrlCheck> {
        let url = url.to_string();
        tokio::task::spawn_blocking(move || providers::check_custom_url(&url)).await.map_err(|e| e.to_string())?
    }

    /// Save a custom provider. `consent` must be true: the user has read the
    /// URL check's warnings and agreed to send data to that host.
    pub async fn add_custom_provider(
        &self,
        name: &str,
        url: &str,
        protocol: Protocol,
        requires_key: bool,
        consent: bool,
        source: providers::CredentialSource,
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
        // Different names can make the same ID ("Work Server" / "work-server");
        // replacing that one would hand it the other's saved key.
        if let Some(other) = list.iter().find(|c| c.id == id && !c.display_name.eq_ignore_ascii_case(name)) {
            return Err(format!("'{name}' is too close to your provider '{}'. Choose a different name.", other.display_name));
        }
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
            credential_source: source,
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

    /// The config folder for the Codex / Claude Code CLI: your usual one
    /// (shared), or a folder of PrismFlow's own (separate).
    fn cli_home(&self, app_id: &str) -> PathBuf {
        match app_id {
            codex::CLI_SEPARATE_APP_ID => self.state.dir().join("codex-cli-separate"),
            claude_code::SEPARATE_APP_ID => self.state.dir().join("claude-separate"),
            a if claude_code::is_cli(a) => self.claude_dir.clone(),
            _ => self.codex_home.clone(),
        }
    }

    /// Creates a separate app's own folder before PrismFlow writes into it
    /// or starts the app (previews write nothing).
    fn prepare_home(&self, app_id: &str) -> R<()> {
        let dir = match app_id {
            codex::CLI_SEPARATE_APP_ID => self.cli_home(app_id),
            claude_code::SEPARATE_APP_ID => {
                return claude_code::prepare_separate_dir(&self.cli_home(app_id))
                    .map_err(|e| format!("Couldn't set up Claude Code's own folder: {e}"));
            }
            codex::DESKTOP_SEPARATE_APP_ID => self.desktop_home(app_id),
            vscode::SEPARATE_APP_ID => vscode::separate_data_dir(self.state.dir()).join("User"),
            editors::ZED_SEPARATE => editors::zed_separate_dir(self.state.dir()).join("config"),
            _ => return Ok(()),
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))
    }

    /// Environment a launch needs (the separate CLIs' own folders).
    fn launch_env(&self, app_id: &str) -> Vec<(&'static str, String)> {
        match app_id {
            codex::CLI_SEPARATE_APP_ID => vec![("CODEX_HOME", self.cli_home(app_id).display().to_string())],
            claude_code::SEPARATE_APP_ID => vec![("CLAUDE_CONFIG_DIR", self.cli_home(app_id).display().to_string())],
            _ => Vec::new(),
        }
    }

    /// Where a Codex app connection keeps its config: your usual Codex
    /// folder (shared), or PrismFlow's own folder (separate).
    fn desktop_home(&self, app_id: &str) -> PathBuf {
        if app_id == codex::DESKTOP_SEPARATE_APP_ID { codex::separate_home(self.state.dir()) } else { self.codex_home.clone() }
    }

    /// PrismFlow-owned Codex model catalog, kept in the state dir so Codex's
    /// own folder holds only PrismFlow's profile. One per connection: the
    /// CLI and both Codex apps can use different models of one provider.
    pub fn codex_catalog_path(&self, app_id: &str, provider_id: &str) -> PathBuf {
        let name = if app_id == codex::APP_ID { provider_id.to_string() } else { format!("{app_id}-{provider_id}") };
        self.state.dir().join("codex-catalogs").join(format!("{name}.json"))
    }

    /// Write the catalog for `model`. Context window comes from the
    /// provider's own model list when it reports one; otherwise it is omitted.
    /// The models for the app's own menu: the connection's saved list (the
    /// user may have edited it), or else the chosen model plus the
    /// provider's coding models, chosen one first, at most 100.
    async fn app_menu(&self, app_id: &str, provider_id: &str, model: &str) -> AppMenu {
        let listed = tokio::time::timeout(Duration::from_secs(15), self.discover_models(provider_id)).await
            .ok().and_then(Result::ok).unwrap_or_default();
        let saved = self.state.get(&connection_id(app_id, provider_id)).ok().flatten()
            .map(|r| r.app_models).filter(|m| !m.is_empty());
        let mut ids: Vec<String> = match saved {
            Some(s) => s,
            None => listed.iter().filter(|m| m.coding && !not_for_chat(&m.id)).map(|m| m.id.clone()).take(100).collect(),
        };
        ids.retain(|m| m != model);
        ids.insert(0, model.to_string());
        ids.truncate(100);
        let models = ids.into_iter().map(|id| {
            let info = listed.iter().find(|m| m.id == id);
            ModelInfo {
                name: info.and_then(|m| m.name.clone()),
                context_length: info.and_then(|m| m.context_length),
                reports_tools: None, price_in: None, price_out: None, coding: true,
                id,
            }
        }).collect();
        AppMenu { models }
    }

    async fn write_codex_catalog(&self, app_id: &str, p: &ProviderDefinition, menu: &AppMenu) -> R<PathBuf> {
        let bin = codex::find_binary().ok_or("Codex CLI is not installed.")?;
        let template = tokio::task::spawn_blocking(move || codex::catalog_template(&bin))
            .await
            .map_err(|e| e.to_string())??;
        // Every menu model, so Codex's /model can switch between them.
        let entries: Vec<codex::CatalogEntry> = menu.models.iter().map(|m| codex::CatalogEntry {
            slug: m.id.clone(),
            context_window: m.context_length,
            images: false,
        }).collect();
        let path = self.codex_catalog_path(app_id, p.id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let body = serde_json::to_string_pretty(&codex::render_catalog(&template, &entries)).map_err(|e| e.to_string())?;
        config_engine::atomic_write(&path, &body).map_err(|e| e.to_string())?;
        Ok(path)
    }

    fn plan(&self, app_id: &str, provider_id: &str, model: &str) -> R<(&'static ProviderDefinition, Route, config_engine::EditPlan)> {
        let catalog = (codex::is_cli(app_id) || codex::is_desktop(app_id)).then(|| self.codex_catalog_path(app_id, provider_id));
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
            a if p.credential_source == providers::CredentialSource::CodexLogin && (codex::is_cli(a) || codex::is_desktop(a)) => {
                "Codex signs in to ChatGPT itself; use Codex's own sign-in instead of PrismFlow.".to_string()
            }
            a if codex::is_cli(a) || codex::is_desktop(a) || claude_code::is_cli(a) || claude_desktop::is_claude_desktop(a) || a == jetbrains::APP_ID || vscode::is_vscode(a) || editors::ALL.contains(&a) || agents::spec(a).is_some() => format!(
                "{} needs a base URL; custom providers are not supported in this build.", p.display_name
            ),
            other => format!("Unknown application '{other}'."),
        })?;
        let id = connection_id(app_id, p.id);
        let url = self.broker.url_for_config();
        if app_id == claude_desktop::SEPARATE_APP_ID {
            let root = claude_desktop::separate_root(self.state.dir());
            let summary = claude_desktop::describe_separate(&root);
            return Ok((p, route, config_engine::EditPlan { file: root, before: String::new(), after: String::new(), summary }));
        }
        if app_id == claude_desktop::APP_ID {
            let summary = claude_desktop::describe_auto();
            return Ok((p, route, config_engine::EditPlan { file: claude_desktop::config_root(), before: String::new(), after: String::new(), summary }));
        }
        if app_id == jetbrains::APP_ID {
            let jp = self.jetbrains_plan(p, model, &id, &url)?;
            let summary = vec![
                format!("Add Claude Code and Codex agents to {}", jp.file.display()),
                "Your other agents in that file stay as they are".into(),
                "Your own ~/.claude and ~/.codex settings aren't used or changed".into(),
                "The key is fetched from PrismFlow when an agent runs; it isn't stored in the file".into(),
            ];
            return Ok((p, route, config_engine::EditPlan { file: jp.file, before: jp.before, after: jp.after, summary }));
        }
        if editors::ALL.contains(&app_id) {
            let ep = editors::plan(app_id, &agents::agent_dir(self.state.dir(), &id), self.state.dir(), &id, &format!("{url}/v1"), model, None)?;
            let summary = if app_id == editors::ZED_SEPARATE {
                vec![format!("Set up a Zed of its own in {}", editors::zed_separate_dir(self.state.dir()).display()), "Your usual Zed stays as it is".into()]
            } else if app_id == editors::CONTINUE_IDE || app_id == editors::ZED {
                editors::guide(app_id, &format!("{url}/v1"), model, "(shown after you connect)")
            } else {
                vec![format!("Set up {} in {}", editors::name(app_id), ep.file.display()), "Your other settings stay as they are".into()]
            };
            return Ok((p, route, config_engine::EditPlan { file: ep.file, before: ep.before, after: ep.after, summary }));
        }
        if vscode::is_vscode(app_id) {
            let vp = self.vscode_plan(app_id, p, model, &id, &url, None)?;
            let summary = if app_id == vscode::CLAUDE_SHARED_APP_ID {
                vec![
                    format!("Have the Claude Code extension run Claude through PrismFlow ({})", vp.file.display()),
                    "It keeps using your own ~/.claude chats and settings".into(),
                    "The key is fetched from PrismFlow when Claude runs; it isn't stored".into(),
                ]
            } else if app_id == vscode::SEPARATE_APP_ID {
                vec![
                    format!("Set up a VS Code window of its own in {}", vscode::separate_data_dir(self.state.dir()).display()),
                    "Claude Code and VS Code Chat use PrismFlow there".into(),
                    "Your usual VS Code settings stay as they are".into(),
                ]
            } else if app_id == vscode::CLAUDE_APP_ID {
                vec![
                    format!("Set the Claude Code extension's CLAUDE_CONFIG_DIR in {} to a folder PrismFlow owns", vp.file.display()),
                    "Your other VS Code settings and your own ~/.claude stay as they are".into(),
                    "The key is fetched from PrismFlow when Claude Code runs; it isn't stored".into(),
                ]
            } else {
                vec![
                    format!("Add a PrismFlow model group to {}", vp.file.display()),
                    "Your other chat models stay as they are".into(),
                    "VS Code needs the key in this file; it only works on this computer and is removed when you disconnect".into(),
                ]
            };
            return Ok((p, route, config_engine::EditPlan { file: vp.file, before: vp.before, after: vp.after, summary }));
        }
        if let Some(spec) = agents::spec(app_id) {
            let files = self.agent_files(spec, p, model)?;
            let (script, body) = files[0].clone();
            let before = std::fs::read_to_string(&script).unwrap_or_default();
            let mut summary = vec![format!("Create {} (opens {} with PrismFlow)", script.display(), spec.display_name)];
            summary.extend(files[1..].iter().map(|(f, _)| format!("Create isolated config {}", f.display())));
            summary.push(format!("Your own {} configuration is not read or changed", spec.display_name));
            return Ok((p, route, config_engine::EditPlan { file: script, before, after: body, summary }));
        }
        let plan = match (app_id, route) {
            (a, _) if codex::is_desktop(a) => {
                let args = vec!["token".into(), "print".into(), "--connection".into(), id.clone()];
                codex::plan_connect_base(&self.desktop_home(app_id), p, model, &format!("{url}/v1"), &self.helper, args, catalog).map_err(|e| e.to_string())?
            }
            (a, _) if codex::is_cli(a) => codex::plan_connect_gateway(&self.cli_home(a), p, model, &self.helper, &id, &url, catalog)
                .map_err(|e| e.to_string())?,
            _ => {
                let routing = self.state.get(&id).ok().flatten().map(|r| r.routing).unwrap_or_default();
                claude_code::plan_connect(&self.cli_home(app_id), p, model, &self.helper, &id, &url, &routing).map_err(|e| e.to_string())?
            }
        };
        Ok((p, route, plan))
    }

    fn jetbrains_plan(&self, p: &'static ProviderDefinition, model: &str, id: &str, url: &str) -> R<jetbrains::Plan> {
        let dir = agents::agent_dir(self.state.dir(), id);
        let routing = self.state.get(id).ok().flatten().map(|r| r.routing).unwrap_or_default();
        let claude = claude_code::plan_connect(&dir.join("claude"), p, model, &self.helper, id, url, &routing).map_err(|e| e.to_string())?.after;
        let args = vec!["token".to_string(), "print".into(), "--connection".into(), id.to_string()];
        let codex = codex::plan_connect_base(&dir.join("codex"), p, model, &format!("{url}/v1"), &self.helper, args, None).map_err(|e| e.to_string())?.after;
        let cl = jetbrains::launch(jetbrains::CLAUDE_PKG, jetbrains::CLAUDE_VERSION, "PRISMFLOW_ACP_CLAUDE_JS")?;
        let cx = jetbrains::launch(jetbrains::CODEX_PKG, jetbrains::CODEX_VERSION, "PRISMFLOW_ACP_CODEX_JS")?;
        jetbrains::plan(&dir, p.display_name, id, url, claude, codex, cl, cx)
    }

    /// `token`: the real local token (connect) or None for a preview.
    fn vscode_plan(&self, app_id: &str, p: &'static ProviderDefinition, model: &str, id: &str, url: &str, token: Option<&str>) -> R<vscode::Plan> {
        let claude_settings = |dir: &std::path::Path| -> R<String> {
            let routing = self.state.get(id).ok().flatten().map(|r| r.routing).unwrap_or_default();
            Ok(claude_code::plan_connect(dir, p, model, &self.helper, id, url, &routing).map_err(|e| e.to_string())?.after)
        };
        if app_id == vscode::CLAUDE_SHARED_APP_ID {
            let dir = agents::agent_dir(self.state.dir(), id);
            return vscode::plan_claude_shared(&dir, id, claude_settings(&dir.join("claude"))?, &self.helper);
        }
        if app_id == vscode::SEPARATE_APP_ID {
            let dir = agents::agent_dir(self.state.dir(), id);
            let user = vscode::separate_data_dir(self.state.dir()).join("User");
            let mut plan = vscode::plan_claude_in(&user, &dir, id, claude_settings(&dir.join("claude"))?)?;
            let chat = vscode::plan_chat_in(&user, id, p.display_name, model, url, token.unwrap_or(editors::KEY_PLACEHOLDER), None)?;
            plan.owned.push((chat.file, chat.after));
            return Ok(plan);
        }
        if app_id == vscode::CLAUDE_APP_ID {
            let dir = agents::agent_dir(self.state.dir(), id);
            let routing = self.state.get(id).ok().flatten().map(|r| r.routing).unwrap_or_default();
            let settings = claude_code::plan_connect(&dir.join("claude"), p, model, &self.helper, id, url, &routing).map_err(|e| e.to_string())?.after;
            vscode::plan_claude(&dir, id, settings)
        } else {
            vscode::plan_chat(id, p.display_name, model, url, token.unwrap_or(editors::KEY_PLACEHOLDER), None)
        }
    }

    fn agent_files(&self, spec: &agents::AgentSpec, p: &ProviderDefinition, model: &str) -> R<Vec<(PathBuf, String)>> {
        let bin = agents::find_binary(spec).ok_or_else(|| format!("{} is not installed.", spec.display_name))?;
        let id = connection_id(spec.id, p.id);
        Ok(agents::plan_files(spec, &agents::agent_dir(self.state.dir(), &id), &bin, &self.helper, &id, &self.broker.url_for_config(), model))
    }

    fn profile_for(&self, app_id: &str, provider_id: &str) -> String {
        if agents::spec(app_id).is_some() {
            connection_id(app_id, provider_id).replace(':', "-")
        } else if codex::is_desktop(app_id) || claude_desktop::is_claude_desktop(app_id) || app_id == jetbrains::APP_ID || vscode::is_vscode(app_id) || editors::ALL.contains(&app_id) {
            String::new() // desktop apps have no PrismFlow profile
        } else if claude_code::is_cli(app_id) {
            claude_code::profile_name(provider_id)
        } else {
            codex::profile_name(provider_id)
        }
    }

    /// The start command as the user would type it, with any environment
    /// it needs written in front.
    fn launch_line(&self, app_id: &str, profile: &str, file: &std::path::Path) -> String {
        with_env(&self.launch_env(app_id), self.command_line(app_id, profile, file))
    }

    /// The start command without its environment.
    fn command_line(&self, app_id: &str, profile: &str, file: &std::path::Path) -> String {
        // The desktop apps are opened with macOS's `open`.
        if !cfg!(target_os = "macos") && (codex::is_desktop(app_id) || claude_desktop::is_claude_desktop(app_id)) {
            return String::new();
        }
        if app_id == claude_desktop::SEPARATE_APP_ID {
            return format!("open -n -a Claude --args --user-data-dir={}", quote(&claude_desktop::separate_data_dir(self.state.dir()).display().to_string()));
        }
        if app_id == claude_desktop::APP_ID {
            return format!("open -b {}", claude_desktop::BUNDLE_ID);
        }
        if app_id == codex::DESKTOP_SEPARATE_APP_ID {
            return format!("open -n -b {} --env CODEX_HOME={}", codex::DESKTOP_BUNDLE_ID, quote(&self.desktop_home(app_id).display().to_string()));
        }
        if app_id == codex::DESKTOP_APP_ID {
            return format!("open -b {}", codex::DESKTOP_BUNDLE_ID);
        }
        if app_id == vscode::SEPARATE_APP_ID {
            let code = vscode::code_command().map(|c| c.display().to_string()).unwrap_or_else(|| "code".into());
            return format!("{} --user-data-dir {}", quote(&code), quote(&vscode::separate_data_dir(self.state.dir()).display().to_string()));
        }
        if app_id == jetbrains::APP_ID || vscode::is_vscode(app_id) || editors::ALL.contains(&app_id) {
            return String::new();
        }
        if agents::spec(app_id).is_some() {
            return agents::launch_command_line(file);
        }
        if claude_code::is_cli(app_id) {
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
        // Short, plain notes (owner feedback: no jargon). Most important first.
        let mut warnings: Vec<String> = Vec::new();
        if self.usable_key(p).is_none() {
            warnings.push(format!("Add a {} key on the Providers page first.", p.display_name));
        }
        if route == Route::Gateway {
            if let Err(e) = GatewayRuntime::discover() {
                warnings.push(format!("PrismFlow's background service can't start yet: {e}"));
            }
        }
        if p.local {
            warnings.push(format!("Runs on this computer with {}. Free.", p.display_name));
        } else if p.requires_key {
            warnings.push(format!("Usage is charged to your {} account.", p.display_name));
        }
        let app = app_name(app_id);
        if app_id == claude_desktop::SEPARATE_APP_ID {
            warnings.push("A second Claude Desktop, just for PrismFlow. Open it from PrismFlow.".into());
            warnings.push("Your usual Claude Desktop stays signed in to your Claude account; both can be open at once.".into());
        } else if app_id == claude_desktop::APP_ID {
            warnings.push("PrismFlow sets up Claude Desktop for you. Quit and reopen Claude Desktop afterwards.".into());
            warnings.push("While it's set up this way, Claude Desktop won't use your Claude account. Disconnect switches it back.".into());
        } else if app_id == vscode::CLAUDE_APP_ID {
            warnings.push("Start a new Claude Code chat in VS Code after connecting. Reload the window if it still uses the old setup.".into());
            warnings.push("Your own ~/.claude settings and history aren't used while it's connected.".into());
        } else if app_id == vscode::CLAUDE_SHARED_APP_ID {
            warnings.push("Start a new Claude Code chat in VS Code after connecting. Reload the window if it still uses the old setup.".into());
            warnings.push("It uses your usual Claude Code chats and settings.".into());
        } else if app_id == vscode::SEPARATE_APP_ID {
            warnings.push("Open it from PrismFlow. It's a separate VS Code window with its own settings and chats.".into());
            warnings.push("In it, use the Claude Code extension or pick the PrismFlow model in Chat.".into());
        } else if app_id == vscode::CHAT_APP_ID {
            warnings.push("Pick the PrismFlow model in VS Code Chat's model list.".into());
            warnings.push("If you're signed in to GitHub, your Copilot plan must allow your own models.".into());
        } else if app_id == jetbrains::APP_ID {
            warnings.push("In your JetBrains IDE, open AI Assistant and pick the PrismFlow agent.".into());
            warnings.push("Your other agents and your own Claude Code and Codex settings stay as they are.".into());
        } else if app_id == codex::CLI_SEPARATE_APP_ID || app_id == claude_code::SEPARATE_APP_ID {
            warnings.push(format!("{app} gets its own chats and settings, just for PrismFlow. Your usual one stays as it is."));
            warnings.push("Open it from PrismFlow, or use the command below.".into());
        } else if app_id == codex::DESKTOP_SEPARATE_APP_ID {
            warnings.push("Codex opens with its own chats and settings, just for PrismFlow. Your usual Codex stays as it is.".into());
            warnings.push("Quit the Codex app first, then open it from PrismFlow.".into());
        } else if app_id == codex::DESKTOP_APP_ID {
            if let Some(cur) = codex::read_base_priors(&self.codex_home).unwrap_or_default().get("model_provider").cloned().flatten() {
                if !cur.starts_with("prismflow_") {
                    warnings.push(format!("The Codex app currently uses \"{cur}\". PrismFlow puts it back when you disconnect."));
                }
            }
            warnings.push("Restart the Codex app after connecting.".into());
        } else {
            warnings.push(format!("Your own {app} settings won't change."));
        }
        if route == Route::Gateway {
            warnings.push("Keep PrismFlow open while you use it.".into());
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

    /// The checks `connect` would fail on, without changing anything: used
    /// before an app with one setup gives up its current one.
    pub fn ready_to_connect(&self, app_id: &str, provider_id: &str, model: &str) -> R<()> {
        let (p, route, _) = self.plan(app_id, provider_id, model)?;
        if self.usable_key(p).is_none() {
            return Err(format!("Add a {} key under Providers first.", p.display_name));
        }
        if route == Route::Gateway {
            GatewayRuntime::discover()?;
        }
        Ok(())
    }

    pub async fn connect(&self, app_id: &str, provider_id: &str, model: &str) -> R<ConnectionRecord> {
        let id = connection_id(app_id, provider_id);
        let existed = self.state.get(&id).ok().flatten().is_some();
        let result = self.connect_inner(app_id, provider_id, model).await;
        // A new app that couldn't be set up keeps no PrismFlow key behind.
        if result.is_err() && !existed && self.state.get(&id).ok().flatten().is_none() {
            let _ = tokens::revoke(self.store.as_ref(), &id);
        }
        result
    }

    async fn connect_inner(&self, app_id: &str, provider_id: &str, model: &str) -> R<ConnectionRecord> {
        if one_setup(app_id) {
            if let Some(other) = self.connections()?.into_iter().find(|r| r.app_id == app_id && r.provider_id != provider_id) {
                let name = providers::get(&other.provider_id).map_or(other.provider_id.as_str(), |p| p.display_name);
                return Err(format!("{} is connected to {name} already. Disconnect it first; the app has one setup.", app_name(app_id)));
            }
        }
        let existed = self.state.get(&connection_id(app_id, provider_id)).ok().flatten().is_some();
        let menu = self.app_menu(app_id, provider_id, validate_model_id(model)?).await;
        self.prepare_home(app_id)?;
        // Desktop: keep the original priors across model switches.
        let desktop_priors = if codex::is_desktop(app_id) {
            match self.state.get(&connection_id(app_id, provider_id)).ok().flatten() {
                Some(existing) => existing.priors,
                None => codex::read_base_priors(&self.desktop_home(app_id)).map_err(|e| e.to_string())?,
            }
        } else {
            Default::default()
        };
        let (p, route, mut plan) = self.plan(app_id, provider_id, model)?;
        if (codex::is_cli(app_id) || codex::is_desktop(app_id)) && self.usable_key(p).is_some() {
            if let Err(e) = self.write_codex_catalog(app_id, p, &menu).await {
                // Codex still works without a catalog (it falls back to defaults).
                self.diag.record(Level::Warn, "connect.catalog", &format!(
                    "No Codex model catalog for {}: {e}. Codex will use default model metadata.", p.display_name
                ));
                plan = self.plan_with(app_id, provider_id, model, None)?.2;
            }
        }
        if self.usable_key(p).is_none() {
            if p.credential_source == providers::CredentialSource::Keychain {
                if let Err(e @ crate::credentials::CredentialError::Store(_)) = self.store.get(&account_key(p.id, "default")) {
                    return Err(crate::credentials::store_problem(&e));
                }
            }
            return Err(format!("Add a {} key under Providers first.", p.display_name));
        }
        if route == Route::Gateway {
            GatewayRuntime::discover().map_err(|e| format!("Nothing was changed: {e}"))?;
        }
        let id = connection_id(app_id, p.id);
        if route == Route::Gateway {
            tokens::ensure(self.store.as_ref(), &id).map_err(|e| crate::credentials::store_problem(&e))?;
        }
        if let Some(spec) = agents::spec(app_id) {
            agents::write(&self.agent_files(spec, p, validate_model_id(model)?)?).map_err(|e| format!("Nothing usable was written: {e}"))?;
        }
        if app_id == jetbrains::APP_ID {
            let jp = self.jetbrains_plan(p, validate_model_id(model)?, &id, &self.broker.url_for_config())?;
            agents::write(&jp.owned).map_err(|e| format!("Nothing usable was written: {e}"))?;
        }
        if app_id == claude_desktop::SEPARATE_APP_ID {
            let token = tokens::ensure(self.store.as_ref(), &id).map_err(|e| crate::credentials::store_problem(&e))?;
            claude_desktop::connect_auto_in(&claude_desktop::separate_root(self.state.dir()), &self.broker.url_for_config(), &token, &id, &menu.labels())?;
        }
        if app_id == claude_desktop::APP_ID {
            let token = tokens::ensure(self.store.as_ref(), &id).map_err(|e| crate::credentials::store_problem(&e))?;
            if let Err(e) = claude_desktop::connect_auto(&self.broker.url_for_config(), &token, &id, &menu.labels()) {
                // Fall back to the guided steps (shown on the connection).
                self.diag.record(Level::Warn, "connect.claude_desktop", &format!("Automatic setup skipped: {e}"));
            }
        }
        if editors::ALL.contains(&app_id) {
            let token = tokens::ensure(self.store.as_ref(), &id).map_err(|e| crate::credentials::store_problem(&e))?;
            let ep = editors::plan(app_id, &agents::agent_dir(self.state.dir(), &id), self.state.dir(), &id, &format!("{}/v1", self.broker.url_for_config()), validate_model_id(model)?, Some(&token))?;
            agents::write(&ep.owned).map_err(|e| format!("Nothing usable was written: {e}"))?;
            plan = config_engine::EditPlan { file: ep.file, before: ep.before, after: ep.after, summary: plan.summary };
        }
        if vscode::is_vscode(app_id) {
            let token = tokens::ensure(self.store.as_ref(), &id).map_err(|e| crate::credentials::store_problem(&e))?;
            let vp = self.vscode_plan(app_id, p, validate_model_id(model)?, &id, &self.broker.url_for_config(), Some(&token))?;
            agents::write(&vp.owned).map_err(|e| format!("Nothing usable was written: {e}"))?;
            plan = config_engine::EditPlan { file: vp.file, before: vp.before, after: vp.after, summary: plan.summary };
        }
        let applied = if agents::spec(app_id).is_some() || claude_desktop::is_claude_desktop(app_id) || app_id == editors::CONTINUE_IDE {
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
            app_models: menu.models.iter().map(|m| m.id.clone()).collect(),
            priors: desktop_priors,
        };
        self.state.upsert(record.clone()).map_err(|e| e.to_string())?;
        self.diag.record(Level::Info, "connect.applied", &format!(
            "Wrote {} for {} → {} with model {} ({:?} route).", plan.file.display(), app_name(app_id), p.display_name, record.model, route
        ));
        if route == Route::Gateway {
            if let Err(e) = self.sync_gateway().await {
                self.diag.record(Level::Error, "connect.gateway", &format!("{id} is configured, but the gateway is not running: {e}"));
                // A new app that can't reach PrismFlow would only fail later;
                // undo it and say why now.
                if !existed {
                    let _ = self.disconnect(&id).await;
                    return Err(format!("Nothing was changed: {e}"));
                }
            }
        }
        Ok(record)
    }

    /// Choose which models the app shows in its own menu (the connection's
    /// model is always included, first). Rewrites the app's settings.
    pub async fn set_app_models(&self, id: &str, models: Vec<String>) -> R<ConnectionRecord> {
        let mut r = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let mut list: Vec<String> = models.into_iter().map(|m| m.trim().to_string()).filter(|m| !m.is_empty() && *m != r.model).collect();
        list.dedup();
        list.insert(0, r.model.clone());
        list.truncate(100);
        r.app_models = list;
        self.state.upsert(r.clone()).map_err(|e| e.to_string())?;
        self.connect(&r.app_id, &r.provider_id, &r.model).await
    }

    /// Change a gateway connection's tiers, reasoning policy and fallbacks.
    pub async fn set_routing(&self, id: &str, routing: crate::routing::Routing) -> R<ConnectionRecord> {
        let mut record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        if record.route != Route::Gateway {
            return Err("These options only work when the app goes through PrismFlow.".into());
        }
        if !claude_code::is_cli(&record.app_id) && !claude_desktop::is_claude_desktop(&record.app_id) && !routing.tiers.is_empty() {
            return Err("Different models per slot work with Claude Code and Claude Desktop only.".into());
        }
        if routing.fallbacks.len() > 5 {
            return Err("Use at most 5 fallback models.".into());
        }
        if routing.output_filter {
            if !claude_code::is_cli(&record.app_id) {
                return Err("The output filter works with Claude Code only.".into());
            }
            if claude_code::find_rtk().is_none() {
                return Err("RTK isn't installed. Install it from github.com/rtk-ai/rtk, then try again.".into());
            }
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
        if claude_code::is_cli(&record.app_id) {
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
    /// start it, reconfigure it, or stop it when none remain.
    /// True when a gateway connection uses a sign-in whose token expires
    /// (gcloud): the gateway then needs a fresh token regularly.
    pub fn needs_token_refresh(&self) -> bool {
        self.connections().unwrap_or_default().iter().any(|r| {
            r.route == Route::Gateway && providers::get(&r.provider_id).is_some_and(|p| p.credential_source == providers::CredentialSource::Gcloud)
        })
    }

    /// True when a Copilot token the gateway holds expires soon.
    pub fn copilot_needs_refresh(&self) -> bool {
        crate::credentials::copilot::expiring_within(Duration::from_secs(8 * 60))
    }

    pub async fn sync_gateway(&self) -> R<BrokerStatus> {
        // Renew PrismFlow's own ChatGPT sign-in before handing it out.
        if let Err(e) = self.renew_chatgpt(false).await {
            self.diag.record(Level::Warn, "chatgpt.renew", &e);
        }
        let records: Vec<ConnectionRecord> = self
            .state
            .load()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|r| r.route == Route::Gateway)
            .collect();
        let playground = PLAYGROUND.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if records.is_empty() && playground.is_none() {
            self.broker.stop().await;
            return Ok(self.broker.status());
        }
        let mut models: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some((p, m, _)) = &playground {
            models.entry(p.clone()).or_default().push(m.clone());
        }
        for r in &records {
            let primary = crate::routing::ModelRef::new(&r.provider_id, &r.model);
            // The app-menu models must be known to the engine too.
            for m in &r.app_models {
                let list = models.entry(r.provider_id.clone()).or_default();
                if !list.contains(m) {
                    list.push(m.clone());
                }
            }
            for target in r.routing.refs(&primary) {
                let m = models.entry(target.provider_id.clone()).or_default();
                if !m.contains(&target.model) {
                    m.push(target.model.clone());
                }
            }
        }
        // Local Ollama: use a large-context variant of each model (its default
        // 4k context is too small for coding agents).
        let mut renamed: std::collections::HashMap<(String, String), String> = Default::default();
        if let Some(list) = models.get_mut("ollama") {
            let root = crate::ollama::api_root(&self.upstream_url(provider("ollama")?));
            for m in list.iter_mut() {
                let want = u64::from(self.settings().ollama_context.max(8_192));
                match crate::ollama::ensure_context(&self.client, &root, m, want).await {
                    Ok(v) if v != *m => {
                        renamed.insert(("ollama".into(), m.clone()), v.clone());
                        *m = v;
                    }
                    Ok(_) => {}
                    Err(e) => self.diag.record(Level::Warn, "ollama.context", &format!("{m}: {e} Agents may run out of room on long prompts.")),
                }
            }
        }
        let rename = |r: &crate::routing::ModelRef| -> crate::routing::ModelRef {
            match renamed.get(&(r.provider_id.clone(), r.model.clone())) {
                Some(v) => crate::routing::ModelRef::new(&r.provider_id, v),
                None => r.clone(),
            }
        };
        let mut bindings = Vec::new();
        for r in &records {
            let token = match tokens::ensure(self.store.as_ref(), &r.id) {
                Ok(t) => t,
                Err(e) => {
                    // Leave this app out; the others keep working.
                    self.diag.record(Level::Error, "gateway.token", &format!("{}: {}", r.id, crate::credentials::store_problem(&e)));
                    continue;
                }
            };
            let mut routing = r.routing.clone();
            routing.tiers = routing.tiers.iter().map(|(t, m)| (*t, rename(m))).collect();
            routing.fallbacks = routing.fallbacks.iter().map(&rename).collect();
            bindings.push((token, Binding {
                connection_id: r.id.clone(),
                provider_id: r.provider_id.clone(),
                model: rename(&crate::routing::ModelRef::new(&r.provider_id, &r.model)).model,
                routing: std::sync::Arc::new(routing),
                models: std::sync::Arc::new(r.app_models.iter()
                    .map(|m| (m.clone(), rename(&crate::routing::ModelRef::new(&r.provider_id, m)).model))
                    .collect()),
            }));
        }
        if let Some((p, m, token)) = &playground {
            bindings.push((token.clone(), Binding {
                connection_id: format!("playground:{p}"),
                provider_id: p.clone(),
                model: rename(&crate::routing::ModelRef::new(p, m)).model,
                routing: Default::default(),
                models: Default::default(),
            }));
        }
        let mut specs = Vec::new();
        for (provider_id, models) in models {
            let p = provider(&provider_id)?;
            let Some(key) = self.usable_key(p) else {
                self.diag.record(Level::Warn, "gateway.no_key", &format!(
                    "{} has no key yet, so apps using it won't work until you add one.", p.display_name
                ));
                continue;
            };
            if p.credential_source == providers::CredentialSource::CopilotCli {
                // Copilot binds tokens to a model; one endpoint per provider here (its first model).
                crate::credentials::copilot::reset_expiry();
                let first = models.first().cloned();
                let ep = tokio::task::spawn_blocking(move || crate::credentials::copilot::endpoint(first.as_deref()))
                    .await
                    .map_err(|e| e.to_string())?;
                match ep {
                    Ok(ep) => specs.push(UpstreamSpec {
                        provider_id: p.id.into(),
                        engine_type: ep.engine_type,
                        base_url: ep.base_url,
                        models,
                        key: ep.api_key,
                        codex_oauth: false,
                        headers: ep.headers,
                        extra_keys: Vec::new(),
                    }),
                    Err(e) => {
                        crate::credentials::copilot::retry_soon();
                        self.diag.record(Level::Warn, "gateway.copilot", &format!("GitHub Copilot isn't available right now (trying again in a minute): {e}"));
                    }
                }
                continue;
            }
            specs.push(UpstreamSpec {
                provider_id: p.id.into(),
                engine_type: engine_type(p),
                base_url: self.upstream_url(p),
                models,
                key,
                codex_oauth: p.credential_source == providers::CredentialSource::CodexLogin,
                // ChatGPT's backend wants the account the token belongs to.
                headers: if p.credential_source == providers::CredentialSource::CodexLogin {
                    self.chatgpt_account_id().map(|a| vec![("chatgpt-account-id".to_string(), a)]).unwrap_or_default()
                } else {
                    Vec::new()
                },
                extra_keys: self.extra_keys(p.id),
            });
        }
        self.broker.start(specs, bindings).await.map_err(|e| e.to_string())
    }

    /// "Try a model": one chat turn with any provider + model, through the
    /// gateway (so every provider and sign-in works the same way).
    pub async fn try_chat(&self, provider_id: &str, model: &str, messages: Vec<serde_json::Value>) -> R<String> {
        let p = provider(provider_id)?;
        let model = validate_model_id(model)?;
        if self.usable_key(p).is_none() {
            return Err(format!("Add a {} key on the Providers page first.", p.display_name));
        }
        let changed = {
            let mut g = PLAYGROUND.lock().unwrap_or_else(|e| e.into_inner());
            let same = g.as_ref().is_some_and(|(pp, mm, _)| pp == provider_id && mm == model);
            if !same {
                *g = Some((provider_id.into(), model.into(), tokens::generate()));
            }
            !same
        };
        if changed || !self.broker.is_running().await {
            self.sync_gateway().await?;
        }
        let token = PLAYGROUND.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|x| x.2.clone()).unwrap_or_default();
        let body = serde_json::json!({ "model": model, "messages": messages, "max_tokens": 2048, "stream": false });
        let r = self.client.post(format!("{}/v1/chat/completions", self.broker.url())).bearer_auth(&token)
            .json(&body).timeout(self.agent_timeout).send().await
            .map_err(|e| if e.is_timeout() { "The model didn't answer in time. It may be busy or down; try again.".to_string() } else { "PrismFlow's background service didn't answer.".to_string() })?;
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        if status >= 400 {
            return Err(crate::usage::plain_error(status, &text));
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| "The reply couldn't be read.".to_string())?;
        Ok(v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
    }

    /// What to add by hand for Zed / Continue (includes this connection's local key).
    pub fn editor_guide(&self, id: &str) -> R<Vec<String>> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let token = tokens::ensure(self.store.as_ref(), id).map_err(|e| format!("Couldn't read the saved key: {e}"))?;
        Ok(editors::guide(&record.app_id, &format!("{}/v1", self.broker.url_for_config()), &record.model, &token))
    }

    pub fn gateway_status(&self) -> BrokerStatus {
        self.broker.status()
    }

    /// Connections from older versions where Codex fetched the provider key
    /// itself (`credential print`, now refused) are set up again through the
    /// gateway. Returns how many were moved.
    pub async fn move_direct_connections(&self) -> usize {
        let direct: Vec<ConnectionRecord> = self.connections().unwrap_or_default().into_iter().filter(|r| r.route == Route::Direct).collect();
        let mut moved = 0;
        for r in direct {
            match self.connect(&r.app_id, &r.provider_id, &r.model).await {
                Ok(_) => {
                    moved += 1;
                    self.diag.record(Level::Info, "connect.moved", &format!(
                        "{} now reaches {} through PrismFlow with its own key. Restart it to use the new setup.", app_name(&r.app_id), r.provider_id));
                }
                Err(e) => self.diag.record(Level::Warn, "connect.move_failed", &format!(
                    "{} still needs to be connected again: {e}", app_name(&r.app_id))),
            }
        }
        moved
    }

    pub async fn restart_gateway(&self) -> R<BrokerStatus> {
        self.broker.stop().await;
        self.sync_gateway().await
    }

    pub async fn verify(&self, id: &str) -> R<VerificationReport> {
        let _one = RunningTest::start(&format!("{}|{id}", self.state.dir().display()))?;
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
            state_dir: self.state.dir().to_path_buf(),
            gateway: (record.route == Route::Gateway).then(|| self.broker.status()),
        };
        let report = verify::verify(&env, p, &record).await;
        // The app may have been removed or switched while the test ran;
        // only record the result on the setup that was tested.
        if let Some(mut now) = self.state.get(id).map_err(|e| e.to_string())? {
            if now.provider_id == record.provider_id && now.model == record.model {
                now.status = report.status;
                now.last_report = Some(report.clone());
                self.state.upsert(now).map_err(|e| e.to_string())?;
            }
        }
        record.status = report.status;
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
        let removed = if record.app_id == claude_desktop::SEPARATE_APP_ID {
            // The copy's chats stay in its folder; only PrismFlow's setup goes.
            claude_desktop::disconnect_auto_in(&claude_desktop::separate_root(self.state.dir()), &record.id).map(|()| None)
        } else if record.app_id == claude_desktop::APP_ID {
            // Automatic setup: back to the Claude account, PrismFlow's entry removed.
            // Guided setup wrote nothing; revoking the token below is the disconnect.
            // If its settings are locked by a policy or damaged, PrismFlow
            // couldn't have written there, so removing the record is enough.
            claude_desktop::disconnect_auto(&record.id).map(|()| None).or_else(|e| {
                self.diag.record(Level::Warn, "disconnect.claude_desktop", &format!("Left Claude Desktop's files as they are: {e}"));
                Ok(None)
            })
        } else if codex::is_desktop(&record.app_id) {
            codex::plan_disconnect_base(&self.desktop_home(&record.app_id), &record.provider_id, &record.priors)
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
        } else if record.app_id == vscode::SEPARATE_APP_ID {
            let user = vscode::separate_data_dir(self.state.dir()).join("User");
            vscode::disconnect_claude_in(&user, &record.id)
                .and_then(|()| vscode::disconnect_chat_in(&user, &record.id))
                .map(|()| {
                    let _ = std::fs::remove_dir_all(agents::agent_dir(self.state.dir(), &record.id));
                    None
                })
        } else if vscode::is_claude(&record.app_id) {
            vscode::disconnect_claude(&record.id).map(|()| {
                let _ = std::fs::remove_dir_all(agents::agent_dir(self.state.dir(), &record.id));
                None
            })
        } else if record.app_id == vscode::CHAT_APP_ID {
            vscode::disconnect_chat(&record.id).map(|()| None)
        } else if editors::ALL.contains(&record.app_id.as_str()) {
            editors::disconnect(&record.app_id, &record.id, self.state.dir()).map(|()| {
                let _ = std::fs::remove_dir_all(agents::agent_dir(self.state.dir(), &record.id));
                None
            })
        } else if record.app_id == jetbrains::APP_ID {
            jetbrains::disconnect(&record.id).map(|()| {
                let _ = std::fs::remove_dir_all(agents::agent_dir(self.state.dir(), &record.id));
                None
            })
        } else if claude_code::is_cli(&record.app_id) {
            claude_code::disconnect(&record.managed_file, &self.state.backups_dir()).map_err(|e| e.to_string())
        } else {
            codex::disconnect(&record.managed_file, &self.state.backups_dir()).map_err(|e| e.to_string())
        };
        let backup = removed.map_err(|e| {
            let msg = format!("Disconnect stopped; nothing was removed: {e}");
            self.diag.record(Level::Error, "disconnect.failed", &msg);
            msg
        })?;
        if codex::is_cli(&record.app_id) || codex::is_desktop(&record.app_id) {
            let mut catalogs = vec![self.codex_catalog_path(&record.app_id, &record.provider_id)];
            // Codex app connections made before each had its own list used
            // the CLI's.
            let legacy = self.codex_catalog_path(codex::APP_ID, &record.provider_id);
            let cli_connected = self.connections().unwrap_or_default().iter()
                .any(|r| r.app_id == codex::APP_ID && r.provider_id == record.provider_id);
            if record.app_id == codex::DESKTOP_APP_ID && !cli_connected {
                catalogs.push(legacy.clone());
            }
            // ...and such a Codex app still points at the CLI's list.
            let in_use = codex::read_base_priors(&self.codex_home).ok()
                .and_then(|v| v.get("model_catalog_json").cloned().flatten())
                .is_some_and(|c| std::path::Path::new(&c) == legacy);
            for c in catalogs.iter().filter(|c| !(in_use && **c == legacy)) {
                let _ = std::fs::remove_file(c);
            }
        }
        if record.route == Route::Gateway {
            if let Err(e) = tokens::revoke(self.store.as_ref(), id) {
                // The app's files are already restored; finish removing it.
                // The key is useless once the broker forgets this connection.
                self.diag.record(Level::Warn, "disconnect.token", &format!("{id}: couldn't delete its key: {}", crate::credentials::store_problem(&e)));
            }
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
    /// profile; they keep their old settings until restarted.
    pub fn running_sessions(&self, id: &str) -> R<Vec<u32>> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let procs = adapters::processes::list();
        Ok(if claude_desktop::is_claude_desktop(&record.app_id) || codex::is_desktop(&record.app_id) || record.app_id == jetbrains::APP_ID || vscode::is_vscode(&record.app_id) || editors::ALL.contains(&record.app_id.as_str()) {
            Vec::new() // desktop apps: restart is shown in the UI instead
        } else if agents::spec(&record.app_id).is_some() {
            let dir = agents::agent_dir(self.state.dir(), &record.id).display().to_string();
            procs.iter().filter(|p| p.command.contains(&dir)).map(|p| p.pid).collect()
        } else if claude_code::is_cli(&record.app_id) {
            adapters::processes::claude_sessions(&procs, &record.managed_file.display().to_string())
        } else if record.app_id == codex::CLI_SEPARATE_APP_ID {
            // Its profile has the same name as the shared one's and CODEX_HOME
            // isn't on the command line, so its sessions can't be told apart.
            Vec::new()
        } else {
            adapters::processes::codex_sessions(&procs, &record.profile)
        })
    }

    /// Remove everything PrismFlow added:
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
        accounts.push((crate::webtools::BRAVE_ACCOUNT.to_string(), "Brave Search key".into()));
        accounts.push((crate::credentials::chatgpt::ACCOUNT.to_string(), "PrismFlow's ChatGPT sign-in".into()));
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
            push("Stop the background service".into(), Planned, String::new());
            push(format!("Delete {}", dir.display()), Planned, "State, backups, diagnostics, settings, custom providers".into());
        } else {
            self.broker.stop().await;
            push("Stop the background service".into(), Done, String::new());
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
        let token = tokens::ensure(self.store.as_ref(), id).map_err(|e| format!("Couldn't read the saved key: {e}"))?;
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

    /// The command that starts the app with this connection ("" when the
    /// app is opened from its own window).
    /// How the app starts with this connection: the command (empty when
    /// there's none to type) and which button PrismFlow offers.
    pub fn launch_info(&self, id: &str) -> R<LaunchInfo> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        let a = record.app_id.as_str();
        let opens = if agents::spec(a).is_some() || codex::is_cli(a) || claude_code::is_cli(a) {
            Opens::Terminal
        } else if [vscode::SEPARATE_APP_ID, editors::ZED_SEPARATE].contains(&a)
            || (cfg!(target_os = "macos") && (codex::is_desktop(a) || claude_desktop::is_claude_desktop(a))) {
            Opens::App
        } else {
            Opens::Nothing // opened from its own window (IDEs, editors)
        };
        Ok(LaunchInfo { line: self.launch_line(a, &record.profile, &record.managed_file), opens })
    }

    pub fn launch(&self, id: &str) -> R<()> {
        let record = self.state.get(id).map_err(|e| e.to_string())?.ok_or("Unknown connection.")?;
        if record.app_id == claude_desktop::SEPARATE_APP_ID {
            claude_desktop::open_separate(self.state.dir())?;
            self.diag.record(Level::Info, "launch", "Opened Claude Desktop (separate).");
            return Ok(());
        }
        if record.app_id == claude_desktop::APP_ID {
            open_app(claude_desktop::BUNDLE_ID)?;
            self.diag.record(Level::Info, "launch", "Opened Claude Desktop.");
            return Ok(());
        }
        if record.app_id == jetbrains::APP_ID {
            return Err("Open your JetBrains IDE. The PrismFlow agents are in AI Assistant's agent list.".into());
        }
        if record.app_id == vscode::SEPARATE_APP_ID {
            vscode::open_separate(&vscode::separate_data_dir(self.state.dir()))?;
            self.diag.record(Level::Info, "launch", "Opened VS Code (separate).");
            return Ok(());
        }
        if vscode::is_vscode(&record.app_id) {
            return Err("Open VS Code. PrismFlow is set up there.".into());
        }
        if record.app_id == editors::ZED_SEPARATE {
            let token = tokens::ensure(self.store.as_ref(), &record.id).map_err(|e| e.to_string())?;
            editors::open_zed_separate(self.state.dir(), &token)?;
            self.diag.record(Level::Info, "launch", "Opened Zed (separate).");
            return Ok(());
        }
        if editors::ALL.contains(&record.app_id.as_str()) {
            return Err(format!("Open {}. PrismFlow is set up there.", editors::name(&record.app_id)));
        }
        if record.app_id == codex::DESKTOP_SEPARATE_APP_ID {
            open_separate_codex(&self.desktop_home(&record.app_id))?;
            self.diag.record(Level::Info, "launch", "Opened the Codex app (separate).");
            return Ok(());
        }
        if record.app_id == codex::DESKTOP_APP_ID {
            open_app(codex::DESKTOP_BUNDLE_ID)?;
            self.diag.record(Level::Info, "launch", "Opened the Codex app.");
            return Ok(());
        }
        if let Some(a) = agents::spec(&record.app_id) {
            agents::find_binary(a).ok_or_else(|| format!("{} is not installed.", a.display_name))?;
        } else if claude_code::is_cli(&record.app_id) {
            claude_code::find_binary().ok_or("Claude Code is not installed.")?;
        } else {
            codex::find_binary().ok_or("Codex CLI is not installed.")?;
        }
        self.prepare_home(&record.app_id)?;
        let env = self.launch_env(&record.app_id);
        let plain = self.command_line(&record.app_id, &record.profile, &record.managed_file);
        // The Terminal on macOS/Linux runs the line in a shell, so the
        // variables go in front of it; on Windows they go on the new window.
        #[cfg(not(windows))]
        let line = with_env(&env, plain);
        #[cfg(not(windows))]
        open_terminal(&line)?;
        #[cfg(windows)]
        let line = plain;
        #[cfg(windows)]
        open_terminal(&line, &env)?;
        self.diag.record(Level::Info, "launch", &format!("Opened Terminal with `{line}`."));
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn open_app(bundle_id: &str) -> R<()> {
    let ok = crate::spawn::command("open").args(["-b", bundle_id]).status().is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err("The app could not be opened.".into()) }
}

/// Opens a new Codex app with PrismFlow's own CODEX_HOME. A Codex app that
/// is already open would take the request instead, so it must be quit first.
#[cfg(target_os = "macos")]
fn open_separate_codex(home: &std::path::Path) -> R<()> {
    let app = codex::detect_desktop().path.ok_or("The Codex app isn't installed.")?;
    let running = crate::spawn::command("pgrep").args(["-f", &format!("{app}/Contents/MacOS/")]).output()
        .is_ok_and(|o| o.status.success());
    if running {
        return Err("The Codex app is already open. Quit it (Codex → Quit), then press Open again.".into());
    }
    std::fs::create_dir_all(home).map_err(|_| "PrismFlow couldn't set up the Codex app's own folder.")?;
    let ok = crate::spawn::command("open")
        .args(["-n", "-b", codex::DESKTOP_BUNDLE_ID, "--env", &format!("CODEX_HOME={}", home.display())])
        .status().is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err("The app could not be opened.".into()) }
}

#[cfg(not(target_os = "macos"))]
fn open_separate_codex(_home: &std::path::Path) -> R<()> {
    Err("Opening the separate Codex app is macOS-only in this build.".into())
}

#[cfg(not(target_os = "macos"))]
fn open_app(_bundle_id: &str) -> R<()> {
    Err("Opening desktop apps from PrismFlow is macOS-only in this build; open the app yourself.".into())
}

#[cfg(target_os = "macos")]
fn open_terminal(line: &str) -> R<()> {
    let script = line.replace('\\', "\\\\").replace('"', "\\\"");
    let status = crate::spawn::command("osascript")
        .args(["-e", &format!("tell application \"Terminal\" to do script \"{script}\"")])
        .args(["-e", "tell application \"Terminal\" to activate"])
        .status()
        .map_err(|e| format!("Could not open Terminal: {e}"))?;
    if status.success() { Ok(()) } else { Err("Terminal refused the launch request.".into()) }
}

/// Linux: the first available terminal emulator runs the line in a shell
/// that stays open afterwards.
#[cfg(all(unix, not(target_os = "macos")))]
fn open_terminal(line: &str) -> R<()> {
    let keep = format!("{line}; exec \"${{SHELL:-/bin/sh}}\"");
    let candidates: [(&str, Vec<&str>); 5] = [
        ("x-terminal-emulator", vec!["-e", "sh", "-c"]),
        ("gnome-terminal", vec!["--", "sh", "-c"]),
        ("konsole", vec!["-e", "sh", "-c"]),
        ("xfce4-terminal", vec!["-x", "sh", "-c"]),
        ("xterm", vec!["-e", "sh", "-c"]),
    ];
    for (term, args) in candidates {
        if crate::adapters::find_exe(term).is_some() {
            crate::spawn::command(term).args(&args).arg(&keep).spawn().map_err(|e| format!("Could not open {term}: {e}"))?;
            return Ok(());
        }
    }
    Err("No terminal emulator found; run the command shown instead.".into())
}

/// Windows: a new console window that stays open (`cmd /k`).
#[cfg(windows)]
/// `env` goes on the process; the new window inherits it. The command line
/// is passed as written: `start` needs its title in quotes, and cmd.exe
/// doesn't understand the `\"` escaping Rust would add around `line`.
fn open_terminal(line: &str, env: &[(&str, String)]) -> R<()> {
    use std::os::windows::process::CommandExt;
    crate::spawn::command("cmd").raw_arg(format!("/c start \"PrismFlow\" cmd /k {line}"))
        .envs(env.iter().map(|(k, v)| (*k, v.as_str()))).spawn()
        .map_err(|e| format!("Could not open a console: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn chatgpt_models_come_from_codex_cache_without_hidden_ones() {
        let dir = tempfile::tempdir().unwrap();
        assert!(super::codex_plan_models(dir.path()).unwrap_err().contains("Open Codex once"));
        std::fs::write(dir.path().join("models_cache.json"), r#"{"models":[
            {"slug":"gpt-a","display_name":"GPT-A","visibility":"list","context_window":272000},
            {"slug":"gpt-hidden","visibility":"hide"}]}"#).unwrap();
        let m = super::codex_plan_models(dir.path()).unwrap();
        assert_eq!(m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-a"]);
        assert_eq!(m[0].context_length, Some(272000));
    }

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

    #[test]
    fn local_server_addresses_can_change_and_be_imported() {
        let _turn = crate::accounts::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut s = service(dir.path());
        s.upstream_overrides.clear();
        let ollama = providers::get("ollama").unwrap();
        assert_eq!(s.set_provider_url("ollama", "http://192.168.1.20:11434").unwrap(), "http://192.168.1.20:11434/v1");
        assert_eq!(s.upstream_url(ollama), "http://192.168.1.20:11434/v1");
        assert!(s.set_provider_url("openai", "http://x").is_err(), "only local servers");
        assert!(s.set_provider_url("ollama", "not a url").is_err());
        s.set_provider_url("ollama", "").unwrap();
        assert_eq!(s.upstream_url(ollama), ollama.base_url);
        assert_eq!(providers::get("nvidia_nim").unwrap().example_model, "nvidia/nemotron-3-super-120b-a12b");
        let fx = dir.path().join("imp");
        std::fs::create_dir_all(&fx).unwrap();
        std::fs::write(fx.join("shell.env"), "OLLAMA_HOST=127.0.0.1:11500\n").unwrap();
        std::env::set_var("PRISMFLOW_IMPORT_TEST_DIR", &fx);
        let f = s.scan_keys();
        std::env::remove_var("PRISMFLOW_IMPORT_TEST_DIR");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].masked, "http://127.0.0.1:11500", "addresses are shown in full");
    }

    #[tokio::test]
    async fn backup_restores_on_a_fresh_install_without_keys() {
        let d1 = tempfile::tempdir().unwrap();
        let a = service(d1.path());
        a.set_credential("openrouter", "sk-or-v1-secretsecret").unwrap();
        a.connect(CODEX, "openrouter", "a/b").await.unwrap();
        let mut st = a.settings();
        st.budgets.insert("openrouter".into(), 25);
        a.set_settings(st).await.unwrap();
        a.save_profile("Work").unwrap();
        let text = serde_json::to_string(&a.backup().unwrap()).unwrap();
        assert!(!text.contains("secretsecret"), "no keys in a backup");

        let d2 = tempfile::tempdir().unwrap();
        let b = service(d2.path());
        let rep = b.restore(serde_json::from_str(&text).unwrap()).await.unwrap();
        assert_eq!(b.settings().budgets.get("openrouter"), Some(&25));
        assert!(b.profiles().list.contains_key("Work"));
        // No key on the new machine yet: the connection is reported, not lost.
        assert!(rep.problems.iter().any(|p| p.contains("openrouter")), "{rep:?}");
        b.set_credential("openrouter", "sk-or-v1-newmachinekey").unwrap();
        b.switch_profile("Work").await.unwrap();
        assert_eq!(b.connections().unwrap()[0].model, "a/b");
    }

    #[tokio::test]
    async fn keys_import_extra_keys_and_profiles() {
        let _turn = crate::accounts::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let fx = dir.path().join("import");
        std::fs::create_dir_all(&fx).unwrap();
        std::fs::write(fx.join("shell.env"), "OPENROUTER_API_KEY=sk-or-v1-aaaaaaaaaaaa\nHOME=/x\n").unwrap();
        std::fs::write(fx.join("home.env"), "OPENROUTER_API_KEY=sk-or-v1-bbbbbbbbbbbb\nGROQ_API_KEY=gsk_cccccccccccc\n").unwrap();
        std::env::set_var("PRISMFLOW_IMPORT_TEST_DIR", &fx);
        let found = s.scan_keys();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().all(|f| !f.masked.contains("aaaaaaaa")), "masked only");
        let chosen: Vec<_> = found.iter().map(|f| (f.provider_id.clone(), f.variable.clone(), f.source.clone())).collect();
        assert_eq!(s.import_keys(&chosen).unwrap(), 3);
        // Two OpenRouter keys: main + one extra; nothing imported twice.
        assert_eq!(s.key_count("openrouter"), 2);
        assert_eq!(s.extra_keys("openrouter"), vec!["sk-or-v1-bbbbbbbbbbbb".to_string()]);
        assert!(s.scan_keys().iter().all(|f| f.already_saved));
        std::env::remove_var("PRISMFLOW_IMPORT_TEST_DIR");
        s.remove_extra_keys("openrouter").unwrap();
        assert_eq!(s.key_count("openrouter"), 1);

        // Profiles: save, change, switch back.
        s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        s.save_profile("Work").unwrap();
        s.disconnect("codex_cli:openrouter").await.unwrap();
        s.connect(CODEX, "openrouter", "c/d").await.unwrap();
        s.save_profile("Personal").unwrap();
        s.switch_profile("Work").await.unwrap();
        let conns = s.connections().unwrap();
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].model, "a/b");
        assert_eq!(s.profiles().active.as_deref(), Some("Work"));
    }

    /// Live: "Try a model" through the real gateway to the local Ollama, which
    /// also creates the 32k-context variant. PRISMFLOW_LIVE_TRY=1 cargo test try_chat_live -- --ignored
    #[tokio::test]
    #[ignore]
    async fn try_chat_live_with_local_ollama() {
        if std::env::var("PRISMFLOW_LIVE_TRY").is_err() { return; }
        let dir = tempfile::tempdir().unwrap();
        let mut s = service(dir.path());
        s.upstream_overrides.insert("ollama".into(), "http://127.0.0.1:11434/v1".into());
        s.agent_timeout = Duration::from_secs(300);
        let model = "qwen3:4b-instruct-2507-q4_K_M";
        let reply = s.try_chat("ollama", model, vec![serde_json::json!({"role": "user", "content": "Reply with exactly PRISMFLOW_TRY_OK and nothing else."})]).await.unwrap();
        eprintln!("reply: {reply}");
        assert!(reply.contains("PRISMFLOW_TRY_OK"));
        let mut logged = Vec::new();
        for _ in 0..40 {
            logged = s.broker.usage().recent(5);
            if !logged.is_empty() { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        eprintln!("logged: {:?}", logged.first().map(|r| (&r.model, r.input_tokens, r.output_tokens)));
        assert!(logged.first().is_some_and(|r| r.model.ends_with("-pf32k")), "routed to the 32k variant");
        s.broker.stop().await;
        let _ = s.client.delete("http://127.0.0.1:11434/api/delete").json(&serde_json::json!({"model": format!("{model}-pf32k")})).send().await;
    }

    #[tokio::test]
    async fn connect_requires_a_key_then_records_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        assert!(s.connect(CODEX, "openrouter", "a/b").await.unwrap_err().contains("key"));
        s.set_credential("openrouter", "  sk-or-v1-abcdefabcdef  ").unwrap();
        let rec = s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        assert_eq!(rec.status, ConnectionStatus::Unverified);
        assert_eq!(rec.route, Route::Gateway, "Codex gets its own local token, never the provider key");
        assert!(rec.managed_file.exists());
        assert_eq!(s.connections().unwrap().len(), 1);
        assert!(s.providers().iter().any(|p| p.def.id == "openrouter" && p.has_credential));
    }

    #[test]
    fn routing_matrix_matches_the_compatibility_matrix() {
        let get = |id| providers::get(id).unwrap();
        assert_eq!(route_for(CODEX, get("openrouter")), Some(Route::Gateway));
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
        assert!(preview.warnings.iter().any(|w| w.contains("Keep PrismFlow open")));
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
        let err = s.add_custom_provider("Box", "http://localhost:4000/v1", Protocol::OpenAiChat, false, false, Default::default()).await.unwrap_err();
        assert!(err.contains("Confirm"));
        let p = s.add_custom_provider("Box", "http://localhost:4000/v1/", Protocol::OpenAiChat, false, true, Default::default()).await.unwrap();
        assert_eq!(p.id, "custom_box");
        assert_eq!(p.base_url, "http://localhost:4000/v1");
        assert_eq!(route_for(CLAUDE, &p), Some(Route::Gateway));
        assert_eq!(s.state.load_custom_providers().unwrap().len(), 1);
        // reload from disk into a fresh registry view
        providers::set_custom(&[]);
        s.load_custom_providers().unwrap();
        assert!(providers::get("custom_box").is_some());
        assert!(s.add_custom_provider("Bad", "http://example.com/v1", Protocol::OpenAiChat, true, true, Default::default()).await.is_err());
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
        let original = "# mine\nmodel = \"nvidia_nim/z-ai/glm\"\nmodel_provider = \"myproxy\"\n\n[model_providers.myproxy]\nbase_url = \"http://127.0.0.1:9090/v1\"\n";
        std::fs::write(&base, original).unwrap();
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        let preview = s.preview(codex::DESKTOP_APP_ID, "openrouter", "a/b").unwrap();
        assert!(preview.warnings.iter().any(|w| w.contains("\"myproxy\"")), "{:?}", preview.warnings);
        let rec = s.connect(codex::DESKTOP_APP_ID, "openrouter", "a/b").await.unwrap();
        assert_eq!(rec.priors["model_provider"].as_deref(), Some("myproxy"));
        assert!(std::fs::read_to_string(&base).unwrap().contains("model_provider = \"prismflow_openrouter\""));
        s.set_credential("groq", "gsk_abcdefabcdefabcdef").unwrap();
        assert!(s.connect(codex::DESKTOP_APP_ID, "groq", "x").await.unwrap_err().contains("Disconnect it first"));
        // A model switch keeps the original priors.
        let rec = s.connect(codex::DESKTOP_APP_ID, "openrouter", "c/d").await.unwrap();
        assert_eq!(rec.priors["model_provider"].as_deref(), Some("myproxy"));
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

    #[tokio::test]
    async fn separate_codex_app_leaves_your_codex_alone() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let base = s.codex_home.join("config.toml");
        std::fs::write(&base, "# mine\nmodel = \"gpt-5\"\n").unwrap();
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        // Both at once: the shared app and the separate one.
        let shared = s.connect(codex::DESKTOP_APP_ID, "openrouter", "a/b").await.unwrap();
        let mine = std::fs::read_to_string(&base).unwrap();
        let sep = s.connect(codex::DESKTOP_SEPARATE_APP_ID, "openrouter", "c/d").await.unwrap();
        assert_eq!(std::fs::read_to_string(&base).unwrap(), mine, "separate must not touch the usual Codex config");
        let home = codex::separate_home(s.state.dir());
        assert_eq!(sep.managed_file, home.join("config.toml"));
        let cfg = std::fs::read_to_string(&sep.managed_file).unwrap();
        assert!(cfg.contains("model = \"c/d\"") && cfg.contains("model_provider = \"prismflow_openrouter\""), "{cfg}");
        // macOS opens it with `open --env`; elsewhere PrismFlow can't open it.
        let line = s.preview(codex::DESKTOP_SEPARATE_APP_ID, "openrouter", "c/d").unwrap().launch_command;
        assert_eq!(line.contains("--env CODEX_HOME="), cfg!(target_os = "macos"), "{line}");
        // Each has its own model list (written when the Codex CLI is installed).
        assert_ne!(s.codex_catalog_path(codex::DESKTOP_APP_ID, "openrouter"), s.codex_catalog_path(codex::DESKTOP_SEPARATE_APP_ID, "openrouter"));
        s.disconnect(&sep.id).await.unwrap();
        assert!(!s.codex_catalog_path(codex::DESKTOP_SEPARATE_APP_ID, "openrouter").exists());
        assert!(!std::fs::read_to_string(&sep.managed_file).unwrap_or_default().contains("prismflow_openrouter"));
        assert!(std::fs::read_to_string(&base).unwrap().contains("prismflow_openrouter"), "the shared one is still connected");
        s.disconnect(&shared.id).await.unwrap();
        assert!(!s.codex_catalog_path(codex::DESKTOP_APP_ID, "openrouter").exists());
    }

    #[tokio::test]
    async fn separate_clis_use_their_own_folders() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        let codex_before = walk(&s.codex_home);
        let claude_before = walk(&s.claude_dir);
        let cx = s.connect(codex::CLI_SEPARATE_APP_ID, "openrouter", "a/b").await.unwrap();
        let cc = s.connect(claude_code::SEPARATE_APP_ID, "openrouter", "a/b").await.unwrap();
        assert_eq!(walk(&s.codex_home), codex_before, "usual Codex folder untouched");
        assert_eq!(walk(&s.claude_dir), claude_before, "usual Claude folder untouched");
        assert!(cx.managed_file.starts_with(s.state.dir().join("codex-cli-separate")));
        assert!(cc.managed_file.starts_with(s.state.dir().join("claude-separate")));
        assert!(s.state.dir().join("claude-separate/.claude.json").is_file(), "first-run screens skipped");
        let line = s.preview(codex::CLI_SEPARATE_APP_ID, "openrouter", "a/b").unwrap().launch_command;
        assert!(line.contains("CODEX_HOME=") && line.contains("-p prismflow-openrouter"), "{line}");
        let line = s.preview(claude_code::SEPARATE_APP_ID, "openrouter", "a/b").unwrap().launch_command;
        assert!(line.contains("CLAUDE_CONFIG_DIR=") && line.contains("--settings"), "{line}");
        // The shared ones still work next to them.
        let shared = s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        assert!(shared.managed_file.starts_with(&s.codex_home));
        for r in [cx, cc, shared] {
            s.disconnect(&r.id).await.unwrap();
        }
        assert!(walk(&s.state.dir().join("codex-cli-separate")).iter().all(|f| !f.display().to_string().ends_with(".config.toml")));
    }

    #[tokio::test]
    async fn vscode_shared_and_separate() {
        let _turn = crate::accounts::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("vscode-user");
        std::fs::create_dir_all(&user).unwrap();
        std::env::set_var("PRISMFLOW_VSCODE_USER_DIR", &user);
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();

        // Shared: the extension runs Claude through PrismFlow's binary; no key
        // in VS Code's settings, ~/.claude still used.
        let mine = r#"{"editor.fontSize": 14, "claudeCode.disableLoginPrompt": false, "claudeCode.environmentVariables": [{"name": "FOO", "value": "1"}]}"#;
        std::fs::write(user.join("settings.json"), mine).unwrap();
        let sh = s.connect(vscode::CLAUDE_SHARED_APP_ID, "openrouter", "a/b").await.unwrap();
        let text = std::fs::read_to_string(user.join("settings.json")).unwrap();
        assert!(!text.contains("pft_"), "no key in VS Code's settings: {text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["claudeCode.claudeProcessWrapper"], s.helper.display().to_string());
        assert_eq!(v["claudeCode.disableLoginPrompt"], false, "the user's own value stays");
        let vars = v["claudeCode.environmentVariables"].as_array().unwrap();
        let get = |n: &str| vars.iter().find(|x| x["name"] == n).map(|x| x["value"].as_str().unwrap().to_string());
        assert_eq!(get("FOO").as_deref(), Some("1"));
        assert!(get("CLAUDE_CONFIG_DIR").is_none(), "shared uses the usual ~/.claude");
        let settings = vscode::claude_shared_settings(&sh.id).unwrap();
        let claude: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(claude["apiKeyHelper"].as_str().unwrap().contains("token print"), "the key is fetched at run time");
        // One setup at a time in the extension.
        assert!(s.connect(vscode::CLAUDE_APP_ID, "openrouter", "a/b").await.unwrap_err().contains("Disconnect it first"));
        s.disconnect(&sh.id).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(user.join("settings.json")).unwrap()).unwrap();
        assert_eq!(v, serde_json::from_str::<serde_json::Value>(mine).unwrap(), "disconnect puts the settings back exactly");

        // Separate window: everything in PrismFlow's own folder.
        let before = std::fs::read_to_string(user.join("settings.json")).unwrap();
        let sep = s.connect(vscode::SEPARATE_APP_ID, "openrouter", "a/b").await.unwrap();
        assert_eq!(std::fs::read_to_string(user.join("settings.json")).unwrap(), before, "usual VS Code untouched");
        let own = vscode::separate_data_dir(s.state.dir()).join("User");
        assert_eq!(sep.managed_file, own.join("settings.json"));
        assert!(vscode::claude_config_dir_in(&own, &sep.id).unwrap().join("settings.json").is_file());
        assert!(std::fs::read_to_string(own.join("chatLanguageModels.json")).unwrap().contains("/v1/messages"));
        assert!(s.preview(vscode::SEPARATE_APP_ID, "openrouter", "a/b").unwrap().launch_command.contains("--user-data-dir"));
        s.disconnect(&sep.id).await.unwrap();
        assert!(vscode::claude_config_dir_in(&own, &sep.id).is_none());
        assert!(!std::fs::read_to_string(own.join("chatLanguageModels.json")).unwrap().contains("/v1/messages"));
        std::env::remove_var("PRISMFLOW_VSCODE_USER_DIR");
    }

    #[tokio::test]
    async fn switching_a_one_setup_app_replaces_its_setup() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        s.set_credential("groq", "gsk_abcdefabcdefabcdef").unwrap();
        let first = s.connect(claude_desktop::SEPARATE_APP_ID, "openrouter", "a/b").await.unwrap();
        let err = s.connect(claude_desktop::SEPARATE_APP_ID, "groq", "x").await.unwrap_err();
        assert!(err.contains("Disconnect it first") && err.contains("OpenRouter"), "{err}");
        crate::tray::switch(&s, &first.id, "groq", "x").await.unwrap();
        let now: Vec<String> = s.connections().unwrap().into_iter().map(|c| c.id).collect();
        assert_eq!(now, vec![connection_id(claude_desktop::SEPARATE_APP_ID, "groq")]);
        assert!(claude_desktop::auto_active(&claude_desktop::separate_root(s.state.dir()), &now[0]), "the new setup is the live one");
    }

    #[tokio::test]
    async fn an_older_codex_app_setup_keeps_its_model_list() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        // Before per-connection lists, the Codex app pointed at the CLI's.
        let legacy = s.codex_catalog_path(CODEX, "openrouter");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "{}").unwrap();
        std::fs::write(s.codex_home.join("config.toml"), format!("model_catalog_json = {:?}\n", legacy.display().to_string())).unwrap();
        let cli = s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        s.disconnect(&cli.id).await.unwrap();
        assert!(legacy.exists(), "the Codex app still uses it");
    }

    #[tokio::test]
    async fn older_direct_codex_connections_move_to_the_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        s.set_credential("openrouter", "sk-or-v1-abcdefabcdef").unwrap();
        // What an older version left: a direct record whose Codex profile
        // fetched the provider key with `credential print`.
        let rec = s.connect(CODEX, "openrouter", "a/b").await.unwrap();
        std::fs::write(&rec.managed_file, format!("{}\n[model_providers.prismflow_openrouter.auth]\ncommand = \"x\"\nargs = [\"credential\", \"print\", \"--provider\", \"openrouter\"]\n", codex::MANAGED_MARKER)).unwrap();
        s.state.upsert(ConnectionRecord { route: Route::Direct, ..rec }).unwrap();

        assert_eq!(s.move_direct_connections().await, 1);
        let now = s.connections().unwrap();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].route, Route::Gateway);
        let profile = std::fs::read_to_string(&now[0].managed_file).unwrap();
        assert!(profile.contains("\"token\", \"print\"") && !profile.contains("credential"), "{profile}");
        assert_eq!(s.move_direct_connections().await, 0, "nothing left to move");
        // No file an app reads asks for a provider key (backups keep the old one).
        for f in walk(&s.codex_home).into_iter().chain(walk(s.state.dir())).filter(|f| !f.starts_with(s.state.backups_dir())) {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            assert!(!text.contains("\"credential\", \"print\""), "{} still uses credential print", f.display());
        }
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

    #[cfg(windows)]
    #[test]
    fn launch_commands_quote_paths_on_windows() {
        assert_eq!(launch_command("C:/a b/codex.exe", "p"), "\"C:/a b/codex.exe\" -p p");
    }

    #[cfg(unix)]
    #[test]
    fn launch_commands_quote_paths() {
        assert_eq!(launch_command("/a b/codex", "prismflow-openrouter"), "'/a b/codex' -p prismflow-openrouter");
        assert_eq!(launch_command("/it's/codex", "p"), r"'/it'\''s/codex' -p p");
        assert_eq!(claude_launch_command("/x/claude", "/h/.claude/p.settings.json"), "'/x/claude' --settings '/h/.claude/p.settings.json'");
    }
}
