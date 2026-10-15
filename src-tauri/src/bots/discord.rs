//! Discord: receives messages over the Gateway WebSocket, answers over REST.
//! https://discord.com/developers/docs/events/gateway
//!
//! The bot needs the Message Content intent (Developer Portal → Bot). It
//! answers allowed users in DMs, and in server channels only when the
//! channel is on the allow list.

use super::{chunks, set_status, BotCore, PlatformStatus, Reply};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub const API_BASE: &str = "https://discord.com/api/v10";
const MAX_MESSAGE: usize = 1900;
/// GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT
const INTENTS: u64 = (1 << 9) | (1 << 12) | (1 << 15);

#[derive(Clone)]
pub struct Config {
    pub api: String,
    pub token: String,
    pub channels: Vec<String>,
}

#[derive(Clone)]
struct Rest {
    client: reqwest::Client,
    api: String,
    auth: String,
}

impl Rest {
    async fn send(&self, channel: &str, text: &str) {
        for part in chunks(text, MAX_MESSAGE) {
            let _ = self.client.post(format!("{}/channels/{channel}/messages", self.api))
                .header("authorization", &self.auth)
                .json(&json!({ "content": part, "allowed_mentions": { "parse": [] } }))
                .send().await;
        }
    }

    async fn typing(&self, channel: &str) {
        let _ = self.client.post(format!("{}/channels/{channel}/typing", self.api)).header("authorization", &self.auth).send().await;
    }
}

enum End {
    Stop,
    Fatal(String),
    Retry(String),
}

pub async fn run(
    client: reqwest::Client,
    cfg: Config,
    core: Arc<BotCore>,
    voice: Arc<super::voice::Transcriber>,
    status: Arc<Mutex<PlatformStatus>>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let rest = Rest { client: client.clone(), api: cfg.api.trim_end_matches('/').into(), auth: format!("Bot {}", cfg.token.trim()) };
    let mut backoff = 1u64;
    loop {
        match session(&rest, &cfg, &core, &voice, &status, &mut stop).await {
            End::Stop => break,
            End::Fatal(e) => return set_status(&status, false, e),
            End::Retry(e) => {
                set_status(&status, true, format!("Reconnecting: {e}"));
                tokio::select! {
                    _ = stop.changed() => break,
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                }
                backoff = (backoff * 2).min(60);
            }
        }
    }
    set_status(&status, false, "Stopped");
}

