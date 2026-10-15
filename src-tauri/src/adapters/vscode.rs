//! VS Code: the Claude Code extension and VS Code's own Chat.
//!
//! Facts checked 2026-10-15 against anthropic.claude-code 2.1.300 (VSIX) and
//! VS Code 1.142.0 source:
//! - The Claude Code extension starts its bundled `claude` with the
//!   `claudeCode.environmentVariables` from VS Code's user settings. PrismFlow
//!   sets `CLAUDE_CONFIG_DIR` to a folder it owns (holding `apiKeyHelper`), so
//!   no token is stored and the user's `~/.claude` is not used.
//! - VS Code Chat reads `chatLanguageModels.json` (vendor `customendpoint`);
//!   PrismFlow adds one group pointing at the gateway's Messages endpoint. VS
//!   Code has no helper option there, so the group holds the local token,
//!   which only works on this computer and is revoked on disconnect.
//! - The Codex extension reads `~/.codex/config.toml` like the Codex app, so
//!   the Codex app connection already covers it.

use super::{jsonfile, AppDetection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const CLAUDE_APP_ID: &str = "vscode_claude";
pub const CHAT_APP_ID: &str = "vscode_chat";
pub const MARKER_KEY: &str = "prismflowConnection";

/// VS Code's user folder (default profile).
pub fn user_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("PRISMFLOW_VSCODE_USER_DIR") {
        return PathBuf::from(p);
    }
    let base = if cfg!(target_os = "macos") {
        dirs::home_dir().unwrap_or_default().join("Library/Application Support")
    } else {
        dirs::config_dir().unwrap_or_default()
    };
    base.join("Code").join("User")
}

pub fn settings_file() -> PathBuf {
    user_dir().join("settings.json")
}

pub fn chat_models_file() -> PathBuf {
    user_dir().join("chatLanguageModels.json")
}

fn installed() -> bool {
    user_dir().is_dir() || super::find_exe("code").is_some() || Path::new("/Applications/Visual Studio Code.app").exists()
}

/// The Claude Code extension's bundled binary, newest first.
pub fn bundled_claude() -> Option<PathBuf> {
    let ext = dirs::home_dir()?.join(".vscode").join("extensions");
    let mut found: Vec<PathBuf> = std::fs::read_dir(ext).ok()?.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("anthropic.claude-code-"))
        .map(|e| e.path().join("resources/native-binary").join(if cfg!(windows) { "claude.exe" } else { "claude" }))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop()
}

pub fn detect(app_id: &str) -> AppDetection {
    let found = installed();
    let (name, notes) = if app_id == CLAUDE_APP_ID {
        ("Claude Code for VS Code", vec!["Uses PrismFlow in the Claude Code extension. Your own ~/.claude stays as it is.".to_string()])
    } else {
        ("VS Code Chat", vec!["Adds a PrismFlow model to VS Code's chat model picker.".to_string()])
    };
    AppDetection {
        app_id: app_id.into(),
        display_name: name.into(),
        installed: found,
        path: found.then(|| user_dir().display().to_string()),
        version: None,
        version_verified: false,
        notes,
    }
}

pub struct Plan {
    pub file: PathBuf,
    pub before: String,
    pub after: String,
    pub owned: Vec<(PathBuf, String)>,
}

/// Claude Code extension: point it at PrismFlow's own Claude config folder.
pub fn plan_claude(owned_dir: &Path, connection_id: &str, settings_json: String) -> Result<Plan, String> {
    let file = settings_file();
    let (before, mut obj) = jsonfile::read_object(&file)?;
    let config_dir = owned_dir.join("claude");
    let mut vars: Vec<Value> = obj.get("claudeCode.environmentVariables").and_then(Value::as_array).cloned().unwrap_or_default();
    if let Some(v) = vars.iter().find(|v| v["name"] == "CLAUDE_CONFIG_DIR") {
        if !v["value"].as_str().is_some_and(|s| s.contains("prismflow") || s.contains("PrismFlow")) {
            return Err("The Claude Code extension already uses its own CLAUDE_CONFIG_DIR. Remove it from VS Code's settings first.".into());
        }
    }
    vars.retain(|v| v["name"] != "CLAUDE_CONFIG_DIR" && v["name"] != "PRISMFLOW_CONNECTION");
    vars.push(json!({ "name": "CLAUDE_CONFIG_DIR", "value": config_dir.display().to_string() }));
    vars.push(json!({ "name": "PRISMFLOW_CONNECTION", "value": connection_id }));
    jsonfile::set(&mut obj, &["claudeCode.environmentVariables"], Value::Array(vars))?;
    jsonfile::set(&mut obj, &["claudeCode.disableLoginPrompt"], json!(true))?;
    Ok(Plan { after: jsonfile::render(&obj), file, before, owned: vec![(config_dir.join("settings.json"), settings_json)] })
}

