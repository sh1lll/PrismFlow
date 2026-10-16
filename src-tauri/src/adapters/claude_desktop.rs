//! Claude Desktop (third-party inference).
//!
//! Two ways, like another tool:
//! - **Automatic** (default): PrismFlow adds a "PrismFlow" gateway entry to
//!   Claude Desktop's third-party configuration library (`Claude-3p/
//!   configLibrary`), selects it and switches the app to third-party mode.
//!   Claude Desktop picks it up when reopened. Disconnect selects the empty
//!   "Default" entry, switches back to your Claude account, and removes
//!   PrismFlow's entry. Layout checked 2026-10-15 against Claude Desktop
//!   2.37337.1's library on this Mac and another tool's integration (facts only).
//! - **Guided**: the values to enter in Developer → Configure Third-Party
//!   Inference yourself (`guided_setup`).

use super::AppDetection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_ID: &str = "claude_desktop";
pub const BUNDLE_ID: &str = "com.anthropic.claudefordesktop";
/// A second Claude Desktop just for PrismFlow, next to your signed-in one.
/// Electron keeps one running copy per `--user-data-dir`, and in third-party
/// mode Claude Desktop uses `<data dir>-3p` (checked 2026-10-16 with Claude
/// Desktop 2.42304.0: both copies ran at once and the usual folders were
/// not touched).
pub const SEPARATE_APP_ID: &str = "claude_desktop_separate";

pub fn is_claude_desktop(app_id: &str) -> bool {
    app_id == APP_ID || app_id == SEPARATE_APP_ID
}

/// The separate copy's `--user-data-dir`.
pub fn separate_data_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("claude-desktop-separate").join("data")
}

/// Where the separate copy keeps its third-party setup.
pub fn separate_root(state_dir: &Path) -> PathBuf {
    state_dir.join("claude-desktop-separate").join("data-3p")
}

pub fn detect_separate() -> AppDetection {
    let mut d = detect();
    d.app_id = SEPARATE_APP_ID.into();
    d.display_name = "Claude Desktop (separate)".into();
    d.notes = vec![
        "A second Claude Desktop just for PrismFlow. Your usual one stays signed in to your Claude account.".into(),
        "Open it from PrismFlow; both can be open at once.".into(),
    ];
    d
}

/// Opens the separate copy.
#[cfg(target_os = "macos")]
pub fn open_separate(state_dir: &Path) -> Result<(), String> {
    let app = find_app().ok_or("Claude Desktop isn't installed.")?;
    let data = separate_data_dir(state_dir);
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let ok = Command::new("open").arg("-n").arg("-a").arg(&app)
        .arg("--args").arg(format!("--user-data-dir={}", data.display()))
        .status().map_err(|e| e.to_string())?.success();
    if ok { Ok(()) } else { Err("Claude Desktop could not be opened.".into()) }
}

#[cfg(not(target_os = "macos"))]
pub fn open_separate(_state_dir: &Path) -> Result<(), String> {
    Err("The separate Claude Desktop is macOS-only in this build.".into())
}

