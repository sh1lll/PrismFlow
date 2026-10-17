//! End-to-end and stress run in a sandbox: the real `claude` and `codex`
//! CLIs, PrismFlow's real connect / verify / switch / disconnect flow and
//! gateway, against your local Ollama model and a stand-in provider.
//!
//! Nothing outside a temp folder is touched: HOME, ~/.claude, ~/.codex and
//! the key store (a JSON file instead of the Keychain) all live in it.
//!
//!   PRISMFLOW_E2E=1 cargo test --test sandbox_e2e
//!   PRISMFLOW_E2E_MODEL=<ollama model>   (default qwen3:4b-instruct-2507-q4_K_M)
//!   PRISMFLOW_E2E_APPS=claude_code,codex_cli   (default: shared and separate of both)
//!   PRISMFLOW_E2E_PROVIDER=stand-in   (no Ollama: a second stand-in provider instead; used on CI)
//!   PRISMFLOW_E2E_HOLD=<secs>   (debug: connect, print how to start each app, wait)

use prismflow_lib::adapters::{claude_code, codex};
use prismflow_lib::credentials::{CredentialError, CredentialStore};
use prismflow_lib::diagnostics::DiagnosticsLog;
use prismflow_lib::gateway::{tokens, Broker};
use prismflow_lib::service::Service;
use prismflow_lib::state::{ConnectionStatus, StateStore};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MOCK_MODEL: &str = "meta/llama-3.3-70b-instruct";
const UPSTREAM_KEY: &str = "nvapi-prismflow-sandbox-0123456789";
const TOOL_TOKEN: &str = "PRISMFLOW_TOOL_OK";

/// A key store in one JSON file, shared by this process and the helper it
/// spawns (the apps read their gateway key through it).
struct FileStore(PathBuf, Mutex<()>);

impl FileStore {
    fn load(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(&self.0).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save(&self, m: &BTreeMap<String, String>) -> Result<(), CredentialError> {
        let tmp = self.0.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string(m).unwrap()).and_then(|_| std::fs::rename(&tmp, &self.0))
            .map_err(|e| CredentialError::Store(e.to_string()))
    }
}

impl CredentialStore for FileStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        let _g = self.1.lock().unwrap();
        let mut m = self.load();
        m.insert(account.into(), secret.into());
        self.save(&m)
    }
    fn get(&self, account: &str) -> Result<String, CredentialError> {
        self.load().get(account).cloned().ok_or_else(|| CredentialError::NotFound(account.into()))
    }
    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        let _g = self.1.lock().unwrap();
        let mut m = self.load();
        m.remove(account).ok_or_else(|| CredentialError::NotFound(account.into()))?;
        self.save(&m)
    }
}

fn store() -> FileStore {
    FileStore(PathBuf::from(std::env::var("PRISMFLOW_E2E_STORE").expect("store path")), Mutex::new(()))
}

fn text_of(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Stand-in Chat Completions provider: calls tools when the verification
/// prompts ask, and fails on demand ("PF_FAIL_500", "PF_FAIL_429",
/// "PF_DROP" mid-stream, "PF_SLOW" 3 s delay).
struct ToolModel;

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
        if users.contains("PF_FAIL_500") {
            return ResponseTemplate::new(500).set_body_json(serde_json::json!({"error": {"message": "upstream exploded"}}));
        }
        if users.contains("PF_FAIL_429") {
            return ResponseTemplate::new(429).insert_header("retry-after", "1").set_body_json(serde_json::json!({"error": {"message": "slow down"}}));
        }

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
        let chunk = |delta: serde_json::Value, finish: Option<&str>| serde_json::json!({
            "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": MOCK_MODEL,
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
                if users.contains("PF_DROP_LONG") {
                    // A long answer (≈2 MB) that is already flowing when it stops.
                    for _ in 0..8000 {
                        chunks.push(chunk(serde_json::json!({"content": "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua "}), None));
                    }
                }
                if !users.contains("PF_DROP") {
                    chunks.push(chunk(serde_json::json!({}), Some("stop")));
                }
            }
        }
        let mut sse: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        if !users.contains("PF_DROP") {
            sse.push_str("data: [DONE]\n\n");
        }
        if body["stream"] != serde_json::json!(true) {
            let text = if users.contains("PF_DROP") { "" } else { "PRISMFLOW_OK" };
            return ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "c1", "object": "chat.completion", "created": 1, "model": MOCK_MODEL,
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
            }));
        }
        let t = ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream");
        if users.contains("PF_SLOW") { t.set_delay(Duration::from_secs(3)) } else { t }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("helper") {
        let code = prismflow_lib::cli::handle_args(&args[1..], &store(), &mut std::io::stdout()).unwrap_or(64);
        std::process::exit(code);
    }
    if std::env::var("PRISMFLOW_E2E").as_deref() != Ok("1") {
        println!("sandbox_e2e: skipped (set PRISMFLOW_E2E=1)");
        return;
    }
    // Sandbox first, before any thread starts: every child process inherits it.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("CODEX_HOME", home.join(".codex"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    std::env::set_var("PRISMFLOW_E2E_STORE", dir.path().join("store.json"));
    let failures = tokio::runtime::Runtime::new().unwrap().block_on(run(dir.path(), &home));
    if failures.is_empty() {
        println!("\nsandbox_e2e: PASS");
    } else {
        eprintln!("\nsandbox_e2e: {} FAILURE(S)\n- {}", failures.len(), failures.join("\n- "));
        std::process::exit(1);
    }
}

