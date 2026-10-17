//! One place for MCP servers, shared rules and skills across apps.
//!
//! MCP: PrismFlow keeps your list of MCP servers and writes them into each
//! app's own MCP settings, adding or removing only the entries it wrote
//! (remembered in `mcp.json`). Formats, checked 2026-10-15:
//! - Claude Code: `~/.claude.json` → `mcpServers` (user scope; `$CLAUDE_CONFIG_DIR/.claude.json` when set)
//! - Codex: `~/.codex/config.toml` → `[mcp_servers.<name>]`
//! - Claude Desktop: `…/Claude/claude_desktop_config.json` → `mcpServers`
//! - Cursor: `~/.cursor/mcp.json` → `mcpServers`
//! - Gemini CLI: `~/.gemini/settings.json` → `mcpServers` (`httpUrl` for http)
//! - VS Code: `<User>/mcp.json` → `servers`
//! - Windsurf: `~/.codeium/windsurf/mcp_config.json` → `mcpServers` (`serverUrl` for http)
//!
//! Rules: one text, written between PrismFlow markers into each app's
//! global rules file (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
//! `~/.gemini/GEMINI.md`), or into a project's `AGENTS.md` with `CLAUDE.md`
//! importing it. Skills: copy a skill folder between Claude Code and Codex.

use crate::adapters::jsonfile;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type R<T> = Result<T, String>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    pub name: String,
    /// A command to start (stdio), or empty when `url` is set (http).
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpState {
    pub servers: Vec<Server>,
    /// app id → server names PrismFlow wrote there.
    pub applied: BTreeMap<String, Vec<String>>,
}

pub const TARGETS: &[(&str, &str)] = &[
    ("claude_code", "Claude Code"), ("codex", "Codex"), ("claude_desktop", "Claude Desktop"), ("cursor", "Cursor"),
    ("gemini", "Gemini CLI"), ("vscode", "VS Code"), ("windsurf", "Windsurf"),
];

fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("PRISMFLOW_MCP_HOME") {
        return PathBuf::from(h);
    }
    if cfg!(test) {
        return std::env::temp_dir().join(format!("prismflow-test-{}-mcp-home", std::process::id()));
    }
    dirs::home_dir().unwrap_or_default()
}

fn app_support() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData/Roaming"))
    } else {
        home().join(".config")
    }
}

/// The MCP settings file for `app`.
pub fn target_file(app: &str) -> Option<PathBuf> {
    Some(match app {
        "claude_code" => std::env::var_os("CLAUDE_CONFIG_DIR").filter(|_| std::env::var_os("PRISMFLOW_MCP_HOME").is_none() && !cfg!(test))
            .map(|d| PathBuf::from(d).join(".claude.json")).unwrap_or_else(|| home().join(".claude.json")),
        "codex" => std::env::var_os("CODEX_HOME").filter(|_| std::env::var_os("PRISMFLOW_MCP_HOME").is_none() && !cfg!(test))
            .map(|d| PathBuf::from(d).join("config.toml")).unwrap_or_else(|| home().join(".codex/config.toml")),
        "claude_desktop" => app_support().join("Claude/claude_desktop_config.json"),
        "cursor" => home().join(".cursor/mcp.json"),
        "gemini" => home().join(".gemini/settings.json"),
        "vscode" => app_support().join("Code/User/mcp.json"),
        "windsurf" => home().join(".codeium/windsurf/mcp_config.json"),
        _ => return None,
    })
}

/// Is the app's settings folder there (i.e. the app is likely installed)?
pub fn present(app: &str) -> bool {
    target_file(app).and_then(|f| f.parent().map(Path::to_path_buf)).is_some_and(|d| d.is_dir())
}

fn json_entry(app: &str, s: &Server) -> Value {
    let http = !s.url.trim().is_empty();
    match (app, http) {
        ("gemini", true) => json!({ "httpUrl": s.url }),
        ("windsurf", true) => json!({ "serverUrl": s.url }),
        ("vscode", true) | ("claude_code", true) => json!({ "type": "http", "url": s.url }),
        (_, true) => json!({ "url": s.url }),
        ("vscode", false) | ("claude_code", false) => json!({ "type": "stdio", "command": s.command, "args": s.args, "env": s.env }),
        (_, false) => json!({ "command": s.command, "args": s.args, "env": s.env }),
    }
}

