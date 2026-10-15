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
    // Muse Code's model catalog (answered here, see `muse_models`).
    ("GET", "/muse-code/models"),
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

/// Muse Code reads `/muse-code/models` (without `/v1`) before any request:
/// OpenAI-style rows plus `metadata["muse-code"]` (shape verified with
/// Muse Code 1.4.8 on 2026-10-15). The connection's own model is listed.
fn muse_models(binding: &Binding) -> Response {
    let model = if binding.model.is_empty() { "prismflow" } else { binding.model.as_str() };
    Json(json!({ "object": "list", "data": [{
        "id": model, "object": "model", "created": 0, "owned_by": "prismflow",
        "metadata": { "muse-code": { "name": model, "is_hidden": false, "reasoning": false,
            "limit": { "context": 32768, "output": 4096 } } }
    }]})).into_response()
}

/// The Claude model names Claude Desktop shows, like another tool's list. Each name
/// routes to the model set for its slot (Fable/Opus/Sonnet/Haiku), or to
/// the connection's own model when that slot isn't set.
pub const CLAUDE_DESKTOP_MODELS: &[(&str, &str, &str)] = &[
    ("claude-fable-5", "Claude Fable 5", "2026-06-09T00:00:00Z"),
    ("claude-opus-4-20250514", "Claude Opus 4", "2025-05-14T00:00:00Z"),
    ("claude-sonnet-4-20250514", "Claude Sonnet 4", "2025-05-14T00:00:00Z"),
    ("claude-haiku-4-20250514", "Claude Haiku 4", "2025-05-14T00:00:00Z"),
    ("claude-3-opus-20240229", "Claude 3 Opus", "2024-02-29T00:00:00Z"),
    ("claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet", "2024-10-22T00:00:00Z"),
    ("claude-3-haiku-20240307", "Claude 3 Haiku", "2024-03-07T00:00:00Z"),
    ("claude-3-5-haiku-20241022", "Claude 3.5 Haiku", "2024-10-22T00:00:00Z"),
];

