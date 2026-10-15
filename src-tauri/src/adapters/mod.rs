//! Application adapters (SPEC §4). Provider and application support stay
//! independent: an adapter consumes a `ProviderDefinition`, never owns one.

pub mod agents;
pub mod claude_code;
pub mod codex;
pub mod processes;

use std::path::PathBuf;

/// Directories GUI-launched apps usually lack on PATH (macOS launchd gives
/// `/usr/bin:/bin:/usr/sbin:/sbin`). Codex's npm shim also needs `node`.
pub fn extra_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
    if let Some(home) = dirs::home_dir() {
        for rel in [".local/bin", ".npm-global/bin", ".volta/bin", ".bun/bin"] {
            dirs.push(home.join(rel));
        }
    }
    dirs
}


#[derive(Debug, Clone, serde::Serialize)]
pub struct AppDetection {
    pub app_id: String,
    pub display_name: String,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// True only when the installed version is inside the adapter's
    /// verified range (SPEC §21.2).
    pub version_verified: bool,
    pub notes: Vec<String>,
}

pub fn detect_all() -> Vec<AppDetection> {
    let mut all = vec![codex::detect(), codex::detect_desktop(), claude_code::detect()];
    all.extend(agents::AGENTS.iter().map(agents::detect));
    all
}
