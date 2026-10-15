pub mod adapters;
pub mod bots;
pub mod cli;
pub mod companion;
pub mod config_engine;
pub mod credentials;
pub mod diagnostics;
pub mod discovery;
pub mod gateway;
pub mod ollama;
pub mod providers;
pub mod routing;
pub mod service;
pub mod sessions;
pub mod state;
pub mod verify;
pub mod webtools;

use service::{ConnectPreview, ProviderView, Service};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

type Cmd<T> = Result<T, String>;

/// Runs work that may wait on the Keychain or start other programs off the
/// main thread, so the window keeps drawing (and can say what it waits for).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Cmd<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// True while a Keychain request has waited more than a moment, which on
/// macOS means its "allow access" window is open.
#[tauri::command]
fn keychain_waiting() -> bool {
    credentials::keychain::waiting()
}

#[tauri::command]
async fn list_providers(svc: State<'_, Arc<Service>>) -> Cmd<Vec<ProviderView>> {
    let svc = svc.inner().clone();
    blocking(move || svc.providers()).await
}

#[tauri::command]
async fn set_credential(svc: State<'_, Arc<Service>>, provider_id: String, secret: String) -> Cmd<()> {
    svc.set_credential(&provider_id, &secret)?;
    // The engine reads upstream keys at spawn; restart it to pick this one up.
    if svc.provider_in_gateway(&provider_id) {
        svc.sync_gateway().await?;
    }
    Ok(())
}

/// "Sign in with OpenRouter" (official OAuth): the new key is saved like a pasted one.
#[tauri::command]
async fn openrouter_sign_in(svc: State<'_, Arc<Service>>) -> Cmd<()> {
    use credentials::oauth;
    let key = oauth::openrouter_sign_in(&svc.client, oauth::OPENROUTER_AUTH, oauth::OPENROUTER_EXCHANGE, oauth::open_in_browser).await?;
    let s = svc.inner().clone();
    blocking(move || s.set_credential("openrouter", &key)).await??;
    if svc.provider_in_gateway("openrouter") {
        svc.sync_gateway().await?;
    }
    Ok(())
}

#[tauri::command]
async fn delete_credential(svc: State<'_, Arc<Service>>, provider_id: String) -> Cmd<()> {
    let svc = svc.inner().clone();
    blocking(move || svc.delete_credential(&provider_id)).await?
}

#[tauri::command]
async fn discover_models(svc: State<'_, Arc<Service>>, provider_id: String) -> Cmd<Vec<discovery::ModelInfo>> {
    svc.discover_models(&provider_id).await
}

#[tauri::command]
async fn list_applications(svc: State<'_, Arc<Service>>) -> Cmd<Vec<adapters::AppDetection>> {
    let svc = svc.inner().clone();
    blocking(move || svc.applications()).await
}

#[tauri::command]
async fn preview_connection(svc: State<'_, Arc<Service>>, app_id: String, provider_id: String, model: String) -> Cmd<ConnectPreview> {
    let svc = svc.inner().clone();
    blocking(move || svc.preview(&app_id, &provider_id, &model)).await?
}

#[tauri::command]
async fn connect(svc: State<'_, Arc<Service>>, app_id: String, provider_id: String, model: String) -> Cmd<state::ConnectionRecord> {
    svc.connect(&app_id, &provider_id, &model).await
}

#[tauri::command]
async fn verify_connection(svc: State<'_, Arc<Service>>, id: String) -> Cmd<state::VerificationReport> {
    svc.verify(&id).await
}

#[tauri::command]
async fn disconnect(svc: State<'_, Arc<Service>>, id: String) -> Cmd<Option<String>> {
    svc.disconnect(&id).await
}

#[tauri::command]
fn gateway_status(svc: State<'_, Arc<Service>>) -> gateway::BrokerStatus {
    svc.gateway_status()
}

#[tauri::command]
async fn restart_gateway(svc: State<'_, Arc<Service>>) -> Cmd<gateway::BrokerStatus> {
    svc.restart_gateway().await
}

#[tauri::command]
fn running_sessions(svc: State<'_, Arc<Service>>, id: String) -> Cmd<Vec<u32>> {
    svc.running_sessions(&id)
}

#[tauri::command]
async fn check_custom_url(svc: State<'_, Arc<Service>>, url: String) -> Cmd<providers::UrlCheck> {
    svc.check_custom_url(&url).await
}

