//! Manual live check of shared vs separate terminal agents: each agent runs a
//! one-shot prompt through its generated launcher, with HOME and the XDG
//! folders pointed at a scratch home. Shared must write into that home;
//! separate must leave it alone and write into the connection's folder.
//!
//! PRISMFLOW_LIVE_MODES_DIR=<scratch> PRISMFLOW_LIVE_MODES_URL=http://127.0.0.1:11434 \
//! PRISMFLOW_LIVE_MODES_MODEL=<model> PRISMFLOW_LIVE_MODES="qwen qwen_separate …" \
//! PRISMFLOW_<AGENT>_BIN=<binary> [PRISMFLOW_LIVE_MODES_TOKEN=<gateway token>] \
//!   cargo test --test live_agent_modes -- --ignored --nocapture

#![cfg(unix)] // the fake token helper is a POSIX script

use prismflow_lib::adapters::agents;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Written to the usual home even in separate mode, and not chats or
/// settings: Aider's usage stats and price cache (it has no home setting),
/// Copilot CLI's unpacked program and device id, npm's download cache.
const HOME_ANYWAY: &[&str] = &[".aider/", "Library/Caches/copilot/", "Library/Application Support/Microsoft/DeveloperTools/", ".npm/"];

/// Files below `root` (relative).
fn files_under(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    out.sort();
    out
}

#[test]
#[ignore]
fn agents_keep_to_their_mode() {
    let (Ok(root), Ok(url), Ok(model), Ok(ids)) = (
        std::env::var("PRISMFLOW_LIVE_MODES_DIR"),
        std::env::var("PRISMFLOW_LIVE_MODES_URL"),
        std::env::var("PRISMFLOW_LIVE_MODES_MODEL"),
        std::env::var("PRISMFLOW_LIVE_MODES"),
    ) else {
        return;
    };
    let root = PathBuf::from(root);
    let mut failures = Vec::new();
    for id in ids.split_whitespace() {
        let spec = agents::spec(id).unwrap_or_else(|| panic!("unknown agent {id}"));
        let bin = agents::find_binary(spec).unwrap_or_else(|| panic!("{id}: set PRISMFLOW_{}_BIN", spec.family.to_uppercase()));
        let case = root.join(id);
        let _ = std::fs::remove_dir_all(&case);
        let (home, conn, work) = (case.join("home"), case.join("connection"), case.join("work"));
        for d in [&home, &work] {
            std::fs::create_dir_all(d).unwrap();
        }
        // Stands in for PrismFlow's helper (`token print --connection …`).
        let helper = case.join("helper");
        let token = std::env::var("PRISMFLOW_LIVE_MODES_TOKEN").unwrap_or_else(|_| "pft_live_modes".into());
        std::fs::write(&helper, format!("#!/bin/sh\nprintf %s '{token}'\n")).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let files = agents::plan_files(spec, &conn, &bin, &helper, &format!("{id}:live"), &url, &model);
        agents::write(&files).unwrap();
        let before_conn = files_under(&conn);

        let (prog, lead) = agents::script_runner(&agents::launch_script(&conn));
        let mut cmd = tokio::process::Command::new(prog);
        cmd.args(lead).args(agents::oneshot_args(spec, "Reply with the single word PONG."))
            .current_dir(&work)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("TERM", "dumb")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(async { tokio::time::timeout(Duration::from_secs(300), cmd.output()).await });
        let (ok, text) = match out {
            Ok(Ok(o)) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Ok(Err(e)) => (false, e.to_string()),
            Err(_) => (false, "timed out".into()),
        };
        let in_home: Vec<String> = files_under(&home).into_iter()
            .filter(|f| spec.shared || !HOME_ANYWAY.iter().any(|a| f.starts_with(a)))
            .collect();
        let in_conn: Vec<String> = files_under(&conn).into_iter().filter(|f| !before_conn.contains(f)).collect();
        let in_work = files_under(&work);
        let replied = text.to_uppercase().contains("PONG");
        // No agent may save the token where it keeps its own files.
        let saved_token: Vec<String> = [&home, &conn].iter().flat_map(|root| files_under(root).into_iter().map(move |f| root.join(f)))
            .filter(|p| std::fs::read(p).is_ok_and(|b| b.windows(token.len()).any(|w| w == token.as_bytes())))
            .filter(|p| !p.ends_with("droid-settings.json")) // Droid's per-launch settings, by design
            .map(|p| p.display().to_string())
            .collect();
        let _ = std::fs::remove_file(&helper);
        eprintln!("== {id} ({}) ran ok={ok} replied={replied}", if spec.shared { "shared" } else { "separate" });
        eprintln!("   home: {} files, e.g. {:?}", in_home.len(), in_home.iter().take(4).collect::<Vec<_>>());
        eprintln!("   connection (new): {} files, e.g. {:?}", in_conn.len(), in_conn.iter().take(4).collect::<Vec<_>>());
        eprintln!("   project folder: {:?}", in_work);
        if !replied {
            eprintln!("   output: {}", text.lines().rev().take(6).collect::<Vec<_>>().join(" | "));
        }
        let mode_ok = if spec.shared { !in_home.is_empty() } else { in_home.is_empty() && !in_conn.is_empty() };
        if !saved_token.is_empty() {
            eprintln!("   token saved in: {saved_token:?}");
        }
        if !replied || !mode_ok || !saved_token.is_empty() {
            failures.push(format!("{id}: replied={replied} mode_ok={mode_ok} token_saved={}", !saved_token.is_empty()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    eprintln!("LIVE AGENT MODES PASS");
}