fn report(svc: &Service, label: &str, r: &prismflow_lib::state::VerificationReport, took: Duration, failures: &mut Vec<String>) {
    eprintln!("\n== {label}: {:?} ({:.0?})", r.status, took);
    for s in &r.stages {
        eprintln!("  {} {:<13} {:?}: {}", s.stage, s.name, s.outcome, s.detail.lines().next().unwrap_or(""));
        if matches!(s.outcome, prismflow_lib::state::StageOutcome::Fail) {
            for l in s.detail.lines().skip(1).filter(|l| !l.trim().is_empty()).take(30) {
                eprintln!("      | {l}");
            }
        }
    }
    if !matches!(r.status, ConnectionStatus::ConnectedAndVerified) {
        for e in svc.diagnostics(30).iter().filter(|e| e.code.starts_with("broker.")) {
            eprintln!("  diag {} {}", e.code, e.message);
        }
        failures.push(format!("{label}: {:?}", r.status));
    }
}

/// The answer text in a reply, joined across streamed pieces (Claude-style
/// `"text":"…"` deltas, Codex-style `"delta":"…"`), or the whole body.
fn answer(body: &str) -> String {
    let mut out = String::from(body);
    for key in ["\"text\":\"", "\"delta\":\""] {
        for piece in body.split(key).skip(1) {
            out.push_str(piece.split('"').next().unwrap_or(""));
        }
    }
    out
}

fn snapshot(home: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for f in [".claude/settings.json", ".codex/config.toml", ".claude.json"] {
        if let Ok(s) = std::fs::read_to_string(home.join(f)) {
            out.insert(f.to_string(), s);
        }
    }
    out
}

