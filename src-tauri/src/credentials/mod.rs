pub mod keychain;
#[cfg(debug_assertions)]
pub mod devfile;
pub mod memory;
pub mod copilot;
pub mod oauth;
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

/// What to tell the user when the system's key store isn't working (locked,
/// turned off, or missing), as opposed to a key simply not being there.
pub fn store_problem(e: &CredentialError) -> String {
    match e {
        CredentialError::NotFound(_) => "That key isn't saved anymore.".into(),
        CredentialError::Store(_) => {
            let fix = if cfg!(target_os = "macos") {
                "If your Mac asked for a password, enter it and click Always Allow, then try again."
            } else if cfg!(windows) {
                "Make sure you're signed in to Windows normally, then try again."
            } else {
                "Unlock your keyring (for example GNOME Keyring or KWallet), or install one, then try again."
            };
            format!("PrismFlow can't use your computer's password store right now (it may be locked or turned off). {fix}")
        }
    }
}

pub fn account_key(provider_id: &str, account_name: &str) -> String {
    format!("prismflow:{}:{}", provider_id, account_name)
}

pub fn default_store() -> Box<dyn CredentialStore> {
    // Debug builds can keep keys in a sandbox file (never in release builds).
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("PRISMFLOW_DEV_KEY_FILE") {
        return Box::new(devfile::DevFileStore::new(p.into()));
    }
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
