//! A self-check anyone can run (Settings → Check PrismFlow, or
//! `prismflow self-check`): the secure store, the gateway engine and the
//! Node runtime it runs on, and one real request through the gateway to a
//! built-in stand-in provider. No account or network is needed.

use crate::credentials::CredentialStore;
use crate::gateway::supervisor::{GatewayRuntime, UpstreamSpec};
use crate::gateway::{tokens, Binding, Broker};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

const REPLY: &str = "PRISMFLOW_SELF_CHECK_OK";
const MODEL: &str = "self-check-model";

pub async fn run(store: &dyn CredentialStore, work_dir: &Path) -> Vec<Check> {
    let mut checks = vec![secure_store(store)];
    match GatewayRuntime::discover() {
        Ok(rt) => {
            checks.push(Check { name: "Gateway engine", ok: true, detail: format!("Found; Node {}.", rt.node_version) });
            checks.push(gateway(work_dir).await);
        }
        Err(e) => checks.push(Check { name: "Gateway engine", ok: false, detail: e }),
    }
    checks
}

fn secure_store(store: &dyn CredentialStore) -> Check {
    let account = "self-check/probe";
    let value = tokens::generate();
    let result = store.set(account, &value)
        .and_then(|_| store.get(account))
        .map(|read| read == value);
    let _ = store.delete(account);
    match result {
        Ok(true) => Check { name: "Saving keys", ok: true, detail: "Keys can be saved and read.".into() },
        Ok(false) => Check { name: "Saving keys", ok: false, detail: "A saved key read back differently.".into() },
        Err(e) => Check { name: "Saving keys", ok: false, detail: format!("The system's key store refused: {e}") },
    }
}

/// Starts a gateway in front of a stand-in provider and sends it one
/// Claude-style request, the way Claude Code would.
async fn gateway(work_dir: &Path) -> Check {
    let name = "Gateway";
    let upstream = match stand_in_provider().await {
        Ok(u) => u,
        Err(e) => return Check { name, ok: false, detail: format!("Couldn't start the stand-in provider: {e}") },
    };
    let dir = work_dir.join("self-check");
    let _ = std::fs::create_dir_all(&dir);
    let broker = Broker::new(0, dir, None);
    let token = tokens::generate();
    let spec = UpstreamSpec {
        provider_id: "self_check".into(), engine_type: "openai_chat_completions",
        base_url: format!("http://{upstream}/v1"), models: vec![MODEL.into()],
        key: "self-check-key".into(), codex_oauth: false, headers: vec![], extra_keys: vec![],
    };
    let binding = Binding { connection_id: "self-check:self_check".into(), provider_id: "self_check".into(), model: MODEL.into(), ..Default::default() };
    if let Err(e) = broker.start(vec![spec], vec![(token.clone(), binding)]).await {
        return Check { name, ok: false, detail: format!("The gateway didn't start: {e}") };
    }
    let body = serde_json::json!({ "model": MODEL, "max_tokens": 16, "messages": [{ "role": "user", "content": "hi" }] });
    let reply = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(60)).build()
        .map_err(|e| e.to_string());
    let reply = match reply {
        Ok(c) => c.post(format!("{}/v1/messages", broker.url())).header("x-api-key", &token).json(&body).send().await
            .map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    let text = match reply {
        Ok(r) => r.text().await.unwrap_or_default(),
        Err(e) => String::from(e),
    };
    broker.stop().await;
    if text.contains(REPLY) {
        Check { name, ok: true, detail: "Started and answered a request.".into() }
    } else {
        Check { name, ok: false, detail: format!("Started, but the request failed: {}", text.chars().take(300).collect::<String>()) }
    }
}

/// A tiny OpenAI-style provider on a free loopback port that always answers
/// with REPLY (streamed, as providers do).
async fn stand_in_provider() -> std::io::Result<std::net::SocketAddr> {
    use axum::{response::IntoResponse, routing::post, Router};
    let chunk = |delta: serde_json::Value, finish: Option<&str>| {
        format!("data: {}\n\n", serde_json::json!({ "id": "c", "object": "chat.completion.chunk", "created": 0, "model": MODEL,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] }))
    };
    let body = format!("{}{}data: [DONE]\n\n",
        chunk(serde_json::json!({ "role": "assistant", "content": REPLY }), None),
        chunk(serde_json::json!({}), Some("stop")));
    let app = Router::new().route("/v1/chat/completions", post(move || {
        let body = body.clone();
        async move { ([("content-type", "text/event-stream")], body).into_response() }
    }));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(120), axum::serve(listener, app)).await;
    });
    Ok(addr)
}

/// Where an installed PrismFlow keeps its bundled engine and Node: next to
/// the program (Windows), in the app's Resources (macOS), or in lib/ (Linux).
pub fn installed_resource_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.to_path_buf(), dir.join("../Resources"), dir.join("../lib/PrismFlow"), dir.join("../lib/prismflow")]
        .into_iter()
        .find(|d| d.join("gateway").is_dir())
}

/// `prismflow self-check`: prints each check; exit code 0 when all pass.
pub fn cli(store: &dyn CredentialStore, out: &mut dyn std::io::Write) -> i32 {
    if let Some(res) = installed_resource_dir() {
        crate::gateway::supervisor::use_bundled_runtime(&res);
    }
    let work = std::env::temp_dir().join(format!("prismflow-self-check-{}", std::process::id()));
    let checks = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(run(store, &work)),
        Err(e) => vec![Check { name: "Start", ok: false, detail: e.to_string() }],
    };
    let _ = std::fs::remove_dir_all(&work);
    for c in &checks {
        let _ = writeln!(out, "{} {}: {}", if c.ok { "OK  " } else { "FAIL" }, c.name, c.detail);
    }
    i32::from(!checks.iter().all(|c| c.ok))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::memory::InMemoryStore;

    #[tokio::test]
    async fn self_check_passes_on_a_working_install() {
        let dir = tempfile::tempdir().unwrap();
        let checks = run(&InMemoryStore::new(), dir.path()).await;
        assert!(checks.iter().all(|c| c.ok), "{checks:#?}");
        assert_eq!(checks.len(), 3);
    }
}
