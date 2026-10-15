//! The broker's HTTP front (loopback only).
//!
//! Every proxied request must pass four checks:
//! 1. Host is the loopback address and our port (blocks DNS rebinding).
//! 2. No `Origin` header, so browsers and web pages are refused.
//! 3. Method and path are on the allow-list (engine admin/agent/MCP routes are unreachable).
//! 4. A valid local token in `x-api-key` or `Authorization: Bearer`.
//!
//! Then the client's credentials and any routing headers are stripped. The
//! engine's internal key and `x-target-provider` from the token binding are
//! set, and the response streams back unchanged. Logs record connection,
//! method, path, status and latency. Headers, query strings and bodies are never logged.

use super::{Binding, Inner, Phase};
use crate::diagnostics::Level;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const SERVICE_ID: &str = "prismflow-broker";
/// Matches the engine's own default request limit (50 MiB).
const BODY_LIMIT: usize = 50 * 1024 * 1024;

const ALLOWED: &[(&str, &str)] = &[
    ("POST", "/v1/messages"),
    ("POST", "/v1/messages/count_tokens"),
    ("POST", "/v1/chat/completions"),
    ("POST", "/v1/responses"),
    ("GET", "/v1/models"),
];

/// Hop-by-hop and credential headers that never cross the broker.
const STRIP_REQUEST: &[&str] = &[
    "host", "authorization", "x-api-key", "connection", "keep-alive", "proxy-authorization",
    "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "content-length", "cookie",
];
const STRIP_RESPONSE: &[&str] = &["connection", "keep-alive", "transfer-encoding", "trailer", "upgrade", "set-cookie"];

pub(crate) fn router(inner: Arc<Inner>) -> Router {
    Router::new()
        .route("/prismflow/health", get(health))
        .route("/prismflow/ready", get(ready))
        .fallback(proxy)
        .with_state(inner)
}

async fn health() -> impl IntoResponse {
    Json(json!({ "service": SERVICE_ID, "version": env!("CARGO_PKG_VERSION") }))
}

async fn ready(State(inner): State<Arc<Inner>>) -> Response {
    match inner.upstream() {
        Ok(_) => (StatusCode::OK, Json(json!({ "ready": true, "phase": Phase::Ready }))).into_response(),
        Err(phase) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "ready": false, "phase": phase }))).into_response(),
    }
}

/// Used on a port conflict: is the program on `port` another PrismFlow broker?
pub(crate) async fn is_prismflow_broker(client: &reqwest::Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/prismflow/health");
    let Ok(r) = client.get(url).timeout(Duration::from_secs(1)).send().await else { return false };
    r.json::<serde_json::Value>().await.ok().is_some_and(|v| v["service"] == SERVICE_ID)
}

/// Error body readable by both Anthropic and OpenAI clients (`error.message`).
fn reject(status: StatusCode, kind: &str, message: &str) -> Response {
    let body = json!({
        "type": "error",
        "error": { "type": kind, "message": format!("PrismFlow broker: {message}") }
    });
    (status, Json(body)).into_response()
}

pub(crate) fn host_allowed(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

fn presented_token(headers: &HeaderMap) -> Vec<&str> {
    let mut out = Vec::new();
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        out.push(v.trim());
    }
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            out.push(t.trim());
        }
    }
    out
}

