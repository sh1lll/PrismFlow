//! Debug builds only: keys in a JSON file instead of the system store, so
//! the real app can be clicked through in a sandbox without touching the
//! user's Keychain. Switched on by `PRISMFLOW_DEV_KEY_FILE=<path>`.

use super::{CredentialError, CredentialStore};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct DevFileStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl DevFileStore {
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

impl CredentialStore for DevFileStore {
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