#[tauri::command]
async fn add_custom_provider(
    svc: State<'_, Arc<Service>>,
    name: String,
    url: String,
    protocol: providers::Protocol,
    requires_key: bool,
    consent: bool,
    gcloud: Option<bool>,
) -> Cmd<providers::ProviderDefinition> {
    let source = if gcloud.unwrap_or(false) { providers::CredentialSource::Gcloud } else { providers::CredentialSource::Keychain };
    svc.add_custom_provider(&name, &url, protocol, requires_key, consent, source).await
}

#[tauri::command]
async fn remove_custom_provider(svc: State<'_, Arc<Service>>, id: String) -> Cmd<()> {
    let svc = svc.inner().clone();
    blocking(move || svc.remove_custom_provider(&id)).await?
}

#[tauri::command]
async fn set_routing(svc: State<'_, Arc<Service>>, id: String, routing: routing::Routing) -> Cmd<state::ConnectionRecord> {
    svc.set_routing(&id, routing).await
}

#[tauri::command]
fn get_settings(svc: State<'_, Arc<Service>>) -> gateway::RuntimeSettings {
    svc.settings()
}

#[tauri::command]
async fn set_settings(svc: State<'_, Arc<Service>>, settings: gateway::RuntimeSettings) -> Cmd<gateway::RuntimeSettings> {
    svc.set_settings(settings).await
}

#[tauri::command]
async fn uninstall(svc: State<'_, Arc<Service>>, dry_run: bool) -> Cmd<service::UninstallReport> {
    svc.uninstall(dry_run).await
}

#[derive(serde::Serialize)]
struct UpdateInfo {
    current: String,
    available: Option<String>,
    notes: Option<String>,
}

#[cfg(feature = "updater")]
#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Cmd<UpdateInfo> {
    use tauri_plugin_updater::UpdaterExt;
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    Ok(UpdateInfo {
        current: env!("CARGO_PKG_VERSION").into(),
        available: update.as_ref().map(|u| u.version.clone()),
        notes: update.and_then(|u| u.body),
    })
}

#[cfg(feature = "updater")]
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Cmd<()> {
    use tauri_plugin_updater::UpdaterExt;
    // Signature verification against the embedded pubkey happens inside the plugin.
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?
        .ok_or("PrismFlow is up to date.")?;
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    app.restart();
}

#[cfg(not(feature = "updater"))]
#[tauri::command]
async fn check_update() -> Cmd<UpdateInfo> {
    Err("This test version of PrismFlow can't update itself.".into())
}

#[cfg(not(feature = "updater"))]
#[tauri::command]
async fn install_update() -> Cmd<()> {
    Err("This build of PrismFlow has no updater.".into())
}

#[tauri::command]
async fn guided_setup(svc: State<'_, Arc<Service>>, id: String) -> Cmd<adapters::claude_desktop::GuidedSetup> {
    svc.guided_setup(&id).await
}

/// Remote access state held beside the service (default off).
pub struct Companion {
    registry: Arc<companion::Registry>,
    running: tokio::sync::Mutex<Option<companion::server::Running>>,
    dir: std::path::PathBuf,
}

#[derive(serde::Serialize)]
struct CompanionView {
    enabled: bool,
    url: Option<String>,
    fingerprint: Option<String>,
    settings: companion::CompanionSettings,
    devices: Vec<companion::Device>,
    suggested_address: Option<String>,
}

async fn companion_view(c: &Companion) -> CompanionView {
    let r = c.running.lock().await;
    CompanionView {
        enabled: r.is_some(),
        url: r.as_ref().map(|x| x.url.clone()),
        fingerprint: r.as_ref().map(|x| x.fingerprint.clone()),
        settings: c.registry.settings(),
        devices: c.registry.devices(),
        suggested_address: companion::primary_address(),
    }
}

async fn companion_start(svc: &Arc<Service>, c: &Companion, s: companion::CompanionSettings) -> Cmd<()> {
    let ip: std::net::IpAddr = s.address.trim().parse().map_err(|_| "Enter this computer's address on the network your phone uses, e.g. 192.168.1.20 or a Tailscale 100.x address.".to_string())?;
    let mut r = c.running.lock().await;
    if let Some(old) = r.take() {
        old.stop().await;
    }
    let backend: Arc<dyn companion::server::Backend> = Arc::new(companion::backend::ServiceBackend(svc.clone()));
    let run = companion::server::start(ip, s.port, &c.dir, c.registry.clone(), backend).await?;
    svc.diag.record(diagnostics::Level::Warn, "companion.on", &format!("Remote access is ON at {} (certificate {}).", run.url, &run.fingerprint[..16]));
    *r = Some(run);
    c.registry.set_settings(s)
}

