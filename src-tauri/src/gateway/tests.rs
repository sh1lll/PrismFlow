//! Broker + real engine tests. The engine is the pinned ai-gateway child (run
//! `scripts/fetch-gateway.sh` once); upstream providers are wiremock servers
//! that speak OpenAI Chat Completions, including SSE streaming and tool calls.

use super::*;
use crate::diagnostics::DiagnosticsLog;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UPSTREAM_KEY: &str = "nvapi-upstreamsecret0123456789";
const MODEL: &str = "meta/llama-3.3-70b-instruct";

fn sse(chunks: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for c in chunks {
        out.push_str(&format!("data: {c}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

fn chunk(delta: serde_json::Value, finish: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": MODEL,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }]
    })
}

fn text_stream() -> String {
    sse(&[
        chunk(serde_json::json!({"role": "assistant", "content": "PRISM"}), None),
        chunk(serde_json::json!({"content": "FLOW_OK"}), None),
        chunk(serde_json::json!({}), Some("stop")),
    ])
}

fn tool_stream(tool: &str) -> String {
    sse(&[
        chunk(serde_json::json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
            "function": {"name": tool, "arguments": ""}}]}), None),
        chunk(serde_json::json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":"}}]}), None),
        chunk(serde_json::json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"a.txt\"}"}}]}), None),
        chunk(serde_json::json!({}), Some("tool_calls")),
    ])
}

/// A Chat Completions upstream that only answers when the PrismFlow-held
/// key arrives, and calls a tool whenever tools are offered.
async fn chat_upstream() -> MockServer {
    let server = MockServer::start().await;
    let auth = format!("Bearer {UPSTREAM_KEY}");
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .and(header("authorization", auth.as_str()))
        .and(body_string_contains("\"tools\""))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(tool_stream("read_file")))
        .with_priority(1)
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(text_stream()))
        .with_priority(2)
        .mount(&server).await;
    server
}

struct Harness {
    broker: Broker,
    token: String,
    _dir: tempfile::TempDir,
    diag_path: std::path::PathBuf,
    _upstream: MockServer,
}

async fn harness() -> Harness {
    if let Err(e) = GatewayRuntime::discover() {
        panic!("gateway engine unavailable for tests: {e}");
    }
    let upstream = chat_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let diag_path = dir.path().join("diagnostics.jsonl");
    let broker = Broker::new(0, dir.path().to_path_buf(), Some(Arc::new(DiagnosticsLog::new(diag_path.clone()))));
    let token = tokens::generate();
    let spec = UpstreamSpec {
        provider_id: "nvidia_nim".into(),
        engine_type: "openai_chat_completions",
        base_url: format!("{}/v1", upstream.uri()),
        models: vec![MODEL.into()],
        key: UPSTREAM_KEY.into(), codex_oauth: false };
    let binding = Binding { connection_id: "claude_code:nvidia_nim".into(), provider_id: "nvidia_nim".into(), ..Default::default() };
    let status = broker.start(vec![spec], vec![(token.clone(), binding)]).await.expect("broker starts");
    assert_eq!(status.phase, Phase::Ready);
    Harness { broker, token, _dir: dir, diag_path, _upstream: upstream }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

impl Harness {
    fn post(&self, path: &str, body: serde_json::Value) -> reqwest::RequestBuilder {
        client().post(format!("{}{path}", self.broker.url())).json(&body)
    }
}

fn messages_body(stream: bool, tools: bool) -> serde_json::Value {
    let mut b = serde_json::json!({
        "model": MODEL, "max_tokens": 64, "stream": stream,
        "messages": [{"role": "user", "content": "hi"}]
    });
    if tools {
        b["tools"] = serde_json::json!([{ "name": "read_file", "description": "Read a file",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}} }]);
    }
    b
}

#[tokio::test]
async fn anthropic_front_streams_text_from_a_chat_upstream() {
    let h = harness().await;
    let r = h.post("/v1/messages?beta=true", messages_body(true, false))
        .header("x-api-key", &h.token).header("anthropic-version", "2023-06-01")
        .send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let body = r.text().await.unwrap();
    for needle in ["event: message_start", "\"text_delta\"", "PRISM", "FLOW_OK", "\"stop_reason\":\"end_turn\"", "event: message_stop"] {
        assert!(body.contains(needle), "missing {needle} in:\n{body}");
    }
    h.broker.stop().await;
}

