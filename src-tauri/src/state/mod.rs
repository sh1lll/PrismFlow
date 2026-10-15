//! Persistent, nonsecret connection state (SPEC §11 persistence, §13.13).
//! No secret ever lands here: credentials live only in the secure store.

use crate::config_engine::{atomic_write, ConfigError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Connection statuses — SPEC §9 vocabulary, plus `Unverified`, which
/// SPEC §13.14 requires: configured but not yet (re)verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    NotConfigured,
    NeedsCredentials,
    ConfigurationInvalid,
    AuthenticationFailed,
    ProviderUnavailable,
    ModelUnavailable,
    PartiallyCompatible,
    ConnectedAndVerified,
    Disconnected,
    RequiresAttention,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageOutcome {
    Pass,
    Warn,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: u8,
    pub name: String,
    pub outcome: StageOutcome,
    /// Sanitized, user-facing explanation with remediation where relevant.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub started_at: u64,
    pub finished_at: u64,
    pub stages: Vec<StageResult>,
    pub status: ConnectionStatus,
}

/// How the app reaches the provider (COMPATIBILITY-MATRIX §C paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// The app talks to the provider itself (DIRECT-CONFIG).
    #[default]
    Direct,
    /// Through PrismFlow's broker + translation engine (GATEWAY-A / GATEWAY-O).
    Gateway,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionRecord {
    pub id: String,
    pub provider_id: String,
    pub app_id: String,
    pub model: String,
    /// Profile name: `codex -p <profile>`, or the settings-file stem for
    /// `claude --settings`.
    pub profile: String,
    /// Absent in slice-1 records, which were all direct.
    #[serde(default)]
    pub route: Route,
    /// The PrismFlow-owned file this connection created.
    pub managed_file: PathBuf,
    pub created_at: u64,
    pub status: ConnectionStatus,
    pub last_report: Option<VerificationReport>,
}

/// Resolve the state directory: `PRISMFLOW_STATE_DIR` (tests, portable
/// installs) or the platform data dir + bundle identifier.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PRISMFLOW_STATE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("app.prismflow.desktop")
}

pub struct StateStore {
    dir: PathBuf,
}

impl StateStore {
    pub fn open(dir: &Path) -> Result<Self, ConfigError> {
        for d in [dir.to_path_buf(), dir.join("backups")] {
            fs::create_dir_all(&d)
                .map_err(|e| ConfigError::Io { file: d.clone(), detail: e.to_string() })?;
        }
        Ok(Self { dir: dir.to_path_buf() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.dir.join("backups")
    }

    fn connections_file(&self) -> PathBuf {
        self.dir.join("connections.json")
    }

    pub fn load(&self) -> Result<Vec<ConnectionRecord>, ConfigError> {
        let file = self.connections_file();
        match fs::read_to_string(&file) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| ConfigError::Parse { file, detail: e.to_string() }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(ConfigError::Io { file, detail: e.to_string() }),
        }
    }

    pub fn save(&self, records: &[ConnectionRecord]) -> Result<(), ConfigError> {
        let file = self.connections_file();
        let json = serde_json::to_string_pretty(records)
            .map_err(|e| ConfigError::Io { file: file.clone(), detail: e.to_string() })?;
        atomic_write(&file, &json)
    }

    /// Insert or replace by id.
    pub fn upsert(&self, record: ConnectionRecord) -> Result<(), ConfigError> {
        let mut all = self.load()?;
        all.retain(|r| r.id != record.id);
        all.push(record);
        self.save(&all)
    }

    pub fn remove(&self, id: &str) -> Result<(), ConfigError> {
        let mut all = self.load()?;
        all.retain(|r| r.id != id);
        self.save(&all)
    }

    pub fn load_custom_providers(&self) -> Result<Vec<crate::providers::CustomProvider>, ConfigError> {
        let file = self.dir.join("custom_providers.json");
        match fs::read_to_string(&file) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| ConfigError::Parse { file, detail: e.to_string() }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(ConfigError::Io { file, detail: e.to_string() }),
        }
    }

    pub fn save_custom_providers(&self, list: &[crate::providers::CustomProvider]) -> Result<(), ConfigError> {
        let file = self.dir.join("custom_providers.json");
        let json = serde_json::to_string_pretty(list)
            .map_err(|e| ConfigError::Io { file: file.clone(), detail: e.to_string() })?;
        atomic_write(&file, &json)
    }

    pub fn get(&self, id: &str) -> Result<Option<ConnectionRecord>, ConfigError> {
        Ok(self.load()?.into_iter().find(|r| r.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> ConnectionRecord {
        ConnectionRecord {
            id: id.into(),
            provider_id: "openrouter".into(),
            app_id: "codex_cli".into(),
            model: "openai/gpt-oss-20b".into(),
            profile: "prismflow-openrouter".into(),
            route: Route::Direct,
            managed_file: PathBuf::from("/tmp/x.config.toml"),
            created_at: 1,
            status: ConnectionStatus::Unverified,
            last_report: None,
        }
    }

    #[test]
    fn slice_1_records_load_as_direct() {
        let json = r#"[{"id":"codex_cli:openrouter","provider_id":"openrouter","app_id":"codex_cli","model":"m",
            "profile":"prismflow-openrouter","managed_file":"/tmp/x","created_at":1,"status":"Unverified","last_report":null}]"#;
        let records: Vec<ConnectionRecord> = serde_json::from_str(json).unwrap();
        assert_eq!(records[0].route, Route::Direct);
    }

    #[test]
    fn empty_state_loads_as_no_connections() {
        let dir = tempfile::tempdir().unwrap();
        assert!(StateStore::open(dir.path()).unwrap().load().unwrap().is_empty());
    }

    #[test]
    fn upsert_get_remove_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateStore::open(dir.path()).unwrap();
        s.upsert(record("a")).unwrap();
        s.upsert(record("b")).unwrap();
        let mut changed = record("a");
        changed.status = ConnectionStatus::ConnectedAndVerified;
        s.upsert(changed).unwrap();

        let reopened = StateStore::open(dir.path()).unwrap();
        assert_eq!(reopened.load().unwrap().len(), 2);
        assert_eq!(
            reopened.get("a").unwrap().unwrap().status,
            ConnectionStatus::ConnectedAndVerified
        );
        reopened.remove("a").unwrap();
        assert!(reopened.get("a").unwrap().is_none());
    }

    #[test]
    fn corrupt_state_is_a_parse_error_not_a_silent_reset() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("connections.json"), "{nope").unwrap();
        let s = StateStore::open(dir.path()).unwrap();
        assert!(matches!(s.load(), Err(ConfigError::Parse { .. })));
    }
}
