use super::{CredentialError, CredentialStore, SERVICE_NAME};

pub struct KeychainStore;

impl CredentialStore for KeychainStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(SERVICE_NAME, account)
            .map_err(|e| CredentialError::Store(e.to_string()))?;
        entry.set_password(secret).map_err(|e| CredentialError::Store(e.to_string()))
    }

    fn get(&self, account: &str) -> Result<String, CredentialError> {
        let entry = keyring::Entry::new(SERVICE_NAME, account)
            .map_err(|e| CredentialError::Store(e.to_string()))?;
        entry.get_password().map_err(|e| match e {
            keyring::Error::NoEntry => CredentialError::NotFound(account.to_string()),
            other => CredentialError::Store(other.to_string()),
        })
    }

    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(SERVICE_NAME, account)
            .map_err(|e| CredentialError::Store(e.to_string()))?;
        entry.delete_credential().map_err(|e| match e {
            keyring::Error::NoEntry => CredentialError::NotFound(account.to_string()),
            other => CredentialError::Store(other.to_string()),
        })
    }
}
