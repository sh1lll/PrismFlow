//! Chat bots: use a Codex code session from Telegram or Discord.
//!
//! A message from an allowed user becomes a turn in that chat's code session
//! (in the folder and with the connection the user picked). Commands:
//! `/stats`, `/stop`, `/clear`, `/help`. Voice notes are transcribed first
//! (`voice`). Bot tokens live in the Keychain; everything else in `bots.json`.
//!
//! Safety: nobody is allowed by default. A bot only answers user IDs on its
//! allow list (and, on Discord, only in allowed channels or DMs), and each
//! user is rate-limited. Off by default.

pub mod discord;
pub mod telegram;
pub mod voice;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TELEGRAM_TOKEN: &str = "bot:telegram";
pub const DISCORD_TOKEN: &str = "bot:discord";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformSettings {
    pub enabled: bool,
    /// Numeric user IDs allowed to use the bot. Empty = nobody.
    pub allowed_users: Vec<String>,
    /// Discord only: channel IDs where the bot answers (DMs always allowed
    /// for allowed users). Empty = DMs only.
    pub allowed_channels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BotSettings {
    pub telegram: PlatformSettings,
    pub discord: PlatformSettings,
    /// The folder the code sessions work in.
    pub folder: String,
    /// A Codex connection id (`codex_cli:<provider>`).
    pub connection_id: String,
    pub allow_edits: bool,
    pub messages_per_minute: u32,
    /// Outgoing proxy for the bot connections (e.g. where Telegram is blocked).
    pub proxy: String,
    pub voice: voice::VoiceSettings,
}

impl Default for BotSettings {
    fn default() -> Self {
        Self {
            telegram: Default::default(),
            discord: Default::default(),
            folder: String::new(),
            connection_id: String::new(),
            allow_edits: false,
            messages_per_minute: 6,
            proxy: String::new(),
            voice: Default::default(),
        }
    }
}

impl BotSettings {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("bots.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::config_engine::atomic_write(&dir.join("bots.json"), &text).map_err(|e| e.to_string())
    }

    /// Plain-language problems that stop the bots from starting.
    pub fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        for (name, s) in [("Telegram", &self.telegram), ("Discord", &self.discord)] {
            if s.enabled && s.allowed_users.iter().all(|u| u.trim().is_empty()) {
                p.push(format!("Add at least one allowed user ID for {name}."));
            }
            if s.allowed_users.iter().chain(&s.allowed_channels).any(|u| !u.trim().chars().all(|c| c.is_ascii_digit())) {
                p.push(format!("{name} IDs are numbers only."));
            }
        }
        if (self.telegram.enabled || self.discord.enabled) && !Path::new(self.folder.trim()).is_dir() {
            p.push("Pick a folder for the bots to work in.".into());
        }
        if (self.telegram.enabled || self.discord.enabled) && self.connection_id.is_empty() {
            p.push("Pick a Codex connection for the bots.".into());
        }
        p
    }
}

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Does the actual work for a chat. The real one runs Codex code sessions.
pub trait Runner: Send + Sync {
    fn run<'a>(&'a self, chat: &'a str, prompt: &'a str) -> BoxFut<'a, Result<String, String>>;
    fn stop(&self, chat: &str) -> Result<(), String>;
    fn clear(&self, chat: &str);
    fn stats(&self, chat: &str) -> String;
}

pub const HELP: &str = "Send a message and I'll work on it in your project folder.\n\
/stop — stop the current task\n/clear — start fresh\n/stats — what this chat has done\n\
You can also send a voice note.";

/// Platform-independent message handling: access, rate limit, commands.
pub struct BotCore {
    pub runner: Arc<dyn Runner>,
    allowed: Vec<String>,
    per_minute: u32,
    recent: Mutex<HashMap<String, Vec<Instant>>>,
    pub handled: Arc<std::sync::atomic::AtomicU64>,
}

pub enum Reply {
    /// Not for us (unknown user, wrong channel): stay silent.
    Ignore,
    /// Answer right away.
    Now(String),
    /// A task: say we're on it, then send the result.
    Task(String),
}

impl BotCore {
    pub fn new(runner: Arc<dyn Runner>, s: &PlatformSettings, per_minute: u32) -> Self {
        Self {
            runner,
            allowed: s.allowed_users.iter().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect(),
            per_minute,
            recent: Mutex::new(HashMap::new()),
            handled: Default::default(),
        }
    }

    pub fn allowed(&self, user: &str) -> bool {
        self.allowed.iter().any(|u| u == user)
    }