pub fn disconnect_claude(connection_id: &str) -> Result<(), String> {
    let file = settings_file();
    let (_, mut obj) = jsonfile::read_object(&file)?;
    let Some(vars) = obj.get("claudeCode.environmentVariables").and_then(Value::as_array).cloned() else { return Ok(()) };
    if !vars.iter().any(|v| v["name"] == "PRISMFLOW_CONNECTION" && v["value"] == connection_id) {
        return Ok(());
    }
    let kept: Vec<Value> = vars.into_iter().filter(|v| v["name"] != "CLAUDE_CONFIG_DIR" && v["name"] != "PRISMFLOW_CONNECTION").collect();
    if kept.is_empty() {
        obj.remove("claudeCode.environmentVariables");
    } else {
        obj.insert("claudeCode.environmentVariables".into(), Value::Array(kept));
    }
    obj.remove("claudeCode.disableLoginPrompt");
    crate::config_engine::atomic_write(&file, &jsonfile::render(&obj)).map_err(|e| e.to_string())
}

/// The config folder the extension uses, if PrismFlow set it for this connection.
pub fn claude_config_dir(connection_id: &str) -> Option<PathBuf> {
    let (_, obj) = jsonfile::read_object(&settings_file()).ok()?;
    let vars = obj.get("claudeCode.environmentVariables")?.as_array()?;
    vars.iter().any(|v| v["name"] == "PRISMFLOW_CONNECTION" && v["value"] == connection_id).then_some(())?;
    vars.iter().find(|v| v["name"] == "CLAUDE_CONFIG_DIR").and_then(|v| v["value"].as_str()).map(PathBuf::from)
}

fn read_groups(file: &Path) -> Result<(String, Vec<Value>), String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((String::new(), Vec::new())),
        Err(e) => return Err(format!("Couldn't read {}: {e}", file.display())),
    };
    if text.trim().is_empty() {
        return Ok((text, Vec::new()));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(a)) => Ok((text, a)),
        _ => Err(format!("{} has comments or a mistake in it, so PrismFlow won't rewrite it.", file.display())),
    }
}

/// VS Code Chat: one model group using the gateway's Messages endpoint.
pub fn plan_chat(connection_id: &str, provider_name: &str, model: &str, broker_url: &str, token: &str, context: Option<u64>) -> Result<Plan, String> {
    let file = chat_models_file();
    let (before, mut groups) = read_groups(&file)?;
    groups.retain(|g| g[MARKER_KEY] != connection_id);
    let max_in = context.unwrap_or(32_000).saturating_sub(4_096).max(8_000);
    groups.push(json!({
        "name": format!("PrismFlow · {provider_name}"),
        "vendor": "customendpoint",
        "apiType": "messages",
        MARKER_KEY: connection_id,
        "models": [{
            "id": format!("prismflow-{}", connection_id.replace(':', "-")),
            "name": format!("{model} (PrismFlow)"),
            "url": format!("{broker_url}/v1/messages"),
            "toolCalling": true,
            "vision": false,
            "maxInputTokens": max_in,
            "maxOutputTokens": 4096,
            "requestHeaders": { "x-api-key": token },
        }],
    }));
    let after = serde_json::to_string_pretty(&Value::Array(groups)).unwrap_or_default() + "\n";
    Ok(Plan { file, before, after, owned: Vec::new() })
}

