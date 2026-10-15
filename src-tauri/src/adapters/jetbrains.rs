//! JetBrains IDEs (AI Assistant) through ACP agents.
//!
//! PrismFlow adds two entries to `~/.jetbrains/acp.json` (JetBrains docs,
//! ai-assistant/acp.html): "Claude Code (PrismFlow · …)" and
//! "Codex (PrismFlow · …)". Each runs the official ACP adapter
//! (`@agentclientprotocol/claude-agent-acp`, `…/codex-acp`) with a config
//! folder PrismFlow owns, so the user's own `~/.claude` / `~/.codex` are not
//! used, and the token comes from PrismFlow's helper at request time
//! (`apiKeyHelper` / `auth.command`), never stored in acp.json.
//! Verified 2026-10-15: claude-agent-acp 0.90.0 and codex-acp 2.2.2 driven
//! over stdio like an IDE, through the broker, reading a nonce file.

use super::{find_exe, jsonfile, AppDetection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const APP_ID: &str = "jetbrains";
pub const CLAUDE_PKG: &str = "@agentclientprotocol/claude-agent-acp";
pub const CLAUDE_VERSION: &str = "0.90.0";
pub const CODEX_PKG: &str = "@agentclientprotocol/codex-acp";
pub const CODEX_VERSION: &str = "2.2.2";
/// Marks entries PrismFlow wrote (value = connection id).
pub const MARKER_ENV: &str = "PRISMFLOW_JETBRAINS_ACP";

pub fn acp_file() -> PathBuf {
    if let Some(p) = std::env::var_os("PRISMFLOW_JETBRAINS_ACP_FILE") {
        return PathBuf::from(p);
    }
    dirs::home_dir().unwrap_or_default().join(".jetbrains").join("acp.json")
}

fn jetbrains_dirs() -> Vec<PathBuf> {
    [dirs::data_dir(), dirs::cache_dir()].into_iter().flatten().map(|d| d.join("JetBrains")).filter(|d| d.is_dir()).collect()
}

/// Any JetBrains IDE on this computer?
pub fn ide_found() -> bool {
    let apps = ["/Applications", &format!("{}/Applications", dirs::home_dir().unwrap_or_default().display())]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .any(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.ends_with(".app") && ["IntelliJ", "PyCharm", "WebStorm", "GoLand", "CLion", "Rider", "RubyMine", "PhpStorm", "RustRover", "DataGrip", "Android Studio"].iter().any(|k| n.contains(k))
        });
    apps || !jetbrains_dirs().is_empty() || acp_file().exists()
}

/// JetBrains' own copy of an ACP adapter (installed from its agent registry).
fn installed_adapter(pkg: &str) -> Option<PathBuf> {
    let rel = Path::new("node_modules").join(pkg).join("dist").join("index.js");
    for base in jetbrains_dirs() {
        // <JetBrains>/<IDE>/acp-agents/<agent>/<version>/node_modules/<pkg>/dist/index.js
        // and <JetBrains>/acp-agents/<agent>/<version>/…
        let mut roots = vec![base.join("acp-agents")];
        if let Ok(rd) = std::fs::read_dir(&base) {
            roots.extend(rd.flatten().map(|e| e.path().join("acp-agents")));
        }
        for root in roots {
            for agent in std::fs::read_dir(&root).into_iter().flatten().flatten() {
                let mut versions: Vec<PathBuf> = std::fs::read_dir(agent.path()).into_iter().flatten().flatten().map(|e| e.path()).collect();
                versions.sort();
                if let Some(js) = versions.into_iter().rev().map(|v| v.join(&rel)).find(|p| p.is_file()) {
                    return Some(js);
                }
            }
        }
    }
    None
}

