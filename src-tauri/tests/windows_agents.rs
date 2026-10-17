//! Windows check with real agents installed from npm: PrismFlow writes the
//! PowerShell launcher and the agent starts through it. Run on Windows CI:
//! PRISMFLOW_WIN_AGENTS="qwen opencode continue_cli" cargo test --test windows_agents -- --ignored

#![cfg(windows)]

use prismflow_lib::adapters::{agents, find_exe};

#[test]
#[ignore]
fn real_agents_start_through_launch_ps1() {
    let ids = std::env::var("PRISMFLOW_WIN_AGENTS").unwrap_or_default();
    let base = tempfile::tempdir().unwrap();
    // A user folder with a space and an accent, run under Windows PowerShell
    // 5.1 too (it reads scripts without a BOM in the old code page).
    let tmp = base.path().join("Ann Lée");
    std::fs::create_dir_all(&tmp).unwrap();
    // Stands in for PrismFlow's own helper (`token print --connection …`).
    let helper = tmp.as_path().join("helper.cmd");
    std::fs::write(&helper, "@echo pft_not_a_real_token\r\n").unwrap();
    for id in ids.split_whitespace() {
        let spec = agents::spec(id).unwrap();
        let bin = find_exe(spec.binary).unwrap_or_else(|| panic!("{} not installed", spec.binary));
        let dir = tmp.as_path().join(id);
        let files = agents::plan_files(spec, &dir, &bin, &helper, &format!("{id}:x"), "http://127.0.0.1:9", "m");
        agents::write(&files).unwrap();
        let (prog, lead) = agents::script_runner(&agents::launch_script(&dir));
        let out = std::process::Command::new(prog).args(lead).arg("--version").output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        eprintln!("{id} via launch.ps1: {}", text.trim());
        assert!(out.status.success(), "{id}: {text}");
        let old = std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(agents::launch_script(&dir)).arg("--version").output().unwrap();
        let old_text = format!("{}{}", String::from_utf8_lossy(&old.stdout), String::from_utf8_lossy(&old.stderr));
        eprintln!("{id} via Windows PowerShell 5.1: {}", old_text.trim());
        assert!(old.status.success(), "{id} under powershell 5.1: {old_text}");
        assert!(text.chars().any(|c| c.is_ascii_digit()), "{id}: no version printed: {text}");
    }
    eprintln!("WINDOWS AGENTS PASS");
}