pub fn disconnect_chat(connection_id: &str) -> Result<(), String> {
    let file = chat_models_file();
    let (_, mut groups) = read_groups(&file)?;
    let n = groups.len();
    groups.retain(|g| g[MARKER_KEY] != connection_id);
    if groups.len() == n {
        return Ok(());
    }
    crate::config_engine::atomic_write(&file, &(serde_json::to_string_pretty(&Value::Array(groups)).unwrap_or_default() + "\n")).map_err(|e| e.to_string())
}

/// The model entry PrismFlow wrote for this connection.
pub fn chat_model(connection_id: &str) -> Option<Value> {
    let (_, groups) = read_groups(&chat_models_file()).ok()?;
    groups.into_iter().find(|g| g[MARKER_KEY] == connection_id).and_then(|g| g["models"].get(0).cloned())
}

/// Sends what VS Code Chat sends (Anthropic Messages, with its headers and
/// a `read_file` tool) to the model entry's URL. Returns the reply JSON.
pub async fn chat_request(client: &reqwest::Client, model: &Value, messages: Value, tools: bool) -> Result<Value, String> {
    let url = model["url"].as_str().ok_or("no url")?;
    let mut body = json!({ "model": model["id"], "max_tokens": 1024, "messages": messages });
    if tools {
        body["tools"] = json!([{ "name": "read_file", "description": "Read a file in the workspace.",
            "input_schema": { "type": "object", "properties": { "filePath": { "type": "string" } }, "required": ["filePath"] } }]);
    }
    let mut req = client.post(url).header("anthropic-version", "2023-06-01").header("anthropic-beta", "interleaved-thinking-2025-05-14").json(&body);
    if let Some(h) = model["requestHeaders"].as_object() {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                req = req.header(k.as_str(), v);
            }
        }
    }
    let r = req.timeout(std::time::Duration::from_secs(600)).send().await.map_err(|e| format!("couldn't reach PrismFlow: {e}"))?;
    let status = r.status();
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status.as_u16(), v["error"]["message"].as_str().unwrap_or("error")));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_extension_entry_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PRISMFLOW_VSCODE_USER_DIR", dir.path());
        std::fs::write(settings_file(), r#"{"editor.fontSize": 14, "claudeCode.environmentVariables": [{"name": "FOO", "value": "1"}]}"#).unwrap();
        let p = plan_claude(&dir.path().join("prismflow-owned"), "vscode_claude:ollama", "{}".into()).unwrap();
        std::fs::write(&p.file, &p.after).unwrap();
        let v: Value = serde_json::from_str(&p.after).unwrap();
        assert_eq!(v["editor.fontSize"], 14);
        assert_eq!(v["claudeCode.environmentVariables"].as_array().unwrap().len(), 3);
        assert_eq!(v["claudeCode.disableLoginPrompt"], true);
        assert!(claude_config_dir("vscode_claude:ollama").unwrap().ends_with("prismflow-owned/claude"));
        disconnect_claude("vscode_claude:ollama").unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(settings_file()).unwrap()).unwrap();
        assert_eq!(v["claudeCode.environmentVariables"], json!([{"name": "FOO", "value": "1"}]));
        assert!(v.get("claudeCode.disableLoginPrompt").is_none());

        // VS Code Chat: our group added and removed, others kept.
        std::fs::write(chat_models_file(), r#"[{"name": "Mine", "vendor": "customendpoint", "models": []}]"#).unwrap();
        let c = plan_chat("vscode_chat:ollama", "Ollama", "qwen3", "http://127.0.0.1:1", "pft_x", Some(32_768)).unwrap();
        std::fs::write(&c.file, &c.after).unwrap();
        let m = chat_model("vscode_chat:ollama").unwrap();
        assert_eq!(m["url"], "http://127.0.0.1:1/v1/messages");
        assert_eq!(m["maxInputTokens"], 28_672);
        disconnect_chat("vscode_chat:ollama").unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(chat_models_file()).unwrap()).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        std::env::remove_var("PRISMFLOW_VSCODE_USER_DIR");
    }
}
