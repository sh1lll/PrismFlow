//! Manual live check of Claude Desktop's automatic setup in a sandbox copy
//! of its configuration library: PrismFlow writes and selects its gateway
//! profile, then the requests Claude Desktop makes (model list, streamed
//! Messages turn with a tool) are sent with exactly the address and key
//! from that profile. Disconnect must leave the library as it was.
//! PRISMFLOW_LIVE_BROKER=<dir with url+token> cargo test --test live_claude_desktop -- --ignored --nocapture

use prismflow_lib::adapters::claude_desktop as cd;
use serde_json::{json, Value};

#[tokio::test]
#[ignore]
async fn claude_desktop_gateway_profile() {
    let Ok(broker) = std::env::var("PRISMFLOW_LIVE_BROKER") else { return };
    let broker = std::path::PathBuf::from(broker);
    let url = std::fs::read_to_string(broker.join("url")).unwrap();
    let token = std::fs::read_to_string(broker.join("token")).unwrap();
    let lib = tempfile::tempdir().unwrap();
    std::env::set_var("PRISMFLOW_CLAUDE_3P_DIR", lib.path());
    // Same starting point as this Mac: a neutral Default selected, account mode.
    std::fs::create_dir_all(lib.path().join("configLibrary")).unwrap();
    let meta0 = json!({"entries": [{"id": cd::DEFAULT_ID, "name": "Default"}], "appliedId": cd::DEFAULT_ID});
    std::fs::write(lib.path().join("configLibrary/_meta.json"), meta0.to_string()).unwrap();
    std::fs::write(lib.path().join(format!("configLibrary/{}.json", cd::DEFAULT_ID)), "{}").unwrap();
    std::fs::write(lib.path().join("claude_desktop_config.json"), r#"{"deploymentMode":"1p"}"#).unwrap();

    cd::connect_auto(&url, &token, "claude_desktop:ollama").unwrap();
    assert!(cd::auto_active(&cd::config_root(), "claude_desktop:ollama"));
    let p = cd::auto_profile(&cd::config_root()).unwrap();
    let base = p["inferenceGatewayBaseUrl"].as_str().unwrap().to_string();
    let key = p["inferenceGatewayApiKey"].as_str().unwrap().to_string();
    let client = reqwest::Client::new();

    let models: Value = client.get(format!("{base}/v1/models")).bearer_auth(&key).send().await.unwrap().json().await.unwrap();
    eprintln!("models: {}", models.to_string().chars().take(300).collect::<String>());
    assert!(models["data"].as_array().is_some_and(|d| !d.is_empty()));

    let model = models["data"][0]["id"].as_str().unwrap().to_string();
    let body = json!({
        "model": model, "max_tokens": 256, "stream": true,
        "system": [{"type": "text", "text": "You are Claude, running in the Claude desktop app."}],
        "tools": [{"name": "web_fetch", "description": "Fetch a URL", "input_schema": {"type": "object", "properties": {"url": {"type": "string"}}}}],
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Reply with exactly PRISMFLOW_DESKTOP_OK and nothing else."}]}]
    });
    let sse = client.post(format!("{base}/v1/messages")).bearer_auth(&key)
        .header("anthropic-version", "2023-06-01").json(&body).send().await.unwrap().text().await.unwrap();
    eprintln!("stream tail: {}", sse.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>());
    let text: String = sse.lines().filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|v| v["delta"]["text"].as_str().map(String::from)).collect();
    eprintln!("streamed text: {text:?}");
    assert!(text.contains("PRISMFLOW_DESKTOP_OK") && sse.contains("message_stop"));

    cd::disconnect_auto("claude_desktop:ollama").unwrap();
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(lib.path().join("configLibrary/_meta.json")).unwrap()).unwrap();
    assert_eq!(meta, meta0, "library restored");
    let mode: Value = serde_json::from_str(&std::fs::read_to_string(lib.path().join("claude_desktop_config.json")).unwrap()).unwrap();
    assert_eq!(mode["deploymentMode"], "1p");
    eprintln!("CLAUDE DESKTOP LIVE PASS");
}
