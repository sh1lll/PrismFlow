//! "Sign in with ChatGPT" run by PrismFlow itself: the same browser sign-in
//! Codex performs (OpenAI's OAuth with PKCE, Codex's public client), so a
//! ChatGPT plan works without Codex installed. PrismFlow keeps the tokens in
//! the system's secure store and renews the access token before it expires
//! (OpenAI replaces the refresh token on each renewal; the new one is kept).
//!
//! The other way, reusing the sign-in Codex already saved, is in signin.rs.

use super::oauth::{await_code, encode, pkce, random};
use super::{CredentialError, CredentialStore};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::net::TcpListener;

pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Codex's public OAuth client (the one OpenAI accepts for ChatGPT plans).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const SCOPE: &str = "openid profile email offline_access";
/// The only callback address registered for that client.
pub const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const WAIT: Duration = Duration::from_secs(9 * 60);
/// Where the tokens live in the secure store.
pub const ACCOUNT: &str = "prismflow:chatgpt:oauth";
/// Renew this long before the access token expires.
pub const RENEW_BEFORE_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub account_id: Option<String>,
    /// Unix seconds; 0 when unknown.
    #[serde(default)]
    pub expires_at: u64,
}

/// The claims in a JWT (unverified: only used to read expiry and account).
pub fn claims(jwt: &str) -> serde_json::Value {
    jwt.split('.').nth(1)
        .and_then(|p| {
            let p = p.replace('-', "+").replace('_', "/");
            let pad = (4 - p.len() % 4) % 4;
            base64_decode(&format!("{p}{}", "=".repeat(pad)))
        })
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = A.iter().position(|a| *a == c)? as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits & 0xff) as u8);
        }
    }
    Some(out)
}

/// Expiry (unix seconds) of an access token, 0 when it doesn't say.
pub fn expires_at(access_token: &str) -> u64 {
    claims(access_token)["exp"].as_u64().unwrap_or(0)
}

/// The ChatGPT account the token belongs to (sent as `chatgpt-account-id`).
pub fn account_id(token: &str) -> Option<String> {
    claims(token)["https://api.openai.com/auth"]["chatgpt_account_id"].as_str().map(String::from)
}

fn tokens_from(body: &serde_json::Value, old_refresh: Option<&str>) -> Result<Tokens, String> {
    let access = body["access_token"].as_str().filter(|t| !t.is_empty()).ok_or("OpenAI didn't send a sign-in token. Try again.")?;
    let refresh = body["refresh_token"].as_str().filter(|t| !t.is_empty()).or(old_refresh)
        .ok_or("OpenAI didn't send a renewal token. Try again.")?;
    let account = body["id_token"].as_str().and_then(account_id).or_else(|| account_id(access));
    Ok(Tokens { access_token: access.into(), refresh_token: refresh.into(), account_id: account, expires_at: expires_at(access) })
}

/// Opens the browser at OpenAI's sign-in page and waits for it to come back.
pub async fn sign_in(
    client: &reqwest::Client,
    authorize_url: &str,
    token_url: &str,
    port: u16,
    open: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Tokens, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|_| {
        format!("Another sign-in is using port {port} (often Codex's own sign-in). Finish or close it, then try again.")
    })?;
    let (verifier, challenge) = pkce();
    let state = random(24);
    let redirect = format!("http://localhost:{port}{CALLBACK_PATH}");
    let url = format!(
        "{authorize_url}?response_type=code&client_id={CLIENT_ID}&redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state={state}&originator=codex_cli_rs",
        encode(&redirect), encode(SCOPE)
    );
    open(&url)?;
    let code = await_code(&listener, CALLBACK_PATH, &state, WAIT).await?;
    let r = client.post(token_url)
        .form(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect), ("client_id", CLIENT_ID), ("code_verifier", &verifier)])
        .send().await
        .map_err(|e| crate::discovery::unreachable_text(&e))?;
    let status = r.status();
    let body: serde_json::Value = r.json().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("OpenAI didn't accept the sign-in (HTTP {}). Try again.", status.as_u16()));
    }
    tokens_from(&body, None)
}

/// A fresh access token from the refresh token.
pub async fn renew(client: &reqwest::Client, token_url: &str, t: &Tokens) -> Result<Tokens, String> {
    let r = client.post(token_url)
        .json(&serde_json::json!({ "client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": t.refresh_token, "scope": "openid profile email" }))
        .send().await
        .map_err(|e| crate::discovery::unreachable_text(&e))?;
    let status = r.status();
    let body: serde_json::Value = r.json().await.unwrap_or_default();
    if status.as_u16() == 400 || status.as_u16() == 401 {
        return Err("Your ChatGPT sign-in ended. Sign in again.".into());
    }
    if !status.is_success() {
        return Err(format!("OpenAI couldn't renew the sign-in right now (HTTP {}).", status.as_u16()));
    }
    let mut new = tokens_from(&body, Some(&t.refresh_token))?;
    new.account_id = new.account_id.or_else(|| t.account_id.clone());
    Ok(new)
}

