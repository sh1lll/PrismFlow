pub mod adapters;
pub mod cli;
pub mod config_engine;
pub mod credentials;
pub mod diagnostics;
pub mod discovery;
pub mod gateway;
pub mod providers;
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
    service.diag.record(diagnostics::Level::Info, "app.start", concat!("PrismFlow ", env!("CARGO_PKG_VERSION"), " started."));
    tauri::Builder::default()
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
            launch_connection,
            get_diagnostics,
            export_diagnostics,
            gateway_status,
            restart_gateway,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
