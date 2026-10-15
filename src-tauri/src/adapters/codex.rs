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
    catalog: Option<&Path>,
) -> Result<EditPlan, CodexError> {
    if !provider.protocols.contains(&Protocol::OpenAiResponses) {
        return Err(CodexError::ProtocolUnsupported(provider.display_name.into()));
    }
    if provider.base_url.is_empty() {
        return Err(CodexError::MissingBaseUrl(provider.display_name.into()));
    }
    let args = vec!["credential".into(), "print".into(), "--provider".into(), provider.id.into()];
    plan_profile(home, provider, model, provider.base_url, helper, args, catalog)
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
    catalog: Option<&Path>,
) -> Result<EditPlan, CodexError> {
    if provider.base_url.is_empty() {
        return Err(CodexError::MissingBaseUrl(provider.display_name.into()));
    }
    let args = vec!["token".into(), "print".into(), "--connection".into(), connection_id.into()];
    plan_profile(home, provider, model, &format!("{broker_url}/v1"), helper, args, catalog)
}

fn plan_profile(
    home: &Path,
    provider: &ProviderDefinition,
    model: &str,
    base_url: &str,
    helper: &Path,
    auth_args: Vec<String>,
    catalog: Option<&Path>,
) -> Result<EditPlan, CodexError> {
    let file = profile_file(home, provider.id);
    if let Ok(existing) = std::fs::read_to_string(&file) {
        if !is_ours(&existing) {
            return Err(CodexError::NotOurs(file));
        }
    }
    let table = provider_table(provider.id);
    let mut top = vec![
        ("model".to_string(), TomlVal::Str(model.into())),
        ("model_provider".to_string(), TomlVal::Str(table.clone())),
    ];
    if let Some(c) = catalog {
        top.push(("model_catalog_json".to_string(), TomlVal::Str(c.display().to_string())));
    }
    let tables = vec![
        (vec![], top),
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

// ---- Model catalog (`model_catalog_json`) -------------------------------
//
// Without a catalog entry Codex logs "Model metadata ... not found" and
// guesses limits. Verified against 0.162.1 (2026-10-14):
// - Required per model: slug, display_name, priority, visibility,
//   supported_in_api, shell_type, support_verbosity,
//   supported_reasoning_levels, experimental_supported_tools,
//   truncation_policy, and base_instructions (the agent's system prompt).
// - `context_window` is optional, so an unknown window is left out, never guessed.
// - Copying a whole OpenAI entry switches Codex to OpenAI-only `custom` tools
//   and code mode. A minimal entry keeps the plain `function` tool surface
//   Codex uses for unknown models, which is what translated providers handle.
// The system prompt and shell settings come from the installed Codex
// (`codex debug models`), so they track Codex updates instead of a copy.

#[derive(Debug, Clone)]
pub struct CatalogTemplate {
    pub base_instructions: String,
    pub shell_type: serde_json::Value,
    pub truncation_policy: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    pub slug: String,
    pub context_window: Option<u64>,
    pub images: bool,
}

/// Pick the template from `codex debug models` output: the highest-priority
/// listed model that carries base instructions.
pub fn template_from_models(raw: &str) -> Result<CatalogTemplate, String> {
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|e| format!("unreadable model list: {e}"))?;
    let models = v["models"].as_array().ok_or("no `models` array")?;
    let best = models
        .iter()
        .filter(|m| m["visibility"] == "list" && m["base_instructions"].as_str().is_some_and(|s| !s.is_empty()))
        .min_by_key(|m| m["priority"].as_i64().unwrap_or(i64::MAX))
        .ok_or("no listed model with base instructions")?;
    Ok(CatalogTemplate {
        base_instructions: best["base_instructions"].as_str().unwrap_or_default().to_string(),
        shell_type: best["shell_type"].clone(),
        truncation_policy: best["truncation_policy"].clone(),
    })
}

/// Ask the installed Codex for its built-in catalog. A throwaway
/// `CODEX_HOME` keeps the user's own config and catalogs out of it.
pub fn catalog_template(bin: &Path) -> Result<CatalogTemplate, String> {
    let scratch = std::env::temp_dir().join(format!("prismflow-codex-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let out = Command::new(bin)
        .args(["debug", "models"])
        .env("CODEX_HOME", &scratch)
        .env("PATH", child_path(bin))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run codex: {e}"));
    let _ = std::fs::remove_dir_all(&scratch);
    let out = out?;
    if !out.status.success() {
        return Err(format!("`codex debug models` failed ({})", out.status));
    }
    template_from_models(&String::from_utf8_lossy(&out.stdout))
}

pub fn render_catalog(t: &CatalogTemplate, entries: &[CatalogEntry]) -> serde_json::Value {
    let models: Vec<_> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut m = serde_json::json!({
                "slug": e.slug,
                "display_name": e.slug,
                "description": "Connected by PrismFlow",
                "priority": i,
                "visibility": "list",
                "supported_in_api": true,
                "shell_type": t.shell_type,
                "support_verbosity": false,
                "supported_reasoning_levels": [],
                "experimental_supported_tools": [],
                "truncation_policy": t.truncation_policy,
                "input_modalities": if e.images { serde_json::json!(["text", "image"]) } else { serde_json::json!(["text"]) },
                "base_instructions": t.base_instructions,
            });
            if let Some(cw) = e.context_window {
                m["context_window"] = cw.into();
                m["max_context_window"] = cw.into();
            }
            m
        })
        .collect();
    serde_json::json!({ "models": models })
}


// ---- Codex Desktop: the base config ------------------------------------
//
// The Codex app (bundle id com.openai.codex) reads ~/.codex/config.toml and
// has no `-p`. Codex 0.162 rejects the legacy top-level `profile` key
// (verified 2026-10-14), so the desktop connection edits the base file:
// the top-level keys in BASE_KEYS plus one `[model_providers.prismflow_*]`
// table. Prior values are recorded and restored exactly on disconnect.

pub const DESKTOP_APP_ID: &str = "codex_desktop";
pub const DESKTOP_BUNDLE_ID: &str = "com.openai.codex";
pub const BASE_KEYS: &[&str] = &["model", "model_provider", "model_catalog_json"];

pub fn base_config(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// Plan the base-config edit. `auth_args` + `base_url` decide direct vs
/// gateway exactly as for the CLI profile.
#[allow(clippy::too_many_arguments)]
pub fn plan_connect_base(
    home: &Path,
    provider: &ProviderDefinition,
    model: &str,
    base_url: &str,
    helper: &Path,
    auth_args: Vec<String>,
    catalog: Option<&Path>,
) -> Result<EditPlan, CodexError> {
    let file = base_config(home);
    let table = provider_table(provider.id);
    let mut top = vec![
        ("model".to_string(), TomlVal::Str(model.into())),
        ("model_provider".to_string(), TomlVal::Str(table.clone())),
    ];
    if let Some(c) = catalog {
        top.push(("model_catalog_json".to_string(), TomlVal::Str(c.display().to_string())));
    }
    let tables = vec![
        (vec![], top),
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
    Ok(config_engine::plan_set_tables(&file, &tables)?)
}

/// Current values of BASE_KEYS (None = absent), for restore on disconnect.
pub fn read_base_priors(home: &Path) -> Result<std::collections::BTreeMap<String, Option<String>>, CodexError> {
    Ok(config_engine::read_top_level_strings(&base_config(home), BASE_KEYS)?)
}

/// Undo the desktop connection: drop our provider table, restore priors.
/// Returns the plans to apply (removal first), or none if nothing remains.
pub fn plan_disconnect_base(
    home: &Path,
    provider_id: &str,
    priors: &std::collections::BTreeMap<String, Option<String>>,
) -> Result<Vec<EditPlan>, CodexError> {
    let file = base_config(home);
    let mut plans = Vec::new();
    let before = std::fs::read_to_string(&file).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = before.parse().map_err(|e: toml_edit::TomlError| ConfigError::Parse { file: file.clone(), detail: e.to_string() })?;
    if let Some(t) = doc.get_mut("model_providers").and_then(|t| t.as_table_like_mut()) {
        t.remove(&provider_table(provider_id));
    }
    for k in BASE_KEYS {
        match priors.get(*k).cloned().flatten() {
            Some(v) => { doc[*k] = toml_edit::value(v); }
            None => { doc.remove(k); }
        }
    }
    let after = doc.to_string();
    if after != before {
        plans.push(EditPlan { file, before, after, summary: vec![format!("Restore Codex's own provider settings and remove {}", provider_table(provider_id))] });
    }
    Ok(plans)
}

/// Codex Desktop detection: the app bundle, not a CLI.
pub fn detect_desktop() -> AppDetection {
    let app = ["/Applications/ChatGPT.app", "/Applications/Codex.app"]
        .iter()
        .map(PathBuf::from)
        .chain(dirs::home_dir().map(|h| h.join("Applications/ChatGPT.app")))
        .find(|p| {
            Command::new("/usr/bin/defaults")
                .args(["read", &p.join("Contents/Info").display().to_string(), "CFBundleIdentifier"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == DESKTOP_BUNDLE_ID)
        });
    let mut d = AppDetection {
        app_id: DESKTOP_APP_ID.into(),
        display_name: "Codex Desktop".into(),
        installed: app.is_some(),
        path: app.as_ref().map(|p| p.display().to_string()),
        version: None,
        version_verified: false,
        notes: vec![
            "Uses your ~/.codex/config.toml (the desktop app has no profiles). PrismFlow changes three top-level keys and adds one provider table, and restores the previous values on disconnect.".into(),
            "Restart the Codex app after connecting or disconnecting.".into(),
        ],
    };
    if let Some(p) = &app {
        d.version = Command::new("/usr/bin/defaults")
            .args(["read", &p.join("Contents/Info").display().to_string(), "CFBundleShortVersionString"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        d.notes.push("Verified through the Codex CLI, which shares the app's config loader; the app itself is not driven by PrismFlow.".into());
    } else {
        d.notes = vec!["The Codex desktop app was not found in /Applications.".into()];
    }
    d
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
        let plan = plan_connect(home.path(), providers::get("openrouter").unwrap(), "openai/gpt-oss-20b", &helper(), None).unwrap();
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
        config_engine::apply(&plan_connect(home.path(), or, "a/one", &helper(), None).unwrap()).unwrap();
        let again = plan_connect(home.path(), or, "a/one", &helper(), None).unwrap();
        assert_eq!(again.before, again.after);
        let switched = plan_connect(home.path(), or, "b/two", &helper(), None).unwrap();
        assert!(switched.after.contains("\"b/two\""));
        assert_eq!(switched.after.matches(MANAGED_MARKER).count(), 1);
    }

    #[test]
    fn refuses_foreign_file_and_chat_only_providers() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("prismflow-openrouter.config.toml"), "model = \"user's\"\n").unwrap();
        assert!(matches!(
            plan_connect(home.path(), providers::get("openrouter").unwrap(), "m", &helper(), None),
            Err(CodexError::NotOurs(_))
        ));
        assert!(matches!(
            plan_connect(home.path(), providers::get("nvidia_nim").unwrap(), "m", &helper(), None),
            Err(CodexError::ProtocolUnsupported(_))
        ));
    }

    #[test]
    fn gateway_profile_points_at_broker_with_token_helper() {
        let home = tempfile::tempdir().unwrap();
        let nim = providers::get("nvidia_nim").unwrap();
        let plan = plan_connect_gateway(home.path(), nim, "meta/llama-3.3-70b-instruct", &helper(), "codex_cli:nvidia_nim", "http://127.0.0.1:47821", None).unwrap();
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
        let plan = plan_connect(home.path(), providers::get("openrouter").unwrap(), "m", &helper(), None).unwrap();
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

    #[test]
    fn template_picks_highest_priority_listed_model() {
        let raw = r#"{"models":[
            {"slug":"hidden","visibility":"hide","priority":0,"base_instructions":"H"},
            {"slug":"b","visibility":"list","priority":5,"base_instructions":"B","shell_type":"x","truncation_policy":{}},
            {"slug":"a","visibility":"list","priority":1,"base_instructions":"A","shell_type":"unified_exec","truncation_policy":{"mode":"tokens","limit":10}}
        ]}"#;
        let t = template_from_models(raw).unwrap();
        assert_eq!(t.base_instructions, "A");
        assert_eq!(t.shell_type, "unified_exec");
        assert!(template_from_models(r#"{"models":[]}"#).is_err());
    }

    #[test]
    fn catalog_has_required_fields_and_never_invents_a_window() {
        let t = CatalogTemplate { base_instructions: "P".into(), shell_type: "unified_exec".into(), truncation_policy: serde_json::json!({}) };
        let v = render_catalog(&t, &[
            CatalogEntry { slug: "a/b".into(), context_window: Some(131072), images: false },
            CatalogEntry { slug: "c/d".into(), context_window: None, images: true },
        ]);
        let m = &v["models"];
        for k in ["slug", "display_name", "priority", "visibility", "supported_in_api", "shell_type", "support_verbosity",
                  "supported_reasoning_levels", "experimental_supported_tools", "truncation_policy", "base_instructions"] {
            assert!(!m[0][k].is_null(), "missing {k}");
        }
        assert_eq!(m[0]["context_window"], 131072);
        assert!(m[1].get("context_window").is_none());
        assert_eq!(m[1]["input_modalities"], serde_json::json!(["text", "image"]));
        assert!(m[0].get("apply_patch_tool_type").is_none(), "freeform patch is OpenAI-only");
    }

    #[test]
    fn profile_points_at_catalog_when_given() {
        let home = tempfile::tempdir().unwrap();
        let cat = home.path().join("cat.json");
        let plan = plan_connect(home.path(), providers::get("openrouter").unwrap(), "m", &helper(), Some(&cat)).unwrap();
        assert!(plan.after.contains(&format!("model_catalog_json = \"{}\"", cat.display())));
    }

    #[test]
    fn desktop_base_edit_restores_priors_exactly() {
        let home = tempfile::tempdir().unwrap();
        let original = "# my codex\nmodel = \"nvidia_nim/z-ai/glm\"\nmodel_provider = \"other\"\n\n[model_providers.other]\nbase_url = \"http://127.0.0.1:9090/v1\"\n\n[mcp_servers.x]\ncommand = \"node\"\n";
        std::fs::write(base_config(home.path()), original).unwrap();
        let priors = read_base_priors(home.path()).unwrap();
        assert_eq!(priors["model_provider"].as_deref(), Some("other"));
        assert_eq!(priors["model_catalog_json"], None);
        let cat = home.path().join("cat.json");
        let plan = plan_connect_base(home.path(), providers::get("openrouter").unwrap(), "a/b", "https://openrouter.ai/api/v1",
            &helper(), vec!["credential".into()], Some(&cat)).unwrap();
        config_engine::apply(&plan).unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(base_config(home.path())).unwrap().parse().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("prismflow_openrouter"));
        assert_eq!(doc["model_providers"]["other"]["base_url"].as_str(), Some("http://127.0.0.1:9090/v1"), "other tables untouched");
        for p in plan_disconnect_base(home.path(), "openrouter", &priors).unwrap() {
            config_engine::apply(&p).unwrap();
        }
        let restored: toml_edit::DocumentMut = std::fs::read_to_string(base_config(home.path())).unwrap().parse().unwrap();
        let orig: toml_edit::DocumentMut = original.parse().unwrap();
        assert_eq!(restored["model"].as_str(), orig["model"].as_str());
        assert_eq!(restored["model_provider"].as_str(), Some("other"));
        assert!(restored.get("model_catalog_json").is_none());
        assert!(restored["model_providers"].get("prismflow_openrouter").is_none());
        let text = std::fs::read_to_string(base_config(home.path())).unwrap();
        assert!(text.contains("# my codex") && text.contains("[mcp_servers.x]"));
    }
}
