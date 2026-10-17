//! Comment-preserving, atomic TOML editing for managed configuration blocks.
//! Structured parsers only (toml_edit preserves comments and
//! formatting); backup before write; atomic rename; managed content lives
//! under the `prismflow_` prefix so removal never touches user content.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("TOML parse error in {file}: {detail}")]
    Parse { file: PathBuf, detail: String },
    #[error("configuration conflict at '{path}': existing value is not a table")]
    Conflict { path: String },
    #[error("I/O error on {file}: {detail}")]
    Io { file: PathBuf, detail: String },
}

/// A value the engine can write into a TOML document.
#[derive(Debug, Clone, PartialEq)]
pub enum TomlVal {
    Str(String),
    StrArray(Vec<String>),
    Bool(bool),
}

fn toml_value(v: &TomlVal) -> Item {
    match v {
        TomlVal::Str(s) => toml_edit::value(s.as_str()),
        TomlVal::Bool(b) => toml_edit::value(*b),
        TomlVal::StrArray(items) => {
            let mut arr = toml_edit::Array::new();
            for s in items {
                arr.push(s.as_str());
            }
            toml_edit::value(arr)
        }
    }
}

/// A planned, previewable edit to one TOML file.
#[derive(Debug)]
pub struct EditPlan {
    pub file: PathBuf,
    pub before: String,
    pub after: String,
    pub summary: Vec<String>,
}

/// What a successful apply did — the rollback record.
#[derive(Debug)]
pub struct AppliedEdit {
    pub file: PathBuf,
    pub backup_path: Option<PathBuf>,
}

fn read_file(file: &Path) -> Result<String, ConfigError> {
    match fs::read_to_string(file) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(ConfigError::Io { file: file.to_path_buf(), detail: e.to_string() }),
    }
}

fn parse(content: &str, file: &Path) -> Result<DocumentMut, ConfigError> {
    if content.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    content.parse::<DocumentMut>().map_err(|e| ConfigError::Parse {
        file: file.to_path_buf(),
        detail: e.to_string(),
    })
}

/// Walk to `path`, creating tables as needed. Intermediate tables are created
/// implicit (no header line); the leaf is always made explicit so call order
/// never matters. An existing non-table at any segment is a Conflict — never
/// overwrite user data. (Recursive: one borrow per level keeps this simple.)
fn walk_mut<'a>(table: &'a mut Table, path: &[String]) -> Result<&'a mut Table, ConfigError> {
    fn go<'a>(
        table: &'a mut Table,
        path: &[String],
        prefix: String,
    ) -> Result<&'a mut Table, ConfigError> {
        if path.is_empty() {
            return Ok(table);
        }
        let seg = &path[0];
        let here = if prefix.is_empty() { seg.clone() } else { format!("{prefix}.{seg}") };
        if let Some(existing) = table.get(seg) {
            if !existing.is_table() {
                return Err(ConfigError::Conflict { path: here });
            }
        }
        let is_leaf = path.len() == 1;
        let entry = table.entry(seg).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(!is_leaf);
            Item::Table(t)
        });
        if is_leaf {
            if let Item::Table(t) = entry {
                t.set_implicit(false);
            }
        }
        let child = entry.as_table_mut().expect("checked or inserted a table");
        go(child, &path[1..], here)
    }
    go(table, path, String::new())
}

/// Plan setting key/value entries at (possibly nested) table paths. An empty
/// path means top-level keys. Never writes; the file is only read.
pub fn plan_set_tables(
    file: &Path,
    tables: &[(Vec<String>, Vec<(String, TomlVal)>)],
) -> Result<EditPlan, ConfigError> {
    let before = read_file(file)?;
    let mut doc = parse(&before, file)?;
    let mut summary = Vec::new();
    for (path, entries) in tables {
        let table = walk_mut(doc.as_table_mut(), path)?;
        for (key, val) in entries {
            let display = if path.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", path.join("."), key)
            };
            match table.get(key) {
                Some(existing) => {
                    summary.push(format!("~ {display} (was {})", render_existing(Some(existing))))
                }
                None => summary.push(format!("+ {display}")),
            }
            table[key] = toml_value(val);
        }
    }
    Ok(EditPlan { file: file.to_path_buf(), before, after: doc.to_string(), summary })
}

