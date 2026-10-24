//! Keys in a JSON file only the user's account can read. Used for each
//! app's local PrismFlow key (see `split.rs`) and, in debug builds, for all
//! keys when `PRISMFLOW_DEV_KEY_FILE` is set (sandboxed click-throughs).

use super::{CredentialError, CredentialStore};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: Mutex::new(()) }
    }
    fn load(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(&self.path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save(&self, m: &BTreeMap<String, String>) -> Result<(), CredentialError> {
        crate::config_engine::atomic_write(&self.path, &serde_json::to_string(m).unwrap_or_default())
            .map_err(|e| CredentialError::Store(e.to_string()))
    }
}

impl CredentialStore for FileStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut m = self.load();
        m.insert(account.into(), secret.into());
        self.save(&m)
    }
    fn get(&self, account: &str) -> Result<String, CredentialError> {
        self.load().get(account).cloned().ok_or_else(|| CredentialError::NotFound(account.into()))
    }
    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut m = self.load();
        m.remove(account).ok_or_else(|| CredentialError::NotFound(account.into()))?;
        self.save(&m)
    }
}
