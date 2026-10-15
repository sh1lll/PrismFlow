//! Real-binary smoke for Slice 2: the installed `claude` and `codex` CLIs, the
//! built `prismflow` helper, the real macOS Keychain, PrismFlow's broker and
//! the pinned engine. Only the upstream provider is mocked: a Chat
//! Completions server that calls tools when the verification prompts ask.
//!
//! Skipped unless `PRISMFLOW_REAL_SMOKE=1`, because it needs both CLIs,
//! Node 20+, the engine and Keychain access:
//!   PRISMFLOW_REAL_SMOKE=1 cargo test --test real_agents
//!
//! Keychain identity: macOS binds an item's access to the code identity of
//! the process that created it. A different unsigned binary reading it
//! triggers a password prompt, and `security -A` does not lift the
//! partition check. So this test's own executable is also the helper. A
//! wrapper script execs it with `helper …`, and it then runs the same
//! `cli::handle_args` + `KeychainStore` code as `prismflow`'s 5-line main.
//! The shipping binary's helper path was smoke-tested in slice 1.
//!
//! Keychain safety: it refuses to run if PrismFlow already holds a real
//! NVIDIA key or tokens for these connections, and deletes its own entries
//! on exit.

use prismflow_lib::adapters::{claude_code, codex};
use prismflow_lib::credentials::{account_key, CredentialStore, SERVICE_NAME};
use prismflow_lib::diagnostics::DiagnosticsLog;
use prismflow_lib::gateway::{tokens, Broker};
use prismflow_lib::service::Service;
use prismflow_lib::state::{ConnectionStatus, StateStore};

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MODEL: &str = "meta/llama-3.3-70b-instruct";
const UPSTREAM_KEY: &str = "nvapi-prismflow-selftest-0123456789";
const TOOL_TOKEN: &str = "PRISMFLOW_TOOL_OK";

/// Chat Completions upstream that behaves like a tool-capable model.
struct ToolModel;

fn text_of(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Respond for ToolModel {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let auth = req.headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
        if auth != format!("Bearer {UPSTREAM_KEY}") {
            return ResponseTemplate::new(401).set_body_json(serde_json::json!({"error": {"message": "bad upstream key"}}));
        }
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let tools: Vec<String> = body["tools"].as_array().cloned().unwrap_or_default().iter()
            .filter_map(|t| t["function"]["name"].as_str().map(String::from)).collect();
        let users: String = messages.iter().filter(|m| m["role"] == "user").map(|m| text_of(&m["content"])).collect::<Vec<_>>().join(" ");
        let tool_results: Vec<String> = messages.iter().filter(|m| m["role"] == "tool").map(|m| text_of(&m["content"])).collect();

        let mut call: Option<(String, serde_json::Value)> = None;
        let answer;
        if !tool_results.is_empty() {
            answer = if tool_results.iter().any(|t| t.contains(TOOL_TOKEN)) { format!("DONE {TOOL_TOKEN}") } else { "DONE".into() };
        } else if users.contains("PRISMFLOW_TOOL_PROBE") && tools.iter().any(|t| t == "Read") {
            let file = users.split(|c: char| c.is_whitespace() || c == '"' || c == '\\')
                .find(|w| w.starts_with('/') && w.ends_with("prismflow-probe.txt")).unwrap_or("prismflow-probe.txt");
            call = Some(("Read".into(), serde_json::json!({ "file_path": file })));
            answer = String::new();
        } else if users.contains(&format!("echo {TOOL_TOKEN}")) && tools.iter().any(|t| t == "exec_command") {
            call = Some(("exec_command".into(), serde_json::json!({ "cmd": format!("echo {TOOL_TOKEN}") })));
            answer = String::new();
        } else if users.contains(&format!("echo {TOOL_TOKEN}")) && tools.iter().any(|t| t == "shell") {
            call = Some(("shell".into(), serde_json::json!({ "command": ["echo", TOOL_TOKEN] })));
            answer = String::new();
        } else {
            answer = "PRISMFLOW_OK".into();
        }
        eprintln!("[upstream] {} msgs, {} tools, call={:?}", messages.len(), tools.len(), call.as_ref().map(|c| &c.0));

        let chunk = |delta: serde_json::Value, finish: Option<&str>| serde_json::json!({
            "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": MODEL,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }]
        });
        let mut chunks = Vec::new();
        match call {
            Some((name, args)) => {
                let args = args.to_string();
                let (a, b) = args.split_at(args.len() / 2);
                chunks.push(chunk(serde_json::json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "call_smoke", "type": "function", "function": {"name": name, "arguments": ""}}]}), None));
                chunks.push(chunk(serde_json::json!({"tool_calls": [{"index": 0, "function": {"arguments": a}}]}), None));
                chunks.push(chunk(serde_json::json!({"tool_calls": [{"index": 0, "function": {"arguments": b}}]}), None));
                chunks.push(chunk(serde_json::json!({}), Some("tool_calls")));
            }
            None => {
                let (a, b) = answer.split_at(answer.len() / 2);
                chunks.push(chunk(serde_json::json!({"role": "assistant", "content": a}), None));
                chunks.push(chunk(serde_json::json!({"content": b}), None));
                chunks.push(chunk(serde_json::json!({}), Some("stop")));
            }
        }
        let mut sse: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        sse.push_str("data: [DONE]\n\n");
        ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(sse)
    }
}

