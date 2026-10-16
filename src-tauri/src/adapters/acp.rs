//! A minimal Agent Client Protocol client, used to test ACP agents the way
//! an IDE drives them: `initialize` → `session/new` → `session/prompt`, over
//! JSON-RPC lines on stdio. https://agentclientprotocol.com
//!
//! Permission requests are answered with the first "allow" option and file
//! reads are served from disk, like an IDE that trusts the folder.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct AcpRun {
    /// The agent's reply text (all `agent_message_chunk`s joined).
    pub text: String,
    /// `session/update` kinds seen, e.g. `tool_call`.
    pub updates: Vec<String>,
}

pub async fn prompt(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    cwd: &Path,
    text: &str,
    timeout: Duration,
) -> Result<AcpRun, String> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .env_clear()
        .envs(["PATH", "HOME", "TMPDIR", "LANG", "USER", "SHELL"].iter().filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v))))
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::proctree::own_group(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start the agent: {e}"))?;
    // The agent is stopped when this returns, with whatever it started.
    let _tree = crate::proctree::TreeGuard::new(child.id());
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
    let stderr = child.stderr.take();
    let err_tail = tokio::spawn(async move {
        let mut tail: Vec<String> = Vec::new();
        if let Some(e) = stderr {
            let mut l = BufReader::new(e).lines();
            while let Ok(Some(line)) = l.next_line().await {
                tail.push(line);
                if tail.len() > 8 {
                    tail.remove(0);
                }
            }
        }
        tail.join("\n")
    });

    let mut run = AcpRun { text: String::new(), updates: Vec::new() };
    let mut next_id = 0u64;
    let work = async {
        let mut call = |method: &str, params: Value| {
            next_id += 1;
            (next_id, json!({ "jsonrpc": "2.0", "id": next_id, "method": method, "params": params }))
        };
        let (id, msg) = call("initialize", json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "prismflow", "version": env!("CARGO_PKG_VERSION") }
        }));
        exchange(&mut stdin, &mut lines, msg, id, &mut run).await?;
        let (id, msg) = call("session/new", json!({ "cwd": cwd.display().to_string(), "mcpServers": [] }));
        let session = exchange(&mut stdin, &mut lines, msg, id, &mut run).await?;
        let sid = session["sessionId"].as_str().ok_or("the agent didn't start a session")?.to_string();
        let (id, msg) = call("session/prompt", json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": text }] }));
        exchange(&mut stdin, &mut lines, msg, id, &mut run).await?;
        Ok::<(), String>(())
    };
    let result = tokio::time::timeout(timeout, work).await;
    let _ = child.kill().await;
    let tail = err_tail.await.unwrap_or_default();
    match result {
        Err(_) => Err(format!("the agent didn't finish within {}s", timeout.as_secs())),
        Ok(Err(e)) if tail.trim().is_empty() => Err(e),
        Ok(Err(e)) => Err(format!("{e}\n{}", crate::diagnostics::redact(&tail))),
        Ok(Ok(())) => Ok(run),
    }
}

type Lines = tokio::io::Lines<BufReader<tokio::process::ChildStdout>>;

async fn send(stdin: &mut tokio::process::ChildStdin, v: &Value) -> Result<(), String> {
    stdin.write_all(format!("{v}\n").as_bytes()).await.map_err(|_| "the agent closed its input".to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())
}

/// Sends a request and serves the agent's own requests until the answer comes.
async fn exchange(stdin: &mut tokio::process::ChildStdin, lines: &mut Lines, msg: Value, id: u64, run: &mut AcpRun) -> Result<Value, String> {
    let method = msg["method"].as_str().unwrap_or("").to_string();
    send(stdin, &msg).await?;
    loop {
        let line = lines.next_line().await.map_err(|e| e.to_string())?.ok_or_else(|| format!("the agent exited during {method}"))?;
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        if v["id"] == id && v.get("method").is_none() {
            if let Some(e) = v.get("error") {
                return Err(format!("{method} failed: {}", e["message"].as_str().unwrap_or("error")));
            }
            return Ok(v["result"].clone());
        }
        match v["method"].as_str() {
            Some("session/update") => {
                let u = &v["params"]["update"];
                if let Some(k) = u["sessionUpdate"].as_str() {
                    run.updates.push(k.to_string());
                }
                if u["sessionUpdate"] == "agent_message_chunk" && u["content"]["type"] == "text" {
                    run.text.push_str(u["content"]["text"].as_str().unwrap_or(""));
                }
            }
            Some("session/request_permission") => {
                let opts = v["params"]["options"].as_array().cloned().unwrap_or_default();
                let pick = opts.iter().find(|o| o["kind"] == "allow_once")
                    .or_else(|| opts.iter().find(|o| o["kind"].as_str().is_some_and(|k| k.starts_with("allow"))))
                    .or(opts.first());
                let reply = match pick {
                    Some(o) => json!({ "jsonrpc": "2.0", "id": v["id"], "result": { "outcome": { "outcome": "selected", "optionId": o["optionId"] } } }),
                    None => json!({ "jsonrpc": "2.0", "id": v["id"], "result": { "outcome": { "outcome": "cancelled" } } }),
                };
                send(stdin, &reply).await?;
            }
            Some("fs/read_text_file") => {
                let reply = match std::fs::read_to_string(v["params"]["path"].as_str().unwrap_or("")) {
                    Ok(content) => json!({ "jsonrpc": "2.0", "id": v["id"], "result": { "content": content } }),
                    Err(e) => json!({ "jsonrpc": "2.0", "id": v["id"], "error": { "code": -32000, "message": e.to_string() } }),
                };
                send(stdin, &reply).await?;
            }
            Some(_) if v.get("id").is_some() => {
                send(stdin, &json!({ "jsonrpc": "2.0", "id": v["id"], "error": { "code": -32601, "message": "not supported" } })).await?;
            }
            _ => {}
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A shell-script ACP agent: answers the handshake, asks permission,
    /// reads a file through the client, then replies with its contents.
    const FAKE: &str = r#"#!/bin/sh
read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read l; echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s1"}}'
read l
echo '{"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"options":[{"optionId":"no","kind":"reject_once"},{"optionId":"yes","kind":"allow_once"}]}}'
read perm
case "$perm" in *'"optionId":"yes"'*) ;; *) exit 3;; esac
echo "{\"jsonrpc\":\"2.0\",\"id\":\"r1\",\"method\":\"fs/read_text_file\",\"params\":{\"path\":\"$PWD/n.txt\"}}"
read file
nonce=$(echo "$file" | sed 's/.*"content":"\([^"\\]*\).*/\1/')
echo '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call"}}}'
echo "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"$nonce\"}}}}"
echo '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
"#;

    #[tokio::test]
    async fn drives_an_agent_like_an_ide() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent.sh");
        std::fs::write(&agent, FAKE).unwrap();
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.path().join("n.txt"), "PFNONCE123").unwrap();
        let run = prompt(&agent.display().to_string(), &[], &HashMap::new(), dir.path(), "read n.txt", Duration::from_secs(10)).await.unwrap();
        assert_eq!(run.text, "PFNONCE123");
        assert!(run.updates.contains(&"tool_call".to_string()));
    }

    #[tokio::test]
    async fn reports_an_agent_that_exits() {
        let dir = tempfile::tempdir().unwrap();
        let e = prompt("/usr/bin/false", &[], &HashMap::new(), dir.path(), "x", Duration::from_secs(5)).await.err().unwrap();
        assert!(e.contains("exited") || e.contains("closed"), "{e}");
    }
}