#[tauri::command]
async fn companion_state(c: State<'_, Companion>) -> Cmd<CompanionView> {
    Ok(companion_view(&c).await)
}

#[tauri::command]
async fn companion_enable(svc: State<'_, Arc<Service>>, c: State<'_, Companion>, address: String, port: u16, keep_on: bool) -> Cmd<CompanionView> {
    companion_start(&svc, &c, companion::CompanionSettings { address, port, keep_on }).await?;
    Ok(companion_view(&c).await)
}

#[tauri::command]
async fn companion_disable(svc: State<'_, Arc<Service>>, c: State<'_, Companion>) -> Cmd<CompanionView> {
    if let Some(run) = c.running.lock().await.take() {
        run.stop().await;
    }
    c.registry.cancel_pairing();
    let mut s = c.registry.settings();
    s.keep_on = false;
    c.registry.set_settings(s)?;
    svc.diag.record(diagnostics::Level::Info, "companion.off", "Remote access is off.");
    Ok(companion_view(&c).await)
}

#[derive(serde::Serialize)]
struct PairingView {
    uri: String,
    qr_svg: String,
    code: String,
    url: String,
    fingerprint: String,
    expires_in_secs: u64,
}

#[tauri::command]
async fn companion_pair(c: State<'_, Companion>) -> Cmd<PairingView> {
    let r = c.running.lock().await;
    let run = r.as_ref().ok_or("Turn remote access on first.")?;
    let code = c.registry.start_pairing();
    let link = format!("{}/?c={code}", run.url);
    Ok(PairingView {
        uri: companion::pairing_uri(&run.url, &run.fingerprint, &code),
        qr_svg: companion::qr_svg(&link)?,
        code,
        url: run.url.clone(),
        fingerprint: run.fingerprint.clone(),
        expires_in_secs: companion::PAIRING_TTL.as_secs(),
    })
}

#[tauri::command]
async fn companion_revoke(svc: State<'_, Arc<Service>>, c: State<'_, Companion>, id: String) -> Cmd<CompanionView> {
    c.registry.revoke(&id)?;
    svc.diag.record(diagnostics::Level::Info, "companion.revoked", "A paired device was removed.");
    Ok(companion_view(&c).await)
}

#[tauri::command]
async fn companion_allow_actions(c: State<'_, Companion>, id: String, allow: bool) -> Cmd<CompanionView> {
    c.registry.set_allow_actions(&id, allow)?;
    Ok(companion_view(&c).await)
}

#[tauri::command]
async fn set_signin_consent(svc: State<'_, Arc<Service>>, provider_id: String, consent: bool) -> Cmd<()> {
    svc.set_signin_consent(&provider_id, consent).await
}

#[tauri::command]
async fn set_web_search_key(svc: State<'_, Arc<Service>>, key: String) -> Cmd<()> {
    let svc = svc.inner().clone();
    blocking(move || svc.set_web_search_key(&key)).await?
}

#[tauri::command]
async fn has_web_search_key(svc: State<'_, Arc<Service>>) -> Cmd<bool> {
    let svc = svc.inner().clone();
    blocking(move || svc.has_web_search_key()).await
}


/// Chat bots (Telegram / Discord), see src/bots.
pub struct BotState {
    bots: bots::Bots,
    dir: std::path::PathBuf,
}

#[derive(serde::Serialize)]
struct BotsView {
    settings: bots::BotSettings,
    status: bots::BotsStatus,
    has_telegram_token: bool,
    has_discord_token: bool,
    whisper_installed: bool,
    ffmpeg_installed: bool,
}

async fn bots_apply(svc: &Arc<Service>, sessions: &sessions::Sessions, b: &BotState, settings: &bots::BotSettings) -> Cmd<()> {
    let voice_key = providers::get(&settings.voice.provider_id).and_then(|p| svc.usable_key(p));
    let (svc2, sessions2, s2) = (svc.clone(), sessions.clone(), settings.clone());
    let make = move |platform: &'static str| -> Arc<dyn bots::Runner> {
        Arc::new(bots::CodexRunner::new(svc2.clone(), sessions2.clone(), &s2, platform))
    };
    b.bots.apply(settings, svc.store.as_ref(), make, voice_key).await
}

