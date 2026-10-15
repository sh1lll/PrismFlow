pub mod cli;
pub mod config_engine;
pub mod credentials;
pub mod providers;

#[tauri::command]
fn ping() -> String {
    "pong".to_string()
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
