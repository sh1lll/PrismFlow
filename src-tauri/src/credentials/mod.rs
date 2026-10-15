pub mod keychain;
pub mod memory;
pub mod signin;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("no credential stored for account {0}")]
    NotFound(String),
    #[error("secure store error: {0}")]
    Store(String),
}

pub trait CredentialStore: Send + Sync {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError>;
    fn get(&self, account: &str) -> Result<String, CredentialError>;
    fn delete(&self, account: &str) -> Result<(), CredentialError>;
}

pub const SERVICE_NAME: &str = "app.prismflow.desktop";

pub fn account_key(provider_id: &str, account_name: &str) -> String {
    format!("prismflow:{}:{}", provider_id, account_name)
}

pub fn default_store() -> Box<dyn CredentialStore> {
    Box::new(keychain::KeychainStore)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(store: &dyn CredentialStore) {
        // Dedicated self-test account: the live variant must never touch a real key.
        let acct = account_key("prismflow_selftest", "roundtrip");
        assert!(matches!(store.get(&acct), Err(CredentialError::NotFound(_))));
        store.set(&acct, "sk-or-test-123").unwrap();
        assert_eq!(store.get(&acct).unwrap(), "sk-or-test-123");
        store.set(&acct, "sk-or-test-456").unwrap(); // overwrite
        assert_eq!(store.get(&acct).unwrap(), "sk-or-test-456");
        store.delete(&acct).unwrap();
        assert!(matches!(store.get(&acct), Err(CredentialError::NotFound(_))));
        // delete of absent is idempotent-style NotFound, not a crash
        assert!(matches!(store.delete(&acct), Err(CredentialError::NotFound(_))));
    }

    #[test]
    fn memory_store_roundtrip() {
        roundtrip(&memory::InMemoryStore::new());
    }

    /// Runs against the real macOS Keychain. `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn keychain_roundtrip_live() {
        roundtrip(&keychain::KeychainStore);
    }

    #[test]
    fn account_key_format() {
        assert_eq!(account_key("openrouter", "default"), "prismflow:openrouter:default");
    }
}
