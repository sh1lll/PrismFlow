//! More editors: Roo Code, Kilo Code, Zed and Continue (IDE extension).
//! Facts from each project's docs/source, checked 2026-10-15:
//! - Roo Code: VS Code setting `roo-cline.autoImportSettingsPath` → a JSON
//!   file imported when the extension starts (`providerProfiles.apiConfigs`).
//! - Kilo Code (OpenCode-based): `~/.config/kilo/kilo.json`, `provider.<id>`
//!   with `npm: @ai-sdk/openai-compatible`, `options.baseURL`, `options.apiKey`.
//! - Zed: `settings.json` → `language_models.openai_compatible.<name>.api_url`;
//!   the key comes from Zed's own key store (paste it once) or an env var.
//! - Continue (IDE): `~/.continue/config.yaml` model with `apiBase`; PrismFlow
//!   shows the block to add (PrismFlow doesn't rewrite YAML it doesn't own).
//!
//! The key in these files is PrismFlow's local token: it only works on this
//! computer and is revoked on disconnect.

use super::{jsonfile, AppDetection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const ROO: &str = "roo_code";
pub const KILO: &str = "kilo_code";
pub const ZED: &str = "zed";
pub const CONTINUE_IDE: &str = "continue_ide";
/// Zed with its own data and settings (`zed --user-data-dir`), opened by PrismFlow.
pub const ZED_SEPARATE: &str = "zed_separate";
pub const ALL: [&str; 5] = [ROO, KILO, ZED, ZED_SEPARATE, CONTINUE_IDE];

/// The separate Zed's data folder; its settings are in `config/settings.json`
/// (Zed's paths.rs: a custom data dir moves config_dir to `<dir>/config`).
pub fn zed_separate_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("zed-separate")
}

/// Zed's environment variable for the "PrismFlow" provider's key (the docs
/// derive it from the provider name; both spellings are set).
pub const ZED_KEY_VARS: [&str; 2] = ["PRISMFLOW_API_KEY", "PRISM_FLOW_API_KEY"];

/// Opens the separate Zed with the key in its environment, so nothing is pasted.
pub fn open_zed_separate(state_dir: &Path, token: &str) -> Result<(), String> {
    let dir = zed_separate_dir(state_dir);
    let mut cmd = if cfg!(target_os = "macos") {
        let app = ["/Applications/Zed.app", "/Applications/Zed Preview.app"].into_iter().find(|p| Path::new(p).exists()).ok_or("Zed isn't installed.")?;
        let mut c = std::process::Command::new("open");
        c.args(["-n", "-a", app]);
        for k in ZED_KEY_VARS {
            c.arg("--env").arg(format!("{k}={token}"));
        }
        c.arg("--args");
        c
    } else {
        let zed = super::find_exe("zed").or_else(|| super::find_exe("zeditor")).ok_or("Zed's `zed` command wasn't found.")?;
        let mut c = std::process::Command::new(zed);
        for k in ZED_KEY_VARS {
            c.env(k, token);
        }
        c
    };
    cmd.arg("--user-data-dir").arg(&dir)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().map_err(|e| format!("Could not open Zed: {e}"))?;
    Ok(())
}

fn zed_file_for(id: &str, state_dir: &Path) -> PathBuf {
    if id == ZED_SEPARATE { zed_separate_dir(state_dir).join("config/settings.json") } else { zed_file() }
}
pub const MARKER: &str = "prismflowConnection";

pub fn name(id: &str) -> &'static str {
    match id {
        ROO => "Roo Code",
        KILO => "Kilo Code",
        ZED => "Zed",
        ZED_SEPARATE => "Zed (separate)",
        _ => "Continue (VS Code / JetBrains)",
    }
}

fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("PRISMFLOW_EDITORS_HOME") {
        return PathBuf::from(h);
    }
    if cfg!(test) {
        return std::env::temp_dir().join(format!("prismflow-test-{}-editors", std::process::id()));
    }
    dirs::home_dir().unwrap_or_default()
}

pub fn kilo_file() -> PathBuf {
    home().join(".config/kilo/kilo.json")
}

pub fn zed_file() -> PathBuf {
    home().join(".config/zed/settings.json")
}

fn vscode_extension_installed(prefix: &str) -> bool {
    std::fs::read_dir(home().join(".vscode/extensions")).into_iter().flatten().flatten()
        .any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with(prefix))
}