#[tokio::test]
async fn anthropic_front_translates_streamed_tool_calls() {
    let h = harness().await;
    let r = h.post("/v1/messages", messages_body(true, true))
        .header("authorization", format!("Bearer {}", h.token)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body = r.text().await.unwrap();
    for needle in ["\"type\":\"tool_use\"", "\"name\":\"read_file\"", "\"id\":\"call_1\"", "input_json_delta",
                   "\\\"a.txt\\\"", "\"stop_reason\":\"tool_use\""] {
        assert!(body.contains(needle), "missing {needle} in:\n{body}");
    }
    h.broker.stop().await;
}

#[tokio::test]
async fn anthropic_front_non_streaming_returns_a_message() {
    let h = harness().await;
    let v: serde_json::Value = h.post("/v1/messages", messages_body(false, false))
        .header("x-api-key", &h.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["type"], "message", "{v}");
    assert_eq!(v["content"][0]["text"], "PRISMFLOW_OK", "{v}");
    h.broker.stop().await;
}

#[tokio::test]
async fn responses_front_serves_codex_style_requests_from_a_chat_upstream() {
    let h = harness().await;
    let body = h.post("/v1/responses", serde_json::json!({"model": MODEL, "stream": true, "input": "hi"}))
        .header("authorization", format!("Bearer {}", h.token)).send().await.unwrap().text().await.unwrap();
    for needle in ["response.created", "response.output_text.delta", "PRISMFLOW_OK", "response.completed"] {
        assert!(body.contains(needle), "missing {needle} in:\n{body}");
    }
    let tools = serde_json::json!({"model": MODEL, "stream": true, "input": "read a.txt",
        "tools": [{"type": "function", "name": "read_file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}]});
    let body = h.post("/v1/responses", tools)
        .header("authorization", format!("Bearer {}", h.token)).send().await.unwrap().text().await.unwrap();
    for needle in ["\"type\":\"function_call\"", "\"name\":\"read_file\"", "response.function_call_arguments", "response.completed"] {
        assert!(body.contains(needle), "missing {needle} in:\n{body}");
    }
    h.broker.stop().await;
}

#[tokio::test]
async fn broker_rejects_bad_tokens_foreign_hosts_browsers_and_hidden_routes() {
    let h = harness().await;
    let url = h.broker.url();
    let c = client();
    let status = |r: reqwest::Response| r.status().as_u16();

    let none = c.post(format!("{url}/v1/messages")).json(&messages_body(false, false)).send().await.unwrap();
    assert_eq!(status(none), 401);
    let wrong = c.post(format!("{url}/v1/messages")).header("x-api-key", tokens::generate())
        .json(&messages_body(false, false)).send().await.unwrap();
    assert_eq!(status(wrong), 401);
    let foreign = c.post(format!("{url}/v1/messages")).header("x-api-key", &h.token).header("host", "evil.example")
        .json(&messages_body(false, false)).send().await.unwrap();
    assert_eq!(status(foreign), 403);
    let browser = c.post(format!("{url}/v1/messages")).header("x-api-key", &h.token).header("origin", "https://evil.example")
        .json(&messages_body(false, false)).send().await.unwrap();
    assert_eq!(status(browser), 403);
    for (m, p) in [("GET", "/manager/config"), ("PUT", "/manager/config"), ("POST", "/agent/sessions"), ("POST", "/mcp"), ("GET", "/v1/messages")] {
        let r = c.request(m.parse().unwrap(), format!("{url}{p}")).header("x-api-key", &h.token).send().await.unwrap();
        assert_eq!(status(r), 404, "{m} {p} must not be exposed");
    }

    // Revocation: dropping the binding makes the same token fail at once.
    h.broker.set_bindings(Vec::new());
    let revoked = c.post(format!("{url}/v1/messages")).header("x-api-key", &h.token)
        .json(&messages_body(false, false)).send().await.unwrap();
    assert_eq!(status(revoked), 401);
    let body: serde_json::Value = revoked_body(&c, &url, &h.token).await;
    assert!(body["error"]["message"].as_str().unwrap().contains("revoked"));
    h.broker.stop().await;
}

async fn revoked_body(c: &reqwest::Client, url: &str, token: &str) -> serde_json::Value {
    c.post(format!("{url}/v1/messages")).header("x-api-key", token)
        .json(&messages_body(false, false)).send().await.unwrap().json().await.unwrap()
}

#[tokio::test]
async fn engine_port_itself_requires_the_internal_key_and_hides_manager_api() {
    let h = harness().await;
    let up = h.broker.inner.upstream().expect("ready");
    let c = client();
    let direct = c.post(format!("http://127.0.0.1:{}/v1/messages", up.port))
        .header("authorization", format!("Bearer {}", h.token))
        .json(&messages_body(false, false)).send().await.unwrap();
    assert_eq!(direct.status(), 401, "a local token must not work against the engine directly");
    let manager = c.get(format!("http://127.0.0.1:{}/manager/config", up.port)).send().await.unwrap();
    assert!(matches!(manager.status().as_u16(), 401 | 403), "manager API must be locked, got {}", manager.status());
    h.broker.stop().await;
}

#[tokio::test]
async fn client_routing_headers_are_overridden_by_the_token_binding() {
    let h = harness().await;
    let r = h.post("/v1/messages", messages_body(false, false))
        .header("x-api-key", &h.token)
        .header("x-target-provider", "some-other-provider")
        .header("x-target-providers", "x,y")
        .send().await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    h.broker.stop().await;
}

#[tokio::test]
async fn unknown_model_is_reported_not_rewritten() {
    let h = harness().await;
    let mut body = messages_body(false, false);
    body["model"] = serde_json::json!("claude-haiku-4-5");
    let r = h.post("/v1/messages", body).header("x-api-key", &h.token).send().await.unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.text().await.unwrap().contains("not configured"));
    h.broker.stop().await;
}

#[tokio::test]
async fn logs_and_engine_config_never_hold_secrets() {
    let h = harness().await;
    let _ = h.post("/v1/messages", messages_body(false, false)).header("x-api-key", tokens::generate()).send().await;
    let _ = h.post("/v1/messages", messages_body(true, true)).header("x-api-key", &h.token).send().await.unwrap().text().await;
    h.broker.stop().await;
    let mut files = vec![h.diag_path.clone()];
    for e in std::fs::read_dir(h._dir.path().join("gateway")).unwrap().flatten() {
        files.push(e.path());
    }
    let diag = std::fs::read_to_string(&h.diag_path).unwrap();
    assert!(diag.contains("broker.unauthorized"), "{diag}");
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        assert!(!text.contains("upstreamsecret"), "upstream key in {}", f.display());
        assert!(!text.contains(&h.token[4..]), "local token in {}", f.display());
        assert!(!text.contains("\"hi\""), "prompt text in {}", f.display());
    }
}

#[tokio::test]
async fn port_conflicts_are_explained() {
    // A foreign program on the port.
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = squatter.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let b = Broker::new(port, dir.path().to_path_buf(), None);
    assert_eq!(b.start(Vec::new(), Vec::new()).await.unwrap_err(), BrokerError::PortInUse(port));
    assert_eq!(b.status().phase, Phase::Failed);
    drop(squatter);

    // A second PrismFlow on the port of a running one.
    let h = harness().await;
    let port = h.broker.port();
    let second = Broker::new(port, dir.path().to_path_buf(), None);
    assert_eq!(second.start(Vec::new(), Vec::new()).await.unwrap_err(), BrokerError::OtherInstance(port));
    h.broker.stop().await;
}

#[tokio::test]
async fn stop_terminates_the_engine_and_closes_the_port() {
    let h = harness().await;
    let up = h.broker.inner.upstream().unwrap();
    let ready = client().get(format!("{}/prismflow/ready", h.broker.url())).send().await.unwrap();
    assert_eq!(ready.status(), 200);
    h.broker.stop().await;
    assert_eq!(h.broker.status().phase, Phase::Stopped);
    assert!(std::net::TcpStream::connect(("127.0.0.1", up.port)).is_err(), "engine still listening");
    assert!(std::net::TcpStream::connect(("127.0.0.1", h.broker.port())).is_err(), "broker still listening");
}

#[tokio::test]
async fn crashed_engine_is_restarted() {
    let h = harness().await;
    let before = h.broker.inner.upstream().unwrap();
    // Kill the engine behind the supervisor's back.
    let pid = std::process::Command::new("lsof")
        .args(["-t", "-sTCP:LISTEN", &format!("-iTCP:{}", before.port)])
        .output().unwrap();
    let pid = String::from_utf8_lossy(&pid.stdout).trim().to_string();
    assert!(!pid.is_empty());
    std::process::Command::new("kill").args(["-9", &pid]).status().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(u) = h.broker.inner.upstream() {
            if u.port != before.port {
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline, "engine was not restarted: {:?}", h.broker.status());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(h.broker.status().restarts >= 1);
    let r = h.post("/v1/messages", messages_body(false, false)).header("x-api-key", &h.token).send().await.unwrap();
    assert_eq!(r.status(), 200);
    h.broker.stop().await;
}

// ---- Anthropic Messages as the upstream (e.g. Codex → Anthropic) ---------

const ANTHROPIC_KEY: &str = "sk-ant-upstreamsecret0123456789";
const CLAUDE_MODEL: &str = "claude-test-model";

fn anthropic_sse(events: &[serde_json::Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect()
}

fn anthropic_text_stream() -> String {
    anthropic_sse(&[
        serde_json::json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "model": CLAUDE_MODEL,
            "content": [], "stop_reason": null, "usage": {"input_tokens": 3, "output_tokens": 0}}}),
        serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "PRISMFLOW_OK"}}),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
        serde_json::json!({"type": "message_stop"}),
    ])
}

