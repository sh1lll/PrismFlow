//! Manual live check of VS Code Chat's PrismFlow model: PrismFlow writes the
//! chatLanguageModels.json group, then sends what VS Code sends (Anthropic
//! Messages with a `read_file` tool) to that entry, completing the tool loop.
//! PRISMFLOW_LIVE_BROKER=<dir with url+token> cargo test --test live_vscode -- --ignored --nocapture

use prismflow_lib::adapters::vscode;
use serde_json::json;

#[tokio::test]
#[ignore]
async fn vscode_chat_tool_loop() {
    let Ok(broker) = std::env::var("PRISMFLOW_LIVE_BROKER") else { return };
    let broker = std::path::PathBuf::from(broker);
    let url = std::fs::read_to_string(broker.join("url")).unwrap();
    let token = std::fs::read_to_string(broker.join("token")).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PRISMFLOW_VSCODE_USER_DIR", tmp.path());
    std::fs::write(vscode::chat_models_file(), r#"[{"name":"Mine","vendor":"customendpoint","models":[]}]"#).unwrap();
    let plan = vscode::plan_chat("vscode_chat:ollama", "Ollama", "qwen3:4b-instruct-2507-q4_K_M", &url, &token, Some(32_768)).unwrap();
    std::fs::write(&plan.file, &plan.after).unwrap();
    let model = vscode::chat_model("vscode_chat:ollama").unwrap();
    let client = reqwest::Client::new();

    let nonce = format!("PF{}", uuid::Uuid::new_v4().as_u128() % 1_000_000);
    let ask = json!([{ "role": "user", "content": "Use the read_file tool to read notes.txt, then reply with exactly the text the tool returned and nothing else." }]);
    let first = vscode::chat_request(&client, &model, ask.clone(), true).await.unwrap();
    eprintln!("turn 1: {first}");
    let call = first["content"].as_array().unwrap().iter().find(|b| b["type"] == "tool_use").expect("tool_use").clone();
    let mut msgs = ask.as_array().unwrap().clone();
    msgs.push(json!({ "role": "assistant", "content": first["content"] }));
    msgs.push(json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": call["id"], "content": nonce }] }));
    let second = vscode::chat_request(&client, &model, json!(msgs), true).await.unwrap();
    eprintln!("turn 2: {second}");
    assert!(second.to_string().contains(&nonce), "nonce not in final answer");
    vscode::disconnect_chat("vscode_chat:ollama").unwrap();
    assert_eq!(std::fs::read_to_string(vscode::chat_models_file()).unwrap().matches("customendpoint").count(), 1);
    eprintln!("VSCODE CHAT LIVE PASS");
}