/// Plan removing every table whose name starts with `prefix` directly under
/// `parent_path`. Returns None when there is nothing to remove.
pub fn plan_remove_prefixed_tables(
    file: &Path,
    parent_path: &[String],
    prefix: &str,
) -> Result<Option<EditPlan>, ConfigError> {
    let before = read_file(file)?;
    if before.trim().is_empty() {
        return Ok(None);
    }
    let mut doc = parse(&before, file)?;
    let doomed: Vec<String> = {
        let mut t: &Table = doc.as_table();
        let mut found = true;
        for seg in parent_path {
            match t.get(seg).and_then(|i| i.as_table()) {
                Some(next) => t = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if !found {
            Vec::new()
        } else {
            t.iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .map(|(k, _)| k.to_string())
                .collect()
        }
    };
    if doomed.is_empty() {
        return Ok(None);
    }
    let mut summary = Vec::new();
    let parent = walk_mut(doc.as_table_mut(), parent_path)?;
    for key in &doomed {
        summary.push(format!("- {key}"));
        parent.remove(key);
    }
    Ok(Some(EditPlan { file: file.to_path_buf(), before, after: doc.to_string(), summary }))
}

/// Plan restoring captured prior top-level values: `Some(v)` sets the key,
/// `None` removes it (PrismFlow had added it). Returns None when nothing
/// would change.
pub fn plan_restore_top_level(
    file: &Path,
    priors: &BTreeMap<String, Option<TomlVal>>,
) -> Result<Option<EditPlan>, ConfigError> {
    let before = read_file(file)?;
    if before.trim().is_empty() {
        return Ok(None);
    }
    let mut doc = parse(&before, file)?;
    let mut summary = Vec::new();
    let root = doc.as_table_mut();
    for (key, prior) in priors {
        match prior {
            Some(v) => {
                summary.push(format!("~ {key} (restored)"));
                root[key] = toml_value(v);
            }
            None => {
                if root.get(key).is_some() {
                    summary.push(format!("- {key} (removed; added by PrismFlow)"));
                    root.remove(key);
                }
            }
        }
    }
    if summary.is_empty() {
        return Ok(None);
    }
    Ok(Some(EditPlan { file: file.to_path_buf(), before, after: doc.to_string(), summary }))
}

/// Read current top-level string values (for capturing rollback priors).
pub fn read_top_level_strings(
    file: &Path,
    keys: &[&str],
) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
    let mut out = BTreeMap::new();
    if !file.exists() {
        for k in keys {
            out.insert(k.to_string(), None);
        }
        return Ok(out);
    }
    let before = read_file(file)?;
    let doc = parse(&before, file)?;
    for k in keys {
        let v = doc.get(k).and_then(|i| i.as_str()).map(|s| s.to_string());
        out.insert(k.to_string(), v);
    }
    Ok(out)
}

fn render_existing(item: Option<&Item>) -> String {
    match item {
        None => "∅".to_string(),
        Some(i) => {
            if let Some(s) = i.as_str() {
                format!("\"{s}\"")
            } else if let Some(b) = i.as_bool() {
                b.to_string()
            } else {
                i.to_string().trim().to_string()
            }
        }
    }
}

/// Copy `path` to `<name>.prismflow-backup-<unix_ts>` in the same directory.
/// Returns None when the file does not exist (nothing to back up).
pub fn backup(path: &Path) -> Result<Option<PathBuf>, ConfigError> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".prismflow-backup-{ts}"));
    let dest = path.with_file_name(name);
    fs::copy(path, &dest)
        .map_err(|e| ConfigError::Io { file: path.to_path_buf(), detail: e.to_string() })?;
    // A backup can hold a key the original held; only the owner reads it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))
            .map_err(|e| ConfigError::Io { file: dest.clone(), detail: e.to_string() })?;
    }
    Ok(Some(dest))
}

/// Write via a temp file in the same directory + rename (atomic on the same
/// filesystem). Cleans up the temp file on failure.
/// A new file readable only by its owner (on Unix; Windows files take the
/// folder's permissions).
fn new_private_file(path: &Path) -> std::io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

pub fn atomic_write(path: &Path, content: &str) -> Result<(), ConfigError> {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(".{file_name}.prismflow-tmp-{}", uuid::Uuid::new_v4()));
    let write_result = (|| {
        // An app installed but never opened may not have its settings folder yet.
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = new_private_file(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        // Keep the file's own permissions; a new file stays owner-only
        // (some of these files hold PrismFlow's local key).
        if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, path)
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(ConfigError::Io { file: path.to_path_buf(), detail: e.to_string() });
    }
    Ok(())
}