fn anthropic_tool_stream() -> String {
    anthropic_sse(&[
        serde_json::json!({"type": "message_start", "message": {"id": "msg_2", "type": "message", "role": "assistant", "model": CLAUDE_MODEL,
            "content": [], "stop_reason": null, "usage": {"input_tokens": 3, "output_tokens": 0}}}),
        serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {}}}),
        serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
        serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"a.txt\"}"}}),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 9}}),
        serde_json::json!({"type": "message_stop"}),
    ])
}

/// Answers only with PrismFlow's Anthropic key in `x-api-key`; streams when
/// asked to, otherwise returns a JSON message (the engine uses both modes).
async fn anthropic_upstream() -> MockServer {
    let server = MockServer::start().await;
    let msg = |content: serde_json::Value, stop: &str| serde_json::json!({
        "id": "msg_j", "type": "message", "role": "assistant", "model": CLAUDE_MODEL, "content": content,
        "stop_reason": stop, "usage": {"input_tokens": 3, "output_tokens": 5}
    });
    let cases: Vec<(bool, bool, ResponseTemplate)> = vec![
        (true, true, ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(anthropic_tool_stream())),
        (true, false, ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(anthropic_text_stream())),
        (false, true, ResponseTemplate::new(200).set_body_json(msg(serde_json::json!([{"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.txt"}}]), "tool_use"))),
        (false, false, ResponseTemplate::new(200).set_body_json(msg(serde_json::json!([{"type": "text", "text": "PRISMFLOW_OK"}]), "end_turn"))),
    ];
    for (i, (stream, tools, resp)) in cases.into_iter().enumerate() {
        let mut m = Mock::given(method("POST")).and(path("/v1/messages")).and(header("x-api-key", ANTHROPIC_KEY));
        if stream {
            m = m.and(body_string_contains("\"stream\":true"));
        }
        if tools {
            m = m.and(body_string_contains("\"tools\""));
        }
        m.respond_with(resp).with_priority(i as u8 + 1).mount(&server).await;
    }
    server
}