fn claude_desktop_models(_binding: &Binding) -> Response {
    let data: Vec<serde_json::Value> = CLAUDE_DESKTOP_MODELS.iter().map(|(id, name, created)| json!({
        "type": "model", "object": "model", "id": id, "display_name": name, "created_at": created, "owned_by": "prismflow",
    })).collect();
    let (first, last) = (CLAUDE_DESKTOP_MODELS[0].0, CLAUDE_DESKTOP_MODELS[CLAUDE_DESKTOP_MODELS.len() - 1].0);
    Json(json!({ "object": "list", "data": data, "has_more": false, "first_id": first, "last_id": last })).into_response()
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
    if !inner.admit(&binding.connection_id) {
        inner.count(|c| c.rejected += 1);
        inner.record(Level::Warn, "broker.rate_limited", &format!("{}: over the per-minute request limit.", binding.connection_id));
        return reject(StatusCode::TOO_MANY_REQUESTS, "rate_limit_error", "this connection is over its per-minute request limit (Settings)");
    }
    if path == "/muse-code/models" {
        return muse_models(&binding);
    }
    if path == "/v1/models" && binding.connection_id.starts_with("claude_desktop:") {
        return claude_desktop_models(&binding);
    }
    let upstream = match inner.upstream() {
        Ok(u) => u,
        Err(phase) => {
            inner.count(|c| c.upstream_errors += 1);
            return reject(StatusCode::SERVICE_UNAVAILABLE, "overloaded_error", &format!("the gateway engine is {phase:?}; retry shortly"));
        }
    };

    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let mut base_headers = HeaderMap::new();
    for (name, value) in req.headers() {
        let n = name.as_str();
        if STRIP_REQUEST.contains(&n) || n.starts_with("x-target-") || n.starts_with("x-prismflow-") {
            continue;
        }
        base_headers.append(name.clone(), value.clone());
    }
    let auth = HeaderValue::from_str(&format!("Bearer {}", upstream.client_key)).expect("hex key is a valid header");
    base_headers.insert(header::AUTHORIZATION, auth);

    let body: Bytes = match axum::body::to_bytes(req.into_body(), BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => {
            inner.count(|c| c.rejected += 1);
            return reject(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large", "request body exceeds 50 MiB");
        }
    };

    // Routing (tiers, reasoning, fallbacks) applies to JSON POST bodies.
    let front = crate::routing::Front::of(&path);
    let mut json: Option<serde_json::Value> =
        (method == axum::http::Method::POST).then(|| serde_json::from_slice(&body).ok()).flatten();
    let requested = json.as_ref().and_then(|j| j["model"].as_str()).map(str::to_string);
    let primary = crate::routing::ModelRef::new(
        &binding.provider_id,
        if binding.model.is_empty() { requested.as_deref().unwrap_or("") } else { &binding.model },
    );
    let (tier, chain) = binding.routing.plan(&primary, requested.as_deref());
    let effort = binding.routing.effort_for(tier);
    if let Some(j) = json.as_mut() {
        crate::routing::apply_effort(j, front, effort);
    }
    let body = match &json {
        Some(j) if effort != crate::routing::Effort::FromClient => Bytes::from(serde_json::to_vec(j).unwrap_or_default()),
        _ => body,
    };

    let url = format!("http://127.0.0.1:{}{path}{query}", upstream.port);

    // Claude Code's background requests (titles, suggestions): answered here.
    let shortcuts_on = inner.settings.read().unwrap_or_else(|e| e.into_inner()).shortcuts;
    if shortcuts_on && front == crate::routing::Front::Messages && path == "/v1/messages" {
        if let Some((j, s)) = json.as_ref().and_then(|j| super::shortcuts::detect(j).map(|s| (j, s))) {
            inner.count(|c| c.shortcuts += 1);
            let (sse, text) = super::shortcuts::respond(j, &s);
            let kind = if sse { "text/event-stream" } else { "application/json" };
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, kind)
                .header("x-prismflow-shortcut", s.kind())
                .body(Body::from(text))
                .unwrap_or_else(|_| reject(StatusCode::INTERNAL_SERVER_ERROR, "api_error", "shortcut response"));
        }
    }

    // Local web tools: Anthropic's server-side WebSearch/WebFetch, run here.
    let web_on = inner.settings.read().unwrap_or_else(|e| e.into_inner()).web_tools.enabled;
    if web_on && front == crate::routing::Front::Messages {
        if let Some(j) = json.as_ref().filter(|j| super::webloop::wants_web_tools(j)) {
            let target = chain[0].clone();
            let model = if !binding.model.is_empty() || tier != crate::routing::Tier::Default { target.model.clone() } else { String::new() };
            let headers = super::webloop::engine_headers(&base_headers, &target.provider_id, &model);
            let client = inner.client.clone();
            let counter = inner.clone();
            let engine_url = url.clone();
            let send = move |b: serde_json::Value| {
                let (client, headers, engine_url, counter) = (client.clone(), headers.clone(), engine_url.clone(), counter.clone());
                async move {
                    counter.count(|c| c.requests += 1);
                    let r = client.post(&engine_url).headers(headers).json(&b).send().await
                        .map_err(|_| "the gateway engine is unreachable".to_string())?;
                    let status = r.status().as_u16();
                    Ok((status, r.text().await.map_err(|e| e.to_string())?))
                }
            };
            return super::webloop::run(&inner, &binding.connection_id, j.clone(), send).await;
        }
    }
    let mut last_error: Option<Response> = None;
    let mut resp = None;
    for (attempt, target) in chain.iter().enumerate() {
        let mut headers = base_headers.clone();
        let Ok(provider) = HeaderValue::from_str(&target.provider_id) else { continue };
        headers.insert(HeaderName::from_static("x-target-provider"), provider);
        // Only rewrite the model when routing chose it; otherwise the body's
        // own model stands (unchanged slice-2 behaviour).
        if !binding.model.is_empty() || attempt > 0 || tier != crate::routing::Tier::Default {
            if let Ok(m) = HeaderValue::from_str(&target.model) {
                if !target.model.is_empty() {
                    headers.insert(HeaderName::from_static("x-target-model"), m);
                }
            }
        }
        let sent = inner.client.request(method.clone(), &url).headers(headers).body(body.clone()).send().await;
        inner.count(|c| c.requests += 1);
        match sent {
            Ok(r) if !crate::routing::should_fall_back(r.status().as_u16()) || attempt + 1 == chain.len() => {
                if attempt > 0 {
                    inner.record(Level::Warn, "broker.fallback", &format!(
                        "{} {method} {path}: served by {}/{} after {attempt} failed attempt(s).",
                        binding.connection_id, target.provider_id, target.model
                    ));
                }
                resp = Some((r, attempt, target.clone()));
                break;
            }
            Ok(r) => {
                inner.count(|c| c.upstream_errors += 1);
                inner.record(Level::Warn, "broker.fallback_next", &format!(
                    "{} {method} {path}: {}/{} answered {}; trying the next fallback.",
                    binding.connection_id, target.provider_id, target.model, r.status().as_u16()
                ));
            }
            Err(e) => {
                inner.count(|c| c.upstream_errors += 1);
                let msg = format!("{} {method} {path}: gateway engine unreachable ({})", binding.connection_id,
                    if e.is_timeout() { "timeout" } else if e.is_connect() { "connect" } else { "transport" });
                inner.record(Level::Error, "broker.engine_unreachable", &msg);
                last_error = Some(reject(StatusCode::BAD_GATEWAY, "api_error", "the gateway engine is unreachable"));
            }
        }
    }
    let Some((resp, attempts, served_by)) = resp else {
        return last_error.unwrap_or_else(|| reject(StatusCode::BAD_GATEWAY, "api_error", "no route could serve this request"));
    };
    let status = resp.status();
    if !status.is_success() {
        inner.count(|c| c.upstream_errors += 1);
        inner.record(Level::Warn, "broker.request", &format!(
            "{} {method} {path} → {} after {} ms", binding.connection_id, status.as_u16(), started.elapsed().as_millis()
        ));
    }
    let mut out = Response::builder().status(status);
    if let Ok(v) = HeaderValue::from_str(&format!("{}/{}", served_by.provider_id, served_by.model)) {
        out = out.header("x-prismflow-served-by", v);
    }
    if attempts > 0 {
        out = out.header("x-prismflow-fallback-count", attempts.to_string());
    }
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
