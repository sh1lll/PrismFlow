//! Manual live check of the JetBrains integration, no Keychain involved:
//! PrismFlow plans and writes the acp.json entry, then talks ACP to that
//! exact entry the way the IDE (and PrismFlow's Test button) does.
//!
//! Needs a running broker from `tests/live_broker.rs` and a downloaded
//! adapter:
//! PRISMFLOW_LIVE_BROKER=<dir with url+token> PRISMFLOW_ACP_CLAUDE_JS=<…/dist/index.js>
//!   cargo test --test live_jetbrains -- --ignored --nocapture

use prismflow_lib::adapters::{acp, agents, claude_code, jetbrains};
use std::time::Duration;

#[tokio::test]
#[ignore]
async fn claude_agent_through_acp_json() {
    let Ok(broker) = std::env::var("PRISMFLOW_LIVE_BROKER") else { return };
    let broker = std::path::PathBuf::from(broker);
    let url = std::fs::read_to_string(broker.join("url")).unwrap();
    let token = std::fs::read_to_string(broker.join("token")).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let acp_file = tmp.path().join("acp.json");
    std::fs::write(&acp_file, r#"{"agent_servers":{"Someone else's agent":{"command":"x"}}}"#).unwrap();
    std::env::set_var("PRISMFLOW_JETBRAINS_ACP_FILE", &acp_file);

    // Stands in for PrismFlow's own helper (`token print --connection …`).
    let helper = tmp.path().join("helper");
    std::fs::write(&helper, format!("#!/bin/sh\nprintf %s '{token}'\n")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let id = "jetbrains:ollama";
    let p = prismflow_lib::providers::get("ollama").unwrap();
    let dir = tmp.path().join("owned");
    let settings = claude_code::plan_connect(&dir.join("claude"), p, "qwen3:4b-instruct-2507-q4_K_M", &helper, id, &url, &Default::default()).unwrap().after;
    let claude = jetbrains::launch(jetbrains::CLAUDE_PKG, jetbrains::CLAUDE_VERSION, "PRISMFLOW_ACP_CLAUDE_JS").unwrap();
    let codex = ("/usr/bin/true".to_string(), vec![]);
    let plan = jetbrains::plan(&dir, p.display_name, id, &url, settings, String::new(), claude, codex).unwrap();
    assert!(!plan.after.contains(&token), "token must not be in acp.json");
    std::fs::write(&acp_file, &plan.after).unwrap();
    agents::write(&plan.owned).unwrap();

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let nonce = format!("PF{}", uuid::Uuid::new_v4().simple());
    std::fs::write(work.join("prismflow_probe.txt"), format!("{nonce}\n")).unwrap();
    let (cmd, args, env) = jetbrains::entry(id, "Claude Code (PrismFlow").expect("entry written");
    eprintln!("running {cmd} {args:?}");
    let prompt = "Use your file-reading tool to read the file prismflow_probe.txt in the current directory, then reply with its exact contents and nothing else.";
    let r = acp::prompt(&cmd, &args, &env, &work, prompt, Duration::from_secs(600)).await.unwrap();
    eprintln!("agent said: {:?}\nupdates: {:?}", r.text, r.updates.iter().collect::<std::collections::BTreeSet<_>>());
    assert!(r.text.contains(&nonce), "nonce not in reply");

    jetbrains::disconnect(id).unwrap();
    let left: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&acp_file).unwrap()).unwrap();
    assert_eq!(left["agent_servers"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["Someone else's agent"]);
    eprintln!("JETBRAINS LIVE PASS");
}