async fn run(root: &Path, home: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    // The first provider: your local Ollama, or (CI) a stand-in under Groq's name.
    let (first, model) = if std::env::var("PRISMFLOW_E2E_PROVIDER").as_deref() == Ok("stand-in") {
        ("groq", MOCK_MODEL.to_string())
    } else {
        ("ollama", std::env::var("PRISMFLOW_E2E_MODEL").unwrap_or_else(|_| "qwen3:4b-instruct-2507-q4_K_M".into()))
    };
    let apps: Vec<String> = std::env::var("PRISMFLOW_E2E_APPS")
        .unwrap_or_else(|_| [claude_code::APP_ID, claude_code::SEPARATE_APP_ID, codex::APP_ID, codex::CLI_SEPARATE_APP_ID].join(","))
        .split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    // The user's own settings, which must come back exactly.
    std::fs::write(home.join(".claude/settings.json"), "{\n  \"theme\": \"dark\",\n  \"env\": { \"MY_VAR\": \"1\" }\n}\n").unwrap();
    std::fs::write(home.join(".codex/config.toml"), "model = \"gpt-5\"\n\n[profiles.mine]\nmodel = \"o3\"\n").unwrap();
    let before = snapshot(home);

    let upstream = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [{"id": MOCK_MODEL}]})))
        .mount(&upstream).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions")).respond_with(ToolModel).mount(&upstream).await;

    let me = std::env::current_exe().unwrap();
    #[cfg(windows)]
    let helper = {
        let h = root.join("prismflow-helper.cmd");
        std::fs::write(&h, format!("@\"{}\" helper %*\r\n", me.display())).unwrap();
        h
    };
    #[cfg(unix)]
    let helper = {
        use std::os::unix::fs::PermissionsExt;
        let h = root.join("prismflow-helper");
        std::fs::write(&h, format!("#!/bin/sh\nexec '{}' helper \"$@\"\n", me.display())).unwrap();
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o755)).unwrap();
        h
    };
    let state_dir = root.join("state");
    let diag = Arc::new(DiagnosticsLog::new(state_dir.join("diagnostics.jsonl")));
    let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
    let svc = Service {
        store: Box::new(store()),
        state: StateStore::open(&state_dir).unwrap(),
        broker: Broker::new(port, state_dir.clone(), Some(diag.clone())),
        diag,
        client: prismflow_lib::discovery::client(),
        codex_home: home.join(".codex"),
        claude_dir: home.join(".claude"),
        helper,
        agent_timeout: Duration::from_secs(300),
        upstream_overrides: [
            ("nvidia_nim".to_string(), format!("{}/v1", upstream.uri())),
            ("groq".to_string(), format!("{}/v1", upstream.uri())),
        ].into(),
    };
    svc.set_credential("nvidia_nim", UPSTREAM_KEY).expect("save stand-in key");
    svc.set_credential("groq", UPSTREAM_KEY).expect("save stand-in key");

    // Debug aid: connect only, print how to start each app, and wait.
    if let Ok(secs) = std::env::var("PRISMFLOW_E2E_HOLD") {
        for app in &apps {
            let rec = svc.connect(app, first, &model).await.expect("connect");
            let info = svc.launch_info(&rec.id).map(|l| l.line).unwrap_or_default();
            eprintln!("HOLD {}: HOME={} {info}", rec.id, home.display());
        }
        tokio::time::sleep(Duration::from_secs(secs.parse().unwrap_or(600))).await;
        return failures;
    }

    // 1. Connect each app to the real local model and run the full test.
    let mut ids = Vec::new();
    for app in &apps {
        let t = Instant::now();
        let rec = match svc.connect(app, first, &model).await {
            Ok(r) => r,
            Err(e) => { failures.push(format!("connect {app} → {first}: {e}")); continue; }
        };
        match svc.verify(&rec.id).await {
            Ok(r) => report(&svc, &format!("{} on {first} {model}", rec.id), &r, t.elapsed(), &mut failures),
            Err(e) => failures.push(format!("verify {}: {e}", rec.id)),
        }
        ids.push(rec.id);
    }

    // 2. Switch every app to another provider (stand-in) and test again.
    let mut switched = Vec::new();
    for id in &ids {
        if let Err(e) = prismflow_lib::tray::switch(&svc, id, "nvidia_nim", MOCK_MODEL).await {
            failures.push(format!("switch {id} → nvidia_nim: {e}"));
            continue;
        }
        let app = id.split(':').next().unwrap();
        let Some(new) = svc.connections().unwrap().into_iter().find(|c| c.app_id == app && c.provider_id == "nvidia_nim") else {
            failures.push(format!("switch {id}: no new connection"));
            continue;
        };
        if svc.connections().unwrap().iter().any(|c| &c.id == id) {
            failures.push(format!("switch {id}: old connection still listed"));
        }
        let t = Instant::now();
        match svc.verify(&new.id).await {
            Ok(r) => report(&svc, &format!("{} after switch", new.id), &r, t.elapsed(), &mut failures),
            Err(e) => failures.push(format!("verify {}: {e}", new.id)),
        }
        switched.push(new.id);
    }

    // 3. Stress the gateway through one connection's real key.
    if let Some(id) = switched.iter().find(|i| i.starts_with("claude_code:")).or(switched.first()) {
        stress(&svc, id, &mut failures).await;
    }

    // 4. Switch back to the first provider.
    for id in &switched {
        if let Err(e) = prismflow_lib::tray::switch(&svc, id, first, &model).await {
            failures.push(format!("switch back {id}: {e}"));
        }
    }
    eprintln!("\nconnections after switching back: {:?}", svc.connections().unwrap().iter().map(|c| c.id.clone()).collect::<Vec<_>>());

    // 5. Remove everything; the user's files must be exactly as before.
    for c in svc.connections().unwrap() {
        if let Err(e) = svc.disconnect(&c.id).await {
            failures.push(format!("disconnect {}: {e}", c.id));
        }
    }
    if svc.broker.is_running().await {
        failures.push("gateway still running after the last app was removed".into());
    }
    let after = snapshot(home);
    for (f, b) in &before {
        if after.get(f) != Some(b) {
            failures.push(format!("{f} not restored:\n--- before\n{b}\n--- after\n{}", after.get(f).map(String::as_str).unwrap_or("(missing)")));
        }
    }
    let left: Vec<String> = store().load().into_keys().filter(|k| k.contains("token")).collect();
    if !left.is_empty() {
        failures.push(format!("gateway keys left after removal: {left:?}"));
    }
    eprintln!("\nfiles left in the sandbox home:");
    for e in walk(home) {
        eprintln!("  {e}");
    }
    failures
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().display().to_string());
            }
        }
    }
    out.sort();
    out
}

