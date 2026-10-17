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
        } else if users.contains(&format!("echo {TOOL_TOKEN}")) && tools.iter().any(|t| t == "shell_command") {
            call = Some(("shell_command".into(), serde_json::json!({ "command": format!("echo {TOOL_TOKEN}") })));
            answer = String::new();
        } else if users.contains(&format!("echo {TOOL_TOKEN}")) && tools.iter().any(|t| t == "shell") {
            call = Some(("shell".into(), serde_json::json!({ "command": ["echo", TOOL_TOKEN] })));
            answer = String::new();
        } else {
            if users.contains(&format!("echo {TOOL_TOKEN}")) {
                eprintln!("[stand-in] asked to run a command but no known shell tool among {tools:?}");
            }
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
    // A user name with a space and an accent, like many real ones.
    let root = dir.path().join("Ann Lée");
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("CODEX_HOME", home.join(".codex"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    // Every other app's settings files, inside the sandbox too.
    std::env::set_var("PRISMFLOW_CLAUDE_3P_DIR", home.join("claude-3p"));
    std::env::set_var("PRISMFLOW_VSCODE_USER_DIR", home.join("vscode-user"));
    std::env::set_var("PRISMFLOW_EDITORS_HOME", &home);
    std::env::set_var("PRISMFLOW_JETBRAINS_ACP_FILE", home.join(".jetbrains/acp.json"));
    std::env::set_var("PRISMFLOW_MCP_HOME", &home);
    std::env::set_var("PRISMFLOW_E2E_STORE", root.join("store.json"));
    let failures = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut f = run(&root, &home).await;
        // Hard conditions run with the default app set only.
        if std::env::var_os("PRISMFLOW_E2E_HOLD").is_none() && std::env::var_os("PRISMFLOW_E2E_APPS").is_none() {
            f.extend(conditions(&root, &home).await);
            f.extend(features(&root, &home).await);
        }
        f
    });
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
    // Apps PrismFlow can't drive itself (desktop apps, editors) stop at
    // "not tested in the app" once the gateway answered for them.
    let gateway_ok = r.stages.iter().any(|s| s.stage == 5 && s.outcome == prismflow_lib::state::StageOutcome::Pass);
    if matches!(r.status, ConnectionStatus::Unverified) && gateway_ok {
        return;
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
        std::fs::write(&h, format!("@set \"PRISMFLOW_E2E_STORE={}\"\r\n@\"{}\" helper %*\r\n", root.join("store.json").display(), me.display())).unwrap();
        h
    };
    #[cfg(unix)]
    let helper = {
        use std::os::unix::fs::PermissionsExt;
        let h = root.join("prismflow-helper");
        std::fs::write(&h, format!("#!/bin/sh\nPRISMFLOW_E2E_STORE='{}' exec '{}' helper \"$@\"\n", root.join("store.json").display(), me.display())).unwrap();
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


// ---------------------------------------------------------------------------
// Hard conditions: no internet, services off, broken files, crashes, races.
// Each case must end in bounded time, tell the user something plain, and
// leave the user's own files as they were.

/// A key store that refuses everything (Keychain locked, Credential Manager
/// or Secret Service off).
struct BrokenStore;

impl CredentialStore for BrokenStore {
    fn set(&self, _: &str, _: &str) -> Result<(), CredentialError> {
        Err(CredentialError::Store("User interaction is not allowed.".into()))
    }
    fn get(&self, _: &str) -> Result<String, CredentialError> {
        Err(CredentialError::Store("User interaction is not allowed.".into()))
    }
    fn delete(&self, _: &str) -> Result<(), CredentialError> {
        Err(CredentialError::Store("User interaction is not allowed.".into()))
    }
}

fn service(root: &Path, home: &Path, state: &str, store: Box<dyn CredentialStore>, port: u16, overrides: &[(&str, String)]) -> Service {
    let state_dir = root.join(state);
    let diag = Arc::new(DiagnosticsLog::new(state_dir.join("diagnostics.jsonl")));
    let helper = if cfg!(windows) { root.join("prismflow-helper.cmd") } else { root.join("prismflow-helper") };
    Service {
        store,
        state: StateStore::open(&state_dir).unwrap(),
        broker: Broker::new(port, state_dir.clone(), Some(diag.clone())),
        diag,
        client: prismflow_lib::discovery::client(),
        codex_home: home.join(".codex"),
        claude_dir: home.join(".claude"),
        helper,
        agent_timeout: Duration::from_secs(90),
        upstream_overrides: overrides.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

/// Words that mean the user is looking at raw internals instead of a message.
fn technical(msg: &str) -> Option<&'static str> {
    ["os error", "reqwest", "hyper::", "error sending request", "Error {", "kind:", "panicked", "Os {", "serde", "expected value at line", "ECONNREFUSED", "tcp connect"]
        .into_iter()
        .find(|w| msg.contains(w))
}

struct Case<'a> {
    name: &'a str,
    failures: &'a mut Vec<String>,
    started: Instant,
}

impl<'a> Case<'a> {
    fn start(name: &'a str, failures: &'a mut Vec<String>) -> Self {
        eprintln!("\n-- {name}");
        Self { name, failures, started: Instant::now() }
    }
    /// What the user would read; flags raw internals.
    fn says(&mut self, what: &str, msg: &str) {
        let first = msg.lines().next().unwrap_or("");
        eprintln!("   {what}: {first}  [{:.1?}]", self.started.elapsed());
        if let Some(w) = technical(msg) {
            self.failures.push(format!("{}: {what} shows raw internals ({w}): {}", self.name, msg.chars().take(200).collect::<String>()));
        }
    }
    fn fail(&mut self, why: String) {
        eprintln!("   FAIL: {why}");
        self.failures.push(format!("{}: {why}", self.name));
    }
    fn within(&mut self, secs: u64) {
        if self.started.elapsed() > Duration::from_secs(secs) {
            let took = self.started.elapsed();
            self.fail(format!("took {took:.0?} (limit {secs}s)"));
        }
    }
}

async fn verify_says(svc: &Service, id: &str, case: &mut Case<'_>) -> Option<ConnectionStatus> {
    match svc.verify(id).await {
        Ok(r) => {
            let bad: Vec<String> = r.stages.iter()
                .filter(|s| !matches!(s.outcome, prismflow_lib::state::StageOutcome::Pass | prismflow_lib::state::StageOutcome::Skipped))
                .map(|s| format!("{} — {}", s.name, s.detail.lines().next().unwrap_or(""))).collect();
            case.says(&format!("test → {:?}", r.status), &bad.join(" | "));
            Some(r.status)
        }
        Err(e) => { case.says("test error", &e); None }
    }
}

async fn conditions(root: &Path, home: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    eprintln!("\n================ hard conditions ================");
    let upstream = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [{"id": MOCK_MODEL}]})))
        .mount(&upstream).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions")).respond_with(ToolModel).mount(&upstream).await;
    let good = format!("{}/v1", upstream.uri());

    // A server that accepts connections and never answers.
    let silent = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let silent_url = format!("http://{}/v1", silent.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = silent.accept().await { held.push(sock); }
    });
    // A Wi-Fi login page answering everything with HTML.
    let portal = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>Sign in to Airport Wi-Fi</body></html>", "text/html"))
        .mount(&portal).await;
    let nothing = "http://127.0.0.1:1/v1".to_string();
    let before = snapshot(home);

    // 1. No internet, three ways.
    for (label, url) in [("no internet: connection refused", nothing.clone()), ("no internet: provider never answers", silent_url.clone()), ("no internet: Wi-Fi login page", format!("{}/v1", portal.uri()))] {
        let mut case = Case::start(label, &mut failures);
        let svc = service(root, home, &format!("state-{}", label.len()), Box::new(store()), free_port(), &[("groq", url)]);
        let _ = svc.set_credential("groq", UPSTREAM_KEY);
        match svc.discover_models("groq").await {
            Ok(m) => case.says("model list", &format!("{} models (expected an error)", m.len())),
            Err(e) => case.says("model list", &e),
        }
        case.within(40);
        match svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL).await {
            Ok(rec) => {
                verify_says(&svc, &rec.id, &mut case).await;
                let _ = svc.disconnect(&rec.id).await;
            }
            Err(e) => case.says("connect", &e),
        }
        case.within(150);
    }

    // 2. The key store refuses (Keychain locked, Credential Manager off).
    {
        let mut case = Case::start("key store refuses", &mut failures);
        let svc = service(root, home, "state-broken-store", Box::new(BrokenStore), free_port(), &[("groq", good.clone())]);
        match svc.set_credential("groq", UPSTREAM_KEY) {
            Ok(()) => case.fail("saving a key 'worked' with a broken store".into()),
            Err(e) => case.says("save key", &e),
        }
        match svc.connect(claude_code::APP_ID, "ollama", "qwen3:4b").await {
            Ok(_) => case.fail("connect 'worked' with a broken store".into()),
            Err(e) => case.says("connect", &e),
        }
        let checks = prismflow_lib::selfcheck::run(&BrokenStore, &root.join("sc")).await;
        let store_check = checks.iter().find(|c| c.name == "Saving keys");
        match store_check {
            Some(c) if !c.ok => case.says("self-check", &c.detail),
            _ => case.fail("self-check didn't flag the key store".into()),
        }
        case.within(30);
    }

    // 3. The key can't be read when the app asks for it.
    {
        let mut case = Case::start("key unreadable at run time", &mut failures);
        let svc = service(root, home, "state-key-gone", Box::new(store()), free_port(), &[("groq", good.clone())]);
        let _ = svc.set_credential("groq", UPSTREAM_KEY);
        match svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL).await {
            Ok(rec) => {
                let token_acct = tokens::account(&rec.id);
                let saved = svc.store.get(&token_acct).ok();
                let _ = svc.store.delete(&token_acct);
                verify_says(&svc, &rec.id, &mut case).await;
                if let Some(t) = saved { let _ = svc.store.set(&token_acct, &t); }
                let _ = svc.disconnect(&rec.id).await;
            }
            Err(e) => case.fail(format!("connect: {e}")),
        }
        case.within(120);
    }

    // 4. The engine or Node is missing.
    for (label, var, val) in [("engine missing", "PRISMFLOW_GATEWAY_DIR", root.join("no-engine")), ("Node missing", "PRISMFLOW_NODE_BIN", root.join("no-node"))] {
        let mut case = Case::start(label, &mut failures);
        let old = std::env::var_os(var);
        std::env::set_var(var, &val);
        let svc = service(root, home, &format!("state-{var}"), Box::new(store()), free_port(), &[("groq", good.clone())]);
        let _ = svc.set_credential("groq", UPSTREAM_KEY);
        let files = snapshot(home);
        match svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL).await {
            Ok(rec) => {
                case.says("connect", "connected anyway");
                verify_says(&svc, &rec.id, &mut case).await;
                let _ = svc.disconnect(&rec.id).await;
            }
            Err(e) => case.says("connect", &e),
        }
        let checks = prismflow_lib::selfcheck::run(&store(), &root.join("sc2")).await;
        if let Some(c) = checks.iter().find(|c| !c.ok) { case.says("self-check", &c.detail) } else { case.fail("self-check passed with no engine".into()) }
        match old { Some(v) => std::env::set_var(var, v), None => std::env::remove_var(var) }
        if snapshot(home) != files { case.fail("user files changed".into()); }
        case.within(60);
    }

    // 5. The gateway's port is taken by something else.
    {
        let mut case = Case::start("gateway port taken", &mut failures);
        let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = squatter.local_addr().unwrap().port();
        let svc = service(root, home, "state-port", Box::new(store()), port, &[("groq", good.clone())]);
        let _ = svc.set_credential("groq", UPSTREAM_KEY);
        let files = snapshot(home);
        match svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL).await {
            Ok(rec) => {
                case.says("connect", "connected anyway");
                verify_says(&svc, &rec.id, &mut case).await;
                let _ = svc.disconnect(&rec.id).await;
            }
            Err(e) => case.says("connect", &e),
        }
        if !svc.connections().unwrap_or_default().is_empty() { case.fail("a connection is left over".into()); }
        if snapshot(home) != files { case.fail("user files changed".into()); }
        drop(squatter);
        case.within(60);
    }

    // 6. Ollama installed but not running.
    {
        let mut case = Case::start("Ollama not running", &mut failures);
        let svc = service(root, home, "state-ollama-off", Box::new(store()), free_port(), &[("ollama", nothing.clone())]);
        match svc.discover_models("ollama").await {
            Ok(m) => case.says("model list", &format!("{} models (expected an error)", m.len())),
            Err(e) => case.says("model list", &e),
        }
        match svc.connect(codex::APP_ID, "ollama", "qwen3:4b").await {
            Ok(rec) => {
                verify_says(&svc, &rec.id, &mut case).await;
                let _ = svc.disconnect(&rec.id).await;
            }
            Err(e) => case.says("connect", &e),
        }
        case.within(150);
    }

    // 7. The user's own settings files are broken.
    {
        let mut case = Case::start("broken user settings", &mut failures);
        let toml = home.join(".codex/config.toml");
        let json = home.join(".claude/settings.json");
        let (old_toml, old_json) = (std::fs::read_to_string(&toml).ok(), std::fs::read_to_string(&json).ok());
        std::fs::write(&toml, "model = \"gpt-5\"\n[profiles.mine\nthis is = = not toml\n").unwrap();
        std::fs::write(&json, "{ \"theme\": \"dark\", oops }").unwrap();
        let broken = snapshot(home);
        let svc = service(root, home, "state-broken-files", Box::new(store()), free_port(), &[("groq", good.clone())]);
        let _ = svc.set_credential("groq", UPSTREAM_KEY);
        for app in [codex::APP_ID, claude_code::APP_ID] {
            match svc.connect(app, "groq", MOCK_MODEL).await {
                Ok(rec) => {
                    case.says(&format!("connect {app}"), "connected");
                    verify_says(&svc, &rec.id, &mut case).await;
                    let _ = svc.disconnect(&rec.id).await;
                }
                Err(e) => case.says(&format!("connect {app}"), &e),
            }
        }
        if snapshot(home) != broken { case.fail("a broken user file was rewritten".into()); }
        match old_toml { Some(t) => std::fs::write(&toml, t).unwrap(), None => { let _ = std::fs::remove_file(&toml); } }
        match old_json { Some(t) => std::fs::write(&json, t).unwrap(), None => { let _ = std::fs::remove_file(&json); } }
        case.within(120);
    }

    // 8. PrismFlow crashes while apps are connected, then starts again.
    {
        let mut case = Case::start("crash and restart", &mut failures);
        let port = free_port();
        let ids: Vec<String> = {
            let svc = service(root, home, "state-crash", Box::new(store()), port, &[("groq", good.clone())]);
            let _ = svc.set_credential("groq", UPSTREAM_KEY);
            let mut ids = Vec::new();
            for app in [claude_code::APP_ID, codex::APP_ID] {
                match svc.connect(app, "groq", MOCK_MODEL).await {
                    Ok(r) => ids.push(r.id),
                    Err(e) => case.fail(format!("connect {app}: {e}")),
                }
            }
            // No disconnect: the process just goes away. Its port is freed
            // and the engine exits (its stdin closes), which is what stop does.
            svc.broker.stop().await;
            drop(svc);
            ids
        };
        let svc = service(root, home, "state-crash", Box::new(store()), port, &[("groq", good.clone())]);
        match svc.sync_gateway().await {
            Ok(st) => case.says("restart gateway", &format!("{:?}", st.phase)),
            Err(e) => case.says("restart gateway", &e),
        }
        for id in &ids {
            if verify_says(&svc, id, &mut case).await != Some(ConnectionStatus::ConnectedAndVerified) {
                case.fail(format!("{id} doesn't work after a restart"));
            }
        }
        for id in &ids { if let Err(e) = svc.disconnect(id).await { case.fail(format!("remove {id}: {e}")); } }
        case.within(120);
    }

    // 9. Two apps switch at the same moment; the same app is set up twice at once.
    {
        let mut case = Case::start("switching at the same moment", &mut failures);
        let svc = service(root, home, "state-race", Box::new(store()), free_port(), &[("groq", good.clone()), ("nvidia_nim", good.clone())]);
        let _ = svc.set_credential("groq", UPSTREAM_KEY);
        let _ = svc.set_credential("nvidia_nim", UPSTREAM_KEY);
        let a = svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL).await;
        let b = svc.connect(codex::APP_ID, "groq", MOCK_MODEL).await;
        if let (Ok(a), Ok(b)) = (a, b) {
            let (x, y) = tokio::join!(
                prismflow_lib::tray::switch(&svc, &a.id, "nvidia_nim", MOCK_MODEL),
                prismflow_lib::tray::switch(&svc, &b.id, "nvidia_nim", MOCK_MODEL),
            );
            if let Err(e) = x { case.fail(format!("switch Claude Code: {e}")); }
            if let Err(e) = y { case.fail(format!("switch Codex: {e}")); }
            let (p, q) = tokio::join!(
                svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL),
                svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL),
            );
            case.says("double setup", &format!("{} / {}", p.is_ok(), q.is_ok()));
            let conns = svc.connections().unwrap_or_default();
            eprintln!("   connections now: {:?}", conns.iter().map(|c| c.id.clone()).collect::<Vec<_>>());
            for c in &conns {
                if verify_says(&svc, &c.id, &mut case).await != Some(ConnectionStatus::ConnectedAndVerified) {
                    case.fail(format!("{} doesn't work after the race", c.id));
                }
            }
            for c in conns { let _ = svc.disconnect(&c.id).await; }
            if svc.broker.is_running().await { case.fail("gateway still running after removing everything".into()); }
        } else {
            case.fail("couldn't connect the two apps".into());
        }
        case.within(120);
    }

    if snapshot(home) != before {
        failures.push("hard conditions: the user's files differ from before".into());
    }
    failures
}