async fn anthropic_harness() -> Harness {
    if let Err(e) = GatewayRuntime::discover() {
        panic!("gateway engine unavailable for tests: {e}");
    }
    let upstream = anthropic_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let diag_path = dir.path().join("diagnostics.jsonl");
    let broker = Broker::new(0, dir.path().to_path_buf(), Some(Arc::new(DiagnosticsLog::new(diag_path.clone()))));
    let token = tokens::generate();
    let p = crate::providers::get("anthropic").unwrap();
    let spec = UpstreamSpec {
        provider_id: "anthropic".into(),
        engine_type: supervisor::engine_type(p),
        base_url: upstream.uri(),
        models: vec![CLAUDE_MODEL.into()],
        key: ANTHROPIC_KEY.into(), codex_oauth: false };
    let binding = Binding { connection_id: "codex_cli:anthropic".into(), provider_id: "anthropic".into(), ..Default::default() };
    broker.start(vec![spec], vec![(token.clone(), binding)]).await.expect("broker starts");
    Harness { broker, token, _dir: dir, diag_path, _upstream: upstream }
}

#[tokio::test]
async fn responses_front_reaches_an_anthropic_upstream_with_text_and_tools() {
    let h = anthropic_harness().await;
    let r = h.post("/v1/responses", serde_json::json!({"model": CLAUDE_MODEL, "stream": true, "input": "hi"}))
        .header("authorization", format!("Bearer {}", h.token)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body = r.text().await.unwrap();
    for needle in ["response.created", "response.output_text.delta", "PRISMFLOW_OK", "response.completed"] {
        assert!(body.contains(needle), "missing {needle} in:\n{body}");
    }
    let tools = serde_json::json!({"model": CLAUDE_MODEL, "stream": true, "input": "read a.txt",
        "tools": [{"type": "function", "name": "read_file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}]});
    let body = h.post("/v1/responses", tools)
        .header("authorization", format!("Bearer {}", h.token)).send().await.unwrap().text().await.unwrap();
    for needle in ["\"type\":\"function_call\"", "\"name\":\"read_file\"", "a.txt", "response.completed"] {
        assert!(body.contains(needle), "missing {needle} in:\n{body}");
    }
    h.broker.stop().await;
}

#[tokio::test]
async fn messages_front_passes_through_to_an_anthropic_upstream() {
    let h = anthropic_harness().await;
    let mut b = messages_body(true, true);
    b["model"] = CLAUDE_MODEL.into();
    let body = h.post("/v1/messages", b).header("x-api-key", &h.token).send().await.unwrap().text().await.unwrap();
    for needle in ["\"type\":\"tool_use\"", "\"name\":\"read_file\"", "\"stop_reason\":\"tool_use\""] {
        assert!(body.contains(needle), "missing {needle} in:\n{body}");
    }
    h.broker.stop().await;
}

// ---- Routing: tiers, fallbacks, reasoning --------------------------------

use crate::routing::{Effort, ModelRef, Routing, Tier};

const KEY_A: &str = "sk-provider-a-0123456789abcdef";
const KEY_B: &str = "sk-provider-b-0123456789abcdef";

/// Provider A is down (503); provider B answers and echoes nothing secret.
async fn two_upstreams() -> (MockServer, MockServer) {
    let a = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
        .mount(&a).await;
    let b = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY_B}").as_str()))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(text_stream()))
        .mount(&b).await;
    (a, b)
}

async fn routed(routing: Routing) -> (Harness, MockServer) {
    let (a, b) = two_upstreams().await;
    let dir = tempfile::tempdir().unwrap();
    let diag_path = dir.path().join("diagnostics.jsonl");
    let broker = Broker::new(0, dir.path().to_path_buf(), Some(Arc::new(DiagnosticsLog::new(diag_path.clone()))));
    let token = tokens::generate();
    let spec = |id: &str, url: String, key: &str, model: &str| UpstreamSpec {
        provider_id: id.into(), engine_type: "openai_chat_completions", base_url: format!("{url}/v1"),
        models: vec![model.into()], key: key.into(), codex_oauth: false };
    let specs = vec![spec("prov_a", a.uri(), KEY_A, "model-a"), spec("prov_b", b.uri(), KEY_B, "model-b")];
    let binding = Binding {
        connection_id: "claude_code:prov_a".into(), provider_id: "prov_a".into(), model: "model-a".into(),
        routing: Arc::new(routing),
    };
    broker.start(specs, vec![(token.clone(), binding)]).await.expect("broker starts");
    (Harness { broker, token, _dir: dir, diag_path, _upstream: a }, b)
}

fn chat_body(model: &str) -> serde_json::Value {
    serde_json::json!({"model": model, "stream": true, "messages": [{"role": "user", "content": "hi"}]})
}

#[tokio::test]
async fn fallback_serves_from_the_next_provider_only_with_consent() {
    let fb = vec![ModelRef::new("prov_b", "model-b")];
    let (h, _b) = routed(Routing { fallbacks: fb.clone(), fallback_consent: false, ..Default::default() }).await;
    let r = h.post("/v1/chat/completions", chat_body("prismflow-default")).header("x-api-key", &h.token).send().await.unwrap();
    assert!(!r.status().is_success(), "without consent the failure must surface, not fall back");
    h.broker.stop().await;

    let (h, b) = routed(Routing { fallbacks: fb, fallback_consent: true, ..Default::default() }).await;
    let r = h.post("/v1/chat/completions", chat_body("prismflow-default")).header("x-api-key", &h.token).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["x-prismflow-served-by"], "prov_b/model-b");
    assert_eq!(r.headers()["x-prismflow-fallback-count"], "1");
    assert!(r.text().await.unwrap().contains("FLOW_OK"));
    assert_eq!(b.received_requests().await.unwrap().len(), 1);
    let diag = std::fs::read_to_string(&h.diag_path).unwrap();
    assert!(diag.contains("broker.fallback") && !diag.contains(KEY_A) && !diag.contains(KEY_B));
    h.broker.stop().await;
}

#[tokio::test]
async fn tier_alias_routes_to_its_own_provider_with_its_reasoning() {
    let mut routing = Routing { reasoning: Effort::Low, ..Default::default() };
    routing.tiers.insert(Tier::Haiku, ModelRef::new("prov_b", "model-b"));
    routing.tier_reasoning.insert(Tier::Haiku, Effort::High);
    let (h, b) = routed(routing).await;
    let r = h.post("/v1/chat/completions", chat_body("prismflow-haiku")).header("x-api-key", &h.token).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["x-prismflow-served-by"], "prov_b/model-b");
    assert!(r.headers().get("x-prismflow-fallback-count").is_none());
    let sent = &b.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(body["model"], "model-b", "upstream must see the real model, not the alias: {body}");
    assert_eq!(body["reasoning_effort"], "high", "{body}");
    h.broker.stop().await;
}

#[tokio::test]
async fn per_connection_rate_limit_answers_429() {
    let h = harness().await;
    h.broker.set_settings(RuntimeSettings { rate_limit_per_minute: 1, ..Default::default() });
    let ok = h.post("/v1/messages", messages_body(false, false)).header("x-api-key", &h.token).send().await.unwrap();
    assert_eq!(ok.status(), 200);
    let limited = h.post("/v1/messages", messages_body(false, false)).header("x-api-key", &h.token).send().await.unwrap();
    assert_eq!(limited.status(), 429);
    assert!(limited.text().await.unwrap().contains("per-minute"));
    h.broker.stop().await;
}

#[test]
fn engine_config_carries_concurrency_and_retry_limits() {
    let spec = UpstreamSpec { provider_id: "p".into(), engine_type: "openai_chat_completions", base_url: "https://x/v1".into(), models: vec![], key: "k".into(), codex_oauth: false };
    let cfg = supervisor::render_config(&[spec], &RuntimeSettings { max_concurrency_per_provider: 4, upstream_retries: 2, ..Default::default() });
    assert_eq!(cfg["upstreamConcurrency"]["enabled"], true);
    assert_eq!(cfg["upstreamConcurrency"]["maxInFlightPerProvider"], 4);
    assert_eq!(cfg["upstreamRetry"]["maxAttempts"], 3);
    let off = supervisor::render_config(&[], &RuntimeSettings { max_concurrency_per_provider: 0, upstream_retries: 0, ..Default::default() });
    assert_eq!(off["upstreamConcurrency"]["enabled"], false);
    assert_eq!(off["upstreamRetry"]["enabled"], false);
}

#[tokio::test]
async fn engine_starts_and_serves_with_concurrency_limits_enabled() {
    let upstream = chat_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::new(0, dir.path().to_path_buf(), None);
    broker.set_settings(RuntimeSettings { max_concurrency_per_provider: 2, upstream_retries: 2, rate_limit_per_minute: 100, ..Default::default() });
    let token = tokens::generate();
    let spec = UpstreamSpec {
        provider_id: "nvidia_nim".into(), engine_type: "openai_chat_completions",
        base_url: format!("{}/v1", upstream.uri()), models: vec![MODEL.into()], key: UPSTREAM_KEY.into(), codex_oauth: false };
    let binding = Binding { connection_id: "c".into(), provider_id: "nvidia_nim".into(), ..Default::default() };
    broker.start(vec![spec], vec![(token.clone(), binding)]).await.expect("engine accepts the limits config");
    let r = client().post(format!("{}/v1/messages", broker.url())).json(&messages_body(false, false))
        .header("x-api-key", &token).send().await.unwrap();
    assert_eq!(r.status(), 200);
    broker.stop().await;
}

// ---- Recorded-traffic corpus (tests/fixtures/recorded) -------------------

fn recorded(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/recorded/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

async fn replay_harness(stream_body: String, json_body: String) -> (Harness, MockServer) {
    let upstream = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/chat/completions")).and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(stream_body))
        .with_priority(1).mount(&upstream).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "application/json").set_body_string(json_body))
        .with_priority(2).mount(&upstream).await;
    let dir = tempfile::tempdir().unwrap();
    let diag_path = dir.path().join("diagnostics.jsonl");
    let broker = Broker::new(0, dir.path().to_path_buf(), None);
    let token = tokens::generate();
    let spec = UpstreamSpec {
        provider_id: "ollama".into(), engine_type: "openai_chat_completions", base_url: format!("{}/v1", upstream.uri()),
        models: vec!["qwen3:4b-instruct-2507-q4_K_M".into()], key: "no-key-required".into(), codex_oauth: false };
    let binding = Binding { connection_id: "claude_code:ollama".into(), provider_id: "ollama".into(),
        model: "qwen3:4b-instruct-2507-q4_K_M".into(), ..Default::default() };
    broker.start(vec![spec], vec![(token.clone(), binding)]).await.expect("broker starts");
    let h = Harness { broker, token, _dir: dir, diag_path, _upstream: MockServer::start().await };
    (h, upstream)
}

/// Concatenate Anthropic `text_delta` texts from an SSE body.
fn anthropic_text(sse: &str) -> String {
    sse.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .filter(|v| v["delta"]["type"] == "text_delta")
        .map(|v| v["delta"]["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn recorded_ollama_text_survives_messages_and_responses_fronts() {
    let (h, _up) = replay_harness(recorded("ollama-chat-text.sse"), recorded("ollama-chat-text.json")).await;
    let sse = h.post("/v1/messages", messages_body(true, false)).header("x-api-key", &h.token).send().await.unwrap().text().await.unwrap();
    assert_eq!(anthropic_text(&sse).trim(), "PRISMFLOW_OK", "{sse}");
    assert!(sse.contains("\"stop_reason\":\"end_turn\""), "{sse}");

    let v: serde_json::Value = h.post("/v1/messages", messages_body(false, false)).header("x-api-key", &h.token)
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(v["content"][0]["text"].as_str().unwrap().trim(), "PRISMFLOW_OK", "{v}");

    let r = h.post("/v1/responses", serde_json::json!({"model": "m", "stream": true, "input": "hi"}))
        .header("authorization", format!("Bearer {}", h.token)).send().await.unwrap().text().await.unwrap();
    assert!(r.contains("PRISMFLOW_OK") && r.contains("response.completed"), "{r}");
    h.broker.stop().await;
}

#[tokio::test]
async fn recorded_ollama_single_chunk_tool_call_is_translated_intact() {
    let (h, _up) = replay_harness(recorded("ollama-chat-tool.sse"), recorded("ollama-chat-text.json")).await;
    let sse = h.post("/v1/messages", messages_body(true, true)).header("x-api-key", &h.token).send().await.unwrap().text().await.unwrap();
    for needle in ["\"type\":\"tool_use\"", "\"name\":\"read_file\"", "\"stop_reason\":\"tool_use\""] {
        assert!(sse.contains(needle), "missing {needle} in:\n{sse}");
    }
    // The arguments arrive whole in one upstream chunk; they must not be lost or duplicated.
    let args: String = sse.lines().filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .filter(|v| v["delta"]["type"] == "input_json_delta")
        .map(|v| v["delta"]["partial_json"].as_str().unwrap_or_default().to_string()).collect();
    let parsed: serde_json::Value = serde_json::from_str(&args).unwrap_or_else(|e| panic!("bad args {args:?}: {e}\n{sse}"));
    assert_eq!(parsed["path"], "notes.txt");

    let tools = serde_json::json!({"model": "m", "stream": true, "input": "read notes.txt",
        "tools": [{"type": "function", "name": "read_file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}]});
    let r = h.post("/v1/responses", tools).header("authorization", format!("Bearer {}", h.token)).send().await.unwrap().text().await.unwrap();
    assert!(r.contains("\"type\":\"function_call\"") && r.contains("notes.txt"), "{r}");
    h.broker.stop().await;
}

// ---- ChatGPT sign-in (engine Codex OAuth plugin) -------------------------
//
// The engine rewrites Codex-OAuth requests to chatgpt.com whatever base URL it
// is given (observed 2026-10-15: a fake test token reached chatgpt.com and was
// rejected as unparseable, which also showed the header injection works). So
// this test stops at what can be checked locally and never sends a request.

#[tokio::test]
async fn chatgpt_signin_plugin_is_accepted_and_the_token_never_written() {
    const ACCESS: &str = "chatgpt-access-token-0123456789abcdef";
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::new(0, dir.path().to_path_buf(), None);
    let spec = UpstreamSpec {
        provider_id: "chatgpt".into(), engine_type: "openai_responses",
        base_url: "https://chatgpt.com/backend-api/codex".into(), models: vec!["gpt-x".into()],
        key: ACCESS.into(), codex_oauth: true,
    };
    let binding = Binding { connection_id: "claude_code:chatgpt".into(), provider_id: "chatgpt".into(), model: "gpt-x".into(), ..Default::default() };
    broker.start(vec![spec], vec![(tokens::generate(), binding)]).await.expect("engine accepts the codexOauth plugin");
    let cfg = std::fs::read_to_string(dir.path().join("gateway").join("gateway.config.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
    let hook = &v["plugins"][0]["providerHooks"]["codexOauth"];
    assert_eq!(hook["accessToken"], "{{ env.PRISMFLOW_UPSTREAM_CHATGPT }}");
    assert_eq!(hook["refreshIfMissingAccessToken"], false, "refreshing would log the user's Codex out");
    assert!(hook.get("refreshToken").is_none());
    assert!(!cfg.contains(ACCESS), "the token must never be written to the engine config");
    broker.stop().await;
}

// ---- Local web tools ------------------------------------------------------

#[tokio::test]
async fn web_search_server_tool_runs_locally_and_the_client_gets_the_answer() {
    let search = MockServer::start().await;
    Mock::given(method("GET")).and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": [
            {"title": "PrismFlow docs", "url": "https://example.org/pf", "content": "The answer is 42."}
        ]})))
        .mount(&search).await;
    let upstream = MockServer::start().await;
    let final_text = serde_json::json!({"id": "c2", "object": "chat.completion", "created": 1, "model": MODEL,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Per the docs, the answer is 42."}, "finish_reason": "stop"}]});
    let call_search = serde_json::json!({"id": "c1", "object": "chat.completion", "created": 1, "model": MODEL,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null, "tool_calls": [{"id": "call_s", "type": "function",
            "function": {"name": "web_search", "arguments": "{\"query\":\"prismflow answer\"}"}}]}, "finish_reason": "tool_calls"}]});
    Mock::given(method("POST")).and(path("/v1/chat/completions")).and(body_string_contains("The answer is 42."))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_text)).with_priority(1).mount(&upstream).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(call_search)).with_priority(2).mount(&upstream).await;

    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::new(0, dir.path().to_path_buf(), None);
    broker.set_settings(RuntimeSettings {
        web_tools: crate::webtools::WebToolsSettings { enabled: true, backend: crate::webtools::SearchBackend::Searxng, searxng_url: search.uri(), allow_private: false },
        ..Default::default()
    });
    let token = tokens::generate();
    let spec = UpstreamSpec { provider_id: "nvidia_nim".into(), engine_type: "openai_chat_completions",
        base_url: format!("{}/v1", upstream.uri()), models: vec![MODEL.into()], key: UPSTREAM_KEY.into(), codex_oauth: false };
    let binding = Binding { connection_id: "claude_code:nvidia_nim".into(), provider_id: "nvidia_nim".into(), model: MODEL.into(), ..Default::default() };
    broker.start(vec![spec], vec![(token.clone(), binding)]).await.unwrap();

    let body = serde_json::json!({"model": "prismflow-default", "max_tokens": 256, "stream": true,
        "messages": [{"role": "user", "content": "What is the answer? Search the web."}],
        "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 3}]});
    let r = client().post(format!("{}/v1/messages", broker.url())).header("x-api-key", &token).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let sse = r.text().await.unwrap();
    assert!(sse.contains("the answer is 42") && sse.contains("event: message_stop"), "{sse}");
    assert!(!sse.contains("\"name\":\"web_search\""), "the client never sees our internal tool call: {sse}");
    assert_eq!(upstream.received_requests().await.unwrap().len(), 2, "search turn + answer turn");
    assert_eq!(search.received_requests().await.unwrap().len(), 1);
    broker.stop().await;
}

#[tokio::test]
async fn web_tools_off_leaves_requests_untouched() {
    let h = harness().await;
    let mut b = messages_body(false, false);
    b["tools"] = serde_json::json!([{"type": "web_search_20250305", "name": "web_search"}]);
    // Off by default: the request goes straight through (the mock answers text).
    let v: serde_json::Value = h.post("/v1/messages", b).header("x-api-key", &h.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["type"], "message", "{v}");
    h.broker.stop().await;
}

#[test]
fn engine_never_gets_a_fixed_whole_request_timeout() {
    // Guard for the 2026-10-15 finding: a 60 s cap cut long streamed answers.
    let src = include_str!("supervisor.rs");
    assert!(src.contains(r#".env("UPSTREAM_TIMEOUT_MS", "0")"#));
}

#[tokio::test]
async fn claude_code_background_requests_never_reach_the_provider() {
    let h = harness().await;
    let title = serde_json::json!({"model": "claude-opus-4-8", "max_tokens": 64000, "stream": true,
        "system": [{"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude. Generate a concise, sentence-case title (3-7 words) that captures the main topic"}],
        "messages": [{"role": "user", "content": [{"type": "text", "text": "<session>\nfix the flaky login test\n</session>"}]}],
        "output_config": {"format": {"type": "json_schema", "schema": {"type": "object", "properties": {"title": {"type": "string"}}}}}});
    let r = h.post("/v1/messages", title).header("x-api-key", &h.token).send().await.unwrap();
    assert_eq!(r.headers()["x-prismflow-shortcut"], "title");
    let sse = r.text().await.unwrap();
    assert!(sse.contains("Fix the flaky login test") && sse.contains("event: message_stop"), "{sse}");

    let suggest = serde_json::json!({"model": "claude-opus-4-8", "max_tokens": 64000, "stream": false,
        "tools": [{"name": "Bash", "input_schema": {"type": "object"}}],
        "messages": [{"role": "user", "content": "hi"}, {"role": "assistant", "content": "hello"},
            {"role": "user", "content": "[SUGGESTION MODE: Suggest what the user might naturally type next into Claude Code.]"}]});
    let v: serde_json::Value = h.post("/v1/messages", suggest).header("x-api-key", &h.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["content"], serde_json::json!([]), "{v}");

    assert!(h._upstream.received_requests().await.unwrap().is_empty(), "no provider call for background requests");
    assert_eq!(h.broker.status().shortcuts, 2);

    // Switched off in Settings: the same request goes to the provider.
    h.broker.set_settings(RuntimeSettings { shortcuts: false, ..Default::default() });
    let mut b = messages_body(false, false);
    b["messages"] = serde_json::json!([{"role": "user", "content": "[SUGGESTION MODE: x]"}]);
    let v: serde_json::Value = h.post("/v1/messages", b).header("x-api-key", &h.token).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["type"], "message");
    assert_eq!(h._upstream.received_requests().await.unwrap().len(), 1);
    h.broker.stop().await;
}

#[tokio::test]
async fn muse_code_catalog_needs_a_token_and_lists_the_connection_model() {
    let h = harness().await;
    let v: serde_json::Value = client().get(format!("{}/muse-code/models", h.broker.url()))
        .bearer_auth(&h.token).send().await.unwrap().json().await.unwrap();
    // The harness binding has no model of its own; real connections list theirs.
    assert!(v["data"][0]["id"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(v["data"][0]["metadata"]["muse-code"]["limit"]["context"], 32768);
    let r = client().get(format!("{}/muse-code/models", h.broker.url())).send().await.unwrap();
    assert_eq!(r.status(), 401);
    h.broker.stop().await;
}