fn container(app: &str) -> &'static str {
    if app == "vscode" { "servers" } else { "mcpServers" }
}

pub fn validate(s: &Server) -> R<()> {
    if s.name.is_empty() || !s.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Server names use letters, numbers, - and _ only.".into());
    }
    match (s.command.trim().is_empty(), s.url.trim().is_empty()) {
        (true, true) => Err("Give a command to run, or a web address.".into()),
        (false, false) => Err("Use either a command or a web address, not both.".into()),
        (true, false) if !(s.url.starts_with("https://") || s.url.starts_with("http://127.0.0.1") || s.url.starts_with("http://localhost")) => {
            Err("Web addresses must start with https:// (or be on this computer).".into())
        }
        _ => Ok(()),
    }
}

/// Writes `servers` into `app`, removing PrismFlow's earlier entries that
/// are no longer listed. Returns the names now applied.
pub fn apply(app: &str, servers: &[Server], previously: &[String]) -> R<Vec<String>> {
    let file = target_file(app).ok_or("Unknown app.")?;
    if app == "codex" {
        return apply_codex(&file, servers, previously);
    }
    let (before, mut obj) = jsonfile::read_object(&file)?;
    let key = container(app);
    let mut map = obj.get(key).and_then(Value::as_object).cloned().unwrap_or_default();
    for name in previously {
        if !servers.iter().any(|s| &s.name == name) {
            map.remove(name);
        }
    }
    for s in servers {
        if map.contains_key(&s.name) && !previously.contains(&s.name) {
            return Err(format!("{} already has an MCP server named \"{}\" that PrismFlow didn't add. Rename one of them.", file.display(), s.name));
        }
        map.insert(s.name.clone(), json_entry(app, s));
    }
    obj.insert(key.into(), Value::Object(map));
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::config_engine::backup(&file).map_err(|e| e.to_string())?;
    crate::config_engine::atomic_write(&file, &jsonfile::render_like(&before, &obj)).map_err(|e| e.to_string())?;
    Ok(servers.iter().map(|s| s.name.clone()).collect())
}

fn apply_codex(file: &Path, servers: &[Server], previously: &[String]) -> R<Vec<String>> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|_| format!("{} has a mistake in it, so PrismFlow won't change it.", file.display()))?;
    if !doc.contains_key("mcp_servers") {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc.insert("mcp_servers", toml_edit::Item::Table(t));
    }
    let tbl = doc["mcp_servers"].as_table_mut().ok_or("mcp_servers isn't a table.")?;
    for name in previously {
        if !servers.iter().any(|s| &s.name == name) {
            tbl.remove(name);
        }
    }
    for s in servers {
        if tbl.contains_key(&s.name) && !previously.contains(&s.name) {
            return Err(format!("Codex already has an MCP server named \"{}\" that PrismFlow didn't add.", s.name));
        }
        let mut t = toml_edit::Table::new();
        if s.url.trim().is_empty() {
            t["command"] = toml_edit::value(s.command.clone());
            let mut a = toml_edit::Array::new();
            for x in &s.args {
                a.push(x.as_str());
            }
            t["args"] = toml_edit::value(a);
            if !s.env.is_empty() {
                let mut e = toml_edit::InlineTable::new();
                for (k, v) in &s.env {
                    e.insert(k, v.as_str().into());
                }
                t["env"] = toml_edit::value(e);
            }
        } else {
            t["url"] = toml_edit::value(s.url.clone());
        }
        tbl.insert(&s.name, toml_edit::Item::Table(t));
    }
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::config_engine::backup(file).map_err(|e| e.to_string())?;
    crate::config_engine::atomic_write(file, &doc.to_string()).map_err(|e| e.to_string())?;
    Ok(servers.iter().map(|s| s.name.clone()).collect())
}