/// Node.js 22 or newer: JetBrains' bundled runtime first, then the system one.
pub fn find_node() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PRISMFLOW_NODE_BIN") {
        return Some(PathBuf::from(p));
    }
    let bundled = jetbrains_dirs().into_iter().flat_map(|b| {
        let mut c = vec![b.join("acp-agents").join(".runtimes").join("node")];
        c.extend(std::fs::read_dir(&b).into_iter().flatten().flatten().map(|e| e.path().join("acp-agents").join(".runtimes").join("node")));
        c
    });
    for dir in bundled {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path().join("bin").join("node")).filter(|p| p.is_file()).collect();
        v.sort();
        if let Some(n) = v.pop() {
            return Some(n);
        }
    }
    find_exe("node").filter(|n| node_major(n).is_some_and(|m| m >= 22))
}

fn node_major(node: &Path) -> Option<u32> {
    let out = std::process::Command::new(node).arg("--version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().trim_start_matches('v').split('.').next()?.parse().ok()
}

/// How to start an adapter: an override, JetBrains' copy, or `npx` (which
/// fetches the pinned version the first time the IDE starts the agent).
pub fn launch(pkg: &str, version: &str, override_env: &str) -> Result<(String, Vec<String>), String> {
    let node = find_node().ok_or("Install Node.js 22 or newer (JetBrains agents need it).")?;
    let js = std::env::var_os(override_env).map(PathBuf::from).or_else(|| installed_adapter(pkg));
    if let Some(js) = js {
        return Ok((node.display().to_string(), vec![js.display().to_string()]));
    }
    let npx = node.parent().map(|d| d.join("npx")).filter(|p| p.is_file()).or_else(|| find_exe("npx")).ok_or("Install Node.js 22 or newer (npx is missing).")?;
    Ok((npx.display().to_string(), vec!["-y".into(), format!("{pkg}@{version}")]))
}

pub fn detect() -> AppDetection {
    let found = ide_found();
    let mut notes = vec!["Adds Claude Code and Codex agents to AI Assistant's agent list.".to_string()];
    if found && find_node().is_none() {
        notes.push("Needs Node.js 22 or newer.".into());
    }
    AppDetection {
        app_id: APP_ID.into(),
        display_name: "JetBrains IDEs".into(),
        installed: found,
        path: found.then(|| acp_file().display().to_string()),
        version: None,
        version_verified: false,
        notes,
    }
}

pub fn entry_names(provider_name: &str) -> (String, String) {
    (format!("Claude Code (PrismFlow · {provider_name})"), format!("Codex (PrismFlow · {provider_name})"))
}

pub struct Plan {
    /// acp.json before and after.
    pub file: PathBuf,
    pub before: String,
    pub after: String,
    /// Files in PrismFlow's own folder.
    pub owned: Vec<(PathBuf, String)>,
}

#[allow(clippy::too_many_arguments)]
pub fn plan(
    dir: &Path,
    provider_name: &str,
    connection_id: &str,
    broker_url: &str,
    claude_settings: String,
    codex_config: String,
    claude_launch: (String, Vec<String>),
    codex_launch: (String, Vec<String>),
) -> Result<Plan, String> {
    let file = acp_file();
    let (before, mut obj) = jsonfile::read_object(&file)?;
    let (claude_name, codex_name) = entry_names(provider_name);
    let claude_dir = dir.join("claude");
    let codex_home = dir.join("codex");
    for name in [&claude_name, &codex_name] {
        if let Some(v) = jsonfile::get(&obj, &["agent_servers", name]) {
            if v["env"][MARKER_ENV] != connection_id {
                return Err(format!("acp.json already has an agent named \"{name}\" that PrismFlow didn't add. Rename or remove it first."));
            }
        }
    }
    let mut claude_args = claude_launch.1;
    claude_args.push("--hide-claude-auth".into());
    jsonfile::set(&mut obj, &["agent_servers", &claude_name], json!({
        "command": claude_launch.0,
        "args": claude_args,
        "env": {
            "ANTHROPIC_BASE_URL": broker_url,
            "CLAUDE_CONFIG_DIR": claude_dir.display().to_string(),
            "DISABLE_AUTOUPDATER": "1",
            MARKER_ENV: connection_id,
        }
    }))?;
    jsonfile::set(&mut obj, &["agent_servers", &codex_name], json!({
        "command": codex_launch.0,
        "args": codex_launch.1,
        "env": { "CODEX_HOME": codex_home.display().to_string(), MARKER_ENV: connection_id }
    }))?;
    Ok(Plan {
        after: jsonfile::render(&obj),
        file,
        before,
        owned: vec![(claude_dir.join("settings.json"), claude_settings), (codex_home.join("config.toml"), codex_config)],
    })
}

/// Removes this connection's entries from acp.json (only ones it wrote).
pub fn disconnect(connection_id: &str) -> Result<(), String> {
    let file = acp_file();
    let (_, mut obj) = jsonfile::read_object(&file)?;
    let names: Vec<String> = jsonfile::get(&obj, &["agent_servers"])
        .and_then(Value::as_object)
        .map(|m| m.iter().filter(|(_, v)| v["env"][MARKER_ENV] == connection_id).map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    if names.is_empty() {
        return Ok(());
    }
    for n in &names {
        jsonfile::remove_if(&mut obj, &["agent_servers", n], |v| v["env"][MARKER_ENV] == connection_id);
    }
    crate::config_engine::atomic_write(&file, &jsonfile::render(&obj)).map_err(|e| e.to_string())
}

/// This connection's entry `name` as written in acp.json (for testing it).
pub fn entry(connection_id: &str, name_prefix: &str) -> Option<(String, Vec<String>, std::collections::HashMap<String, String>)> {
    let (_, obj) = jsonfile::read_object(&acp_file()).ok()?;
    let servers = jsonfile::get(&obj, &["agent_servers"])?.as_object()?;
    let (_, v) = servers.iter().find(|(k, v)| k.starts_with(name_prefix) && v["env"][MARKER_ENV] == connection_id)?;
    let cmd = v["command"].as_str()?.to_string();
    let args = v["args"].as_array()?.iter().filter_map(|a| a.as_str().map(String::from)).collect();
    let env = v["env"].as_object()?.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
    Some((cmd, args, env))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_two_entries_and_removes_only_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let acp = dir.path().join("acp.json");
        std::fs::write(&acp, r#"{"default_mcp_settings":{"use_idea_mcp":true},"agent_servers":{"My agent":{"command":"x"}}}"#).unwrap();
        std::env::set_var("PRISMFLOW_JETBRAINS_ACP_FILE", &acp);
        let p = plan(dir.path(), "Ollama", "jetbrains:ollama", "http://127.0.0.1:47821", "{}".into(), "".into(),
            ("/n/node".into(), vec!["/a/claude.js".into()]), ("/n/npx".into(), vec!["-y".into(), "@agentclientprotocol/codex-acp@2.2.2".into()])).unwrap();
        let v: Value = serde_json::from_str(&p.after).unwrap();
        let c = &v["agent_servers"]["Claude Code (PrismFlow · Ollama)"];
        assert_eq!(c["args"], json!(["/a/claude.js", "--hide-claude-auth"]));
        assert_eq!(c["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:47821");
        assert!(c["env"].get("ANTHROPIC_AUTH_TOKEN").is_none() && !p.after.contains("pft_"), "no token in acp.json");
        assert_eq!(v["agent_servers"]["My agent"]["command"], "x");
        assert_eq!(v["default_mcp_settings"]["use_idea_mcp"], true);
        std::fs::write(&acp, &p.after).unwrap();
        assert_eq!(entry("jetbrains:ollama", "Codex").unwrap().0, "/n/npx");
        disconnect("jetbrains:ollama").unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&acp).unwrap()).unwrap();
        assert_eq!(v["agent_servers"].as_object().unwrap().len(), 1);

        // A same-named entry the user made is never taken over.
        std::fs::write(&acp, r#"{"agent_servers":{"Codex (PrismFlow · Ollama)":{"command":"mine"}}}"#).unwrap();
        assert!(plan(dir.path(), "Ollama", "jetbrains:ollama", "u", "{}".into(), "".into(), ("n".into(), vec![]), ("n".into(), vec![])).is_err());
        std::env::remove_var("PRISMFLOW_JETBRAINS_ACP_FILE");
    }
}
