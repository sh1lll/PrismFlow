use crate::credentials::{account_key, CredentialStore};
use std::io::Write;

/// Handles CLI invocations. Returns `Some(exit_code)` when the arguments
/// requested a CLI action, or `None` when the GUI should launch.
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
        [cmd, sub, flag, provider]
            if cmd == "credential" && sub == "print" && flag == "--provider" =>
        {
            match store.get(&account_key(provider, "default")) {
                Ok(secret) => {
                    let _ = writeln!(out, "{}", secret);
                    Some(0)
                }
                Err(_) => {
                    eprintln!(
                        "prismflow: no credential stored for provider '{provider}'. Add it in the app under Providers."
                    );
                    Some(2)
                }
            }
        }
        [] => None,
        _ => {
            eprintln!("usage: prismflow [--version | credential print --provider <id>]");
            Some(64)
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
    fn credential_print_writes_secret_to_stdout() {
        let store = InMemoryStore::new();
        store.set(&account_key("openrouter", "default"), "sk-or-test").unwrap();
        let (code, out) = run(&["credential", "print", "--provider", "openrouter"], &store);
        assert_eq!(code, Some(0));
        assert_eq!(out.trim(), "sk-or-test");
    }

    #[test]
    fn missing_credential_exits_2_with_empty_stdout() {
        let (code, out) = run(&["credential", "print", "--provider", "nope"], &InMemoryStore::new());
        assert_eq!(code, Some(2));
        assert!(out.is_empty());
    }

    #[test]
    fn unknown_args_exit_64() {
        let (code, _) = run(&["wat"], &InMemoryStore::new());
        assert_eq!(code, Some(64));
    }
}