pub fn detect(id: &str) -> AppDetection {
    let (installed, note) = match id {
        ROO => (vscode_extension_installed("rooveterinaryinc.roo-cline"), "Adds a PrismFlow profile to Roo Code. Restart VS Code to load it."),
        KILO => (home().join(".config/kilo").is_dir() || vscode_extension_installed("kilocode."), "Adds PrismFlow as a provider in Kilo Code."),
        ZED => (Path::new("/Applications/Zed.app").exists() || home().join(".config/zed").is_dir(), "Adds PrismFlow to your usual Zed's model list; you paste the key once."),
        ZED_SEPARATE => (Path::new("/Applications/Zed.app").exists() || super::find_exe("zed").is_some(), "Opens a Zed of its own with PrismFlow ready, no key to paste. Your usual Zed stays as it is."),
        _ => (home().join(".continue").is_dir() || vscode_extension_installed("continue."), "PrismFlow shows the lines to add to Continue's settings."),
    };
    AppDetection { app_id: id.into(), display_name: name(id).into(), installed, path: None, version: None, version_verified: false, notes: vec![note.into()] }
}

pub struct Plan {
    pub file: PathBuf,
    pub before: String,
    pub after: String,
    pub owned: Vec<(PathBuf, String)>,
}

/// `token` is None for a preview (a placeholder is shown instead).
pub fn plan(id: &str, owned_dir: &Path, connection_id: &str, base: &str, model: &str, token: Option<&str>) -> Result<Plan, String> {
    let key = token.unwrap_or("(added when you connect)");
    match id {
        ROO => {
            let import = owned_dir.join("roo-import.json");
            let body = json!({
                "providerProfiles": {
                    "currentApiConfigName": "PrismFlow",
                    "apiConfigs": { "PrismFlow": { "id": "prismflow", "apiProvider": "openai", "openAiBaseUrl": base, "openAiApiKey": key, "openAiModelId": model } },
                    "modeApiConfigs": {}
                },
                "globalSettings": {}
            });
            let file = super::vscode::settings_file();
            let (before, mut obj) = jsonfile::read_object(&file)?;
            if let Some(v) = obj.get("roo-cline.autoImportSettingsPath").and_then(Value::as_str) {
                if !v.contains("prismflow") && !v.contains("PrismFlow") && !v.is_empty() {
                    return Err("Roo Code already imports settings from another file. Remove that setting in VS Code first.".into());
                }
            }
            obj.insert("roo-cline.autoImportSettingsPath".into(), json!(import.display().to_string()));
            Ok(Plan { file, before, after: jsonfile::render(&obj), owned: vec![(import, serde_json::to_string_pretty(&body).unwrap_or_default())] })
        }
        KILO => {
            let file = kilo_file();
            let (before, mut obj) = jsonfile::read_object(&file)?;
            if jsonfile::get(&obj, &["provider", "prismflow"]).is_some_and(|v| v[MARKER] != connection_id && !v.is_null()) {
                return Err("Kilo Code already has a provider named \"prismflow\" that PrismFlow didn't add.".into());
            }
            jsonfile::set(&mut obj, &["provider", "prismflow"], json!({
                "npm": "@ai-sdk/openai-compatible", "name": "PrismFlow", MARKER: connection_id,
                "options": { "baseURL": base, "apiKey": key },
                "models": { model: { "name": format!("{model} (PrismFlow)") } }
            }))?;
            Ok(Plan { file, before, after: jsonfile::render(&obj), owned: vec![] })
        }
        ZED | ZED_SEPARATE => {
            // owned_dir is <state>/agents/<connection>.
            let state = owned_dir.parent().and_then(Path::parent).unwrap_or(owned_dir);
            let file = zed_file_for(id, state);
            if id == ZED_SEPARATE {
                let _ = std::fs::create_dir_all(file.parent().unwrap_or(state));
            }
            let (before, mut obj) = jsonfile::read_object(&file)?;
            jsonfile::set(&mut obj, &["language_models", "openai_compatible", "PrismFlow"], json!({
                "api_url": base,
                "available_models": [{ "name": model, "display_name": format!("{model} (PrismFlow)"), "max_tokens": 32768 }]
            }))?;
            Ok(Plan { file, before, after: jsonfile::render(&obj), owned: vec![] })
        }
        _ => Ok(Plan { file: home().join(".continue/config.yaml"), before: String::new(), after: String::new(), owned: vec![] }),
    }
}

/// Steps and text the user adds by hand (Zed's key, Continue's block).
pub fn guide(id: &str, base: &str, model: &str, token: &str) -> Vec<String> {
    match id {
        ZED => vec![
            "Open Zed's Agent panel → Settings (or Settings → Language Models).".into(),
            "Find PrismFlow and paste this key:".into(),
            token.into(),
        ],
        CONTINUE_IDE => vec![
            "Open ~/.continue/config.yaml (Continue → Settings → Open config) and add under models:".into(),
            format!("  - name: {model} (PrismFlow)\n    provider: openai\n    model: {model}\n    apiBase: {base}\n    apiKey: {token}\n    roles: [chat, edit, apply]\n    capabilities: [tool_use]"),
            "Save. Continue picks it up right away.".into(),
        ],
        _ => vec![],
    }
}

