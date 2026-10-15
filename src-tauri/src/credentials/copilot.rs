//! GitHub Copilot through GitHub's official Copilot CLI.
//!
//! GitHub's Copilot SDK talks to the Copilot CLI over JSON-RPC (stdio,
//! `Content-Length` framing). Its `session.provider.getEndpoint` call
//! "returns the provider endpoint and credentials the session is currently
//! configured to talk to, so the caller can make inference calls directly"
//! (SDK 1.0.22 schema; marked experimental, enabled with
//! `COPILOT_ALLOW_GET_PROVIDER_ENDPOINT=true`). PrismFlow uses the user's own
//! `copilot login`; it never handles GitHub credentials itself. The returned
//! token is short-lived, so the gateway refreshes it before it expires.
//! Checked 2026-10-15 against Copilot CLI 1.0.98 (protocol 3) up to the
//! sign-in check.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub base_url: String,
    /// `openai_chat_completions`, `openai_responses` or `anthropic_messages`.
    pub engine_type: &'static str,
    pub api_key: String,
    pub headers: Vec<(String, String)>,
    pub expires_at: Option<SystemTime>,
}

pub fn find_cli() -> Option<PathBuf> {
    std::env::var_os("PRISMFLOW_COPILOT_BIN").map(PathBuf::from).or_else(|| crate::adapters::find_exe("copilot"))
}

pub fn frame(v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

pub fn read_frame(r: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse::<usize>().ok();
            }
        }
    }
    let mut buf = vec![0u8; len?];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Maps a `ProviderEndpoint` result to what the gateway engine needs.
pub fn parse_endpoint(v: &Value) -> Result<Endpoint, String> {
    let base_url = v["baseUrl"].as_str().ok_or("Copilot didn't return an address.")?.trim_end_matches('/').to_string();
    let kind = v["type"].as_str().unwrap_or("openai").to_ascii_lowercase();
    let wire = v["wireApi"].as_str().unwrap_or("completions").to_ascii_lowercase();
    let engine_type = if kind.contains("anthropic") {
        "anthropic_messages"
    } else if wire.contains("response") {
        "openai_responses"
    } else {
        "openai_chat_completions"
    };
    let mut headers: Vec<(String, String)> = v["headers"].as_object()
        .map(|h| h.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    let mut api_key = v["apiKey"].as_str().unwrap_or("").to_string();
    let mut expires_at = None;
    if let Some(t) = v.get("sessionToken").filter(|t| t.is_object()) {
        let token = t["token"].as_str().unwrap_or("").to_string();
        let header = t["header"].as_str().unwrap_or("Authorization").to_string();
        if header.eq_ignore_ascii_case("authorization") && api_key.is_empty() {
            api_key = token.trim_start_matches("Bearer ").to_string();
        } else {
            headers.push((header, token));
        }
        expires_at = t["expiresAt"].as_str().and_then(parse_rfc3339);
    }
    if api_key.is_empty() {
        api_key = "copilot".into(); // the session token header carries the credential
    }
    Ok(Endpoint { base_url, engine_type, api_key, headers, expires_at })
}

/// Seconds-precision RFC 3339 (`2026-10-15T16:32:45Z` / `.651Z`), UTC only.
fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let s = s.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let d: Vec<i64> = date.split('-').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let t: Vec<f64> = time.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let (y, m, day) = (d[0], d[1], d[2]);
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + (t[0] as i64) * 3600 + (t[1] as i64) * 60 + t.get(2).copied().unwrap_or(0.0) as i64;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64))
}

