use crate::credentials::CredentialStore;
use std::io::Write;

/// Handles CLI invocations. `token print` gives a connection's local token
/// to the app PrismFlow set up (its config names the connection). Any
/// program running as the same user could ask too, so that token only
/// works on this computer's gateway, is held to PrismFlow's budget and
/// rate limits, and is revoked on disconnect; a provider key is never
/// printed. Returns `Some(exit_code)` when the arguments requested a CLI
/// action, or `None` when the GUI should launch.
pub fn handle_args(
    args: &[String],
    store: &dyn CredentialStore,
    out: &mut dyn Write,
) -> Option<i32> {
    match args {
        [flag] if flag == "--version" || flag == "-V" => {
            let _ = writeln!(out, "prismflow {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        // Older versions let Codex fetch the provider key this way. PrismFlow
        // no longer gives a provider key to any program: apps get their own
        // revocable local token instead. (Connections that used this are
        // moved to the gateway when PrismFlow starts.)
        [cmd, sub, ..] if cmd == "credential" && sub == "print" => {
            eprintln!("prismflow: PrismFlow no longer hands out provider keys. Open PrismFlow and connect this app again.");
            Some(2)
        }
        [cmd, sub, flag, connection]
            if cmd == "token" && sub == "print" && flag == "--connection" =>
        {
            match store.get(&crate::gateway::tokens::account(connection)) {
                Ok(token) => {
                    let _ = writeln!(out, "{}", token);
                    Some(0)
                }
                Err(crate::credentials::CredentialError::NotFound(_)) => {
                    eprintln!(
                        "prismflow: connection '{connection}' has no gateway token (it was disconnected or revoked). Reconnect it in the app."
                    );
                    Some(2)
                }
                Err(e) => {
                    eprintln!("prismflow: {}", crate::credentials::store_problem(&e));
                    Some(3)
                }
            }
        }
        [cmd] if cmd == "self-check" => Some(crate::selfcheck::cli(store, out)),
        [] => None,
        _ => {
            eprintln!("usage: prismflow [--version | self-check | token print --connection <id>]");
            Some(64)
        }
    }
}

/// The Claude Code extension's `claudeProcessWrapper` (shared mode, see
/// adapters/vscode.rs): the extension runs `prismflow <its claude> <args…>`
/// with the settings file in the environment; this adds `--settings`.
/// Returns None when this isn't such a call.
pub fn claude_wrapper(args: &[String]) -> Option<i32> {
    let settings = std::env::var_os(crate::adapters::vscode::SETTINGS_VAR)?;
    let (claude, rest) = match args.split_first() {
        Some((first, rest)) if std::path::Path::new(first).is_file() => (std::path::PathBuf::from(first), rest),
        _ => match crate::adapters::claude_code::find_binary() {
            Some(found) => (found, args),
            None => {
                eprintln!("prismflow: Claude Code wasn't found. Reinstall the Claude Code extension.");
                return Some(127);
            }
        },
    };
    let mut cmd = crate::spawn::command(&claude);
    // Claude's own tools (and PrismFlow's token helper) must not see this.
    cmd.arg("--settings").arg(&settings).args(rest).env_remove(crate::adapters::vscode::SETTINGS_VAR);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec();
        eprintln!("prismflow: couldn't start {}: {e}", claude.display());
        Some(126)
    }
    #[cfg(not(unix))]
    {
        // No exec on Windows: run Claude with the same stdin/stdout, wait,
        // and pass its exit code on. If the extension stops this process,
        // Claude's input closes and it exits too.
        match cmd.status() {
            Ok(s) => Some(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("prismflow: couldn't start {}: {e}", claude.display());
                Some(126)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::memory::InMemoryStore;

    fn run(args: &[&str], store: &dyn CredentialStore) -> (Option<i32>, String) {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut buf: Vec<u8> = Vec::new();
        let code = handle_args(&owned, store, &mut buf);
        (code, String::from_utf8(buf).unwrap())
    }

    #[test]
    fn empty_args_launch_gui() {
        let (code, _) = run(&[], &InMemoryStore::new());
        assert_eq!(code, None);
    }

    #[test]
    fn version_prints_package_version() {
        let (code, out) = run(&["--version"], &InMemoryStore::new());
        assert_eq!(code, Some(0));
        assert!(out.starts_with("prismflow "));
    }

    #[test]
    fn credential_print_never_gives_out_a_provider_key() {
        let store = InMemoryStore::new();
        store.set(&crate::credentials::account_key("openrouter", "default"), "sk-or-test").unwrap();
        for args in [&["credential", "print", "--provider", "openrouter"][..], &["credential", "print"]] {
            let (code, out) = run(args, &store);
            assert_eq!(code, Some(2));
            assert!(out.is_empty(), "nothing on stdout: {out}");
        }
    }

    #[test]
    fn token_print_serves_only_existing_tokens() {
        let store = InMemoryStore::new();
        let token = crate::gateway::tokens::mint(&store, "claude_code:openrouter").unwrap();
        let (code, out) = run(&["token", "print", "--connection", "claude_code:openrouter"], &store);
        assert_eq!(code, Some(0));
        assert_eq!(out.trim(), token);
        crate::gateway::tokens::revoke(&store, "claude_code:openrouter").unwrap();
        let (code, out) = run(&["token", "print", "--connection", "claude_code:openrouter"], &store);
        assert_eq!(code, Some(2));
        assert!(out.is_empty());
    }

    #[test]
    fn unknown_args_exit_64() {
        let (code, _) = run(&["wat"], &InMemoryStore::new());
        assert_eq!(code, Some(64));
    }
}