async fn proxy(State(inner): State<Arc<Inner>>, req: Request) -> Response {
    let started = Instant::now();
    let port = *inner.port.lock().unwrap_or_else(|e| e.into_inner());
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !host_allowed(host, port) {
        inner.count(|c| c.rejected += 1);
        return reject(StatusCode::FORBIDDEN, "permission_error", "requests must address 127.0.0.1 directly");
    }
    if req.headers().contains_key(header::ORIGIN) {
        inner.count(|c| c.rejected += 1);
        return reject(StatusCode::FORBIDDEN, "permission_error", "browser requests are not accepted");
    }
    if !ALLOWED.iter().any(|(m, p)| method.as_str() == *m && path == *p) {
        inner.count(|c| c.rejected += 1);
        inner.record(Level::Info, "broker.not_exposed", &format!("Refused {method} {path} (not on the allow-list)."));
        return reject(StatusCode::NOT_FOUND, "not_found_error", "this endpoint is not exposed");
    }
    let binding: Option<Binding> = presented_token(req.headers()).into_iter().find_map(|t| inner.lookup(t));
    let Some(binding) = binding else {
        inner.count(|c| c.rejected += 1);
        inner.record(Level::Warn, "broker.unauthorized", &format!("Rejected {method} {path}: missing, unknown or revoked local token."));
        return reject(StatusCode::UNAUTHORIZED, "authentication_error", "missing, unknown or revoked local token; reconnect this app in PrismFlow");
    };
    let upstream = match inner.upstream() {
        Ok(u) => u,
        Err(phase) => {
            inner.count(|c| c.upstream_errors += 1);
            return reject(StatusCode::SERVICE_UNAVAILABLE, "overloaded_error", &format!("the gateway engine is {phase:?}; retry shortly"));
        }
    };

    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let mut headers = HeaderMap::new();
    for (name, value) in req.headers() {
        let n = name.as_str();
        if STRIP_REQUEST.contains(&n) || n.starts_with("x-target-") || n.starts_with("x-prismflow-") {
            continue;
        }
        headers.append(name.clone(), value.clone());
    }
    let auth = HeaderValue::from_str(&format!("Bearer {}", upstream.client_key)).expect("hex key is a valid header");
    headers.insert(header::AUTHORIZATION, auth);
    headers.insert(
        HeaderName::from_static("x-target-provider"),
        HeaderValue::from_str(&binding.provider_id).expect("provider ids are ASCII"),
    );

    let body: Bytes = match axum::body::to_bytes(req.into_body(), BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => {
            inner.count(|c| c.rejected += 1);
            return reject(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large", "request body exceeds 50 MiB");
        }
    };
    let url = format!("http://127.0.0.1:{}{path}{query}", upstream.port);
    let sent = inner.client.request(method.clone(), url).headers(headers).body(body).send().await;
    inner.count(|c| c.requests += 1);
    let resp = match sent {
        Ok(r) => r,
        Err(e) => {
            inner.count(|c| c.upstream_errors += 1);
            let msg = format!("{} {method} {path}: gateway engine unreachable ({})", binding.connection_id,
                if e.is_timeout() { "timeout" } else if e.is_connect() { "connect" } else { "transport" });
            inner.record(Level::Error, "broker.engine_unreachable", &msg);
            return reject(StatusCode::BAD_GATEWAY, "api_error", "the gateway engine is unreachable");
        }
    };
    let status = resp.status();
    if !status.is_success() {
        inner.count(|c| c.upstream_errors += 1);
        inner.record(Level::Warn, "broker.request", &format!(
            "{} {method} {path} → {} after {} ms", binding.connection_id, status.as_u16(), started.elapsed().as_millis()
        ));
    }
    let mut out = Response::builder().status(status);
    for (name, value) in resp.headers() {
        if !STRIP_RESPONSE.contains(&name.as_str()) {
            out = out.header(name, value);
        }
    }
    out.body(Body::from_stream(resp.bytes_stream())).unwrap_or_else(|_| {
        reject(StatusCode::BAD_GATEWAY, "api_error", "malformed response from the gateway engine")
    })
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn host_check_accepts_only_loopback_names_with_our_port() {
        assert!(host_allowed("127.0.0.1:47821", 47821));
        assert!(host_allowed("localhost:47821", 47821));
        assert!(!host_allowed("127.0.0.1:1", 47821));
        assert!(!host_allowed("evil.example:47821", 47821));
        assert!(!host_allowed("", 47821));
    }

    #[test]
    fn tokens_are_read_from_both_header_styles() {
        let mut h = HeaderMap::new();
        h.insert("x-api-key", HeaderValue::from_static("pft_a"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer pft_b"));
        assert_eq!(presented_token(&h), vec!["pft_a", "pft_b"]);
    }
}
