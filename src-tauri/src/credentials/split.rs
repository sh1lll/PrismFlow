//! Where keys live. Provider keys (and sign-ins) stay in the system's
//! secure store and are read once per run. Each app's local PrismFlow key
//! lives in a private file instead: apps ask for it every few minutes
//! (Claude Code's key helper refreshes every 5), and on macOS each read
//! from the Keychain by a new, unsigned build pops a password prompt. Those
//! keys only work against PrismFlow's own gateway on this computer, and
//! several apps already keep them in their own settings files.

use super::file::FileStore;
use super::{CredentialError, CredentialStore};
use std::collections::HashMap;
use std::sync::Mutex;

const LOCAL_PREFIX: &str = "prismflow:local_token:";

fn is_local(account: &str) -> bool {
    account.starts_with(LOCAL_PREFIX)
}

/// Remembers what the secure store said, so screens don't re-read it (each
/// read can ask for permission).
pub struct Cached<S: CredentialStore> {
    inner: S,
    seen: Mutex<HashMap<String, Option<String>>>,
}

impl<S: CredentialStore> Cached<S> {
    pub fn new(inner: S) -> Self {
        Self { inner, seen: Mutex::new(HashMap::new()) }
    }
}

impl<S: CredentialStore> CredentialStore for Cached<S> {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        self.inner.set(account, secret)?;
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(account.into(), Some(secret.into()));
        Ok(())
    }
    fn get(&self, account: &str) -> Result<String, CredentialError> {
        if let Some(v) = self.seen.lock().unwrap_or_else(|e| e.into_inner()).get(account) {
            return v.clone().ok_or_else(|| CredentialError::NotFound(account.into()));
        }
        let r = self.inner.get(account);
        // Remember found and absent; a store error (locked) is asked again later.
        match &r {
            Ok(v) => { self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(account.into(), Some(v.clone())); }
            Err(CredentialError::NotFound(_)) => { self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(account.into(), None); }
            Err(_) => {}
        }
        r
    }
    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        let r = self.inner.delete(account);
        if matches!(r, Ok(()) | Err(CredentialError::NotFound(_))) {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(account.into(), None);
        }
        r
    }
}

/// App keys in `local`, everything else in `keys`.
pub struct SplitStore {
    pub keys: Box<dyn CredentialStore>,
    pub local: FileStore,
}

impl CredentialStore for SplitStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        if is_local(account) { self.local.set(account, secret) } else { self.keys.set(account, secret) }
    }
    fn get(&self, account: &str) -> Result<String, CredentialError> {
        if !is_local(account) {
            return self.keys.get(account);
        }
        match self.local.get(account) {
            Err(CredentialError::NotFound(_)) => {
                // Saved by an earlier version: move it to the file, once.
                let v = self.keys.get(account)?;
                let _ = self.local.set(account, &v);
                Ok(v)
            }
            other => other,
        }
    }
    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        if !is_local(account) {
            return self.keys.delete(account);
        }
        let file = self.local.delete(account);
        // An old copy left in the secure store would bring a removed key back.
        let old = self.keys.delete(account);
        match (file, old) {
            (Ok(()), _) | (_, Ok(())) => Ok(()),
            (Err(e), _) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::memory::InMemoryStore;

    #[test]
    fn app_keys_go_to_the_file_and_old_ones_move_over() {
        let dir = tempfile::tempdir().unwrap();
        let keys = InMemoryStore::new();
        keys.set("prismflow:local_token:claude_code:groq", "pft_old").unwrap();
        let s = SplitStore { keys: Box::new(keys), local: FileStore::new(dir.path().join("app-keys.json")) };
        // Provider keys stay in the secure store.
        s.set("prismflow:groq:default", "gsk_x").unwrap();
        assert!(!std::fs::read_to_string(dir.path().join("app-keys.json")).unwrap_or_default().contains("gsk_x"));
        // An app key saved by an earlier version moves to the file once.
        assert_eq!(s.get("prismflow:local_token:claude_code:groq").unwrap(), "pft_old");
        assert!(std::fs::read_to_string(dir.path().join("app-keys.json")).unwrap().contains("pft_old"));
        // Removing it removes both copies, so it can't come back.
        s.delete("prismflow:local_token:claude_code:groq").unwrap();
        assert!(matches!(s.get("prismflow:local_token:claude_code:groq"), Err(CredentialError::NotFound(_))));
        // New app keys never touch the secure store.
        s.set("prismflow:local_token:codex_cli:groq", "pft_new").unwrap();
        assert!(matches!(s.keys.get("prismflow:local_token:codex_cli:groq"), Err(CredentialError::NotFound(_))));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = FileStore::new(dir.path().join("app-keys.json"));
        f.set("prismflow:local_token:x", "pft").unwrap();
        let mode = std::fs::metadata(dir.path().join("app-keys.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    struct Counting(InMemoryStore, std::sync::atomic::AtomicUsize);
    impl CredentialStore for Counting {
        fn set(&self, a: &str, s: &str) -> Result<(), CredentialError> { self.0.set(a, s) }
        fn get(&self, a: &str) -> Result<String, CredentialError> { self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst); self.0.get(a) }
        fn delete(&self, a: &str) -> Result<(), CredentialError> { self.0.delete(a) }
    }

    #[test]
    fn secure_store_reads_are_remembered() {
        let inner = Counting(InMemoryStore::new(), Default::default());
        inner.0.set("k", "v").unwrap();
        let c = Cached::new(inner);
        for _ in 0..5 {
            assert_eq!(c.get("k").unwrap(), "v");
            assert!(c.get("missing").is_err());
        }
        assert_eq!(c.inner.1.load(std::sync::atomic::Ordering::SeqCst), 2, "one read each");
        c.set("k", "w").unwrap();
        assert_eq!(c.get("k").unwrap(), "w");
    }
}
