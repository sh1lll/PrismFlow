pub mod adapters;
pub mod cli;
pub mod config_engine;
pub mod credentials;
pub mod diagnostics;
pub mod discovery;
pub mod gateway;
pub mod providers;
pub mod routing;
pub mod service;
pub mod state;
pub mod verify;

use service::{ConnectPreview, ProviderView, Service};
use std::time::Duration;
use tauri::State;

type Cmd<T> = Result<T, String>;

#[tauri::command]
fn list_providers(svc: State<'_, Service>) -> Vec<ProviderView> {
    svc.providers()
}

#[tauri::command]
async fn set_credential(svc: State<'_, Service>, provider_id: String, secret: String) -> Cmd<()> {
    svc.set_credential(&provider_id, &secret)?;
    // The engine reads upstream keys at spawn; restart it to pick this one up.
    if svc.provider_in_gateway(&provider_id) {
        svc.sync_gateway().await?;
    }
    Ok(())
}

#[tauri::command]
fn delete_credential(svc: State<'_, Service>, provider_id: String) -> Cmd<()> {
    svc.delete_credential(&provider_id)
}

#[tauri::command]
async fn discover_models(svc: State<'_, Service>, provider_id: String) -> Cmd<Vec<discovery::ModelInfo>> {
    svc.discover_models(&provider_id).await
}

#[tauri::command]
fn list_applications(svc: State<'_, Service>) -> Vec<adapters::AppDetection> {
    svc.applications()
}

#[tauri::command]
fn preview_connection(svc: State<'_, Service>, app_id: String, provider_id: String, model: String) -> Cmd<ConnectPreview> {
    svc.preview(&app_id, &provider_id, &model)
}

#[tauri::command]
async fn connect(svc: State<'_, Service>, app_id: String, provider_id: String, model: String) -> Cmd<state::ConnectionRecord> {
    svc.connect(&app_id, &provider_id, &model).await
}

#[tauri::command]
async fn verify_connection(svc: State<'_, Service>, id: String) -> Cmd<state::VerificationReport> {
    svc.verify(&id).await
}

#[tauri::command]
async fn disconnect(svc: State<'_, Service>, id: String) -> Cmd<Option<String>> {
    svc.disconnect(&id).await
}

#[tauri::command]
fn gateway_status(svc: State<'_, Service>) -> gateway::BrokerStatus {
    svc.gateway_status()
}

#[tauri::command]
async fn restart_gateway(svc: State<'_, Service>) -> Cmd<gateway::BrokerStatus> {
    svc.restart_gateway().await
}

#[tauri::command]
fn running_sessions(svc: State<'_, Service>, id: String) -> Cmd<Vec<u32>> {
    svc.running_sessions(&id)
}

#[tauri::command]
async fn check_custom_url(svc: State<'_, Service>, url: String) -> Cmd<providers::UrlCheck> {
    svc.check_custom_url(&url).await
}

#[tauri::command]
async fn add_custom_provider(
    svc: State<'_, Service>,
    name: String,
    url: String,
    protocol: providers::Protocol,
    requires_key: bool,
    consent: bool,
) -> Cmd<providers::ProviderDefinition> {
    svc.add_custom_provider(&name, &url, protocol, requires_key, consent).await
}

#[tauri::command]
fn remove_custom_provider(svc: State<'_, Service>, id: String) -> Cmd<()> {
    svc.remove_custom_provider(&id)
}

#[tauri::command]
async fn set_routing(svc: State<'_, Service>, id: String, routing: routing::Routing) -> Cmd<state::ConnectionRecord> {
    svc.set_routing(&id, routing).await
}

#[tauri::command]
fn get_settings(svc: State<'_, Service>) -> gateway::RuntimeSettings {
    svc.settings()
}

#[tauri::command]
async fn set_settings(svc: State<'_, Service>, settings: gateway::RuntimeSettings) -> Cmd<gateway::RuntimeSettings> {
    svc.set_settings(settings).await
}

#[tauri::command]
async fn uninstall(svc: State<'_, Service>, dry_run: bool) -> Cmd<service::UninstallReport> {
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
    Err("This build of PrismFlow has no updater (development build). Release builds check signed updates.".into())
}

#[cfg(not(feature = "updater"))]
#[tauri::command]
async fn install_update() -> Cmd<()> {
    Err("This build of PrismFlow has no updater.".into())
}

#[tauri::command]
fn list_connections(svc: State<'_, Service>) -> Cmd<Vec<state::ConnectionRecord>> {
    svc.connections()
}

#[tauri::command]
fn launch_connection(svc: State<'_, Service>, id: String) -> Cmd<()> {
    svc.launch(&id)
}

#[tauri::command]
fn get_diagnostics(svc: State<'_, Service>) -> Vec<diagnostics::Event> {
    svc.diagnostics(200)
}

#[tauri::command]
fn export_diagnostics(svc: State<'_, Service>) -> Cmd<String> {
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
        agent_timeout: Duration::from_secs(120),
        upstream_overrides: Default::default(),
    };
    if let Err(e) = service.load_custom_providers() {
        service.diag.record(diagnostics::Level::Error, "provider.custom_load", &format!("Custom providers could not be loaded: {e}"));
    }
    service.diag.record(diagnostics::Level::Info, "app.start", concat!("PrismFlow ", env!("CARGO_PKG_VERSION"), " started."));
    let builder = tauri::Builder::default();
    #[cfg(feature = "updater")]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
        .setup(|app| {
            use tauri::Manager;
            // Release bundles ship the pinned engine and a Node runtime as
            // resources (scripts/fetch-*.sh); prefer them over the system.
            if let Ok(res) = app.path().resource_dir() {
                gateway::supervisor::use_bundled_runtime(&res);
            }
            Ok(())
        })
        .manage(service)
        .setup(|app| {
            // Gateway connections from earlier sessions need the broker back.
            use tauri::Manager;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let svc = handle.state::<Service>();
                if let Err(e) = svc.sync_gateway().await {
                    svc.diag.record(diagnostics::Level::Error, "gateway.startup", &e);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