struct Cli {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Cli {
    fn start() -> Result<Self, String> {
        let bin = find_cli().ok_or("Install GitHub's Copilot CLI and run `copilot login` first.")?;
        let mut child = Command::new(bin)
            .args(["--headless", "--no-auto-update", "--stdio"])
            .env("COPILOT_ALLOW_GET_PROVIDER_ENDPOINT", "true")
            // Its own login only, never a token from the environment.
            .env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN").env_remove("COPILOT_GITHUB_TOKEN")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Couldn't start the Copilot CLI: {e}"))?;
        let out = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(Self { child, out, next: 0 })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let stdin = self.child.stdin.as_mut().ok_or("no stdin")?;
        stdin.write_all(&frame(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        loop {
            let msg = read_frame(&mut self.out).ok_or("The Copilot CLI stopped.")?;
            if msg["id"] == id && msg.get("method").is_none() {
                if let Some(e) = msg.get("error") {
                    return Err(e["message"].as_str().unwrap_or("error").to_string());
                }
                return Ok(msg["result"].clone());
            }
            // Requests from the CLI (permissions, UI): decline politely.
            if msg.get("id").is_some() && msg.get("method").is_some() {
                let stdin = self.child.stdin.as_mut().ok_or("no stdin")?;
                let _ = stdin.write_all(&frame(&json!({ "jsonrpc": "2.0", "id": msg["id"], "result": null })));
            }
        }
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs `f` with a deadline; the CLI is killed if it hangs.
fn with_timeout<T: Send + 'static>(secs: u64, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).map_err(|_| "The Copilot CLI didn't answer in time.".to_string())?
}

static SIGNED_IN: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
static EXPIRY: Mutex<Option<SystemTime>> = Mutex::new(None);

/// Is the user signed in to Copilot in the CLI? (Cached for a minute.)
pub fn signed_in() -> bool {
    if let Some((at, v)) = *SIGNED_IN.lock().unwrap_or_else(|e| e.into_inner()) {
        if at.elapsed() < Duration::from_secs(60) {
            return v;
        }
    }
    let v = find_cli().is_some()
        && with_timeout(20, || {
            let mut cli = Cli::start()?;
            cli.call("ping", json!({ "message": "prismflow" }))?;
            Ok(cli.call("auth.getStatus", json!({}))?["isAuthenticated"] == true)
        })
        .unwrap_or(false);
    *SIGNED_IN.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v));
    v
}

/// A fresh endpoint for `model` (Copilot binds tokens to a model).
pub fn endpoint(model: Option<&str>) -> Result<Endpoint, String> {
    let model = model.map(String::from);
    let ep = with_timeout(60, move || {
        let mut cli = Cli::start()?;
        let status = cli.call("auth.getStatus", json!({}))?;
        if status["isAuthenticated"] != true {
            return Err("Sign in to GitHub Copilot first: run `copilot login`.".into());
        }
        let mut b = [0u8; 6];
        getrandom::fill(&mut b).map_err(|e| e.to_string())?;
        let sid = format!("prismflow-{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>());
        let mut params = json!({ "sessionId": sid, "clientName": "prismflow" });
        if let Some(m) = &model {
            params["model"] = json!(m);
        }
        cli.call("session.create", params)?;
        let mut req = json!({ "sessionId": sid });
        if let Some(m) = &model {
            req["modelId"] = json!(m);
        }
        let r = cli.call("session.provider.getEndpoint", req)?;
        let _ = cli.call("session.delete", json!({ "sessionId": sid }));
        parse_endpoint(&r)
    })?;
    let mut e = EXPIRY.lock().unwrap_or_else(|e| e.into_inner());
    *e = match (*e, ep.expires_at) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    Ok(ep)
}

/// True when a Copilot token the gateway holds expires within `margin`.
pub fn expiring_within(margin: Duration) -> bool {
    EXPIRY.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t <= SystemTime::now() + margin)
}

/// Forget the recorded expiry (before a refresh fetches new tokens).
pub fn reset_expiry() {
    *EXPIRY.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let bytes = frame(&json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}));
        let v = read_frame(&mut BufReader::new(&bytes[..])).unwrap();
        assert_eq!(v["result"]["ok"], true);
    }

    #[test]
    fn endpoint_shapes() {
        let e = parse_endpoint(&json!({"type": "openai", "wireApi": "responses", "baseUrl": "https://api.githubcopilot.com/",
            "headers": {"Copilot-Integration-Id": "x"}, "sessionToken": {"token": "tid=abc", "header": "Authorization", "expiresAt": "2026-10-15T16:32:45.651Z"}})).unwrap();
        assert_eq!(e.engine_type, "openai_responses");
        assert_eq!(e.base_url, "https://api.githubcopilot.com");
        assert_eq!(e.api_key, "tid=abc");
        assert_eq!(e.headers, vec![("Copilot-Integration-Id".into(), "x".into())]);
        assert_eq!(e.expires_at.unwrap().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(), 1_792_081_965);
        let a = parse_endpoint(&json!({"type": "anthropic", "baseUrl": "https://x", "apiKey": "k", "headers": {},
            "sessionToken": {"token": "s", "header": "X-Session"}})).unwrap();
        assert_eq!(a.engine_type, "anthropic_messages");
        assert_eq!(a.headers, vec![("X-Session".into(), "s".into())]);
    }

    /// Real Copilot CLI: without a Copilot sign-in, a clear message.
    /// PRISMFLOW_COPILOT_BIN=<copilot> HOME=<empty dir> cargo test copilot_live -- --ignored
    #[test]
    #[ignore]
    fn copilot_live_without_sign_in() {
        let e = endpoint(Some("gpt-4.1")).unwrap_err();
        eprintln!("{e}");
        assert!(e.contains("copilot login"), "{e}");
    }
}
