//! Codex CLI adapter (SPEC §4.1).
//!
//! Mechanism (verified against codex-cli 0.162.1, 2026-10-14):
//! - `codex -p <name>` layers `$CODEX_HOME/<name>.config.toml` over the base
//!   config, and that file may define its own `[model_providers.<id>]`.
//! - `auth.command` + `auth.args` are executed by Codex and stdout is sent as
//!   the Bearer token, so the key never sits in a config file.
//! - `wire_api = "chat"` is rejected at config load; only `"responses"` works.
//!
//! PrismFlow therefore owns one profile file per provider and never edits the
//! user's `config.toml`. Disconnect deletes only that file.

use super::{extra_bin_dirs, AppDetection};
use crate::config_engine::{self, ConfigError, EditPlan, TomlVal};
use crate::providers::{Protocol, ProviderDefinition};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_ID: &str = "codex_cli";
pub const MANAGED_MARKER: &str = "# Managed by PrismFlow";
/// Versions this adapter has been verified against (SPEC §21.2).
pub const VERIFIED_MIN: (u32, u32, u32) = (0, 162, 0);
pub const VERIFIED_MAX: (u32, u32, u32) = (0, 162, u32::MAX);

/// PATH for Codex child processes: its own directory first, then the
/// common install locations, then the inherited PATH.
pub fn child_path(codex_bin: &Path) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = Vec::new();
    if let Some(dir) = codex_bin.parent() {
        parts.push(dir.to_path_buf());
    }
    parts.extend(extra_bin_dirs());
    if let Some(existing) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(parts).unwrap_or_default()
}

pub fn find_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PRISMFLOW_CODEX_BIN") {
        return Some(PathBuf::from(p));
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    candidates.extend(extra_bin_dirs());
    candidates.into_iter().map(|d| d.join("codex")).find(|p| p.is_file())
}

pub fn codex_home() -> PathBuf {
    if let Some(h) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir().unwrap_or_default().join(".codex")
}

/// `"codex-cli 0.162.1"` → `(0, 162, 1)`.
pub fn parse_version(output: &str) -> Option<(u32, u32, u32)> {
    let v = output.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut it = v.split(['.', '-', '+']).map(|n| n.parse::<u32>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?))
}

