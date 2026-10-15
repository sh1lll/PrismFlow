//! Claude Code CLI adapter (SPEC §4.1). Always brokered: Claude Code speaks
//! Anthropic Messages, so every provider is reached through the gateway's
//! Anthropic front (GATEWAY-A).
//!
//! Mechanism (verified against Claude Code 2.1.207, 2026-10-14):
//! - `claude --settings <file>` layers a settings file over the user's own.
//!   Its `env` block overrides an inherited `ANTHROPIC_BASE_URL`.
//! - `apiKeyHelper` is run by Claude Code, and its stdout becomes the
//!   credential. It points at `prismflow token print`, so the local token
//!   stays in the Keychain.
//! - An inherited `ANTHROPIC_AUTH_TOKEN` outranks `apiKeyHelper` and hijacks
//!   the session. The env block therefore blanks it and `ANTHROPIC_API_KEY`.
//!   With them blank, the helper is used, even with an OAuth login present.
//! - Unknown top-level keys are tolerated, so `_prismflow` marks ownership.
//!
//! As with Codex, PrismFlow owns one file per provider and never edits the
//! user's `settings.json`. Disconnect deletes only that file.

use super::{extra_bin_dirs, AppDetection};
use crate::config_engine::{ConfigError, EditPlan};
use crate::providers::ProviderDefinition;
use crate::routing::{ModelRef, Routing, Tier};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_ID: &str = "claude_code";
pub const MARKER_KEY: &str = "_prismflow";
pub const VERIFIED_MIN: (u32, u32, u32) = (2, 1, 0);
pub const VERIFIED_MAX: (u32, u32, u32) = (2, 1, u32::MAX);

/// Every model slot Claude Code may use (2.1.207 strings), each set to a
/// PrismFlow alias the broker resolves (`routing`). Unknown slots fall back
/// to the connection's own model at the broker, never to a model the user
/// did not pick (plan D4).
pub const MODEL_ENV: &[(&str, Tier)] = &[
    ("ANTHROPIC_MODEL", Tier::Default),
    ("ANTHROPIC_DEFAULT_FABLE_MODEL", Tier::Fable),
    ("ANTHROPIC_DEFAULT_OPUS_MODEL", Tier::Opus),
    ("ANTHROPIC_DEFAULT_SONNET_MODEL", Tier::Sonnet),
    ("ANTHROPIC_DEFAULT_HAIKU_MODEL", Tier::Haiku),
    ("ANTHROPIC_SMALL_FAST_MODEL", Tier::Haiku),
    ("CLAUDE_CODE_SUBAGENT_MODEL", Tier::Subagent),
];

/// Display names for the picker (`*_MODEL_NAME`), so users see real models.
const NAME_ENV: &[(&str, Tier)] = &[
    ("ANTHROPIC_DEFAULT_FABLE_MODEL_NAME", Tier::Fable),
    ("ANTHROPIC_DEFAULT_OPUS_MODEL_NAME", Tier::Opus),
    ("ANTHROPIC_DEFAULT_SONNET_MODEL_NAME", Tier::Sonnet),
    ("ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME", Tier::Haiku),
];

pub fn find_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PRISMFLOW_CLAUDE_BIN") {
        return Some(PathBuf::from(p));
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    candidates.extend(extra_bin_dirs());
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".claude").join("local"));
    }
    candidates.iter().find_map(|d| super::exe_in(d, "claude"))
}

/// PATH for Claude Code child processes (its npm form needs `node`).
pub fn child_path(bin: &Path) -> std::ffi::OsString {
    super::codex::child_path(bin)
}

pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    dirs::home_dir().unwrap_or_default().join(".claude")
}

