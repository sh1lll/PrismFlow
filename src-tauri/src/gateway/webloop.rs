//! Local WebSearch/WebFetch for the Anthropic front (ADR-018).
//!
//! Claude Code's WebSearch is an Anthropic *server* tool: Anthropic runs it.
//! Other providers cannot, so when web tools are on, the broker swaps the
//! server tools for ordinary function tools, runs the model non-streaming,
//! executes `web_search` / `web_fetch` calls itself, and continues the
//! conversation until the model answers (at most MAX_ROUNDS). The final
//! message is returned as JSON or re-emitted as Anthropic SSE.

use super::Inner;
use crate::diagnostics::Level;
use crate::webtools;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::sync::Arc;

pub const MAX_ROUNDS: usize = 5;

fn is_server_web_tool(t: &Value) -> Option<&'static str> {
    let ty = t["type"].as_str()?;
    if ty.starts_with("web_search_") {
        Some("web_search")
    } else if ty.starts_with("web_fetch_") {
        Some("web_fetch")
    } else {
        None
    }
}

/// Does this Messages request ask for Anthropic's server-side web tools?
pub fn wants_web_tools(body: &Value) -> bool {
    body["tools"].as_array().is_some_and(|ts| ts.iter().any(|t| is_server_web_tool(t).is_some()))
}

/// Replace server web tools with local function tools.
pub fn rewrite_tools(body: &mut Value) {
    let Some(tools) = body["tools"].as_array_mut() else { return };
    for t in tools.iter_mut() {
        match is_server_web_tool(t) {
            Some("web_search") => *t = json!({
                "name": "web_search",
                "description": "Search the web. Returns titles, URLs and snippets of the top results.",
                "input_schema": {"type": "object", "properties": {"query": {"type": "string", "description": "Search query"}}, "required": ["query"]}
            }),
            Some(_) => *t = json!({
                "name": "web_fetch",
                "description": "Fetch a public web page and return its text.",
                "input_schema": {"type": "object", "properties": {"url": {"type": "string", "description": "http(s) URL"}}, "required": ["url"]}
            }),
            None => {}
        }
    }
}

async fn run_tool(inner: &Inner, name: &str, input: &Value) -> (String, bool) {
    let settings = inner.settings.read().unwrap_or_else(|e| e.into_inner()).web_tools.clone();
    let key = inner.web_key.read().unwrap_or_else(|e| e.into_inner()).clone();
    let res = match name {
        "web_search" => webtools::search(&settings, key.as_deref(), input["query"].as_str().unwrap_or_default()).await.map(|rs| {
            rs.iter().enumerate().map(|(i, r)| format!("{}. {}\n{}\n{}", i + 1, r.title, r.url, r.snippet)).collect::<Vec<_>>().join("\n\n")
        }),
        _ => webtools::fetch(input["url"].as_str().unwrap_or_default(), settings.allow_private).await,
    };
    match res {
        Ok(text) if text.is_empty() => ("No results.".into(), false),
        Ok(text) => (text, false),
        Err(e) => (format!("Error: {e}"), true),
    }
}

/// Re-emit a complete Messages response as Anthropic SSE.
pub fn to_sse(msg: &Value) -> String {
    let mut out = String::new();
    let mut ev = |name: &str, data: Value| out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    let mut start = msg.clone();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    ev("message_start", json!({"type": "message_start", "message": start}));
    let mut idx = 0;
    for block in msg["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => {
                ev("content_block_start", json!({"type": "content_block_start", "index": idx, "content_block": {"type": "text", "text": ""}}));
                ev("content_block_delta", json!({"type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": block["text"]}}));
            }
            Some("tool_use") => {
                ev("content_block_start", json!({"type": "content_block_start", "index": idx,
                    "content_block": {"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}}));
                ev("content_block_delta", json!({"type": "content_block_delta", "index": idx,
                    "delta": {"type": "input_json_delta", "partial_json": block["input"].to_string()}}));
            }
            _ => continue,
        }
        ev("content_block_stop", json!({"type": "content_block_stop", "index": idx}));
        idx += 1;
    }
    ev("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": msg["stop_reason"], "stop_sequence": Value::Null},
        "usage": {"output_tokens": msg["usage"]["output_tokens"].as_u64().unwrap_or(0)}}));
    ev("message_stop", json!({"type": "message_stop"}));
    out
}

