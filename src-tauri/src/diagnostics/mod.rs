//! Sanitized diagnostics. Every message is redacted before
//! it is stored; known secrets are scrubbed verbatim and anything shaped like
//! a provider key or bearer token is masked heuristically.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const REDACTED: &str = "[REDACTED]";
const MAX_EVENTS: usize = 500;
/// Prefixes of provider key formats (OpenAI/OpenRouter/Anthropic `sk-`,
/// Hugging Face `hf_`, NVIDIA `nvapi-`, Google `AIza`, PrismFlow local tokens `pft_`).
const KEY_PREFIXES: &[&str] = &["sk-", "hf_", "nvapi-", "AIza", "pft_"];
const MIN_KEY_LEN: usize = 12;

fn looks_like_key(token: &str) -> bool {
    token.len() >= MIN_KEY_LEN
        && KEY_PREFIXES.iter().any(|p| token.starts_with(p))
        && token[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Mask key-shaped tokens and bearer credentials in free text.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let mut after_bearer = false;
    let flush = |token: &mut String, out: &mut String, after_bearer: &mut bool| {
        if token.is_empty() {
            return;
        }
        if *after_bearer || looks_like_key(token) {
            out.push_str(REDACTED);
        } else {
            out.push_str(token);
        }
        *after_bearer = token.eq_ignore_ascii_case("bearer");
        token.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            token.push(c);
        } else {
            flush(&mut token, &mut out, &mut after_bearer);
            if !(c == ' ' || c == '\t') {
                after_bearer = false;
            }
            out.push(c);
        }
    }
    flush(&mut token, &mut out, &mut after_bearer);
    out
}

/// Scrub exact known secrets, then apply heuristic redaction.
pub fn redact_known(text: &str, secrets: &[&str]) -> String {
    let mut s = text.to_string();
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        s = s.replace(secret, REDACTED);
    }
    redact(&s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: u64,
    pub level: Level,
    /// Stable machine-readable code, e.g. `connect.applied`.
    pub code: String,
    pub message: String,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Append-only JSONL log, capped at `MAX_EVENTS`, local to the state dir.
pub struct DiagnosticsLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl DiagnosticsLog {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: Mutex::new(()) }
    }

    pub fn record(&self, level: Level, code: &str, message: &str) {
        let event = Event {
            ts: now_secs(),
            level,
            code: code.to_string(),
            message: redact(message),
        };
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(line) = serde_json::to_string(&event) else { return };
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{line}");
        }
        self.trim();
    }

    fn trim(&self) {
        let Ok(content) = fs::read_to_string(&self.path) else { return };
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > MAX_EVENTS {
            let kept = lines[lines.len() - MAX_EVENTS..].join("\n") + "\n";
            let _ = crate::config_engine::atomic_write(&self.path, &kept);
        }
    }

    /// Most recent events, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Event> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let content = fs::read_to_string(&self.path).unwrap_or_default();
        content
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str(l).ok())
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_key_shapes() {
        let s = redact("key sk-or-v1-abcdef1234567890 and hf_ABCDEFGHIJKLMN failed");
        assert_eq!(s, "key [REDACTED] and [REDACTED] failed");
        assert_eq!(redact("nvapi-0123456789abcdef"), "[REDACTED]");
    }

    #[test]
    fn redacts_bearer_values_of_any_shape() {
        assert_eq!(
            redact("Authorization: Bearer abc.def-ghi"),
            "Authorization: Bearer [REDACTED]"
        );
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        let t = "model openai/gpt-oss-20b returned 404 at https://openrouter.ai/api/v1";
        assert_eq!(redact(t), t);
        assert_eq!(redact("sk-short"), "sk-short");
    }

    #[test]
    fn redact_known_scrubs_exact_secret() {
        assert_eq!(redact_known("echo weird!secret!value", &["weird!secret!value"]), "echo [REDACTED]");
    }

    #[test]
    fn log_stores_redacted_messages_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let log = DiagnosticsLog::new(dir.path().join("d.jsonl"));
        log.record(Level::Info, "a", "first");
        log.record(Level::Error, "b", "leaked sk-ant-api03-zzzzzzzzzzzz");
        let events = log.recent(10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, "b");
        assert!(!events[0].message.contains("sk-ant"));
        let raw = fs::read_to_string(dir.path().join("d.jsonl")).unwrap();
        assert!(!raw.contains("zzzzzzzzzzzz"));
    }

    #[test]
    fn log_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let log = DiagnosticsLog::new(dir.path().join("d.jsonl"));
        for i in 0..(MAX_EVENTS + 5) {
            log.record(Level::Info, "n", &i.to_string());
        }
        assert_eq!(log.recent(10_000).len(), MAX_EVENTS);
    }
}
