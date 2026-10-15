//! Sign-ins the user already made with a vendor's own tool, read at the
//! moment of use and never stored by PrismFlow (ADR-018 guardrails).
//!
//! - GitHub: `gh auth token` (the GitHub CLI's official sign-in).
//! - ChatGPT: the access token `codex login` saved in `$CODEX_HOME/auth.json`.
//!   Only the access token is used, never the refresh token: refresh tokens
//!   rotate, and refreshing from PrismFlow would silently log the user's
//!   Codex out. When it expires, using Codex refreshes it.

use std::path::Path;

/// A Google Cloud access token from the user's own `gcloud` sign-in (Vertex
/// AI's documented way; tokens last one hour).
pub fn gcloud_token() -> Option<String> {
    let gcloud = crate::adapters::find_exe("gcloud")?;
    let out = std::process::Command::new(gcloud).args(["auth", "print-access-token", "--quiet"]).stdin(std::process::Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty() && !t.contains(char::is_whitespace)).then_some(t)
}

pub fn gh_token() -> Option<String> {
    let gh = crate::adapters::find_exe("gh")?;
    let out = std::process::Command::new(gh).args(["auth", "token"]).stdin(std::process::Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty() && !t.contains(char::is_whitespace)).then_some(t)
}

/// The ChatGPT access token from a Codex sign-in, if it is a ChatGPT
/// (not API-key) login.
pub fn codex_access_token(codex_home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(codex_home.join("auth.json")).ok()?;
    access_token_from(&raw)
}

pub fn access_token_from(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let t = v.get("tokens")?.get("access_token")?.as_str()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_the_access_token() {
        let raw = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"i","access_token":"at-123","refresh_token":"rt-456","account_id":"acc"}}"#;
        assert_eq!(access_token_from(raw).as_deref(), Some("at-123"));
        assert_eq!(access_token_from(r#"{"OPENAI_API_KEY":"sk-x"}"#), None, "API-key logins are not ChatGPT sign-ins");
        assert_eq!(access_token_from("not json"), None);
    }

    #[test]
    fn missing_auth_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(codex_access_token(dir.path()).is_none());
    }
}