#[tauri::command]
async fn bots_get(svc: State<'_, Arc<Service>>, b: State<'_, BotState>) -> Cmd<BotsView> {
    let settings = bots::BotSettings::load(&b.dir);
    let s = svc.inner().clone();
    let (tg, dc) = blocking(move || (s.store.get(bots::TELEGRAM_TOKEN).is_ok(), s.store.get(bots::DISCORD_TOKEN).is_ok())).await?;
    Ok(BotsView {
        settings,
        status: b.bots.status().await,
        has_telegram_token: tg,
        has_discord_token: dc,
        whisper_installed: bots::voice::find_whisper().is_some(),
        ffmpeg_installed: adapters::find_exe("ffmpeg").is_some(),
    })
}

/// Saves the settings and (re)starts the enabled bots.
#[tauri::command]
async fn bots_save(svc: State<'_, Arc<Service>>, sessions: State<'_, sessions::Sessions>, b: State<'_, BotState>, settings: bots::BotSettings) -> Cmd<()> {
    settings.save(&b.dir)?;
    let r = bots_apply(&svc, &sessions, &b, &settings).await;
    let on = settings.telegram.enabled || settings.discord.enabled;
    svc.diag.record(diagnostics::Level::Info, "bots.apply", if r.is_ok() && on { "Chat bots started." } else if r.is_ok() { "Chat bots are off." } else { "Chat bots could not start." });
    r
}

#[tauri::command]
async fn bots_set_token(svc: State<'_, Arc<Service>>, platform: String, token: String) -> Cmd<()> {
    let account = match platform.as_str() {
        "telegram" => bots::TELEGRAM_TOKEN,
        "discord" => bots::DISCORD_TOKEN,
        _ => return Err("Unknown chat app.".into()),
    };
    let token = token.trim().to_string();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err("Paste the bot token exactly as given, with no spaces.".into());
    }
    let s = svc.inner().clone();
    blocking(move || s.store.set(account, &token).map_err(|e| format!("Couldn't save the token: {e}"))).await?
}

#[tauri::command]
async fn bots_stop(b: State<'_, BotState>) -> Cmd<()> {
    b.bots.stop_all().await;
    Ok(())
}

#[tauri::command]
fn session_list(sessions: State<'_, sessions::Sessions>) -> Vec<sessions::Session> {
    sessions.list()
}

#[tauri::command]
fn session_create(sessions: State<'_, sessions::Sessions>, folder: String, sandbox: sessions::Sandbox, title: String) -> Cmd<sessions::Session> {
    sessions.create(std::path::Path::new(folder.trim()), sandbox, &title)
}

/// Send a message; each turn may use a different Codex CLI connection.
#[tauri::command]
async fn session_send(svc: State<'_, Arc<Service>>, sessions: State<'_, sessions::Sessions>, id: String, connection_id: String, prompt: String) -> Cmd<()> {
    sessions.send(&svc, &id, &connection_id, &prompt).await
}

#[tauri::command]
fn session_events(sessions: State<'_, sessions::Sessions>, id: String, since: usize) -> Cmd<sessions::EventsPage> {
    sessions.events(&id, since).ok_or_else(|| "Unknown session.".to_string())
}

#[tauri::command]
fn session_stop(sessions: State<'_, sessions::Sessions>, id: String) -> Cmd<()> {
    sessions.stop(&id)
}

#[tauri::command]
fn session_delete(sessions: State<'_, sessions::Sessions>, id: String) -> Cmd<()> {
    sessions.delete(&id)
}

#[tauri::command]
fn list_connections(svc: State<'_, Arc<Service>>) -> Cmd<Vec<state::ConnectionRecord>> {
    svc.connections()
}

#[tauri::command]
fn launch_connection(svc: State<'_, Arc<Service>>, id: String) -> Cmd<()> {
    svc.launch(&id)
}

#[tauri::command]
fn get_diagnostics(svc: State<'_, Arc<Service>>) -> Vec<diagnostics::Event> {
    svc.diagnostics(200)
}

#[tauri::command]
fn export_diagnostics(svc: State<'_, Arc<Service>>) -> Cmd<String> {
    svc.export_diagnostics()
}