pub fn detect() -> AppDetection {
    let Some(bin) = find_binary() else {
        return AppDetection {
            app_id: APP_ID.into(),
            display_name: "Claude Code CLI".into(),
            installed: false,
            path: None,
            version: None,
            version_verified: false,
            notes: vec!["Claude Code was not found on PATH or in common install locations.".into()],
        };
    };
    let raw = Command::new(&bin)
        .arg("--version")
        .env("PATH", child_path(&bin))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let parsed = raw.as_deref().and_then(super::codex::parse_version);
    let version_verified = parsed.is_some_and(|v| v >= VERIFIED_MIN && v <= VERIFIED_MAX);
    let mut notes = vec!["Connects through PrismFlow's local gateway, which must be running while you use it.".to_string()];
    match (&raw, version_verified) {
        (None, _) => notes.push("Found a claude binary, but `claude --version` failed.".into()),
        (Some(v), false) => notes.push(format!(
            "{v} has not been verified with PrismFlow (verified: 2.1.x). Connections will run staged verification before being trusted."
        )),
        _ => {}
    }
    AppDetection {
        app_id: APP_ID.into(),
        display_name: "Claude Code CLI".into(),
        installed: raw.is_some(),
        path: Some(bin.display().to_string()),
        version: raw,
        version_verified,
        notes,
    }
}

pub fn profile_name(provider_id: &str) -> String {
    format!("prismflow-{}", provider_id.replace('_', "-"))
}

pub fn settings_file(dir: &Path, provider_id: &str) -> PathBuf {
    dir.join(format!("{}.settings.json", profile_name(provider_id)))
}

/// POSIX single-quote a word for the shell that runs `apiKeyHelper`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn helper_command(helper: &Path, connection_id: &str) -> String {
    format!(
        "{} token print --connection {}",
        shell_quote(&helper.display().to_string()),
        shell_quote(connection_id)
    )
}

