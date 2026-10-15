//! Terminal coding agents launched through PrismFlow (Aider, Cline, Pi,
//! Hermes). Mechanisms verified 2026-10-14 (research/agent-mechanisms-…md).
//!
//! Each connection owns a directory in PrismFlow's state dir holding a
//! launch script and, where the agent needs one, an isolated config. The
//! user's own agent config is never read or written. No secret is stored:
//! the script fetches the connection's revocable gateway token at launch
//! (or the agent runs the token helper itself, for Pi).

use super::AppDetection;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MARKER: &str = "# Managed by PrismFlow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Aider,
    Cline,
    Pi,
    Hermes,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentSpec {
    pub id: &'static str,
    pub display_name: &'static str,
    pub binary: &'static str,
    pub kind: Kind,
    /// Version verified on 2026-10-14 (shown when detection differs).
    pub verified_version: &'static str,
    /// The agent works through tool calls (Stage 6 applies).
    pub tools: bool,
}

pub const AGENTS: &[AgentSpec] = &[
    AgentSpec { id: "aider", display_name: "Aider", binary: "aider", kind: Kind::Aider, verified_version: "0.86.2", tools: false },
    AgentSpec { id: "cline", display_name: "Cline CLI", binary: "cline", kind: Kind::Cline, verified_version: "3.0.70", tools: true },
    AgentSpec { id: "pi", display_name: "Pi", binary: "pi", kind: Kind::Pi, verified_version: "1.1.0", tools: true },
    AgentSpec { id: "hermes", display_name: "Hermes Agent", binary: "hermes", kind: Kind::Hermes, verified_version: "0.21.6", tools: true },
];

pub fn spec(app_id: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|a| a.id == app_id)
}

pub fn find_binary(spec: &AgentSpec) -> Option<PathBuf> {
    let env = format!("PRISMFLOW_{}_BIN", spec.id.to_ascii_uppercase());
    if let Some(p) = std::env::var_os(env) {
        return Some(PathBuf::from(p));
    }
    super::find_exe(spec.binary)
}

/// Launch scripts are POSIX shell; Windows support for terminal agents is
/// not built yet (stated in the UI rather than half-working).
pub const SUPPORTED_HERE: bool = cfg!(unix);

pub fn detect(spec: &AgentSpec) -> AppDetection {
    if !SUPPORTED_HERE {
        return AppDetection {
            app_id: spec.id.into(), display_name: spec.display_name.into(), installed: false, path: None,
            version: None, version_verified: false,
            notes: vec![format!("{} through PrismFlow is not available on Windows yet.", spec.display_name)],
        };
    }
    let mut d = AppDetection {
        app_id: spec.id.into(),
        display_name: spec.display_name.into(),
        installed: false,
        path: None,
        version: None,
        version_verified: false,
        notes: vec!["Launched by PrismFlow through its local gateway; your own configuration for this agent is not changed.".into()],
    };
    let Some(bin) = find_binary(spec) else {
        d.notes = vec![format!("{} was not found on PATH or in common install locations.", spec.display_name)];
        return d;
    };
    d.path = Some(bin.display().to_string());
    let out = Command::new(&bin).arg("--version").env("PATH", super::codex::child_path(&bin)).output();
    if let Some(v) = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string()) {
        d.installed = true;
        d.version_verified = v.contains(spec.verified_version);
        if !d.version_verified {
            d.notes.push(format!("Verified with {} {}; this version runs staged verification before being trusted.", spec.display_name, spec.verified_version));
        }
        d.version = Some(v);
    } else {
        d.notes.push(format!("Found {}, but `--version` failed.", bin.display()));
    }
    d
}

pub fn agent_dir(state_dir: &Path, connection_id: &str) -> PathBuf {
    state_dir.join("agents").join(connection_id.replace(':', "-"))
}

pub fn launch_script(dir: &Path) -> PathBuf {
    dir.join("launch.sh")
}

fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Files to write for a connection: (path, contents). The first is the
/// launch script (the connection's managed file).
pub fn plan_files(
    spec: &AgentSpec,
    dir: &Path,
    bin: &Path,
    helper: &Path,
    connection_id: &str,
    broker_url: &str,
    model: &str,
) -> Vec<(PathBuf, String)> {
    let base = format!("{broker_url}/v1");
    let token_cmd = format!("{} token print --connection {}", q(&helper.display().to_string()), q(connection_id));
    let mut files = Vec::new();
    let mut script = format!(
        "#!/bin/sh\n{MARKER} for {} ({connection_id}). Removed on disconnect; edits here are overwritten.\n\
         # The gateway token is fetched at launch from the secure store; nothing secret is stored here.\nset -e\n",
        spec.display_name
    );
    script.push_str(&format!("export PATH={}:\"$PATH\"\n", q(&bin.parent().unwrap_or(Path::new("/")).display().to_string())));
    let bin = q(&bin.display().to_string());
    match spec.kind {
        Kind::Aider => {
            script.push_str(&format!("OPENAI_API_KEY=\"$({token_cmd})\"\nexport OPENAI_API_KEY\n"));
            script.push_str(&format!("export OPENAI_API_BASE={}\n", q(&base)));
            script.push_str(&format!(
                "exec {bin} --model {} --no-show-model-warnings --analytics-disable --no-check-update \"$@\"\n",
                q(&format!("openai/{model}"))
            ));
        }
        Kind::Cline => {
            let providers = serde_json::json!({
                "version": 1,
                "lastUsedProvider": "openai-compatible",
                "modes": {},
                // Cline 3.0.70 silently ignores an entry without `updatedAt` and
                // falls back to api.openai.com (verified 2026-10-14).
                "providers": { "openai-compatible": { "settings": {
                    "provider": "openai-compatible", "model": model, "baseUrl": base
                }, "updatedAt": "2026-10-14T00:00:00.000Z", "tokenSource": "manual" } }
            });
            files.push((dir.join("cline-data/settings/providers.json"), serde_json::to_string_pretty(&providers).unwrap()));
            script.push_str(&format!("OPENAI_API_KEY=\"$({token_cmd})\"\nexport OPENAI_API_KEY\n"));
            // Defense in depth: the gateway is plain-http loopback, so a dead
            // HTTPS proxy keeps the token from ever reaching a remote host if
            // Cline falls back to a default endpoint.
            script.push_str("export HTTPS_PROXY=http://127.0.0.1:9 https_proxy=http://127.0.0.1:9 NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost\n");
            script.push_str(&format!(
                "exec {bin} --data-dir {} -P openai-compatible -m {} \"$@\"\n",
                q(&dir.join("cline-data").display().to_string()),
                q(model)
            ));
        }
        Kind::Pi => {
            let models = serde_json::json!({ "providers": { "prismflow": {
                "baseUrl": base, "api": "openai-completions", "apiKey": format!("!{token_cmd}"),
                "models": [{ "id": model }]
            } } });
            files.push((dir.join("pi-agent/models.json"), serde_json::to_string_pretty(&models).unwrap()));
            script.push_str(&format!("export PI_CODING_AGENT_DIR={}\n", q(&dir.join("pi-agent").display().to_string())));
            script.push_str(&format!("exec {bin} --model {} \"$@\"\n", q(&format!("prismflow/{model}"))));
        }
        Kind::Hermes => {
            let cfg = format!(
                "{MARKER}\nmodel:\n  provider: \"custom\"\n  base_url: {}\n  api_key: \"${{PRISMFLOW_AGENT_TOKEN}}\"\n  default: {}\n",
                serde_json::to_string(&base).unwrap(),
                serde_json::to_string(model).unwrap()
            );
            files.push((dir.join("hermes-home/config.yaml"), cfg));
            script.push_str(&format!("PRISMFLOW_AGENT_TOKEN=\"$({token_cmd})\"\nexport PRISMFLOW_AGENT_TOKEN\n"));
            script.push_str(&format!("export HERMES_HOME={}\n", q(&dir.join("hermes-home").display().to_string())));
            script.push_str(&format!("exec {bin} \"$@\"\n"));
        }
    }
    files.insert(0, (launch_script(dir), script));
    files
}

/// Arguments for a non-interactive one-shot run of `prompt`.
pub fn oneshot_args(spec: &AgentSpec, prompt: &str) -> Vec<String> {
    let p = prompt.to_string();
    match spec.kind {
        Kind::Aider => vec!["--message".into(), p, "--no-git".into(), "--yes-always".into(), "--no-pretty".into()],
        Kind::Cline => vec!["--retries".into(), "2".into(), p],
        Kind::Pi => vec!["--no-extensions".into(), "-p".into(), p],
        Kind::Hermes => vec!["--ignore-rules".into(), "-z".into(), p],
    }
}

