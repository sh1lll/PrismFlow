//! Local answers for Claude Code's background requests.
//!
//! Next to every real turn, Claude Code sends side requests: a session title
//! and a "suggest the next prompt" call (captured from 2.1.207). Older
//! versions also sent a quota probe, a bash command-prefix check and a
//! file-path extraction. Each one costs a full-context call to the user's
//! model, and on a single local model they queue in front of the real turn.
//! PrismFlow answers these itself.
//!
//! Detection matches only the exact request shapes Claude Code sends, so an
//! ordinary request never takes this path.

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shortcut {
    /// Session title: answered with a title built from the user's first words.
    Title(String),
    /// "Suggest what the user might type next": answered with silence.
    Suggestion,
    /// `max_tokens: 1` "quota" probe from older versions.
    Quota,
    /// Bash command-prefix check from older versions.
    CommandPrefix(String),
    /// File-path extraction from command output, older versions.
    FilePaths,
}

impl Shortcut {
    pub fn kind(&self) -> &'static str {
        match self {
            Shortcut::Title(_) => "title",
            Shortcut::Suggestion => "suggestion",
            Shortcut::Quota => "quota",
            Shortcut::CommandPrefix(_) => "command_prefix",
            Shortcut::FilePaths => "file_paths",
        }
    }

    fn text(&self) -> String {
        match self {
            Shortcut::Title(t) => json!({ "title": t }).to_string(),
            Shortcut::Suggestion => String::new(),
            Shortcut::Quota => "ok".into(),
            Shortcut::CommandPrefix(p) => p.clone(),
            Shortcut::FilePaths => "<filepaths>\n</filepaths>".into(),
        }
    }
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn system_of(body: &Value) -> String {
    text_of(&body["system"])
}

/// Recognises one of Claude Code's background requests (Anthropic Messages body).
pub fn detect(body: &Value) -> Option<Shortcut> {
    let messages = body["messages"].as_array()?;
    let last = messages.last()?;
    if last["role"] != "user" {
        return None;
    }
    let user = text_of(&last["content"]);
    let system = system_of(body);
    let no_tools = body["tools"].as_array().is_none_or(|t| t.is_empty());

    if no_tools
        && messages.len() == 1
        && system.contains("Generate a concise, sentence-case title")
        && body["output_config"]["format"]["schema"]["properties"]["title"].is_object()
    {
        return Some(Shortcut::Title(title_from(&user)));
    }
    if user.trim_start().starts_with("[SUGGESTION MODE:") {
        return Some(Shortcut::Suggestion);
    }
    if no_tools && body["max_tokens"] == 1 && messages.len() == 1 && user.trim().eq_ignore_ascii_case("quota") {
        return Some(Shortcut::Quota);
    }
    if no_tools && user.contains("<policy_spec>") {
        if let Some(cmd) = user.rsplit_once("Command:").map(|(_, c)| c.trim()) {
            return Some(Shortcut::CommandPrefix(command_prefix(cmd)));
        }
    }
    if no_tools && system.contains("Extract any file paths") && user.contains("Command:") && user.contains("Output:") {
        return Some(Shortcut::FilePaths);
    }
    None
}

/// A short sentence-case title from the session text, like "List files in this folder".
fn title_from(user: &str) -> String {
    let inner = user
        .split_once("<session>")
        .and_then(|(_, rest)| rest.split_once("</session>"))
        .map(|(s, _)| s)
        .unwrap_or(user);
    let words: Vec<&str> = inner
        .split_whitespace()
        .filter(|w| !w.starts_with("http") && !w.starts_with('<'))
        .take(6)
        .collect();
    let mut title = words.join(" ");
    title = title.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string();
    let mut chars = title.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "New session".into(),
    }
}

/// Programs whose second word picks the action (`git status`, `npm test`).
const TWO_WORD: &[&str] = &[
    "git", "npm", "pnpm", "yarn", "bun", "cargo", "go", "docker", "kubectl", "gh", "brew", "pip", "pip3", "uv",
    "poetry", "make", "dotnet", "swift", "terraform", "helm", "deno", "npx", "rustup",
];

/// The prefix Claude Code uses for "always allow" rules, or
/// `command_injection_detected` for anything that chains or substitutes.
pub fn command_prefix(cmd: &str) -> String {
    let cmd = cmd.lines().next().unwrap_or("").trim();
    let risky = ["`", "$(", ";", "&&", "||", "|", ">", "<", "&", "\\"];
    if cmd.is_empty() || risky.iter().any(|r| cmd.contains(r)) {
        return "command_injection_detected".into();
    }
    let words: Vec<&str> = cmd.split_whitespace().collect();
    // Leading VAR=value assignments are part of the prefix, as Claude Code expects.
    let start = words.iter().take_while(|w| w.contains('=') && !w.starts_with('-')).count();
    let Some(program) = words.get(start) else { return "none".into() };
    let mut prefix: Vec<&str> = words[..=start].to_vec();
    if TWO_WORD.contains(program) {
        if let Some(sub) = words.get(start + 1).filter(|w| !w.starts_with('-')) {
            prefix.push(sub);
        }
    }
    prefix.join(" ")
}