    fn over_limit(&self, user: &str) -> bool {
        if self.per_minute == 0 {
            return false;
        }
        let mut m = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let v = m.entry(user.to_string()).or_default();
        v.retain(|t| t.elapsed() < Duration::from_secs(60));
        if v.len() as u32 >= self.per_minute {
            return true;
        }
        v.push(Instant::now());
        false
    }

    /// Decides what to do with one incoming text.
    pub fn classify(&self, chat: &str, user: &str, text: &str) -> Reply {
        if !self.allowed(user) {
            return Reply::Ignore;
        }
        let text = text.trim();
        let cmd = text.split_whitespace().next().unwrap_or("").split('@').next().unwrap_or("");
        match cmd {
            "/start" | "/help" => Reply::Now(HELP.into()),
            "/stats" => Reply::Now(self.runner.stats(chat)),
            "/stop" => Reply::Now(match self.runner.stop(chat) {
                Ok(()) => "Stopped.".into(),
                Err(_) => "Nothing is running.".into(),
            }),
            "/clear" => {
                self.runner.clear(chat);
                Reply::Now("Cleared. The next message starts a new task.".into())
            }
            _ if text.is_empty() => Reply::Ignore,
            _ if self.over_limit(user) => Reply::Now("Too many messages. Wait a minute and try again.".into()),
            _ => {
                self.handled.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Reply::Task(text.to_string())
            }
        }
    }

    pub async fn run(&self, chat: &str, prompt: &str) -> String {
        match self.runner.run(chat, prompt).await {
            Ok(t) if t.trim().is_empty() => "Done.".into(),
            Ok(t) => t,
            Err(e) => format!("Couldn't do that: {e}"),
        }
    }
}