pub fn load(dir: &Path) -> McpState {
    std::fs::read_to_string(dir.join("mcp.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(dir: &Path, st: &McpState) -> R<()> {
    crate::config_engine::atomic_write(&dir.join("mcp.json"), &serde_json::to_string_pretty(st).unwrap_or_default()).map_err(|e| e.to_string())
}

// ---------- Rules ----------

pub const RULES_START: &str = "<!-- PrismFlow shared rules: start (edit them in PrismFlow) -->";
pub const RULES_END: &str = "<!-- PrismFlow shared rules: end -->";

/// Global rules files, by app.
pub fn rules_files() -> Vec<(&'static str, PathBuf)> {
    vec![
        ("Claude Code", home().join(".claude/CLAUDE.md")),
        ("Codex", home().join(".codex/AGENTS.md")),
        ("Gemini CLI", home().join(".gemini/GEMINI.md")),
    ]
}

/// Puts `rules` between PrismFlow's markers, keeping everything else.
pub fn with_rules(existing: &str, rules: &str) -> String {
    let block = if rules.trim().is_empty() { String::new() } else { format!("{RULES_START}\n{}\n{RULES_END}\n", rules.trim()) };
    match (existing.find(RULES_START), existing.find(RULES_END)) {
        (Some(a), Some(b)) if b > a => {
            let end = b + RULES_END.len();
            let rest = existing[end..].strip_prefix('\n').unwrap_or(&existing[end..]);
            format!("{}{block}{rest}", &existing[..a])
        }
        _ if block.is_empty() => existing.to_string(),
        _ if existing.trim().is_empty() => block,
        _ => format!("{}\n\n{block}", existing.trim_end()),
    }
}

pub fn read_rules(existing: &str) -> String {
    match (existing.find(RULES_START), existing.find(RULES_END)) {
        (Some(a), Some(b)) if b > a => existing[a + RULES_START.len()..b].trim().to_string(),
        _ => String::new(),
    }
}

/// Writes the shared rules into every app's global rules file whose folder exists.
pub fn apply_global_rules(rules: &str) -> R<Vec<String>> {
    let mut done = Vec::new();
    for (app, f) in rules_files() {
        if !f.parent().is_some_and(Path::is_dir) {
            continue;
        }
        let cur = std::fs::read_to_string(&f).unwrap_or_default();
        let next = with_rules(&cur, rules);
        if next != cur {
            crate::config_engine::atomic_write(&f, &next).map_err(|e| e.to_string())?;
        }
        done.push(app.to_string());
    }
    Ok(done)
}

/// Project rules: `AGENTS.md` (Codex, OpenCode, Cursor and others read it)
/// and `CLAUDE.md` importing it (`@AGENTS.md`), so both stay in step.
pub fn apply_project_rules(folder: &Path, rules: &str) -> R<()> {
    if !folder.is_dir() {
        return Err("Pick a project folder.".into());
    }
    let agents = folder.join("AGENTS.md");
    let cur = std::fs::read_to_string(&agents).unwrap_or_default();
    crate::config_engine::atomic_write(&agents, &with_rules(&cur, rules)).map_err(|e| e.to_string())?;
    let claude = folder.join("CLAUDE.md");
    let c = std::fs::read_to_string(&claude).unwrap_or_default();
    if !c.contains("@AGENTS.md") {
        let next = if c.trim().is_empty() { "@AGENTS.md\n".to_string() } else { format!("@AGENTS.md\n\n{c}") };
        crate::config_engine::atomic_write(&claude, &next).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------- Skills ----------

pub fn skill_dirs() -> Vec<(&'static str, PathBuf)> {
    vec![("Claude Code", home().join(".claude/skills")), ("Codex", home().join(".codex/skills"))]
}

#[derive(Debug, Clone, Serialize)]
pub struct Skill {
    pub name: String,
    /// Apps that have it.
    pub apps: Vec<String>,
    pub description: String,
}

pub fn list_skills() -> Vec<Skill> {
    let mut out: BTreeMap<String, Skill> = BTreeMap::new();
    for (app, dir) in skill_dirs() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let md = p.join("SKILL.md");
            if !md.is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let desc = std::fs::read_to_string(&md).unwrap_or_default().lines()
                .find_map(|l| l.strip_prefix("description:").map(|d| d.trim().trim_matches('"').to_string())).unwrap_or_default();
            let s = out.entry(name.clone()).or_insert(Skill { name, apps: vec![], description: desc });
            s.apps.push(app.to_string());
        }
    }
    out.into_values().collect()
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let ty = e.file_type()?;
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()))?;
        } else {
            std::fs::copy(e.path(), to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Copies a skill to the apps that don't have it yet.
pub fn share_skill(name: &str) -> R<Vec<String>> {
    if name.contains('/') || name.contains("..") || name.is_empty() {
        return Err("Unknown skill.".into());
    }
    let dirs = skill_dirs();
    let src = dirs.iter().map(|(_, d)| d.join(name)).find(|d| d.join("SKILL.md").is_file()).ok_or("Unknown skill.")?;
    let mut done = Vec::new();
    for (app, d) in &dirs {
        let dst = d.join(name);
        if dst.exists() {
            continue;
        }
        copy_dir(&src, &dst).map_err(|e| e.to_string())?;
        done.push(app.to_string());
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(name: &str) -> Server {
        Server { name: name.into(), command: "npx".into(), args: vec!["-y".into(), "@modelcontextprotocol/server-memory".into()], ..Default::default() }
    }

    #[test]
    fn mcp_entries_round_trip_without_touching_others() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("PRISMFLOW_MCP_HOME", home.path());
        std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
        std::fs::write(home.path().join(".cursor/mcp.json"), r#"{"mcpServers":{"mine":{"command":"x"}}}"#).unwrap();
        let applied = apply("cursor", &[srv("memory")], &[]).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(home.path().join(".cursor/mcp.json")).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["memory"]["args"][1], "@modelcontextprotocol/server-memory");
        assert_eq!(v["mcpServers"]["mine"]["command"], "x");
        apply("cursor", &[], &applied).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(home.path().join(".cursor/mcp.json")).unwrap()).unwrap();
        assert!(v["mcpServers"].get("memory").is_none() && v["mcpServers"].get("mine").is_some());
        assert!(apply("cursor", &[srv("mine")], &[]).is_err(), "never takes over a user's entry");

        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(home.path().join(".codex/config.toml"), "# mine\nmodel = \"x\"\n").unwrap();
        apply("codex", &[srv("memory"), Server { name: "web".into(), url: "https://mcp.example.com/mcp".into(), ..Default::default() }], &[]).unwrap();
        let t = std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
        assert!(t.starts_with("# mine") && t.contains("[mcp_servers.memory]") && t.contains("url = \"https://mcp.example.com/mcp\""), "{t}");
        std::env::remove_var("PRISMFLOW_MCP_HOME");
    }

    #[test]
    fn rules_blocks() {
        let a = with_rules("# My notes\n", "Use tabs.");
        assert!(a.starts_with("# My notes") && a.contains(RULES_START) && a.contains("Use tabs."));
        let b = with_rules(&a, "Use spaces.");
        assert!(b.contains("Use spaces.") && !b.contains("Use tabs.") && b.matches(RULES_START).count() == 1);
        assert_eq!(read_rules(&b), "Use spaces.");
        assert_eq!(with_rules(&b, "").trim(), "# My notes");
        assert!(validate(&Server { name: "bad name".into(), command: "x".into(), ..Default::default() }).is_err());
        assert!(validate(&Server { name: "w".into(), url: "http://example.com".into(), ..Default::default() }).is_err());
    }
}

#[cfg(test)]
mod live {
    /// Writes a stdio + an http server into $PRISMFLOW_MCP_LIVE for checking with real CLIs.
    #[test]
    #[ignore]
    fn write_for_real_clis() {
        let Ok(h) = std::env::var("PRISMFLOW_MCP_LIVE") else { return };
        std::env::set_var("PRISMFLOW_MCP_HOME", &h);
        let servers = vec![
            super::Server { name: "pf-memory".into(), command: "npx".into(), args: vec!["-y".into(), "@modelcontextprotocol/server-memory".into()], ..Default::default() },
            super::Server { name: "pf-web".into(), url: "https://mcp.example.com/mcp".into(), ..Default::default() },
        ];
        for app in ["claude_code", "codex"] {
            super::apply(app, &servers, &[]).unwrap();
        }
    }
}
