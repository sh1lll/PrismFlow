//! Local broker tokens. One token per gateway
//! connection: 32 random bytes, stored only in the secure store, handed to
//! agents at runtime by `prismflow token print`. The broker keeps SHA-256
//! digests, never the tokens themselves.

use crate::credentials::{account_key, CredentialError, CredentialStore};
use sha2::{Digest, Sha256};

/// Prefix that makes tokens recognizable to redaction and to humans.
pub const TOKEN_PREFIX: &str = "pft_";
const TOKEN_BYTES: usize = 32;

pub type TokenDigest = [u8; 32];

pub fn account(connection_id: &str) -> String {
    account_key("local_token", connection_id)
}

/// A fresh token: `pft_` + 64 hex chars (256 bits from the OS CSPRNG).
pub fn generate() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("the OS random number generator must be available");
    let mut out = String::with_capacity(TOKEN_PREFIX.len() + TOKEN_BYTES * 2);
    out.push_str(TOKEN_PREFIX);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn digest(token: &str) -> TokenDigest {
    Sha256::digest(token.as_bytes()).into()
}

/// Create (or replace) the connection's token. Replacing revokes the old one
/// as soon as the broker reloads its bindings.
pub fn mint(store: &dyn CredentialStore, connection_id: &str) -> Result<String, CredentialError> {
    let token = generate();
    store.set(&account(connection_id), &token)?;
    Ok(token)
}

/// The stored token, minting one if the connection has none yet.
pub fn ensure(store: &dyn CredentialStore, connection_id: &str) -> Result<String, CredentialError> {
    match store.get(&account(connection_id)) {
        Ok(t) => Ok(t),
        Err(CredentialError::NotFound(_)) => mint(store, connection_id),
        Err(e) => Err(e),
    }
}

/// Delete the token. Absent tokens are not an error.
pub fn revoke(store: &dyn CredentialStore, connection_id: &str) -> Result<(), CredentialError> {
    match store.delete(&account(connection_id)) {
        Ok(()) | Err(CredentialError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::memory::InMemoryStore;

    #[test]
    fn tokens_are_prefixed_hex_and_unique() {
        let a = generate();
        let b = generate();
        assert_ne!(a, b);
        assert_eq!(a.len(), TOKEN_PREFIX.len() + 64);
        assert!(a.starts_with(TOKEN_PREFIX));
        assert!(a[TOKEN_PREFIX.len()..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn tokens_are_redacted_in_diagnostics() {
        let t = generate();
        let shown = crate::diagnostics::redact(&format!("token {t} rejected"));
        assert!(!shown.contains(&t[8..]), "{shown}");
    }

    #[test]
    fn ensure_is_stable_mint_rotates_revoke_is_idempotent() {
        let store = InMemoryStore::new();
        let first = ensure(&store, "claude_code:nvidia_nim").unwrap();
        assert_eq!(ensure(&store, "claude_code:nvidia_nim").unwrap(), first);
        let rotated = mint(&store, "claude_code:nvidia_nim").unwrap();
        assert_ne!(rotated, first);
        assert_eq!(store.get(&account("claude_code:nvidia_nim")).unwrap(), rotated);
        revoke(&store, "claude_code:nvidia_nim").unwrap();
        revoke(&store, "claude_code:nvidia_nim").unwrap();
        assert!(store.get(&account("claude_code:nvidia_nim")).is_err());
    }

    #[test]
    fn digest_is_deterministic_and_distinct() {
        assert_eq!(digest("pft_a"), digest("pft_a"));
        assert_ne!(digest("pft_a"), digest("pft_b"));
    }
}