pub fn load(store: &dyn CredentialStore) -> Result<Option<Tokens>, CredentialError> {
    match store.get(ACCOUNT) {
        Ok(s) => Ok(serde_json::from_str(&s).ok()),
        Err(CredentialError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn save(store: &dyn CredentialStore, t: &Tokens) -> Result<(), CredentialError> {
    store.set(ACCOUNT, &serde_json::to_string(t).unwrap_or_default())
}

pub fn forget(store: &dyn CredentialStore) -> Result<(), CredentialError> {
    match store.delete(ACCOUNT) {
        Ok(()) | Err(CredentialError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn needs_renewal(t: &Tokens, now: u64) -> bool {
    t.expires_at != 0 && t.expires_at <= now + RENEW_BEFORE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::memory::InMemoryStore;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// An unsigned JWT with these claims (only the payload is read).
    fn jwt(claims: serde_json::Value) -> String {
        let b = crate::credentials::oauth::base64url(claims.to_string().as_bytes());
        format!("e30.{b}.sig")
    }

    #[test]
    fn reads_expiry_and_account_from_tokens() {
        let t = jwt(serde_json::json!({ "exp": 1_900_000_000u64, "https://api.openai.com/auth": { "chatgpt_account_id": "acc-1" } }));
        assert_eq!(expires_at(&t), 1_900_000_000);
        assert_eq!(account_id(&t).as_deref(), Some("acc-1"));
        assert_eq!(expires_at("not a jwt"), 0);
    }

    #[tokio::test]
    async fn signs_in_through_the_browser_and_renews() {
        let server = MockServer::start().await;
        let access = jwt(serde_json::json!({ "exp": 1_900_000_000u64 }));
        let id = jwt(serde_json::json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acc-9" } }));
        Mock::given(method("POST")).and(path("/oauth/token")).and(body_string_contains("grant_type=authorization_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "access_token": access, "refresh_token": "rt-1", "id_token": id })))
            .mount(&server).await;
        let renewed = jwt(serde_json::json!({ "exp": 1_950_000_000u64 }));
        Mock::given(method("POST")).and(path("/oauth/token")).and(body_string_contains("refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "access_token": renewed, "refresh_token": "rt-2" })))
            .mount(&server).await;

        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let client = reqwest::Client::new();
        let token_url = format!("{}/oauth/token", server.uri());
        let open = |url: &str| -> Result<(), String> {
            assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann") && url.contains("code_challenge_method=S256"), "{url}");
            let state = url.split("state=").nth(1).unwrap().split('&').next().unwrap().to_string();
            // The browser coming back after the user signed in.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = reqwest::get(format!("http://127.0.0.1:{port}/auth/callback?code=c-1&state={state}")).await;
            });
            Ok(())
        };
        let t = sign_in(&client, "https://auth.example/authorize", &token_url, port, open).await.unwrap();
        assert_eq!((t.refresh_token.as_str(), t.account_id.as_deref(), t.expires_at), ("rt-1", Some("acc-9"), 1_900_000_000));

        let store = InMemoryStore::new();
        save(&store, &t).unwrap();
        let back = load(&store).unwrap().unwrap();
        assert_eq!(back, t);
        assert!(needs_renewal(&back, 1_900_000_000 - 3600));
        assert!(!needs_renewal(&back, 1_800_000_000));

        let r = renew(&client, &token_url, &back).await.unwrap();
        assert_eq!((r.refresh_token.as_str(), r.expires_at, r.account_id.as_deref()), ("rt-2", 1_950_000_000, Some("acc-9")));
        forget(&store).unwrap();
        assert!(load(&store).unwrap().is_none());
    }

    #[tokio::test]
    async fn an_ended_sign_in_says_sign_in_again() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({ "error": "invalid_grant" }))).mount(&server).await;
        let t = Tokens { access_token: "a".into(), refresh_token: "r".into(), account_id: None, expires_at: 1 };
        let e = renew(&reqwest::Client::new(), &format!("{}/oauth/token", server.uri()), &t).await.unwrap_err();
        assert!(e.contains("Sign in again"), "{e}");
    }
}