/// Removes this test's Keychain entries however the test ends (same
/// process identity that created them, so no prompt).
struct KeychainCleanup(Vec<String>);

impl Drop for KeychainCleanup {
    fn drop(&mut self) {
        let store = prismflow_lib::credentials::default_store();
        for acct in &self.0 {
            let _ = store.delete(acct);
        }
    }
}

fn keychain_has(acct: &str) -> bool {
    Command::new("security").args(["find-generic-password", "-s", SERVICE_NAME, "-a", acct])
        .output().map(|o| o.status.success()).unwrap_or(false)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("helper") {
        let store = prismflow_lib::credentials::default_store();
        let code = prismflow_lib::cli::handle_args(&args[1..], store.as_ref(), &mut std::io::stdout()).unwrap_or(64);
        std::process::exit(code);
    }
    if let Ok(model) = std::env::var("PRISMFLOW_LIVE_OLLAMA") {
        tokio::runtime::Runtime::new().unwrap().block_on(live_local_ollama(&model));
        println!("real_agents: LIVE OLLAMA PASS");
        return;
    }
    if std::env::var("PRISMFLOW_REAL_SMOKE").as_deref() != Ok("1") {
        println!("real_agents: skipped (set PRISMFLOW_REAL_SMOKE=1 to run the real-binary smoke)");
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(real_claude_code_and_codex_through_the_gateway());
    println!("real_agents: PASS");
}

async fn real_claude_code_and_codex_through_the_gateway() {
    let claude_bin = claude_code::find_binary().expect("claude CLI on PATH");
    let codex_bin = codex::find_binary().expect("codex CLI on PATH");
    let claude_id = "claude_code:nvidia_nim";
    let codex_id = "codex_cli:nvidia_nim";
    let accounts = vec![account_key("nvidia_nim", "default"), tokens::account(claude_id), tokens::account(codex_id)];
    for a in &accounts {
        assert!(!keychain_has(a), "refusing to run: real Keychain item {a} exists");
    }
    let _cleanup = KeychainCleanup(accounts.clone());

    let upstream = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [{"id": MODEL}]})))
        .mount(&upstream).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions")).respond_with(ToolModel).mount(&upstream).await;

    let dir = tempfile::tempdir().unwrap();
    // The helper wrapper: this very executable in helper mode.
    let helper = dir.path().join("prismflow-helper");
    let me = std::env::current_exe().unwrap();
    std::fs::write(&helper, format!("#!/bin/sh\nexec '{}' helper \"$@\"\n", me.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let state_dir = dir.path().join("state");
    let codex_home = dir.path().join("codex");
    let claude_dir = dir.path().join("claude");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::create_dir_all(&claude_dir).unwrap();
    // `codex -p` resolves profiles under CODEX_HOME; point it at the temp home.
    std::env::set_var("CODEX_HOME", &codex_home);
    let diag = Arc::new(DiagnosticsLog::new(state_dir.join("diagnostics.jsonl")));
    let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
    let store: Box<dyn CredentialStore> = prismflow_lib::credentials::default_store();
    let svc = Service {
        store,
        state: StateStore::open(&state_dir).unwrap(),
        broker: Broker::new(port, state_dir.clone(), Some(diag.clone())),
        diag,
        client: prismflow_lib::discovery::client(),
        codex_home,
        claude_dir,
        helper,
        agent_timeout: Duration::from_secs(120),
        upstream_overrides: [("nvidia_nim".to_string(), format!("{}/v1", upstream.uri()))].into(),
    };
    svc.set_credential("nvidia_nim", UPSTREAM_KEY).expect("store self-test key");

    eprintln!("claude: {}  codex: {}", claude_bin.display(), codex_bin.display());
    let mut failures = Vec::new();
    for (app, id) in [(claude_code::APP_ID, claude_id), (codex::APP_ID, codex_id)] {
        let rec = svc.connect(app, "nvidia_nim", MODEL).await.expect("connect");
        assert_eq!(rec.id, id);
        let report = svc.verify(id).await.expect("verify");
        eprintln!("\n== {id}: {:?}", report.status);
        for s in &report.stages {
            eprintln!("  {} {:<13} {:?}: {}", s.stage, s.name, s.outcome, s.detail.lines().next().unwrap_or(""));
        }
        if report.status != ConnectionStatus::ConnectedAndVerified {
            failures.push(format!("{id}: {:?}\n{:#?}", report.status, report.stages));
        }
    }
    let status = svc.gateway_status();
    eprintln!("\ngateway: {:?}, {} requests, {} rejected, {} upstream errors", status.phase, status.requests, status.rejected, status.upstream_errors);

    for e in svc.diagnostics(200).iter().filter(|e| e.code.starts_with("broker.")) {
        eprintln!("  diag {} {}", e.code, e.message);
    }
    for id in [claude_id, codex_id] {
        svc.disconnect(id).await.expect("disconnect");
        assert!(!keychain_has(&tokens::account(id)), "token for {id} must be revoked");
    }
    assert!(!svc.broker.is_running().await, "gateway must stop with the last connection");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Live run against the user's own Ollama: real agents, real model, no mocks.
/// `PRISMFLOW_LIVE_OLLAMA=<model> cargo test --test real_agents`
async fn live_local_ollama(model: &str) {
    let apps: Vec<String> = std::env::var("PRISMFLOW_LIVE_APPS")
        .unwrap_or_else(|_| "claude_code,codex_cli".into())
        .split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let ids: Vec<String> = apps.iter().map(|a| format!("{a}:ollama")).collect();
    let accounts: Vec<String> = ids.iter().map(|i| tokens::account(i)).collect();
    for a in &accounts {
        assert!(!keychain_has(a), "refusing to run: real Keychain item {a} exists");
    }
    let _cleanup = KeychainCleanup(accounts.clone());
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("prismflow-helper");
    let me = std::env::current_exe().unwrap();
    std::fs::write(&helper, format!("#!/bin/sh\nexec '{}' helper \"$@\"\n", me.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let state_dir = dir.path().join("state");
    let codex_home = dir.path().join("codex");
    let claude_dir = dir.path().join("claude");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::env::set_var("CODEX_HOME", &codex_home);
    let diag = Arc::new(DiagnosticsLog::new(state_dir.join("diagnostics.jsonl")));
    let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
    let svc = Service {
        store: prismflow_lib::credentials::default_store(),
        state: StateStore::open(&state_dir).unwrap(),
        broker: Broker::new(port, state_dir.clone(), Some(diag.clone())),
        diag,
        client: prismflow_lib::discovery::client(),
        codex_home,
        claude_dir,
        helper,
        agent_timeout: Duration::from_secs(300),
        upstream_overrides: std::env::var("PRISMFLOW_LIVE_OLLAMA_URL")
            .map(|u| [("ollama".to_string(), u)].into())
            .unwrap_or_default(),
    };
    let mut failures = Vec::new();
    for (app, id) in apps.iter().zip(&ids) {
        let (app, id) = (app.as_str(), id.as_str());
        svc.connect(app, "ollama", model).await.expect("connect");
        let report = svc.verify(id).await.expect("verify");
        eprintln!("\n== {id} ({model}): {:?}", report.status);
        for s in &report.stages {
            eprintln!("  {} {:<13} {:?}: {}", s.stage, s.name, s.outcome, s.detail.lines().next().unwrap_or(""));
        }
        if !matches!(report.status, ConnectionStatus::ConnectedAndVerified | ConnectionStatus::PartiallyCompatible) {
            for e in svc.diagnostics(50).iter().filter(|e| e.code.starts_with("broker.")) {
                eprintln!("  diag {} {}", e.code, e.message);
            }
            let log = state_dir.join("agents").join(id.replace(':', "-")).join("cline-data/logs/cline.log");
            if let Ok(l) = std::fs::read_to_string(&log) {
                eprintln!("  cline.log tail:\n{}", l.lines().rev().take(25).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
            }
            failures.push(format!("{id}: {:?}\n{:#?}", report.status, report.stages));
        }
    }
    for id in &ids {
        svc.disconnect(id).await.expect("disconnect");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