/// Removes what PrismFlow added for `id`.
pub fn disconnect(id: &str, connection_id: &str, state_dir: &Path) -> Result<(), String> {
    let (file, path): (PathBuf, Vec<&str>) = match id {
        ROO => (super::vscode::settings_file(), vec!["roo-cline.autoImportSettingsPath"]),
        KILO => (kilo_file(), vec!["provider", "prismflow"]),
        ZED | ZED_SEPARATE => (zed_file_for(id, state_dir), vec!["language_models", "openai_compatible", "PrismFlow"]),
        _ => return Ok(()),
    };
    if !file.exists() {
        return Ok(());
    }
    let (_, mut obj) = jsonfile::read_object(&file)?;
    let removed = match id {
        ROO => jsonfile::remove_if(&mut obj, &path, |v| v.as_str().is_some_and(|s| s.contains("prismflow") || s.contains("PrismFlow"))),
        KILO => jsonfile::remove_if(&mut obj, &path, |v| v[MARKER] == connection_id),
        _ => jsonfile::remove_if(&mut obj, &path, |_| true),
    };
    if removed {
        crate::config_engine::atomic_write(&file, &jsonfile::render(&obj)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Is PrismFlow's setup still in place?
pub fn check(id: &str, connection_id: &str, state_dir: &Path) -> Result<String, String> {
    let ok = match id {
        ROO => jsonfile::read_object(&super::vscode::settings_file()).ok()
            .and_then(|(_, o)| o.get("roo-cline.autoImportSettingsPath").and_then(Value::as_str).map(|p| Path::new(p).is_file())).unwrap_or(false),
        KILO => jsonfile::read_object(&kilo_file()).ok().is_some_and(|(_, o)| jsonfile::get(&o, &["provider", "prismflow", MARKER]) == Some(&json!(connection_id))),
        ZED | ZED_SEPARATE => jsonfile::read_object(&zed_file_for(id, state_dir)).ok().is_some_and(|(_, o)| jsonfile::get(&o, &["language_models", "openai_compatible", "PrismFlow"]).is_some()),
        _ => true,
    };
    if ok { Ok(format!("{} is set up for PrismFlow.", name(id))) } else { Err(format!("{}'s PrismFlow setup is missing. Connect again.", name(id))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_and_disconnects_keep_other_settings() {
        let h = tempfile::tempdir().unwrap();
        std::env::set_var("PRISMFLOW_EDITORS_HOME", h.path());
        std::fs::create_dir_all(h.path().join(".config/kilo")).unwrap();
        std::fs::write(kilo_file(), r#"{"provider":{"mine":{"npm":"x"}},"theme":"dark"}"#).unwrap();
        let p = plan(KILO, h.path(), "kilo_code:ollama", "http://127.0.0.1:1/v1", "qwen3", Some("pft_t")).unwrap();
        std::fs::write(&p.file, &p.after).unwrap();
        assert!(check(KILO, "kilo_code:ollama", h.path()).is_ok());
        let v: Value = serde_json::from_str(&p.after).unwrap();
        assert_eq!(v["provider"]["prismflow"]["options"]["baseURL"], "http://127.0.0.1:1/v1");
        assert_eq!(v["provider"]["mine"]["npm"], "x");
        disconnect(KILO, "kilo_code:ollama", h.path()).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(kilo_file()).unwrap()).unwrap();
        assert!(v["provider"].get("prismflow").is_none() && v["theme"] == "dark");

        let z = plan(ZED, h.path(), "zed:ollama", "http://127.0.0.1:1/v1", "qwen3", None).unwrap();
        assert!(z.after.contains("openai_compatible") && !z.after.contains("pft_"), "the key isn't written for Zed");
        // Separate Zed: its own settings file under PrismFlow's data.
        let state = h.path().join("state");
        let owned = state.join("agents/zed_separate-ollama");
        let zs = plan(ZED_SEPARATE, &owned, "zed_separate:ollama", "http://127.0.0.1:1/v1", "qwen3", None).unwrap();
        assert_eq!(zs.file, state.join("zed-separate/config/settings.json"));
        std::fs::create_dir_all(zs.file.parent().unwrap()).unwrap();
        std::fs::write(&zs.file, &zs.after).unwrap();
        assert!(check(ZED_SEPARATE, "zed_separate:ollama", &state).is_ok());
        assert!(!zed_file().exists(), "usual Zed untouched");
        disconnect(ZED_SEPARATE, "zed_separate:ollama", &state).unwrap();
        assert!(check(ZED_SEPARATE, "zed_separate:ollama", &state).is_err());
        assert!(guide(CONTINUE_IDE, "http://127.0.0.1:1/v1", "qwen3", "pft_t").iter().any(|l| l.contains("apiBase: http://127.0.0.1:1/v1")));
        std::env::remove_var("PRISMFLOW_EDITORS_HOME");
    }
}
