use super::{CredentialError, CredentialStore};
use std::collections::HashMap;
use std::sync::Mutex;

pub struct InMemoryStore {
    inner: Mutex<HashMap<String, String>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }
}

impl CredentialStore for InMemoryStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        self.inner.lock().unwrap().insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<String, CredentialError> {
        self.inner
            .lock()
            .unwrap()
            .get(account)
            .cloned()
            .ok_or_else(|| CredentialError::NotFound(account.to_string()))
    }

    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        self.inner
            .lock()
            .unwrap()
            .remove(account)
            .map(|_| ())
            .ok_or_else(|| CredentialError::NotFound(account.to_string()))
    }
}