async fn stress(svc: &Service, id: &str, failures: &mut Vec<String>) {
    let key = svc.store.get(&tokens::account(id)).expect("connection key");
    let url = svc.broker.url();
    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(120)).build().unwrap();
    let msg = |text: &str, stream: bool| serde_json::json!({
        "model": "anything", "max_tokens": 64, "stream": stream,
        "messages": [{ "role": "user", "content": text }]
    });
    let send = |body: serde_json::Value, key: String| {
        let (client, url) = (client.clone(), url.clone());
        async move {
            let r = client.post(format!("{url}/v1/messages")).header("x-api-key", key)
                .header("anthropic-version", "2023-06-01").json(&body).send().await.map_err(|e| e.to_string())?;
            let status = r.status().as_u16();
            let body = r.text().await.map_err(|e| format!("connection cut mid-answer ({e})"))?;
            Ok::<_, String>((status, body))
        }
    };
    eprintln!("\n== stress on {id} via {url}");

    // Many requests at once, streamed and not.
    for (n, stream) in [(200, true), (100, false)] {
        let t = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..n {
            set.spawn(send(msg(&format!("hello {i}"), stream), key.clone()));
        }
        let (mut ok, mut bad) = (0, Vec::new());
        while let Some(r) = set.join_next().await {
            match r.unwrap() {
                Ok((200, body)) if answer(&body).contains("PRISMFLOW_OK") => ok += 1,
                Ok((s, body)) => bad.push(format!("{s}: {}", body.chars().take(160).collect::<String>())),
                Err(e) => bad.push(e),
            }
        }
        eprintln!("  {n} at once (stream={stream}): {ok} ok, {} failed, {:.1?}", bad.len(), t.elapsed());
        if !bad.is_empty() {
            failures.push(format!("{n} concurrent (stream={stream}): {} failed, e.g. {}", bad.len(), bad[0]));
        }
    }

    // Codex-style requests at once.
    let t = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for i in 0..50 {
        let (client, url, key) = (client.clone(), url.clone(), key.clone());
        set.spawn(async move {
            let r = client.post(format!("{url}/v1/responses")).bearer_auth(key)
                .json(&serde_json::json!({ "model": "anything", "stream": true, "input": format!("hi {i}") })).send().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((r.status().as_u16(), r.text().await.unwrap_or_default()))
        });
    }
    let mut bad = Vec::new();
    while let Some(r) = set.join_next().await {
        match r.unwrap() {
            Ok((200, b)) if answer(&b).contains("PRISMFLOW_OK") => {}
            Ok((s, b)) => bad.push(format!("{s}: {}", b.chars().take(160).collect::<String>())),
            Err(e) => bad.push(e),
        }
    }
    eprintln!("  50 Codex-style at once: {} failed, {:.1?}", bad.len(), t.elapsed());
    if !bad.is_empty() {
        failures.push(format!("responses concurrent: {} failed, e.g. {}", bad.len(), bad[0]));
    }

    // Provider failures come back as clear errors, and the gateway survives.
    for (trigger, want) in [("PF_FAIL_500", "5xx"), ("PF_FAIL_429", "429"), ("PF_DROP", "error"), ("PF_DROP_LONG", "error")] {
        for stream in [true, false] {
            let t = Instant::now();
            let got = send(msg(trigger, stream), key.clone()).await;
            let line = match &got {
                Ok((s, b)) if trigger.starts_with("PF_DROP") && stream => format!("{s} …{}", b.replace('\n', " ").chars().rev().take(260).collect::<String>().chars().rev().collect::<String>()),
                Ok((s, b)) => format!("{s} {}", b.replace('\n', " ").chars().take(140).collect::<String>()),
                Err(e) => e.clone(),
            };
            eprintln!("  {trigger} stream={stream} (want {want}): {line} [{:.1?}]", t.elapsed());
            if let Ok((200, b)) = &got {
                if !b.contains("\"error\"") || b.contains("\"end_turn\"") {
                    failures.push(format!("{trigger} stream={stream}: provider failure reported as a finished answer"));
                }
            }
        }
    }
    // Slow provider: still answers.
    match send(msg("PF_SLOW", true), key.clone()).await {
        Ok((200, b)) if answer(&b).contains("PRISMFLOW_OK") => eprintln!("  slow provider: ok"),
        other => failures.push(format!("slow provider: {other:?}")),
    }
    // Wrong key, no key: refused.
    for k in ["pft_wrong", ""] {
        match send(msg("hi", false), k.into()).await {
            Ok((s, _)) if s == 401 || s == 403 => {}
            other => failures.push(format!("key {k:?} not refused: {other:?}")),
        }
    }
    // Still healthy afterwards.
    match send(msg("after", true), key.clone()).await {
        Ok((200, b)) if answer(&b).contains("PRISMFLOW_OK") => eprintln!("  healthy after errors: ok"),
        other => failures.push(format!("gateway unhealthy after errors: {other:?}")),
    }
    let st = svc.gateway_status();
    eprintln!("  gateway: {:?}, {} requests, {} rejected, {} upstream errors", st.phase, st.requests, st.rejected, st.upstream_errors);
}
