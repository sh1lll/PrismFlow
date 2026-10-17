//! "Sign in with OpenRouter": OpenRouter's documented OAuth PKCE flow for
//! apps (https://openrouter.ai/docs/guides/overview/auth/oauth, checked
//! 2026-10-15; localhost callbacks are allowed on any port).
//!
//! 1. Listen once on 127.0.0.1 (random port) for the callback.
//! 2. Open the user's browser at `/auth?callback_url=…&code_challenge=…&state=…`.
//! 3. The user approves; OpenRouter redirects to the callback with `code`.
//! 4. Exchange `code` + verifier at `/api/v1/auth/keys` for a normal API key,
//!    which the caller stores in the Keychain like a pasted key.
//!
//! Only the official, provider-sanctioned flow is used here. Sign-ins that
//! reuse another tool's private login (Claude, Copilot, Gemini CLI) are not.

use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const OPENROUTER_AUTH: &str = "https://openrouter.ai/auth";
pub const OPENROUTER_EXCHANGE: &str = "https://openrouter.ai/api/v1/auth/keys";
/// OpenRouter's codes expire after 10 minutes; give up a little before.
const WAIT: Duration = Duration::from_secs(9 * 60);

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn random(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("system randomness");
    base64url(&buf)
}

/// (verifier, S256 challenge)
pub fn pkce() -> (String, String) {
    let verifier = random(48);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn query_param(target: &str, name: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(v) => { out.push(v); i += 3; continue; }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

const PAGE_OK: &str = "<!doctype html><meta charset=utf-8><title>PrismFlow</title><body style=\"font:16px system-ui;padding:40px\"><h2>Signed in.</h2><p>You can close this tab and go back to PrismFlow.</p>";
const PAGE_FAIL: &str = "<!doctype html><meta charset=utf-8><title>PrismFlow</title><body style=\"font:16px system-ui;padding:40px\"><h2>Sign-in didn't finish.</h2><p>Go back to PrismFlow and try again.</p>";

/// Runs the whole flow and returns the new OpenRouter key.
/// `open` is called once with the sign-in page address.
pub async fn openrouter_sign_in(
    client: &reqwest::Client,
    auth_url: &str,
    exchange_url: &str,
    open: impl FnOnce(&str) -> Result<(), String>,
) -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| format!("Couldn't start sign-in: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let (verifier, challenge) = pkce();
    let state = random(18);
    let callback = format!("http://localhost:{port}/callback");
    let url = format!(
        "{auth_url}?callback_url={}&code_challenge={challenge}&code_challenge_method=S256&state={state}&key_label=PrismFlow",
        encode(&callback)
    );
    open(&url)?;

    let code = tokio::time::timeout(WAIT, async {
        loop {
            let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("").to_string();
            if !target.starts_with("/callback") {
                let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                continue;
            }
            let ok = query_param(&target, "state").as_deref() == Some(state.as_str());
            let code = query_param(&target, "code").filter(|_| ok);
            let page = if code.is_some() { PAGE_OK } else { PAGE_FAIL };
            let _ = sock
                .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).as_bytes())
                .await;
            return code.ok_or_else(|| "Sign-in was cancelled or didn't match. Try again.".to_string());
        }
    })
    .await
    .map_err(|_| "Sign-in timed out. Try again.".to_string())??;

    let r = client
        .post(exchange_url)
        .json(&serde_json::json!({ "code": code, "code_verifier": verifier, "code_challenge_method": "S256" }))
        .send()
        .await
        .map_err(|e| format!("Couldn't reach OpenRouter: {e}"))?;
    let status = r.status();
    let body: serde_json::Value = r.json().await.unwrap_or_default();
    match body["key"].as_str() {
        Some(k) if status.is_success() && !k.trim().is_empty() => Ok(k.trim().to_string()),
        _ => Err(format!("OpenRouter didn't give a key (HTTP {}). Try again.", status.as_u16())),
    }
}

/// Opens an address in the user's default browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = crate::spawn::command("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = crate::spawn::command("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = crate::spawn::command("xdg-open");
    cmd.arg(url).status().map_err(|e| format!("Couldn't open your browser: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn base64url_matches_known_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        // RFC 7636 appendix B
        let c = base64url(&Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert_eq!(c, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn decodes_query_values() {
        assert_eq!(query_param("/callback?code=a%2Fb&state=x", "code").as_deref(), Some("a/b"));
        assert_eq!(query_param("/callback?state=x", "code"), None);
    }

    async fn run(browser_state: Option<&'static str>) -> Result<String, String> {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/api/v1/auth/keys"))
            .and(body_partial_json(serde_json::json!({ "code": "the-code", "code_challenge_method": "S256" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "key": "sk-or-v1-test" })))
            .mount(&server).await;
        let client = reqwest::Client::new();
        let exchange = format!("{}/api/v1/auth/keys", server.uri());
        openrouter_sign_in(&client, "https://openrouter.ai/auth", &exchange, |url| {
            // Play the browser: follow the redirect OpenRouter would send.
            let cb = percent_decode(&query_param(url, "callback_url").unwrap());
            let state = browser_state.map(str::to_string).unwrap_or_else(|| query_param(url, "state").unwrap());
            assert!(cb.starts_with("http://localhost:") && url.contains("code_challenge_method=S256"));
            tokio::spawn(async move {
                let _ = reqwest::get(format!("{cb}?code=the-code&state={state}")).await;
            });
            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn full_flow_returns_the_key() {
        assert_eq!(run(None).await.unwrap(), "sk-or-v1-test");
    }

    #[tokio::test]
    async fn wrong_state_is_refused() {
        assert!(run(Some("forged")).await.is_err());
    }
}