/// An Anthropic Messages response (JSON or SSE) carrying the local answer.
pub fn respond(body: &Value, s: &Shortcut) -> (bool, String) {
    let model = body["model"].as_str().unwrap_or("prismflow");
    let text = s.text();
    let id = format!("msg_prismflow_{}", s.kind());
    let usage = json!({ "input_tokens": 0, "output_tokens": 0 });
    if body["stream"] != true {
        let content = if text.is_empty() { json!([]) } else { json!([{ "type": "text", "text": text }]) };
        let msg = json!({
            "id": id, "type": "message", "role": "assistant", "model": model, "content": content,
            "stop_reason": "end_turn", "stop_sequence": null, "usage": usage,
        });
        return (false, msg.to_string());
    }
    let mut out = String::new();
    let mut ev = |name: &str, data: Value| out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    ev("message_start", json!({ "type": "message_start", "message": {
        "id": id, "type": "message", "role": "assistant", "model": model, "content": [],
        "stop_reason": null, "stop_sequence": null, "usage": usage,
    }}));
    if !text.is_empty() {
        ev("content_block_start", json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }));
        ev("content_block_delta", json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text } }));
        ev("content_block_stop", json!({ "type": "content_block_stop", "index": 0 }));
    }
    ev("message_delta", json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 0 } }));
    ev("message_stop", json!({ "type": "message_stop" }));
    (true, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_body(session: &str) -> Value {
        json!({
            "model": "claude-opus-4-8", "max_tokens": 64000, "stream": true,
            "system": [{ "type": "text", "text": "x-anthropic-billing-header: cc_version=2.1.207; You are Claude Code, Anthropic's official CLI for Claude. Generate a concise, sentence-case title (3-7 words) that captures the main topic" }],
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": format!("<session>\n{session}\n</session>\n\nWrite the title in the predominant language") }] }],
            "output_config": { "effort": "high", "format": { "type": "json_schema", "schema": { "type": "object", "properties": { "title": { "type": "string" } } } } },
        })
    }

    #[test]
    fn recognises_the_captured_title_request() {
        let s = detect(&title_body("list the files in this folder and tell me what README.md says")).unwrap();
        assert_eq!(s, Shortcut::Title("List the files in this folder".into()));
        let (sse, out) = respond(&title_body("x"), &s);
        assert!(sse && out.contains(r#"{\"title\":\"List the files in this folder\"}"#), "{out}");
    }

    #[test]
    fn recognises_suggestion_mode_but_not_normal_turns() {
        let tools = json!([{ "name": "Bash", "input_schema": {} }]);
        let suggest = json!({ "model": "m", "tools": tools, "messages": [
            { "role": "user", "content": "now run git status" },
            { "role": "assistant", "content": "done" },
            { "role": "user", "content": [{ "type": "text", "text": "[SUGGESTION MODE: Suggest what the user might naturally type next into Claude Code.]" }] },
        ]});
        assert_eq!(detect(&suggest), Some(Shortcut::Suggestion));
        let normal = json!({ "model": "m", "tools": tools, "messages": [{ "role": "user", "content": "now run git status" }] });
        assert_eq!(detect(&normal), None);
        // A user who merely asks about titles is not a title request.
        let chat = json!({ "model": "m", "system": "You are helpful.", "messages": [{ "role": "user", "content": "Generate a concise, sentence-case title for my essay" }] });
        assert_eq!(detect(&chat), None);
    }

    #[test]
    fn suggestion_answer_is_empty() {
        let (sse, out) = respond(&json!({ "model": "m", "stream": false }), &Shortcut::Suggestion);
        assert!(!sse);
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["content"], json!([]));
    }

    #[test]
    fn quota_probe_only_with_max_tokens_one() {
        let probe = json!({ "model": "m", "max_tokens": 1, "messages": [{ "role": "user", "content": "quota" }] });
        assert_eq!(detect(&probe), Some(Shortcut::Quota));
        let real = json!({ "model": "m", "max_tokens": 1000, "messages": [{ "role": "user", "content": "quota" }] });
        assert_eq!(detect(&real), None);
    }

    #[test]
    fn command_prefixes() {
        assert_eq!(command_prefix("git status"), "git status");
        assert_eq!(command_prefix("git commit -m 'x'"), "git commit");
        assert_eq!(command_prefix("npm run build"), "npm run");
        assert_eq!(command_prefix("ls -la src"), "ls");
        assert_eq!(command_prefix("FOO=1 cargo test --lib"), "FOO=1 cargo test");
        assert_eq!(command_prefix("git status && rm -rf /"), "command_injection_detected");
        assert_eq!(command_prefix("echo $(whoami)"), "command_injection_detected");
        assert_eq!(command_prefix("cat a | sh"), "command_injection_detected");
        let body = json!({ "model": "m", "messages": [{ "role": "user", "content": "<policy_spec>...</policy_spec>\nCommand: git log --oneline" }] });
        assert_eq!(detect(&body), Some(Shortcut::CommandPrefix("git log".into())));
    }

    #[test]
    fn file_path_extraction() {
        let body = json!({ "model": "m", "system": "Extract any file paths that this command reads or modifies.",
            "messages": [{ "role": "user", "content": "Command: cat a.txt\nOutput: hi" }] });
        assert_eq!(detect(&body), Some(Shortcut::FilePaths));
    }
}