#[derive(Debug, thiserror::Error)]
pub enum ClaudeCodeError {
    #[error("{} exists but was not created by PrismFlow; refusing to overwrite it", .0.display())]
    NotOurs(PathBuf),
    #[error(transparent)]
    Config(#[from] ConfigError),
}

pub fn is_ours(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .is_some_and(|v| v[MARKER_KEY]["managed"] == true)
}

/// Plan the settings file. `broker_url` is the gateway's base URL; no token
/// or key is written, only the helper command that fetches the token.
pub fn plan_connect(
    dir: &Path,
    provider: &ProviderDefinition,
    model: &str,
    helper: &Path,
    connection_id: &str,
    broker_url: &str,
    routing: &Routing,
) -> Result<EditPlan, ClaudeCodeError> {
    let file = settings_file(dir, provider.id);
    let before = match std::fs::read_to_string(&file) {
        Ok(c) if !is_ours(&c) => return Err(ClaudeCodeError::NotOurs(file)),
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(ConfigError::Io { file, detail: e.to_string() }.into()),
    };
    let mut env = serde_json::Map::new();
    env.insert("ANTHROPIC_BASE_URL".into(), json!(broker_url));
    env.insert("ANTHROPIC_AUTH_TOKEN".into(), json!(""));
    env.insert("ANTHROPIC_API_KEY".into(), json!(""));
    let primary = ModelRef::new(provider.id, model);
    for (k, tier) in MODEL_ENV {
        env.insert((*k).into(), json!(tier.alias()));
    }
    for (k, tier) in NAME_ENV {
        let r = routing.tiers.get(tier).unwrap_or(&primary);
        env.insert((*k).into(), json!(format!("{} ({})", r.model, r.provider_id)));
    }
    env.insert("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), json!("1"));
    let doc = json!({
        MARKER_KEY: {
            "managed": true,
            "connection": connection_id,
            "note": format!(
                "Managed by PrismFlow for `claude --settings {}`. Removed on disconnect; edits here are overwritten.",
                file.display()
            ),
        },
        "apiKeyHelper": helper_command(helper, connection_id),
        "env": env,
    });
    let after = serde_json::to_string_pretty(&doc).expect("static JSON") + "\n";
    let summary = vec![
        format!("Create {} (your own settings.json is not modified)", file.display()),
        format!("Route Claude Code to PrismFlow's gateway at {broker_url} for {}", provider.display_name),
        if routing.tiers.is_empty() {
            format!("Use model {model} for every Claude Code model slot")
        } else {
            format!("Use model {model} by default, with per-tier overrides: {}", routing.tiers.iter()
                .map(|(t, r)| format!("{t:?} → {}/{}", r.provider_id, r.model)).collect::<Vec<_>>().join(", "))
        },
        "Fetch a revocable local token from the Keychain at runtime via apiKeyHelper".into(),
        "Ignore any ANTHROPIC_AUTH_TOKEN / ANTHROPIC_API_KEY in your shell for this profile only".into(),
    ];
    Ok(EditPlan { file, before, after, summary })
}

/// Remove the managed settings file, keeping a copy in `backup_dir`.
pub fn disconnect(file: &Path, backup_dir: &Path) -> Result<Option<PathBuf>, ClaudeCodeError> {
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConfigError::Io { file: file.into(), detail: e.to_string() }.into()),
    };
    if !is_ours(&content) {
        return Err(ClaudeCodeError::NotOurs(file.into()));
    }
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let backup = backup_dir.join(format!("{name}.{}", crate::diagnostics::now_secs()));
    let io = |e: std::io::Error| ConfigError::Io { file: file.into(), detail: e.to_string() };
    std::fs::copy(file, &backup).map_err(io)?;
    std::fs::remove_file(file).map_err(io)?;
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config_engine, providers};
    use std::fs;

    const URL: &str = "http://127.0.0.1:47821";

    fn helper() -> PathBuf {
        PathBuf::from("/Applications/Prism Flow.app/Contents/MacOS/prismflow")
    }

    fn plan(dir: &Path, model: &str) -> EditPlan {
        plan_connect(dir, providers::get("nvidia_nim").unwrap(), model, &helper(), "claude_code:nvidia_nim", URL, &Routing::default()).unwrap()
    }

    #[test]
    fn settings_route_to_broker_with_helper_and_no_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(dir.path(), "meta/llama-3.3-70b-instruct");
        assert_eq!(p.file, dir.path().join("prismflow-nvidia-nim.settings.json"));
        let v: serde_json::Value = serde_json::from_str(&p.after).unwrap();
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], URL);
        assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], "");
        assert_eq!(v["env"]["ANTHROPIC_API_KEY"], "");
        for (k, tier) in MODEL_ENV {
            assert_eq!(v["env"][*k], tier.alias(), "{k}");
        }
        assert_eq!(v["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL_NAME"], "meta/llama-3.3-70b-instruct (nvidia_nim)");
        assert_eq!(
            v["apiKeyHelper"],
            "'/Applications/Prism Flow.app/Contents/MacOS/prismflow' token print --connection 'claude_code:nvidia_nim'"
        );
        assert!(is_ours(&p.after));
        assert!(!p.after.contains("pft_") && !p.after.contains("nvapi-") && !p.after.contains("sk-"));
    }

    #[test]
    fn user_settings_untouched_and_reconnect_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("settings.json");
        fs::write(&user, "{\n  \"theme\": \"dark\"\n}\n").unwrap();
        config_engine::apply(&plan(dir.path(), "a/one")).unwrap();
        let again = plan(dir.path(), "a/one");
        assert_eq!(again.before, again.after);
        let switched = plan(dir.path(), "b/two");
        assert!(switched.after.contains("b/two (nvidia_nim)"));
        assert_eq!(fs::read_to_string(&user).unwrap(), "{\n  \"theme\": \"dark\"\n}\n");
    }

    #[test]
    fn refuses_foreign_files_and_disconnect_keeps_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        let foreign = settings_file(dir.path(), "nvidia_nim");
        fs::write(&foreign, "{\"env\":{}}").unwrap();
        assert!(matches!(
            plan_connect(dir.path(), providers::get("nvidia_nim").unwrap(), "m", &helper(), "c", URL, &Routing::default()),
            Err(ClaudeCodeError::NotOurs(_))
        ));
        assert!(matches!(disconnect(&foreign, backups.path()), Err(ClaudeCodeError::NotOurs(_))));
        assert!(foreign.exists());
        fs::remove_file(&foreign).unwrap();

        let p = plan(dir.path(), "m");
        config_engine::apply(&p).unwrap();
        let backup = disconnect(&p.file, backups.path()).unwrap().unwrap();
        assert!(!p.file.exists());
        assert!(is_ours(&fs::read_to_string(backup).unwrap()));
        assert!(disconnect(&p.file, backups.path()).unwrap().is_none());
    }

    #[test]
    fn quoting_survives_hostile_paths() {
        assert_eq!(shell_quote("/it's/x"), r"'/it'\''s/x'");
    }
}