pub fn run() {
    let dir = state::state_dir();
    let state_store = state::StateStore::open(&dir).expect("PrismFlow state directory must be writable");
    let helper = std::env::current_exe().expect("current executable path");
    let diag = std::sync::Arc::new(diagnostics::DiagnosticsLog::new(dir.join("diagnostics.jsonl")));
    let service = Service {
        store: credentials::default_store(),
        state: state_store,
        broker: gateway::Broker::new(gateway::configured_port(), dir.clone(), Some(diag.clone())),
        diag,
        client: discovery::client(),
        codex_home: adapters::codex::codex_home(),
        claude_dir: adapters::claude_code::config_dir(),
        helper,
        // Local models can take minutes on a coding agent's first, long prompt.
        agent_timeout: Duration::from_secs(300),
        upstream_overrides: Default::default(),
    };
    if let Err(e) = service.load_custom_providers() {
        service.diag.record(diagnostics::Level::Error, "provider.custom_load", &format!("Custom providers could not be loaded: {e}"));
    }
    service.diag.record(diagnostics::Level::Info, "app.start", concat!("PrismFlow ", env!("CARGO_PKG_VERSION"), " started."));
    let builder = tauri::Builder::default();
    #[cfg(feature = "updater")]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    let service = Arc::new(service);
    let companion_dir = dir.join("companion");
    let companion_state_holder = Companion {
        registry: Arc::new(companion::Registry::open(&companion_dir).expect("companion state directory must be writable")),
        running: tokio::sync::Mutex::new(None),
        dir: companion_dir,
    };
    builder
        // One setup hook: Tauri keeps only the last `.setup()` call.
        .setup(|app| {
            use tauri::Manager;
            // Release bundles ship the pinned engine and a Node runtime as
            // resources (scripts/fetch-*.sh); prefer them over the system.
            if let Ok(res) = app.path().resource_dir() {
                gateway::supervisor::use_bundled_runtime(&res);
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let svc = handle.state::<Arc<Service>>().inner().clone();
                // Gateway connections from earlier sessions need the broker back.
                if let Err(e) = svc.sync_gateway().await {
                    svc.diag.record(diagnostics::Level::Error, "gateway.startup", &e);
                }
                // Google Cloud tokens last an hour: refresh the gateway's copy.
                let refresher = svc.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(45 * 60)).await;
                        if refresher.needs_token_refresh() && !refresher.copilot_needs_refresh() {
                            if let Err(e) = refresher.sync_gateway().await {
                                refresher.diag.record(diagnostics::Level::Warn, "gateway.refresh", &e);
                            }
                        }
                    }
                });
                // GitHub Copilot tokens are short-lived: refresh before they expire.
                let copilot = svc.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        if copilot.copilot_needs_refresh() {
                            if let Err(e) = copilot.sync_gateway().await {
                                copilot.diag.record(diagnostics::Level::Warn, "gateway.copilot_refresh", &e);
                            }
                        }
                    }
                });
                // Chat bots the user turned on come back with the app.
                let b = handle.state::<BotState>();
                let bs = bots::BotSettings::load(&b.dir);
                if bs.telegram.enabled || bs.discord.enabled {
                    let sessions = handle.state::<sessions::Sessions>();
                    if let Err(e) = bots_apply(&svc, &sessions, &b, &bs).await {
                        svc.diag.record(diagnostics::Level::Error, "bots.startup", &e);
                    }
                }
                // Remote access comes back only if the user chose "Keep on".
                let c = handle.state::<Companion>();
                let settings = c.registry.settings();
                if settings.keep_on {
                    if let Err(e) = companion_start(&svc, &c, settings).await {
                        svc.diag.record(diagnostics::Level::Error, "companion.startup", &e);
                    }
                }
            });
            Ok(())
        })
        .manage(service)
        .manage(companion_state_holder)
        .manage(sessions::Sessions::open(&dir))
        .manage(BotState { bots: bots::Bots::default(), dir: dir.clone() })
        .invoke_handler(tauri::generate_handler![
            keychain_waiting,
            bots_get,
            bots_save,
            bots_set_token,
            bots_stop,
            openrouter_sign_in,
            list_providers,
            set_credential,
            delete_credential,
            discover_models,
            list_applications,
            preview_connection,
            connect,
            verify_connection,
            disconnect,
            list_connections,
            running_sessions,
            set_routing,
            session_list,
            session_create,
            session_send,
            session_events,
            session_stop,
            session_delete,
            set_web_search_key,
            has_web_search_key,
            set_signin_consent,
            companion_state,
            companion_enable,
            companion_disable,
            companion_pair,
            companion_revoke,
            companion_allow_actions,
            guided_setup,
            check_update,
            install_update,
            uninstall,
            get_settings,
            set_settings,
            check_custom_url,
            add_custom_provider,
            remove_custom_provider,
            launch_connection,
            get_diagnostics,
            export_diagnostics,
            gateway_status,
            restart_gateway,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
