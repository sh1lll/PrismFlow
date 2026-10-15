//! Manual live harness: runs the broker alone (no Keychain) in front of a
//! local Ollama so a real, interactive agent can be pointed at it.
//! `PRISMFLOW_LIVE_BROKER=<dir> cargo test --test live_broker -- --ignored --nocapture`
//! writes `url` and `token` into <dir> and serves until `<dir>/stop` exists.

use prismflow_lib::gateway::supervisor::UpstreamSpec;
use prismflow_lib::gateway::{tokens, Binding, Broker, RuntimeSettings};
use std::time::Duration;

#[tokio::test]
#[ignore]
async fn serve_broker_for_a_live_agent() {
    let Ok(dir) = std::env::var("PRISMFLOW_LIVE_BROKER") else { return };
    let dir = std::path::PathBuf::from(dir);
    let model = std::env::var("PRISMFLOW_LIVE_MODEL").unwrap_or_else(|_| "qwen3:4b-instruct-2507-q4_K_M".into());
    let ollama = std::env::var("PRISMFLOW_LIVE_OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into());
    let shortcuts = std::env::var("PRISMFLOW_LIVE_SHORTCUTS").as_deref() != Ok("0");
    let work = tempfile::tempdir().unwrap();
    let broker = Broker::new(0, work.path().to_path_buf(), None);
    broker.set_settings(RuntimeSettings { shortcuts, ..Default::default() });
    let token = tokens::generate();
    let spec = UpstreamSpec { provider_id: "ollama".into(), engine_type: "openai_chat_completions",
        base_url: ollama, models: vec![model.clone()], key: "ollama".into(), codex_oauth: false, headers: vec![] };
    let binding = Binding { connection_id: "claude_code:ollama".into(), provider_id: "ollama".into(), model, ..Default::default() };
    broker.start(vec![spec], vec![(token.clone(), binding)]).await.unwrap();
    std::fs::write(dir.join("url"), broker.url()).unwrap();
    std::fs::write(dir.join("token"), &token).unwrap();
    while !dir.join("stop").exists() {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let s = broker.status();
    std::fs::write(dir.join("status"), format!("requests={} shortcuts={} errors={}", s.requests, s.shortcuts, s.upstream_errors)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(dir.join("usage.json"), serde_json::to_string_pretty(&broker.usage().recent(20)).unwrap()).unwrap();
    broker.stop().await;
}