pub fn find_app() -> Option<PathBuf> {
    ["/Applications/Claude.app"]
        .iter()
        .map(PathBuf::from)
        .chain(dirs::home_dir().map(|h| h.join("Applications/Claude.app")))
        .find(|p| {
            Command::new("/usr/bin/defaults")
                .args(["read", &p.join("Contents/Info").display().to_string(), "CFBundleIdentifier"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == BUNDLE_ID)
        })
}

pub fn detect() -> AppDetection {
    let app = find_app();
    let version = app.as_ref().and_then(|p| {
        Command::new("/usr/bin/defaults")
            .args(["read", &p.join("Contents/Info").display().to_string(), "CFBundleShortVersionString"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    AppDetection {
        app_id: APP_ID.into(),
        display_name: "Claude Desktop".into(),
        installed: app.is_some(),
        path: app.map(|p| p.display().to_string()),
        version,
        version_verified: false,
        notes: vec![
            "PrismFlow shows you what to enter in Claude Desktop.".into(),
            "While set up this way, Claude Desktop doesn't use your Claude account.".into(),
        ],
    }
}

/// What the user enters in Claude Desktop. The token is the connection's
/// revocable local token, valid only on this machine's loopback gateway.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GuidedSetup {
    pub gateway_url: String,
    pub auth_scheme: &'static str,
    pub token: String,
    pub model: String,
    pub steps: Vec<String>,
}

pub fn guided_setup(gateway_url: &str, token: String, model: &str) -> GuidedSetup {
    GuidedSetup {
        gateway_url: gateway_url.into(),
        auth_scheme: "Bearer",
        token,
        model: model.into(),
        steps: vec![
            "In Claude Desktop, turn on Developer Mode: Help → Troubleshooting → Enable Developer Mode.".into(),
            "Open Developer → Configure Third-Party Inference, and choose Gateway.".into(),
            format!("Paste the address below."),
            "Paste the key below.".into(),
            format!("Enter the model name: {model}"),
            "Press Test connection, then Save. Restart Claude Desktop if it asks.".into(),
            "To undo: pick your Claude account again in the same screen, then press Disconnect here.".into(),
        ],
    }
}

/// PrismFlow's entry id in the library (stable, PrismFlow's own namespace).
pub const PRISMFLOW_ID: &str = "8b6c2f1e-5a3d-5c7e-9f10-7072697366ff";
/// The empty "Default" entry Claude Desktop falls back to (shared with another tool,
/// so both tools agree on the neutral profile).
pub const DEFAULT_ID: &str = "dfa3d5c4-9f50-5e40-8aac-a12c8c32a0e0";
pub const OWNER_HEADER: &str = "X-PrismFlow-Connection";

pub fn config_root() -> PathBuf {
    if let Some(p) = std::env::var_os("PRISMFLOW_CLAUDE_3P_DIR") {
        return PathBuf::from(p);
    }
    // Tests never touch the real app's files.
    if cfg!(test) {
        return std::env::temp_dir().join(format!("prismflow-test-{}-claude-3p", std::process::id()));
    }
    let base = if cfg!(target_os = "macos") {
        dirs::home_dir().unwrap_or_default().join("Library/Application Support")
    } else if cfg!(windows) {
        dirs::data_local_dir().unwrap_or_default()
    } else {
        dirs::config_dir().unwrap_or_default()
    };
    base.join("Claude-3p")
}

fn read(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(None),
        Ok(t) => serde_json::from_str(t.trim_start_matches('\u{feff}')).map(Some).map_err(|_| format!("{} isn't valid JSON, so PrismFlow won't change it.", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Couldn't read {}: {e}", path.display())),
    }
}

fn write(path: &Path, v: &Value) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::config_engine::atomic_write(path, &(serde_json::to_string_pretty(v).unwrap_or_default() + "\n")).map_err(|e| e.to_string())
}

/// An organization policy (MDM) decides Claude Desktop's setup; don't fight it.
fn managed_by_policy() -> bool {
    let user = std::env::var("USER").unwrap_or_default();
    [
        format!("/Library/Managed Preferences/{BUNDLE_ID}.plist"),
        format!("/Library/Managed Preferences/{user}/{BUNDLE_ID}.plist"),
        "/etc/claude-desktop/managed-settings.json".into(),
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

struct Library {
    root: PathBuf,
    meta: Option<Value>,
    mode: Value,
}

impl Library {
    fn load(root: &Path) -> Result<Self, String> {
        if std::env::var_os("PRISMFLOW_CLAUDE_3P_DIR").is_none() && managed_by_policy() {
            return Err("Your organization manages Claude Desktop's settings, so PrismFlow can't change them. Use the guided setup instead.".into());
        }
        let meta = read(&root.join("configLibrary/_meta.json"))?;
        if let Some(m) = &meta {
            let entries = m["entries"].as_array().ok_or("Claude Desktop's configuration list looks damaged; PrismFlow won't change it.")?;
            if entries.iter().any(|e| e["id"] == PRISMFLOW_ID && e["name"] != "PrismFlow") {
                return Err("Claude Desktop has another configuration using PrismFlow's ID.".into());
            }
        }
        let mode = read(&root.join("claude_desktop_config.json"))?.unwrap_or_else(|| json!({}));
        if !mode.is_object() {
            return Err("Claude Desktop's settings file looks damaged; PrismFlow won't change it.".into());
        }
        Ok(Self { root: root.to_path_buf(), meta, mode })
    }
    fn profile_path(&self) -> PathBuf {
        self.root.join(format!("configLibrary/{PRISMFLOW_ID}.json"))
    }
    fn default_path(&self) -> PathBuf {
        self.root.join(format!("configLibrary/{DEFAULT_ID}.json"))
    }
    fn meta_path(&self) -> PathBuf {
        self.root.join("configLibrary/_meta.json")
    }
    fn mode_path(&self) -> PathBuf {
        self.root.join("claude_desktop_config.json")
    }
}

/// What automatic setup will write (for the preview).
pub fn describe_auto() -> Vec<String> {
    let root = config_root();
    vec![
        format!("Add a \"PrismFlow\" configuration to {}", root.join("configLibrary").display()),
        "Select it and switch Claude Desktop to third-party mode".into(),
        "Quit and reopen Claude Desktop to use it".into(),
        "Disconnect switches back to your Claude account and removes the PrismFlow configuration".into(),
    ]
}

/// What the separate copy's setup will write (for the preview).
pub fn describe_separate(root: &Path) -> Vec<String> {
    vec![
        format!("Set up a second Claude Desktop in {}", root.parent().unwrap_or(root).display()),
        "It uses PrismFlow; your usual Claude Desktop stays signed in to your Claude account".into(),
    ]
}

/// Adds and selects PrismFlow's gateway configuration.
pub fn connect_auto(gateway_url: &str, token: &str, connection_id: &str) -> Result<(), String> {
    connect_auto_in(&config_root(), gateway_url, token, connection_id)
}

pub fn connect_auto_in(root: &Path, gateway_url: &str, token: &str, connection_id: &str) -> Result<(), String> {
    let lib = Library::load(root)?;
    if let Some(existing) = read(&lib.profile_path())? {
        if existing["inferenceCustomHeaders"][OWNER_HEADER].is_null() && !existing.as_object().is_some_and(|o| o.is_empty()) {
            return Err("Claude Desktop already has a configuration in PrismFlow's place. Remove it in Claude Desktop first.".into());
        }
    }
    let profile = json!({
        "inferenceProvider": "gateway",
        "inferenceGatewayBaseUrl": gateway_url.trim_end_matches('/'),
        "inferenceCredentialKind": "static",
        "inferenceGatewayApiKey": token,
        "inferenceGatewayAuthScheme": "bearer",
        "modelDiscoveryEnabled": true,
        // Otherwise Desktop limits tool network access to the gateway host.
        "coworkEgressAllowedHosts": ["*"],
        "inferenceCustomHeaders": { OWNER_HEADER: connection_id },
    });
    write(&lib.profile_path(), &profile)?;
    let mut entries: Vec<Value> = lib.meta.as_ref().and_then(|m| m["entries"].as_array().cloned()).unwrap_or_default();
    if !entries.iter().any(|e| e["id"] == DEFAULT_ID) {
        // A neutral profile to fall back to on disconnect.
        if read(&lib.default_path())?.is_none() {
            write(&lib.default_path(), &json!({}))?;
        }
        entries.push(json!({ "id": DEFAULT_ID, "name": "Default" }));
    }
    if !entries.iter().any(|e| e["id"] == PRISMFLOW_ID) {
        entries.push(json!({ "id": PRISMFLOW_ID, "name": "PrismFlow" }));
    }
    let mut meta = lib.meta.clone().unwrap_or_else(|| json!({}));
    meta["entries"] = Value::Array(entries);
    meta["appliedId"] = json!(PRISMFLOW_ID);
    if let Some(o) = meta.as_object_mut() {
        o.remove("hybridPointer");
    }
    write(&lib.meta_path(), &meta)?;
    let mut mode = lib.mode.clone();
    mode["deploymentMode"] = json!("3p");
    write(&lib.mode_path(), &mode)
}

/// True when PrismFlow's configuration is the one Claude Desktop will use.
pub fn auto_active(root: &Path, connection_id: &str) -> bool {
    let Ok(lib) = Library::load(root) else { return false };
    let profile = read(&lib.profile_path()).ok().flatten();
    lib.meta.as_ref().is_some_and(|m| m["appliedId"] == PRISMFLOW_ID)
        && lib.mode["deploymentMode"] == "3p"
        && profile.is_some_and(|p| p["inferenceCustomHeaders"][OWNER_HEADER] == connection_id)
}

/// The gateway values written for this connection (for testing them).
pub fn auto_profile(root: &Path) -> Option<Value> {
    read(&root.join(format!("configLibrary/{PRISMFLOW_ID}.json"))).ok().flatten()
}

/// Removes PrismFlow's configuration; back to the Claude account if it was selected.
pub fn disconnect_auto(connection_id: &str) -> Result<(), String> {
    disconnect_auto_in(&config_root(), connection_id)
}

/// Leaves the files alone when PrismFlow's configuration there belongs to
/// another connection.
pub fn disconnect_auto_in(root: &Path, connection_id: &str) -> Result<(), String> {
    let lib = Library::load(root)?;
    if read(&lib.profile_path())?.is_some_and(|p| p["inferenceCustomHeaders"][OWNER_HEADER].as_str().is_some_and(|o| o != connection_id)) {
        return Ok(());
    }
    let ours_listed = lib.meta.as_ref().is_some_and(|m| m["entries"].as_array().is_some_and(|e| e.iter().any(|x| x["id"] == PRISMFLOW_ID)));
    if !ours_listed && !lib.profile_path().exists() {
        return Ok(()); // nothing of PrismFlow's there; leave the files untouched
    }
    let Some(meta) = lib.meta.clone() else {
        let _ = std::fs::remove_file(lib.profile_path());
        return Ok(());
    };
    let selected = meta["appliedId"] == PRISMFLOW_ID;
    let mut entries: Vec<Value> = meta["entries"].as_array().cloned().unwrap_or_default();
    entries.retain(|e| e["id"] != PRISMFLOW_ID);
    if selected && !entries.iter().any(|e| e["id"] == DEFAULT_ID) {
        if read(&lib.default_path())?.is_none() {
            write(&lib.default_path(), &json!({}))?;
        }
        entries.push(json!({ "id": DEFAULT_ID, "name": "Default" }));
    }
    let mut meta = meta;
    meta["entries"] = Value::Array(entries);
    if selected {
        meta["appliedId"] = json!(DEFAULT_ID);
    }
    write(&lib.meta_path(), &meta)?;
    if selected {
        let mut mode = lib.mode.clone();
        mode["deploymentMode"] = json!("1p");
        write(&lib.mode_path(), &mode)?;
    }
    match std::fs::remove_file(lib.profile_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_setup_round_trip_keeps_the_library_valid() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PRISMFLOW_CLAUDE_3P_DIR", dir.path());
        // This Mac's starting point: another tool left a neutral Default, account mode on.
        std::fs::create_dir_all(dir.path().join("configLibrary")).unwrap();
        std::fs::write(dir.path().join("configLibrary/_meta.json"), format!(r#"{{"entries":[{{"id":"{DEFAULT_ID}","name":"Default"}}],"appliedId":"{DEFAULT_ID}"}}"#)).unwrap();
        std::fs::write(dir.path().join(format!("configLibrary/{DEFAULT_ID}.json")), "{}").unwrap();
        std::fs::write(dir.path().join("claude_desktop_config.json"), r#"{"deploymentMode":"1p","keepMe":true}"#).unwrap();

        connect_auto("http://127.0.0.1:47821/", "pft_tok", "claude_desktop:ollama").unwrap();
        assert!(auto_active(dir.path(), "claude_desktop:ollama"));
        let p = auto_profile(dir.path()).unwrap();
        assert_eq!(p["inferenceGatewayBaseUrl"], "http://127.0.0.1:47821");
        assert_eq!(p["inferenceGatewayAuthScheme"], "bearer");
        let mode: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("claude_desktop_config.json")).unwrap()).unwrap();
        assert_eq!(mode, json!({"deploymentMode": "3p", "keepMe": true}));

        disconnect_auto("claude_desktop:ollama").unwrap();
        assert!(!auto_active(dir.path(), "claude_desktop:ollama"));
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("configLibrary/_meta.json")).unwrap()).unwrap();
        assert_eq!(meta, json!({"entries": [{"id": DEFAULT_ID, "name": "Default"}], "appliedId": DEFAULT_ID}));
        let mode: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("claude_desktop_config.json")).unwrap()).unwrap();
        assert_eq!(mode["deploymentMode"], "1p");
        assert!(!dir.path().join(format!("configLibrary/{PRISMFLOW_ID}.json")).exists());

        // Fresh install: no library yet.
        let fresh = tempfile::tempdir().unwrap();
        std::env::set_var("PRISMFLOW_CLAUDE_3P_DIR", fresh.path());
        connect_auto("http://127.0.0.1:1", "t", "c").unwrap();
        disconnect_auto("someone-else").unwrap();
        assert!(auto_active(fresh.path(), "c"), "another connection's disconnect leaves it alone");
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(fresh.path().join("configLibrary/_meta.json")).unwrap()).unwrap();
        assert_eq!(meta["appliedId"], PRISMFLOW_ID);
        assert_eq!(meta["entries"].as_array().unwrap().len(), 2);
        disconnect_auto("c").unwrap();
        std::env::remove_var("PRISMFLOW_CLAUDE_3P_DIR");

        // The separate copy: its own root, the same library layout.
        let state = tempfile::tempdir().unwrap();
        let root = separate_root(state.path());
        connect_auto_in(&root, "http://127.0.0.1:1", "t", "claude_desktop_separate:x").unwrap();
        assert!(auto_active(&root, "claude_desktop_separate:x"));
        assert_eq!(root.file_name().unwrap(), "data-3p");
        assert_eq!(separate_data_dir(state.path()).parent(), root.parent(), "Claude Desktop looks for <data dir>-3p");
        disconnect_auto_in(&root, "claude_desktop_separate:x").unwrap();
        assert!(!auto_active(&root, "claude_desktop_separate:x"));
    }
}