/// Splits a long answer into pieces a platform accepts.
pub fn chunks(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        let mut line = line;
        while cur.chars().count() + line.chars().count() > max {
            if cur.is_empty() {
                let cut: String = line.chars().take(max).collect();
                line = &line[cut.len()..];
                out.push(cut);
            } else {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push_str(line);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// The real runner: one Codex code session per chat.
pub struct CodexRunner {
    pub svc: Arc<crate::service::Service>,
    pub sessions: crate::sessions::Sessions,
    pub folder: PathBuf,
    pub connection_id: String,
    pub sandbox: crate::sessions::Sandbox,
    pub platform: &'static str,
    chats: Mutex<HashMap<String, String>>,
}

impl CodexRunner {
    pub fn new(svc: Arc<crate::service::Service>, sessions: crate::sessions::Sessions, s: &BotSettings, platform: &'static str) -> Self {
        Self {
            svc,
            sessions,
            folder: PathBuf::from(s.folder.trim()),
            connection_id: s.connection_id.clone(),
            sandbox: if s.allow_edits { crate::sessions::Sandbox::WorkspaceWrite } else { crate::sessions::Sandbox::ReadOnly },
            platform,
            chats: Mutex::new(HashMap::new()),
        }
    }

    fn session_for(&self, chat: &str) -> Result<String, String> {
        let mut m = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = m.get(chat).filter(|id| self.sessions.events(id, usize::MAX).is_some()) {
            return Ok(id.clone());
        }
        let s = self.sessions.create(&self.folder, self.sandbox, &format!("{} chat {chat}", self.platform))?;
        m.insert(chat.to_string(), s.id.clone());
        Ok(s.id)
    }
}

/// Turns a finished turn's Codex events into a short chat answer.
pub fn summarize(events: &[serde_json::Value]) -> String {
    let mut parts = Vec::new();
    for v in events {
        let item = &v["item"];
        match (v["type"].as_str(), item["type"].as_str()) {
            (Some("item.completed"), Some("agent_message")) => {
                if let Some(t) = item["text"].as_str() {
                    parts.push(t.trim().to_string());
                }
            }
            (Some("item.completed"), Some("command_execution")) => {
                if let Some(c) = item["command"].as_str() {
                    parts.push(format!("Ran: {}", c.chars().take(200).collect::<String>()));
                }
            }
            (Some("item.completed"), Some("file_change")) => {
                let files: Vec<&str> = item["changes"].as_array().map(|a| a.iter().filter_map(|c| c["path"].as_str()).collect()).unwrap_or_default();
                if !files.is_empty() {
                    parts.push(format!("Changed: {}", files.join(", ")));
                }
            }
            (Some("turn.failed"), _) => parts.push(format!("Failed: {}", v["error"]["message"].as_str().unwrap_or("unknown error"))),
            (Some("error"), _) => parts.push(format!("Error: {}", v["message"].as_str().unwrap_or("unknown error"))),
            _ => {}
        }
    }
    parts.join("\n\n")
}

impl Runner for CodexRunner {
    fn run<'a>(&'a self, chat: &'a str, prompt: &'a str) -> BoxFut<'a, Result<String, String>> {
        Box::pin(async move {
            let id = self.session_for(chat)?;
            let start = self.sessions.events(&id, usize::MAX).map(|p| p.next).unwrap_or(0);
            self.sessions.send(&self.svc, &id, &self.connection_id, prompt).await?;
            loop {
                tokio::time::sleep(Duration::from_millis(700)).await;
                let page = self.sessions.events(&id, start).ok_or("The session was removed.")?;
                if !page.session.running() {
                    let status = page.session.turns.last().map(|t| t.status);
                    let text = summarize(&page.events);
                    return match status {
                        Some(crate::sessions::TurnStatus::Stopped) => Ok(format!("{text}\n\nStopped.").trim().to_string()),
                        _ => Ok(text),
                    };
                }
            }
        })
    }

    fn stop(&self, chat: &str) -> Result<(), String> {
        let id = self.chats.lock().unwrap_or_else(|e| e.into_inner()).get(chat).cloned().ok_or("none")?;
        self.sessions.stop(&id)
    }

    fn clear(&self, chat: &str) {
        self.chats.lock().unwrap_or_else(|e| e.into_inner()).remove(chat);
    }

    fn stats(&self, chat: &str) -> String {
        let id = self.chats.lock().unwrap_or_else(|e| e.into_inner()).get(chat).cloned();
        let Some(page) = id.and_then(|id| self.sessions.events(&id, usize::MAX)) else {
            return format!("No task yet. Folder: {}", self.folder.display());
        };
        let s = page.session;
        let model = s.turns.last().map(|t| t.model.as_str()).unwrap_or("-");
        format!(
            "Messages: {}\nModel: {model}\nFolder: {}\nEdits allowed: {}\nWorking now: {}",
            s.turns.len(),
            s.folder.display(),
            if s.sandbox == crate::sessions::Sandbox::WorkspaceWrite { "yes" } else { "no" },
            if s.running() { "yes" } else { "no" }
        )
    }
}

/// What the Bots page shows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PlatformStatus {
    pub running: bool,
    pub detail: String,
    pub messages: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BotsStatus {
    pub telegram: PlatformStatus,
    pub discord: PlatformStatus,
}

struct Running {
    stop: tokio::sync::watch::Sender<bool>,
    status: Arc<Mutex<PlatformStatus>>,
    handled: Arc<std::sync::atomic::AtomicU64>,
}

/// Starts and stops the platform loops.
#[derive(Default)]
pub struct Bots {
    telegram: tokio::sync::Mutex<Option<Running>>,
    discord: tokio::sync::Mutex<Option<Running>>,
}

pub(crate) fn set_status(s: &Arc<Mutex<PlatformStatus>>, running: bool, detail: impl Into<String>) {
    let mut g = s.lock().unwrap_or_else(|e| e.into_inner());
    g.running = running;
    g.detail = detail.into();
}

pub fn http_client(proxy: &str) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder().timeout(Duration::from_secs(70)).connect_timeout(Duration::from_secs(15));
    if !proxy.trim().is_empty() {
        b = b.proxy(reqwest::Proxy::all(proxy.trim()).map_err(|e| format!("The proxy address isn't valid: {e}"))?);
    }
    b.build().map_err(|e| e.to_string())
}

impl Bots {
    pub async fn status(&self) -> BotsStatus {
        let read = |r: &Option<Running>| {
            r.as_ref()
                .map(|r| {
                    let mut s = r.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    s.messages = r.handled.load(std::sync::atomic::Ordering::Relaxed);
                    s
                })
                .unwrap_or_default()
        };
        BotsStatus { telegram: read(&*self.telegram.lock().await), discord: read(&*self.discord.lock().await) }
    }

    pub async fn stop_all(&self) {
        for slot in [&self.telegram, &self.discord] {
            if let Some(r) = slot.lock().await.take() {
                let _ = r.stop.send(true);
            }
        }
    }

