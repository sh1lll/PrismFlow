use super::{CredentialError, CredentialStore, SERVICE_NAME};

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct KeychainStore;

/// Keychain calls in flight, and when the latest one started (ms since epoch).
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static STARTED_MS: AtomicU64 = AtomicU64::new(0);
/// A normal Keychain read takes milliseconds; longer means macOS is asking.
const PROMPT_AFTER_MS: u64 = 1500;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Marks a Keychain call as in flight for the life of the guard.
struct Pending;

impl Pending {
    fn start() -> Self {
        if IN_FLIGHT.fetch_add(1, Ordering::SeqCst) == 0 {
            STARTED_MS.store(now_ms(), Ordering::SeqCst);
        }
        Pending
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// True while a Keychain call has been waiting long enough that macOS is
/// most likely showing its "allow access" window.
pub fn waiting() -> bool {
    IN_FLIGHT.load(Ordering::SeqCst) > 0 && now_ms().saturating_sub(STARTED_MS.load(Ordering::SeqCst)) > PROMPT_AFTER_MS
}

impl CredentialStore for KeychainStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        let _pending = Pending::start();
        let entry = keyring::Entry::new(SERVICE_NAME, account)
            .map_err(|e| CredentialError::Store(e.to_string()))?;
        entry.set_password(secret).map_err(|e| CredentialError::Store(e.to_string()))
    }

    fn get(&self, account: &str) -> Result<String, CredentialError> {
        let _pending = Pending::start();
        let entry = keyring::Entry::new(SERVICE_NAME, account)
            .map_err(|e| CredentialError::Store(e.to_string()))?;
        entry.get_password().map_err(|e| match e {
            keyring::Error::NoEntry => CredentialError::NotFound(account.to_string()),
            other => CredentialError::Store(other.to_string()),
        })
    }

    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        let _pending = Pending::start();
        let entry = keyring::Entry::new(SERVICE_NAME, account)
            .map_err(|e| CredentialError::Store(e.to_string()))?;
        entry.delete_credential().map_err(|e| match e {
            keyring::Error::NoEntry => CredentialError::NotFound(account.to_string()),
            other => CredentialError::Store(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_only_after_a_slow_call() {
        assert!(!waiting());
        let p = Pending::start();
        assert!(!waiting(), "a fresh call is not a prompt yet");
        STARTED_MS.store(now_ms() - PROMPT_AFTER_MS - 10, Ordering::SeqCst);
        assert!(waiting());
        drop(p);
        assert!(!waiting());
    }
}