pub fn detect() -> AppDetection {
    let Some(bin) = find_binary() else {
        return AppDetection {
            app_id: APP_ID.into(),
            display_name: "Codex CLI".into(),
            installed: false,
            path: None,
            version: None,
            version_verified: false,
            notes: vec!["Codex CLI was not found on PATH or in common install locations.".into()],
        };
    };
    let output = Command::new(&bin).arg("--version").env("PATH", child_path(&bin)).output();
    let raw = output
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let parsed = raw.as_deref().and_then(parse_version);
    let version_verified = parsed.is_some_and(|v| v >= VERIFIED_MIN && v <= VERIFIED_MAX);
    let mut notes = Vec::new();
    match (&raw, version_verified) {
        (None, _) => notes.push("Found a codex binary, but `codex --version` failed.".into()),
        (Some(v), false) => notes.push(format!(
            "{v} has not been verified with PrismFlow (verified: 0.162.x). Connections will run staged verification before being trusted."
        )),
        _ => {}
    }
    AppDetection {
        app_id: APP_ID.into(),
        display_name: "Codex CLI".into(),
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

pub fn provider_table(provider_id: &str) -> String {
    format!("prismflow_{provider_id}")
}

pub fn profile_file(home: &Path, provider_id: &str) -> PathBuf {
    home.join(format!("{}.config.toml", profile_name(provider_id)))
}

#[derive(Debug, thiserror::Error)]
pub enum CodexError {
    #[error("{0} does not offer the OpenAI Responses API, which Codex requires; connect it through PrismFlow's gateway")]
    ProtocolUnsupported(String),
    #[error("{0} needs a base URL; custom providers are not supported in this build")]
    MissingBaseUrl(String),
    #[error("{} exists but was not created by PrismFlow; refusing to overwrite it", .0.display())]
    NotOurs(PathBuf),
    #[error(transparent)]
    Config(#[from] ConfigError),
}

fn is_ours(content: &str) -> bool {
    content.starts_with(MANAGED_MARKER)
}

/// Plan the profile file for `provider` + `model`. `helper` is the absolute
/// path of the PrismFlow binary that serves `credential print`.
pub fn plan_connect(
    home: &Path,
    provider: &ProviderDefinition,
    model: &str,
    helper: &Path,
) -> Result<EditPlan, CodexError> {
    if !provider.protocols.contains(&Protocol::OpenAiResponses) {
        return Err(CodexError::ProtocolUnsupported(provider.display_name.into()));
    }
    if provider.base_url.is_empty() {
        return Err(CodexError::MissingBaseUrl(provider.display_name.into()));
    }
    let args = vec!["credential".into(), "print".into(), "--provider".into(), provider.id.into()];
    plan_profile(home, provider, model, provider.base_url, helper, args)
}

/// Plan a profile that reaches `provider` through PrismFlow's gateway
/// (GATEWAY-O): Codex speaks Responses to the broker, the engine speaks the
/// provider's own protocol upstream, and `auth.command` fetches the
/// connection's revocable local token instead of the provider key.
pub fn plan_connect_gateway(
    home: &Path,
    provider: &ProviderDefinition,
    model: &str,
    helper: &Path,
    connection_id: &str,
    broker_url: &str,
) -> Result<EditPlan, CodexError> {
    if provider.base_url.is_empty() {
        return Err(CodexError::MissingBaseUrl(provider.display_name.into()));
    }
    let args = vec!["token".into(), "print".into(), "--connection".into(), connection_id.into()];
    plan_profile(home, provider, model, &format!("{broker_url}/v1"), helper, args)
}

fn plan_profile(
    home: &Path,
    provider: &ProviderDefinition,
    model: &str,
    base_url: &str,
    helper: &Path,
    auth_args: Vec<String>,
) -> Result<EditPlan, CodexError> {
    let file = profile_file(home, provider.id);
    if let Ok(existing) = std::fs::read_to_string(&file) {
        if !is_ours(&existing) {
            return Err(CodexError::NotOurs(file));
        }
    }
    let table = provider_table(provider.id);
    let tables = vec![
        (
            vec![],
            vec![
                ("model".to_string(), TomlVal::Str(model.into())),
                ("model_provider".to_string(), TomlVal::Str(table.clone())),
            ],
        ),
        (
            vec!["model_providers".to_string(), table.clone()],
            vec![
                ("name".to_string(), TomlVal::Str(format!("{} (via PrismFlow)", provider.display_name))),
                ("base_url".to_string(), TomlVal::Str(base_url.into())),
                ("wire_api".to_string(), TomlVal::Str("responses".into())),
            ],
        ),
        (
            vec!["model_providers".to_string(), table, "auth".to_string()],
            vec![
                ("command".to_string(), TomlVal::Str(helper.display().to_string())),
                ("args".to_string(), TomlVal::StrArray(auth_args)),
            ],
        ),
    ];
    let mut plan = config_engine::plan_set_tables(&file, &tables)?;
    if plan.before.is_empty() {
        plan.after = format!(
            "{MANAGED_MARKER} for `codex -p {}`. Removed on disconnect; edits here are overwritten.\n{}",
            profile_name(provider.id),
            plan.after
        );
    }
    Ok(plan)
}

/// Remove the managed profile file. A copy goes to `backup_dir` first so
/// even PrismFlow's own file is recoverable. Returns the backup path.
pub fn disconnect(file: &Path, backup_dir: &Path) -> Result<Option<PathBuf>, CodexError> {
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ConfigError::Io { file: file.into(), detail: e.to_string() }.into())
        }
    };
    if !is_ours(&content) {
        return Err(CodexError::NotOurs(file.into()));
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
    use crate::providers;
    use std::fs;

    fn helper() -> PathBuf {
        PathBuf::from("/Applications/PrismFlow.app/Contents/MacOS/prismflow")
    }

    #[test]
    fn parses_cli_version() {
        assert_eq!(parse_version("codex-cli 0.162.1"), Some((0, 162, 1)));
        assert_eq!(parse_version("codex-cli 1.0.0-beta.2"), Some((1, 0, 0)));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn names_are_stable() {
        assert_eq!(profile_name("nvidia_nim"), "prismflow-nvidia-nim");
        assert_eq!(provider_table("openrouter"), "prismflow_openrouter");
    }

    #[test]
    fn connect_writes_only_a_marked_profile_file() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("config.toml");
        fs::write(&base, "# mine\nmodel = \"gpt-5.4-mini\"\n").unwrap();
        let plan = plan_connect(home.path(), providers::get("openrouter").unwrap(), "openai/gpt-oss-20b", &helper()).unwrap();
        assert_eq!(plan.file, home.path().join("prismflow-openrouter.config.toml"));
        config_engine::apply(&plan).unwrap();

        let written = fs::read_to_string(&plan.file).unwrap();
        assert!(written.starts_with(MANAGED_MARKER));
        let doc: toml_edit::DocumentMut = written.parse().unwrap();
        assert_eq!(doc["model"].as_str(), Some("openai/gpt-oss-20b"));
        assert_eq!(doc["model_provider"].as_str(), Some("prismflow_openrouter"));
        let p = &doc["model_providers"]["prismflow_openrouter"];
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        assert_eq!(p["base_url"].as_str(), Some("https://openrouter.ai/api/v1"));
        assert_eq!(p["auth"]["command"].as_str(), Some(helper().to_str().unwrap()));
        assert!(!written.contains("sk-"), "no secret may be written");
        // the user's config is untouched
        assert_eq!(fs::read_to_string(&base).unwrap(), "# mine\nmodel = \"gpt-5.4-mini\"\n");
    }

    #[test]
    fn reconnect_updates_model_idempotently() {
        let home = tempfile::tempdir().unwrap();
        let or = providers::get("openrouter").unwrap();
        config_engine::apply(&plan_connect(home.path(), or, "a/one", &helper()).unwrap()).unwrap();
        let again = plan_connect(home.path(), or, "a/one", &helper()).unwrap();
        assert_eq!(again.before, again.after);
        let switched = plan_connect(home.path(), or, "b/two", &helper()).unwrap();
        assert!(switched.after.contains("\"b/two\""));
        assert_eq!(switched.after.matches(MANAGED_MARKER).count(), 1);
    }

    #[test]
    fn refuses_foreign_file_and_chat_only_providers() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("prismflow-openrouter.config.toml"), "model = \"user's\"\n").unwrap();
        assert!(matches!(
            plan_connect(home.path(), providers::get("openrouter").unwrap(), "m", &helper()),
            Err(CodexError::NotOurs(_))
        ));
        assert!(matches!(
            plan_connect(home.path(), providers::get("nvidia_nim").unwrap(), "m", &helper()),
            Err(CodexError::ProtocolUnsupported(_))
        ));
        assert!(matches!(
            plan_connect(home.path(), providers::get("custom_openai").unwrap(), "m", &helper()),
            Err(CodexError::MissingBaseUrl(_))
        ));
    }

    #[test]
    fn gateway_profile_points_at_broker_with_token_helper() {
        let home = tempfile::tempdir().unwrap();
        let nim = providers::get("nvidia_nim").unwrap();
        let plan = plan_connect_gateway(home.path(), nim, "meta/llama-3.3-70b-instruct", &helper(), "codex_cli:nvidia_nim", "http://127.0.0.1:47821").unwrap();
        config_engine::apply(&plan).unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&plan.file).unwrap().parse().unwrap();
        let p = &doc["model_providers"]["prismflow_nvidia_nim"];
        assert_eq!(p["base_url"].as_str(), Some("http://127.0.0.1:47821/v1"));
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        let args: Vec<_> = p["auth"]["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(args, ["token", "print", "--connection", "codex_cli:nvidia_nim"]);
        assert!(!plan.after.contains("pft_") && !plan.after.contains("nvapi-"));
    }

    #[test]
    fn disconnect_backs_up_and_removes_only_our_file() {
        let home = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        let plan = plan_connect(home.path(), providers::get("openrouter").unwrap(), "m", &helper()).unwrap();
        config_engine::apply(&plan).unwrap();
        let backup = disconnect(&plan.file, backups.path()).unwrap().unwrap();
        assert!(!plan.file.exists());
        assert!(fs::read_to_string(backup).unwrap().starts_with(MANAGED_MARKER));
        assert!(disconnect(&plan.file, backups.path()).unwrap().is_none(), "second disconnect is a no-op");

        let foreign = home.path().join("prismflow-x.config.toml");
        fs::write(&foreign, "model = \"user's\"\n").unwrap();
        assert!(matches!(disconnect(&foreign, backups.path()), Err(CodexError::NotOurs(_))));
        assert!(foreign.exists());
    }
}