    /// (Re)starts the enabled platforms with `settings`.
    pub async fn apply(
        &self,
        settings: &BotSettings,
        store: &dyn crate::credentials::CredentialStore,
        make_runner: impl Fn(&'static str) -> Arc<dyn Runner>,
        voice_key: Option<String>,
    ) -> Result<(), String> {
        self.stop_all().await;
        let problems = settings.problems();
        if !problems.is_empty() {
            return Err(problems.join(" "));
        }
        let client = http_client(&settings.proxy)?;
        let voice = Arc::new(voice::Transcriber::new(settings.voice.clone(), client.clone(), voice_key));
        if settings.telegram.enabled {
            let token = store.get(TELEGRAM_TOKEN).map_err(|_| "Add your Telegram bot token first.".to_string())?;
            let core = Arc::new(BotCore::new(make_runner("Telegram"), &settings.telegram, settings.messages_per_minute));
            let (tx, rx) = tokio::sync::watch::channel(false);
            let status = Arc::new(Mutex::new(PlatformStatus::default()));
            let handled = core.handled.clone();
            let api = telegram::Api::new(client.clone(), telegram::API_BASE, &token);
            tokio::spawn(telegram::run(api, core, voice.clone(), status.clone(), rx));
            *self.telegram.lock().await = Some(Running { stop: tx, status, handled });
        }
        if settings.discord.enabled {
            let token = store.get(DISCORD_TOKEN).map_err(|_| "Add your Discord bot token first.".to_string())?;
            let core = Arc::new(BotCore::new(make_runner("Discord"), &settings.discord, settings.messages_per_minute));
            let (tx, rx) = tokio::sync::watch::channel(false);
            let status = Arc::new(Mutex::new(PlatformStatus::default()));
            let handled = core.handled.clone();
            let cfg = discord::Config {
                api: discord::API_BASE.into(),
                token,
                channels: settings.discord.allowed_channels.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
            };
            tokio::spawn(discord::run(client.clone(), cfg, core, voice.clone(), status.clone(), rx));
            *self.discord.lock().await = Some(Running { stop: tx, status, handled });
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Echoes prompts; counts stops and clears.
    #[derive(Default)]
    pub struct FakeRunner {
        pub prompts: Mutex<Vec<(String, String)>>,
    }

    impl Runner for FakeRunner {
        fn run<'a>(&'a self, chat: &'a str, prompt: &'a str) -> BoxFut<'a, Result<String, String>> {
            self.prompts.lock().unwrap().push((chat.into(), prompt.into()));
            Box::pin(async move { Ok(format!("did: {prompt}")) })
        }
        fn stop(&self, _: &str) -> Result<(), String> {
            Err("none".into())
        }
        fn clear(&self, _: &str) {}
        fn stats(&self, chat: &str) -> String {
            format!("stats for {chat}")
        }
    }

    fn core(per_minute: u32) -> BotCore {
        let s = PlatformSettings { enabled: true, allowed_users: vec!["42".into()], allowed_channels: vec![] };
        BotCore::new(Arc::new(FakeRunner::default()), &s, per_minute)
    }

    #[test]
    fn strangers_are_ignored_and_commands_answered() {
        let c = core(0);
        assert!(matches!(c.classify("c", "7", "rm -rf"), Reply::Ignore));
        assert!(matches!(c.classify("c", "42", "/stop"), Reply::Now(t) if t == "Nothing is running."));
        assert!(matches!(c.classify("c", "42", "/stats@my_bot"), Reply::Now(t) if t == "stats for c"));
        assert!(matches!(c.classify("c", "42", "fix the test"), Reply::Task(t) if t == "fix the test"));
    }

    #[test]
    fn rate_limit_applies_per_user() {
        let c = core(2);
        assert!(matches!(c.classify("c", "42", "a"), Reply::Task(_)));
        assert!(matches!(c.classify("c", "42", "b"), Reply::Task(_)));
        assert!(matches!(c.classify("c", "42", "c"), Reply::Now(t) if t.starts_with("Too many")));
    }

    #[test]
    fn chunks_respect_the_limit() {
        let text = "a".repeat(10) + "\n" + &"b".repeat(25);
        let c = chunks(&text, 12);
        assert!(c.iter().all(|x| x.chars().count() <= 12), "{c:?}");
        assert_eq!(c.concat(), text);
    }

    #[test]
    fn summary_reads_codex_events() {
        let ev = vec![
            serde_json::json!({"type": "item.completed", "item": {"type": "command_execution", "command": "ls"}}),
            serde_json::json!({"type": "item.completed", "item": {"type": "agent_message", "text": "Two files."}}),
        ];
        assert_eq!(summarize(&ev), "Ran: ls\n\nTwo files.");
    }

    #[test]
    fn settings_need_an_allow_list() {
        let mut s = BotSettings::default();
        s.telegram.enabled = true;
        s.folder = std::env::temp_dir().display().to_string();
        s.connection_id = "codex_cli:ollama".into();
        assert!(s.problems().iter().any(|p| p.contains("allowed user")));
        s.telegram.allowed_users = vec!["12a".into()];
        assert!(s.problems().iter().any(|p| p.contains("numbers")));
        s.telegram.allowed_users = vec!["123".into()];
        assert!(s.problems().is_empty());
    }
}
