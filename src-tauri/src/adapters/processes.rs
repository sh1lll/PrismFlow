//! Running-agent awareness: a config change does not reach an agent
//! process that is already running, so after Connect, Switch or Disconnect
//! PrismFlow finds sessions still on the old settings and says so.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub command: String,
}

/// Parse `ps -axo pid=,command=` output.
pub fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (pid, cmd) = l.split_once(char::is_whitespace)?;
            Some(Proc { pid: pid.parse().ok()?, command: cmd.trim().to_string() })
        })
        .collect()
}

/// All processes visible to this user. Empty when listing is unsupported.
/// One listing is reused for a few seconds: on Windows it takes about a
/// second, and the Connections page asks once per connection.
pub fn list() -> Vec<Proc> {
    static LAST: std::sync::Mutex<Option<(std::time::Instant, Vec<Proc>)>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, procs)) = last.as_ref().filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(3)) {
        return procs.clone();
    }
    let procs = list_now();
    *last = Some((std::time::Instant::now(), procs.clone()));
    procs
}

fn list_now() -> Vec<Proc> {
    #[cfg(unix)]
    {
        crate::spawn::command("ps")
            .args(["-axo", "pid=,command="])
            .output()
            .ok()
            .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default()
    }
    #[cfg(windows)]
    {
        // `wmic` is deprecated; PowerShell's CIM query gives full command lines.
        crate::spawn::command("powershell")
            .args(["-NoProfile", "-Command",
                "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId) $($_.CommandLine)\" }"])
            .output()
            .ok()
            .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default()
    }
}

fn tokens(cmd: &str) -> Vec<&str> {
    cmd.split_whitespace().map(|t| t.trim_matches(|c| c == '\'' || c == '"')).collect()
}

fn is_program(token: &str, name: &str) -> bool {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    base == name || base == format!("{name}.exe") || base == format!("{name}.js")
}

/// PIDs of interactive Codex sessions using `profile` (`-p`/`--profile`).
/// PrismFlow's own `codex exec` verification runs are excluded.
pub fn codex_sessions(procs: &[Proc], profile: &str) -> Vec<u32> {
    procs
        .iter()
        .filter(|p| {
            let t = tokens(&p.command);
            let is_codex = t.iter().take(3).any(|x| is_program(x, "codex"));
            let uses = t.windows(2).any(|w| (w[0] == "-p" || w[0] == "--profile") && w[1] == profile)
                || t.iter().any(|x| *x == format!("--profile={profile}"));
            is_codex && uses && !t.contains(&"exec")
        })
        .map(|p| p.pid)
        .collect()
}

/// PIDs of Claude Code sessions started with PrismFlow's settings file.
/// Headless `claude -p` verification runs are excluded.
pub fn claude_sessions(procs: &[Proc], settings_file: &str) -> Vec<u32> {
    procs
        .iter()
        .filter(|p| {
            let t = tokens(&p.command);
            let is_claude = t.iter().take(3).any(|x| is_program(x, "claude"));
            is_claude && p.command.contains(settings_file) && !t.contains(&"-p") && !t.contains(&"--print")
        })
        .map(|p| p.pid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "  101 /opt/homebrew/bin/node /opt/homebrew/bin/codex -p prismflow-openrouter
  102 codex --profile prismflow-openrouter
  103 /opt/homebrew/bin/codex exec -p prismflow-openrouter --json hi
  104 codex -p prismflow-other
  105 vim notes-about-codex -p prismflow-openrouter
  201 /Users/a/.local/bin/claude --settings /Users/a/.claude/prismflow-nvidia-nim.settings.json
  202 claude -p --settings /Users/a/.claude/prismflow-nvidia-nim.settings.json hello
  203 claude
";

    #[test]
    fn parses_ps_lines() {
        let p = parse_ps(PS);
        assert_eq!(p.len(), 8);
        assert_eq!(p[0].pid, 101);
        assert!(p[0].command.ends_with("prismflow-openrouter"));
    }

    #[test]
    fn finds_interactive_codex_sessions_only() {
        assert_eq!(codex_sessions(&parse_ps(PS), "prismflow-openrouter"), vec![101, 102]);
    }

    #[test]
    fn finds_interactive_claude_sessions_only() {
        let f = "/Users/a/.claude/prismflow-nvidia-nim.settings.json";
        assert_eq!(claude_sessions(&parse_ps(PS), f), vec![201]);
    }

    #[test]
    fn listing_this_machine_includes_ourselves() {
        #[cfg(unix)]
        assert!(list().iter().any(|p| p.pid == std::process::id()));
    }
}