async fn session(
    rest: &Rest,
    cfg: &Config,
    core: &Arc<BotCore>,
    voice: &Arc<super::voice::Transcriber>,
    status: &Arc<Mutex<PlatformStatus>>,
    stop: &mut tokio::sync::watch::Receiver<bool>,
) -> End {
    let r = match rest.client.get(format!("{}/gateway/bot", rest.api)).header("authorization", &rest.auth).send().await {
        Ok(r) => r,
        Err(_) => return End::Retry("Couldn't reach Discord".into()),
    };
    if r.status().as_u16() == 401 {
        return End::Fatal("Discord rejected the bot token.".into());
    }
    let Some(url) = r.json::<Value>().await.ok().and_then(|v| v["url"].as_str().map(String::from)) else {
        return End::Retry("Discord sent an unreadable answer".into());
    };
    let (ws, _) = match tokio_tungstenite::connect_async(format!("{url}/?v=10&encoding=json")).await {
        Ok(x) => x,
        Err(_) => return End::Retry("Couldn't open Discord's live connection".into()),
    };
    let (mut tx, mut rx) = ws.split();
    let mut seq: Option<u64> = None;
    let mut beat = tokio::time::interval(Duration::from_secs(3600));
    beat.tick().await;
    loop {
        tokio::select! {
            _ = stop.changed() => { let _ = tx.close().await; return End::Stop; }
            _ = beat.tick() => {
                if tx.send(Message::text(json!({ "op": 1, "d": seq }).to_string())).await.is_err() {
                    return End::Retry("connection lost".into());
                }
            }
            msg = rx.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.map(|f| u16::from(f.code)).unwrap_or(0);
                        return match code {
                            4004 => End::Fatal("Discord rejected the bot token.".into()),
                            4014 => End::Fatal("Turn on the Message Content intent for this bot in Discord's Developer Portal.".into()),
                            _ => End::Retry(format!("Discord closed the connection ({code})")),
                        };
                    }
                    Some(Ok(_)) => continue,
                    _ => return End::Retry("connection lost".into()),
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(s) = v["s"].as_u64() { seq = Some(s); }
                match v["op"].as_u64() {
                    Some(10) => {
                        let ms = v["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250);
                        beat = tokio::time::interval(Duration::from_millis(ms));
                        beat.tick().await;
                        let identify = json!({ "op": 2, "d": {
                            "token": cfg.token.trim(), "intents": INTENTS,
                            "properties": { "os": std::env::consts::OS, "browser": "prismflow", "device": "prismflow" }
                        }});
                        if tx.send(Message::text(identify.to_string())).await.is_err() {
                            return End::Retry("connection lost".into());
                        }
                    }
                    Some(1) => { let _ = tx.send(Message::text(json!({ "op": 1, "d": seq }).to_string())).await; }
                    Some(7) | Some(9) => return End::Retry("Discord asked to reconnect".into()),
                    Some(0) => match v["t"].as_str() {
                        Some("READY") => {
                            let name = v["d"]["user"]["username"].as_str().unwrap_or("bot");
                            set_status(status, true, format!("Running as {name}"));
                        }
                        Some("MESSAGE_CREATE") => {
                            let (rest, core, voice, channels) = (rest.clone(), core.clone(), voice.clone(), cfg.channels.clone());
                            let d = v["d"].clone();
                            tokio::spawn(async move { handle(&rest, &core, &voice, &channels, &d).await });
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
}

async fn handle(rest: &Rest, core: &BotCore, voice: &super::voice::Transcriber, channels: &[String], d: &Value) {
    if d["author"]["bot"] == true {
        return;
    }
    let (Some(user), Some(channel)) = (d["author"]["id"].as_str(), d["channel_id"].as_str()) else { return };
    let in_dm = d["guild_id"].is_null();
    if !core.allowed(user) || (!in_dm && !channels.iter().any(|c| c == channel)) {
        return;
    }
    let mut text = d["content"].as_str().unwrap_or("").to_string();
    // Drop leading @mentions of the bot.
    while text.trim_start().starts_with("<@") {
        text = text.trim_start().split_once('>').map(|x| x.1.to_string()).unwrap_or_default();
    }
    let audio = d["attachments"].as_array().and_then(|a| {
        a.iter().find(|x| x["content_type"].as_str().is_some_and(|t| t.starts_with("audio/")))
    });
    if text.trim().is_empty() {
        let Some(att) = audio else { return };
        if !voice.enabled() {
            rest.send(channel, "Voice notes are turned off. Turn them on in PrismFlow.").await;
            return;
        }
        rest.typing(channel).await;
        let bytes = match rest.client.get(att["url"].as_str().unwrap_or("")).send().await {
            Ok(r) if r.status().is_success() => r.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string()),
            _ => Err("Couldn't download the voice note.".to_string()),
        };
        match match bytes { Ok(b) => voice.transcribe(b, att["filename"].as_str().unwrap_or("voice.ogg")).await, Err(e) => Err(e) } {
            Ok(t) => {
                rest.send(channel, &format!("Heard: {t}")).await;
                text = t;
            }
            Err(e) => return rest.send(channel, &e).await,
        }
    }
    match core.classify(channel, user, &text) {
        Reply::Ignore => {}
        Reply::Now(t) => rest.send(channel, &t).await,
        Reply::Task(prompt) => {
            rest.send(channel, "Working on it…").await;
            rest.typing(channel).await;
            let answer = core.run(channel, &prompt).await;
            rest.send(channel, &answer).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bots::tests::FakeRunner;
    use crate::bots::voice::{Transcriber, VoiceSettings};
    use crate::bots::PlatformSettings;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A fake Discord gateway: HELLO, check IDENTIFY, READY, then messages.
    async fn fake_gateway(events: Vec<Value>) -> (String, tokio::task::JoinHandle<Value>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            ws.send(Message::text(json!({"op": 10, "d": {"heartbeat_interval": 45000}}).to_string())).await.unwrap();
            let identify: Value = loop {
                if let Some(Ok(Message::Text(t))) = ws.next().await {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["op"] == 2 { break v; }
                }
            };
            ws.send(Message::text(json!({"op": 0, "s": 1, "t": "READY", "d": {"user": {"username": "pf"}}}).to_string())).await.unwrap();
            for (i, e) in events.into_iter().enumerate() {
                ws.send(Message::text(json!({"op": 0, "s": i + 2, "t": "MESSAGE_CREATE", "d": e}).to_string())).await.unwrap();
            }
            // Keep the socket open until the client leaves.
            while let Some(Ok(m)) = ws.next().await { if m.is_close() { break; } }
            identify
        });
        (url, task)
    }

    #[tokio::test]
    async fn identifies_and_answers_only_where_allowed() {
        let events = vec![
            json!({"author": {"id": "42"}, "channel_id": "DM1", "content": "list files"}),
            json!({"author": {"id": "42"}, "channel_id": "G1", "guild_id": "S", "content": "<@999> run tests"}),
            json!({"author": {"id": "42"}, "channel_id": "G2", "guild_id": "S", "content": "not allowed here"}),
            json!({"author": {"id": "7"}, "channel_id": "DM2", "content": "stranger"}),
            json!({"author": {"id": "5", "bot": true}, "channel_id": "G1", "guild_id": "S", "content": "bot loop"}),
        ];
        let (ws_url, gw) = fake_gateway(events).await;
        let api = MockServer::start().await;
        Mock::given(method("GET")).and(path("/gateway/bot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"url": ws_url}))).mount(&api).await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({}))).mount(&api).await;

        let runner = Arc::new(FakeRunner::default());
        let s = PlatformSettings { enabled: true, allowed_users: vec!["42".into()], allowed_channels: vec!["G1".into()] };
        let core = Arc::new(BotCore::new(runner.clone(), &s, 0));
        let voice = Arc::new(Transcriber::new(VoiceSettings::default(), reqwest::Client::new(), None));
        let status = Arc::new(Mutex::new(PlatformStatus::default()));
        let (tx, rx) = tokio::sync::watch::channel(false);
        let cfg = Config { api: api.uri(), token: "TOKEN".into(), channels: vec!["G1".into()] };
        let task = tokio::spawn(run(reqwest::Client::new(), cfg, core, voice, status.clone(), rx));
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(status.lock().unwrap().detail, "Running as pf");
        tx.send(true).unwrap();
        task.await.unwrap();
        let identify = gw.await.unwrap();
        assert_eq!(identify["d"]["token"], "TOKEN");
        assert_eq!(identify["d"]["intents"], INTENTS);

        let mut prompts = runner.prompts.lock().unwrap().clone();
        prompts.sort();
        assert_eq!(prompts, vec![("DM1".to_string(), "list files".to_string()), ("G1".to_string(), "run tests".to_string())]);
        let posted: Vec<String> = api.received_requests().await.unwrap().iter()
            .filter(|r| r.url.path().ends_with("/messages")).map(|r| r.url.path().to_string()).collect();
        assert!(posted.iter().all(|p| p == "/channels/DM1/messages" || p == "/channels/G1/messages"), "{posted:?}");
        let auth = api.received_requests().await.unwrap()[0].headers.get("authorization").unwrap().to_str().unwrap().to_string();
        assert_eq!(auth, "Bot TOKEN");
    }
}