// ---------------------------------------------------------------------------
// Features nobody had run end to end: backup/restore, uninstall, profiles,
// code sessions, tools (MCP) sharing, remote access.

async fn features(root: &Path, home: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    eprintln!("\n================ features ================");
    let upstream = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [{"id": MOCK_MODEL}]})))
        .mount(&upstream).await;
    Mock::given(method("POST")).and(path("/v1/chat/completions")).respond_with(ToolModel).mount(&upstream).await;
    let good = format!("{}/v1", upstream.uri());
    let before = snapshot(home);
    let overrides = [("groq", good.clone()), ("nvidia_nim", good.clone())];
    let port = free_port();
    let svc = Arc::new(service(root, home, "state-features", Box::new(store()), port, &overrides));
    let _ = svc.set_credential("groq", UPSTREAM_KEY);
    let _ = svc.set_credential("nvidia_nim", UPSTREAM_KEY);
    let a = svc.connect(claude_code::APP_ID, "groq", MOCK_MODEL).await;
    let b = svc.connect(codex::APP_ID, "nvidia_nim", MOCK_MODEL).await;
    let (Ok(a), Ok(b)) = (a, b) else {
        failures.push("features: couldn't connect the two apps".into());
        return failures;
    };

    // Profiles: save, change, switch back.
    {
        let mut case = Case::start("profiles", &mut failures);
        match svc.save_profile("Work") {
            Ok(p) => case.says("save", &format!("saved; {} profile(s)", p.list.len())),
            Err(e) => case.fail(format!("save: {e}")),
        }
        let _ = prismflow_lib::tray::switch(&svc, &a.id, "nvidia_nim", MOCK_MODEL).await;
        match svc.switch_profile("Work").await {
            Ok(_) => {
                let ids: Vec<String> = svc.connections().unwrap_or_default().into_iter().map(|c| c.id).collect();
                case.says("switch back", &format!("{ids:?}"));
                if !ids.contains(&a.id) || !ids.contains(&b.id) || ids.len() != 2 {
                    case.fail(format!("profile didn't bring back the saved setup: {ids:?}"));
                }
            }
            Err(e) => case.fail(format!("switch: {e}")),
        }
        if svc.delete_profile("Work").is_err() { case.fail("delete failed".into()); }
        case.within(60);
    }

    // Tools: an MCP server shared into Claude Code and Codex, then removed.
    {
        let mut case = Case::start("tools (MCP) sharing", &mut failures);
        let server = prismflow_lib::mcp::Server { name: "pf-test".into(), command: "npx".into(), args: vec!["-y".into(), "some-mcp".into()], env: Default::default(), url: String::new() };
        for app in ["claude_code", "codex"] {
            match prismflow_lib::mcp::apply(app, &[server.clone()], &[]) {
                Ok(names) => {
                    let file = prismflow_lib::mcp::target_file(app).unwrap();
                    let text = std::fs::read_to_string(&file).unwrap_or_default();
                    case.says(&format!("add to {app}"), &format!("{names:?} in {}", file.file_name().unwrap().to_string_lossy()));
                    if !text.contains("pf-test") { case.fail(format!("{app}: server not written")); }
                    match prismflow_lib::mcp::apply(app, &[], &names) {
                        Ok(_) if std::fs::read_to_string(&file).unwrap_or_default().contains("pf-test") => case.fail(format!("{app}: server not removed")),
                        Ok(_) => {}
                        Err(e) => case.fail(format!("{app} remove: {e}")),
                    }
                }
                Err(e) => case.fail(format!("{app}: {e}")),
            }
        }
        case.within(30);
    }

    // Code sessions: a real Codex turn in a project folder.
    {
        let mut case = Case::start("code sessions", &mut failures);
        let sessions = prismflow_lib::sessions::Sessions::open(&root.join("state-features"));
        let folder = root.join("my project");
        std::fs::create_dir_all(&folder).unwrap();
        match sessions.create(&folder, prismflow_lib::sessions::Sandbox::ReadOnly, "Test") {
            Ok(sess) => {
                if let Err(e) = sessions.send(&svc, &sess.id, &b.id, "Reply with exactly PRISMFLOW_OK and nothing else.").await {
                    case.fail(format!("send: {e}"));
                }
                let deadline = Instant::now() + Duration::from_secs(120);
                let mut text = String::new();
                while Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if let Some(page) = sessions.events(&sess.id, 0) {
                        text = page.events.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
                        if !page.session.running() { break; }
                    }
                }
                case.says("answer", if text.contains("PRISMFLOW_OK") { "got PRISMFLOW_OK" } else { "no answer" });
                if !text.contains("PRISMFLOW_OK") {
                    case.fail(format!("no answer in the session: {}", text.chars().rev().take(300).collect::<String>().chars().rev().collect::<String>()));
                }
                let _ = sessions.delete(&sess.id);
            }
            Err(e) => case.fail(format!("create: {e}")),
        }
        case.within(130);
    }

    // Remote access: start, pair a device, read status, run a test remotely.
    {
        let mut case = Case::start("remote access", &mut failures);
        let dir = root.join("state-features").join("companion");
        let registry = Arc::new(prismflow_lib::companion::Registry::open(&dir).unwrap());
        let backend: Arc<dyn prismflow_lib::companion::server::Backend> = Arc::new(prismflow_lib::companion::backend::ServiceBackend(svc.clone()));
        match prismflow_lib::companion::server::start("127.0.0.1".parse().unwrap(), 0, &dir, registry.clone(), backend).await {
            Ok(run) => {
                let code = registry.start_pairing();
                let client = reqwest::Client::builder().danger_accept_invalid_certs(true).no_proxy().build().unwrap();
                let paired = client.post(format!("{}/pair", run.url)).json(&serde_json::json!({ "code": code, "device_name": "Test phone" })).send().await;
                let token = match paired {
                    Ok(r) => r.json::<serde_json::Value>().await.ok().and_then(|v| v["token"].as_str().map(String::from)),
                    Err(e) => { case.fail(format!("pair: {e}")); None }
                };
                if let Some(t) = token {
                    let st = client.get(format!("{}/api/status", run.url)).bearer_auth(&t).send().await;
                    match st {
                        Ok(r) if r.status().is_success() => case.says("status", &format!("{} bytes", r.text().await.unwrap_or_default().len())),
                        Ok(r) => case.fail(format!("status: HTTP {}", r.status())),
                        Err(e) => case.fail(format!("status: {e}")),
                    }
                    let wrong = client.get(format!("{}/api/status", run.url)).bearer_auth("nope").send().await.map(|r| r.status().as_u16()).ok();
                    if wrong != Some(401) && wrong != Some(403) { case.fail(format!("a wrong device token got {wrong:?}")); }
                    // Read-only devices may not run tests until allowed.
                    let v = client.post(format!("{}/api/connections/{}/verify", run.url, a.id.replace(':', "%3A"))).bearer_auth(&t).send().await.map(|r| r.status().as_u16()).ok();
                    case.says("test while read-only", &format!("{v:?}"));
                } else {
                    case.fail("pairing gave no token".into());
                }
                run.stop().await;
            }
            Err(e) => case.fail(format!("start: {e}")),
        }
        case.within(60);
    }

    // Backup, remove everything, restore.
    {
        let mut case = Case::start("backup and restore", &mut failures);
        match svc.backup() {
            Ok(bk) => {
                let saved = serde_json::to_string(&bk).unwrap();
                for c in svc.connections().unwrap_or_default() { let _ = svc.disconnect(&c.id).await; }
                let fresh = service(root, home, "state-restored", Box::new(store()), free_port(), &overrides);
                let bk: prismflow_lib::backup::Backup = serde_json::from_str(&saved).unwrap();
                match fresh.restore(bk).await {
                    Ok(rep) => {
                        case.says("restore", &format!("restored {:?}; notes {:?}", rep.restored, rep.problems));
                        let ids: Vec<String> = fresh.connections().unwrap_or_default().into_iter().map(|c| c.id).collect();
                        if ids.len() != 2 { case.fail(format!("expected both apps back, got {ids:?}")); }
                        for id in &ids {
                            if verify_says(&fresh, id, &mut case).await != Some(ConnectionStatus::ConnectedAndVerified) {
                                case.fail(format!("{id} doesn't work after restoring"));
                            }
                        }
                        // Uninstall: everything PrismFlow did goes away.
                        match fresh.uninstall(true).await {
                            Ok(plan) => case.says("uninstall plan", &format!("{} item(s)", plan.items.len())),
                            Err(e) => case.fail(format!("uninstall plan: {e}")),
                        }
                        match fresh.uninstall(false).await {
                            Ok(r) if r.complete => case.says("uninstall", "complete"),
                            Ok(r) => case.fail(format!("uninstall incomplete: {:?}", r.items.iter().filter(|i| format!("{:?}", i.outcome).to_lowercase() != "done").map(|i| format!("{} {}", i.what, i.detail)).collect::<Vec<_>>())),
                            Err(e) => case.fail(format!("uninstall: {e}")),
                        }
                        if fresh.broker.is_running().await { case.fail("gateway still running after uninstall".into()); }
                    }
                    Err(e) => case.fail(format!("restore: {e}")),
                }
            }
            Err(e) => case.fail(format!("backup: {e}")),
        }
        case.within(120);
    }
    for c in svc.connections().unwrap_or_default() { let _ = svc.disconnect(&c.id).await; }
    svc.broker.stop().await;
    let after = snapshot(home);
    for (f, b) in &before {
        if after.get(f) != Some(b) {
            failures.push(format!("features: {f} changed:\n--- before\n{b}\n--- after\n{}", after.get(f).map(String::as_str).unwrap_or("(missing)")));
        }
    }
    for f in after.keys().filter(|f| !before.contains_key(*f)) {
        failures.push(format!("features: {f} was created:\n{}", after[f]));
    }
    failures
}
