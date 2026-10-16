//! Application adapters (SPEC §4). Provider and application support stay
//! independent: an adapter consumes a `ProviderDefinition`, never owns one.

pub mod acp;
pub mod agents;
pub mod jetbrains;
pub mod jsonfile;
pub mod vscode;
pub mod claude_code;
pub mod claude_desktop;
pub mod editors;
pub mod codex;
pub mod processes;

use std::path::PathBuf;

/// Directories GUI-launched apps usually lack on PATH (macOS launchd gives
/// `/usr/bin:/bin:/usr/sbin:/sbin`). Codex's npm shim also needs `node`.
pub fn extra_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        // npm global shims and the Node installer's default locations.
        for var in ["APPDATA", "LOCALAPPDATA", "ProgramFiles"] {
            if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
                dirs.push(base.join("npm"));
                dirs.push(base.join("nodejs"));
                dirs.push(base.join("Programs").join("nodejs"));
            }
        }
    } else {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/usr/bin"));
    }
    if let Some(home) = dirs::home_dir() {
        for rel in [".local/bin", ".npm-global/bin", ".volta/bin", ".bun/bin"] {
            dirs.push(home.join(rel));
        }
    }
    dirs
}

/// `dir/name`, trying the Windows executable extensions npm and installers use.
pub fn exe_in(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd"), format!("{name}.bat"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    names.into_iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Search PATH, then the common install locations, for an executable.
pub fn find_exe(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(extra_bin_dirs());
    dirs.iter().find_map(|d| exe_in(d, name))
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
    let mut all = vec![codex::detect(), codex::detect_cli_separate(), codex::detect_desktop(), codex::detect_separate(), claude_code::detect(), claude_code::detect_separate(), claude_desktop::detect(), claude_desktop::detect_separate(), jetbrains::detect(), vscode::detect(vscode::CLAUDE_APP_ID), vscode::detect(vscode::CHAT_APP_ID)];
    all.extend(editors::ALL.iter().map(|id| editors::detect(id)));
    all.extend(agents::AGENTS.iter().map(agents::detect));
    all
}
