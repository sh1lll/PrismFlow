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
        key: UPSTREAM_KEY.into(),
    };
    let binding = Binding { connection_id: "claude_code:nvidia_nim".into(), provider_id: "nvidia_nim".into() };
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