/// Write the planned files (script made executable).
pub fn write(files: &[(PathBuf, String)]) -> Result<(), String> {
    for (path, body) in files {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        crate::config_engine::atomic_write(path, body).map_err(|e| e.to_string())?;
    }
    #[cfg(unix)]
    if let Some((script, _)) = files.first() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Remove a connection's directory, only if its script carries our marker.
pub fn remove(dir: &Path) -> Result<Option<PathBuf>, String> {
    let script = launch_script(dir);
    match std::fs::read_to_string(&script) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
        Ok(s) if !s.contains(MARKER) => Err(format!("{} is not PrismFlow-managed; left alone.", script.display())),
        Ok(_) => std::fs::remove_dir_all(dir).map(|_| Some(dir.to_path_buf())).map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(kind_id: &str) -> Vec<(PathBuf, String)> {
        plan_files(spec(kind_id).unwrap(), Path::new("/s/agents/x"), Path::new("/usr/local/bin/agent"),
            Path::new("/Apps/Prism Flow/prismflow"), "aider:ollama", "http://127.0.0.1:47821", "qwen3:4b")
    }

    #[test]
    fn every_agent_gets_a_marked_script_and_no_secret() {
        for a in AGENTS {
            let f = files(a.id);
            assert!(f[0].0.ends_with("launch.sh"));
            assert!(f[0].1.starts_with("#!/bin/sh\n# Managed by PrismFlow"));
            // The token helper is invoked from the script, or from the agent's own config (Pi).
            assert!(f.iter().any(|(_, b)| b.contains("'/Apps/Prism Flow/prismflow' token print --connection 'aider:ollama'")), "{}", a.id);
            for (_, body) in &f {
                assert!(!body.contains("pft_") && !body.contains("sk-"), "{}", a.id);
            }
        }
    }

    #[test]
    fn per_agent_mechanisms_match_the_verified_research() {
        let aider = files("aider");
        assert!(aider[0].1.contains("export OPENAI_API_BASE='http://127.0.0.1:47821/v1'"));
        assert!(aider[0].1.contains("--model 'openai/qwen3:4b'"));

        let cline = files("cline");
        let providers: serde_json::Value = serde_json::from_str(&cline[1].1).unwrap();
        let s = &providers["providers"]["openai-compatible"]["settings"];
        assert_eq!(s["baseUrl"], "http://127.0.0.1:47821/v1");
        assert!(s.get("apiKey").is_none(), "Cline reads OPENAI_API_KEY when no key is stored");
        assert!(providers["providers"]["openai-compatible"]["updatedAt"].is_string(), "without it Cline ignores the entry");
        assert!(cline[0].1.contains("HTTPS_PROXY=http://127.0.0.1:9"), "token must not be able to leave over https");

        let pi = files("pi");
        let models: serde_json::Value = serde_json::from_str(&pi[1].1).unwrap();
        assert!(models["providers"]["prismflow"]["apiKey"].as_str().unwrap().starts_with("!'/Apps/Prism Flow/prismflow' token print"));
        assert!(pi[0].1.contains("export PI_CODING_AGENT_DIR="));

        let hermes = files("hermes");
        assert!(hermes[1].1.contains("api_key: \"${PRISMFLOW_AGENT_TOKEN}\""));
        assert!(hermes[0].1.contains("export HERMES_HOME="));
    }

    #[test]
    fn oneshot_args_are_non_interactive() {
        assert!(oneshot_args(spec("aider").unwrap(), "x").contains(&"--message".to_string()));
        assert_eq!(oneshot_args(spec("hermes").unwrap(), "x"), vec!["--ignore-rules", "-z", "x"]);
    }

    #[test]
    fn write_then_remove_only_ours() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("agents/c");
        let f = plan_files(spec("pi").unwrap(), &dir, Path::new("/bin/pi"), Path::new("/bin/echo"), "pi:x", "http://127.0.0.1:1", "m");
        write(&f).unwrap();
        assert!(launch_script(&dir).exists() && dir.join("pi-agent/models.json").exists());
        assert_eq!(remove(&dir).unwrap(), Some(dir.clone()));
        assert!(!dir.exists());
        assert_eq!(remove(&dir).unwrap(), None);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(launch_script(&dir), "#!/bin/sh\necho mine\n").unwrap();
        assert!(remove(&dir).is_err());
        assert!(dir.exists());
    }
}