/// Run the request with local web tools. `send` posts one non-streaming
/// Messages body to the engine and returns (status, JSON or raw text).
pub async fn run<F, Fut>(inner: &Arc<Inner>, connection: &str, mut body: Value, send: F) -> Response
where
    F: Fn(Value) -> Fut,
    Fut: std::future::Future<Output = Result<(u16, String), String>>,
{
    let stream = body["stream"].as_bool() == Some(true);
    body["stream"] = json!(false);
    rewrite_tools(&mut body);
    for round in 0..=MAX_ROUNDS {
        let (status, text) = match send(body.clone()).await {
            Ok(r) => r,
            Err(e) => return (StatusCode::BAD_GATEWAY, axum::Json(json!({"type": "error", "error": {"type": "api_error", "message": e}}))).into_response(),
        };
        if !(200..300).contains(&status) {
            return Response::builder().status(status).header("content-type", "application/json").body(Body::from(text)).unwrap_or_default();
        }
        let Ok(mut msg) = serde_json::from_str::<Value>(&text) else {
            return (StatusCode::BAD_GATEWAY, "malformed response from the gateway engine").into_response();
        };
        let blocks: Vec<Value> = msg["content"].as_array().cloned().unwrap_or_default();
        let uses: Vec<&Value> = blocks.iter().filter(|b| b["type"] == "tool_use").collect();
        let web: Vec<&Value> = uses.iter().copied().filter(|b| matches!(b["name"].as_str(), Some("web_search" | "web_fetch"))).collect();
        let finished = web.is_empty() || round == MAX_ROUNDS;
        if finished || web.len() != uses.len() {
            // Mixed turns: the client's own tools win; drop our tool calls.
            if !web.is_empty() {
                let kept: Vec<Value> = blocks.into_iter().filter(|b| !(b["type"] == "tool_use" && matches!(b["name"].as_str(), Some("web_search" | "web_fetch")))).collect();
                msg["content"] = json!(kept);
            }
            return if stream {
                Response::builder().header("content-type", "text/event-stream").body(Body::from(to_sse(&msg))).unwrap_or_default()
            } else {
                axum::Json(msg).into_response()
            };
        }
        let mut results = Vec::new();
        for u in web {
            let name = u["name"].as_str().unwrap_or_default();
            let (out, is_error) = run_tool(inner, name, &u["input"]).await;
            inner.record(Level::Info, "broker.web_tool", &format!("{connection}: ran local {name} ({} chars{}).", out.len(), if is_error { ", error" } else { "" }));
            results.push(json!({"type": "tool_result", "tool_use_id": u["id"], "content": out, "is_error": is_error}));
        }
        if let Some(msgs) = body["messages"].as_array_mut() {
            msgs.push(json!({"role": "assistant", "content": blocks}));
            msgs.push(json!({"role": "user", "content": results}));
        }
    }
    (StatusCode::BAD_GATEWAY, "web tool loop did not finish").into_response()
}

/// Header map for one engine call (shared with the main proxy path).
pub fn engine_headers(base: &HeaderMap, provider: &str, model: &str) -> HeaderMap {
    let mut h = base.clone();
    h.remove("content-length");
    if let Ok(p) = HeaderValue::from_str(provider) {
        h.insert(HeaderName::from_static("x-target-provider"), p);
    }
    if let Ok(m) = HeaderValue::from_str(model) {
        if !model.is_empty() {
            h.insert(HeaderName::from_static("x-target-model"), m);
        }
    }
    h.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_only_server_web_tools() {
        let mut b = json!({"tools": [
            {"type": "web_search_20250305", "name": "web_search", "max_uses": 5},
            {"name": "Read", "input_schema": {"type": "object"}},
            {"type": "web_fetch_20250910", "name": "web_fetch"}
        ]});
        assert!(wants_web_tools(&b));
        rewrite_tools(&mut b);
        assert_eq!(b["tools"][0]["input_schema"]["required"][0], "query");
        assert_eq!(b["tools"][1]["name"], "Read");
        assert_eq!(b["tools"][2]["name"], "web_fetch");
        assert!(!wants_web_tools(&b));
    }

    #[test]
    fn sse_reemission_is_well_formed() {
        let msg = json!({"id": "m", "type": "message", "role": "assistant", "model": "x",
            "content": [{"type": "text", "text": "Hi"}, {"type": "tool_use", "id": "t1", "name": "Read", "input": {"path": "a"}}],
            "stop_reason": "tool_use", "usage": {"input_tokens": 1, "output_tokens": 2}});
        let sse = to_sse(&msg);
        for needle in ["event: message_start", "\"text_delta\"", "\"Hi\"", "input_json_delta", "\\\"path\\\"", "\"stop_reason\":\"tool_use\"", "event: message_stop"] {
            assert!(sse.contains(needle), "missing {needle}:\n{sse}");
        }
    }
}