/// Apply a plan: back up, then atomically write. Never partially applies.
pub fn apply(plan: &EditPlan) -> Result<AppliedEdit, ConfigError> {
    let backup_path = backup(&plan.file)?;
    atomic_write(&plan.file, &plan.after)?;
    Ok(AppliedEdit { file: plan.file.clone(), backup_path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A realistic pre-existing user config: comments, unrelated tables,
    /// dotted project keys — everything we must preserve untouched.
    const USER_CONFIG: &str = r#"
# My Codex configuration — edited by hand, comments matter.
model = "gpt-5.4-mini"
model_reasoning_effort = "high"

[desktop]
followUpQueueMode = "queue"

[mcp_servers.node_repl]
command = "node"

[projects."/Users/alice/work/app"]
trust_level = "trusted"
"#;

    fn tmp_config(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn provider_tables() -> Vec<(Vec<String>, Vec<(String, TomlVal)>)> {
        vec![
            (
                vec!["model_providers".into(), "prismflow_openrouter".into()],
                vec![
                    ("name".into(), TomlVal::Str("OpenRouter (via PrismFlow)".into())),
                    ("base_url".into(), TomlVal::Str("https://openrouter.ai/api/v1".into())),
                    ("wire_api".into(), TomlVal::Str("chat".into())),
                ],
            ),
            (
                vec!["model_providers".into(), "prismflow_openrouter".into(), "auth".into()],
                vec![
                    ("command".into(), TomlVal::Str("/Applications/PrismFlow.app/Contents/MacOS/prismflow".into())),
                    ("args".into(), TomlVal::StrArray(vec![
                        "credential".into(), "print".into(), "--provider".into(), "openrouter".into(),
                    ])),
                ],
            ),
        ]
    }

    #[cfg(unix)]
    #[test]
    fn writes_keep_permissions_and_new_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new.json");
        atomic_write(&new, "{}").unwrap();
        assert_eq!(mode(&new), 0o600);
        let theirs = dir.path().join("theirs.json");
        fs::write(&theirs, "{}").unwrap();
        fs::set_permissions(&theirs, fs::Permissions::from_mode(0o640)).unwrap();
        let b = apply(&EditPlan { file: theirs.clone(), before: "{}".into(), after: "{\"a\":1}".into(), summary: vec![] }).unwrap();
        assert_eq!(mode(&theirs), 0o640, "the file keeps its own permissions");
        assert_eq!(mode(&b.backup_path.unwrap()), 0o600, "backups are owner-only");
    }

    #[test]
    fn set_tables_creates_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let plan = plan_set_tables(&path, &provider_tables()).unwrap();
        assert!(plan.before.is_empty());
        apply(&plan).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[model_providers.prismflow_openrouter]"));
        assert!(written.contains(r#"wire_api = "chat""#));
        assert!(written.contains(r#"args = ["credential", "print", "--provider", "openrouter"]"#));
        written.parse::<toml_edit::DocumentMut>().unwrap();
    }

    #[test]
    fn set_tables_preserves_unrelated_content() {
        let (_dir, path) = tmp_config(USER_CONFIG);
        let plan = plan_set_tables(&path, &provider_tables()).unwrap();
        for needle in [
            "# My Codex configuration — edited by hand, comments matter.",
            r#"model = "gpt-5.4-mini""#,
            "[desktop]",
            r#"followUpQueueMode = "queue""#,
            "[mcp_servers.node_repl]",
            r#"[projects."/Users/alice/work/app"]"#,
        ] {
            assert!(plan.after.contains(needle), "edited file lost user content: {needle}");
        }
    }

    #[test]
    fn reapply_is_idempotent() {
        let (_dir, path) = tmp_config(USER_CONFIG);
        apply(&plan_set_tables(&path, &provider_tables()).unwrap()).unwrap();
        let after_first = fs::read_to_string(&path).unwrap();
        let second = plan_set_tables(&path, &provider_tables()).unwrap();
        assert_eq!(second.after, after_first, "re-applying must not churn the file");
    }

    #[test]
    fn malformed_toml_errors_without_touching_file() {
        let (_dir, path) = tmp_config("this is [ not = valid toml\n");
        let before = fs::read_to_string(&path).unwrap();
        let result = plan_set_tables(&path, &provider_tables());
        assert!(matches!(result, Err(ConfigError::Parse { .. })));
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn conflict_when_segment_is_not_a_table() {
        let (_dir, path) = tmp_config("model_providers = \"oops\"\n");
        let result = plan_set_tables(&path, &provider_tables());
        assert!(matches!(result, Err(ConfigError::Conflict { .. })));
    }

    #[test]
    fn apply_creates_backup_with_original_bytes() {
        let (_dir, path) = tmp_config(USER_CONFIG);
        let plan = plan_set_tables(&path, &provider_tables()).unwrap();
        let applied = apply(&plan).unwrap();
        let backup = applied.backup_path.expect("existing file must be backed up");
        assert_eq!(fs::read_to_string(backup).unwrap(), USER_CONFIG);
    }

    #[test]
    fn remove_prefixed_tables_leaves_siblings() {
        let (_dir, path) = tmp_config(USER_CONFIG);
        apply(&plan_set_tables(&path, &provider_tables()).unwrap()).unwrap();
        let removal = plan_remove_prefixed_tables(&path, &["model_providers".into()], "prismflow_")
            .unwrap()
            .expect("there is a managed table to remove");
        apply(&removal).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("prismflow_openrouter"));
        assert!(written.contains(r#"model = "gpt-5.4-mini""#));
        assert!(written.contains("[mcp_servers.node_repl]"));
    }

    #[test]
    fn restore_top_level_sets_and_removes() {
        let (_dir, path) = tmp_config(USER_CONFIG);
        let mut priors = BTreeMap::new();
        priors.insert("model".to_string(), Some(TomlVal::Str("gpt-5.4-mini".into())));
        priors.insert("model_provider".to_string(), None); // absent before us → remove
        let plan = plan_restore_top_level(&path, &priors).unwrap().unwrap();
        apply(&plan).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains(r#"model = "gpt-5.4-mini""#));
        assert!(!written.contains("model_provider"));
    }

    #[test]
    fn read_top_level_captures_present_and_absent() {
        let (_dir, path) = tmp_config(USER_CONFIG);
        let priors = read_top_level_strings(&path, &["model", "model_provider"]).unwrap();
        assert_eq!(priors["model"].as_deref(), Some("gpt-5.4-mini"));
        assert_eq!(priors["model_provider"], None);
    }
}
