//! Telegram via the Bot API with long polling (`getUpdates`), so nothing
//! listens on this computer. https://core.telegram.org/bots/api

use super::{chunks, set_status, BotCore, PlatformStatus, Reply};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const API_BASE: &str = "https://api.telegram.org";
const MAX_MESSAGE: usize = 4000;

#[derive(Clone)]
pub struct Api {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl Api {
    pub fn new(client: reqwest::Client, base: &str, token: &str) -> Self {
        Self { client, base: base.trim_end_matches('/').into(), token: token.trim().into() }
    }

    async fn call(&self, method: &str, body: Value) -> Result<Value, String> {
        let r = self
            .client
            .post(format!("{}/bot{}/{method}", self.base, self.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Couldn't reach Telegram ({})", if e.is_timeout() { "timed out" } else { "network" }))?;
        let v: Value = r.json().await.map_err(|_| "Telegram sent an unreadable answer.".to_string())?;
        if v["ok"] != true {
            return Err(match v["error_code"].as_i64() {
                Some(401) | Some(404) => "Telegram rejected the bot token.".into(),
                _ => format!("Telegram said: {}", v["description"].as_str().unwrap_or("error")),
            });
        }
        Ok(v["result"].clone())
    }

    pub async fn me(&self) -> Result<String, String> {
        Ok(self.call("getMe", json!({})).await?["username"].as_str().unwrap_or("bot").to_string())
    }

    pub async fn send(&self, chat: i64, text: &str) -> Result<(), String> {
        for part in chunks(text, MAX_MESSAGE) {
            self.call("sendMessage", json!({ "chat_id": chat, "text": part })).await?;
        }
        Ok(())
    }

    async fn typing(&self, chat: i64) {
        let _ = self.call("sendChatAction", json!({ "chat_id": chat, "action": "typing" })).await;
    }

    async fn download(&self, file_id: &str) -> Result<Vec<u8>, String> {
        let f = self.call("getFile", json!({ "file_id": file_id })).await?;
        let path = f["file_path"].as_str().ok_or("Telegram didn't give the file.")?;
        let r = self.client.get(format!("{}/file/bot{}/{path}", self.base, self.token)).send().await.map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err("Couldn't download the voice note.".into());
        }
        Ok(r.bytes().await.map_err(|e| e.to_string())?.to_vec())
    }
}

/// Handles one Telegram update (a message). Public for tests.
pub async fn handle(api: &Api, core: &BotCore, voice: &super::voice::Transcriber, update: &Value) {
    let m = &update["message"];
    let (Some(chat), Some(user)) = (m["chat"]["id"].as_i64(), m["from"]["id"].as_i64()) else { return };
    let user = user.to_string();
    if !core.allowed(&user) {
        return;
    }
    let audio = m["voice"]["file_id"].as_str().or_else(|| m["audio"]["file_id"].as_str());
    let text = match (m["text"].as_str(), audio) {
        (Some(t), _) => t.to_string(),
        (None, Some(file_id)) => {
            if !voice.enabled() {
                let _ = api.send(chat, "Voice notes are turned off. Turn them on in PrismFlow.").await;
                return;
            }
            api.typing(chat).await;
            let heard = match api.download(file_id).await {
                Ok(bytes) => voice.transcribe(bytes, "voice.ogg").await,
                Err(e) => Err(e),
            };
            match heard {
                Ok(t) => {
                    let _ = api.send(chat, &format!("Heard: {t}")).await;
                    t
                }
                Err(e) => {
                    let _ = api.send(chat, &e).await;
                    return;
                }
            }
        }
        _ => return,
    };
    let key = chat.to_string();
    match core.classify(&key, &user, &text) {
        Reply::Ignore => {}
        Reply::Now(t) => {
            let _ = api.send(chat, &t).await;
        }
        Reply::Task(prompt) => {
            let _ = api.send(chat, "Working on it…").await;
            api.typing(chat).await;
            let answer = core.run(&key, &prompt).await;
            let _ = api.send(chat, &answer).await;
        }
    }
}

pub async fn run(
    api: Api,
    core: Arc<BotCore>,
    voice: Arc<super::voice::Transcriber>,
    status: Arc<Mutex<PlatformStatus>>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    // At login the network is often not up yet: keep trying, slower each time.
    let mut wait = 5u64;
    let name = loop {
        match api.me().await {
            Ok(n) => break n,
            // A wrong token won't fix itself; only network trouble is retried.
            Err(e) if !e.starts_with("Couldn't reach Telegram") => return set_status(&status, false, e),
            Err(e) => {
                set_status(&status, false, format!("{e} Trying again shortly."));
                tokio::select! {
                    _ = stop.changed() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                }
                wait = (wait * 2).min(300);
            }
        }
    };
    set_status(&status, true, format!("Running as @{name}"));
    let mut offset: i64 = 0;
    let mut backoff = 1u64;
    loop {
        let poll = api.call("getUpdates", json!({ "offset": offset, "timeout": 25, "allowed_updates": ["message"] }));
        let updates = tokio::select! {
            _ = stop.changed() => break,
            r = poll => r,
        };
        match updates {
            Ok(Value::Array(list)) => {
                backoff = 1;
                set_status(&status, true, format!("Running as @{name}"));
                for u in list {
                    offset = offset.max(u["update_id"].as_i64().unwrap_or(0) + 1);
                    // Each message runs on its own so /stop can arrive mid-task.
                    let (api, core, voice) = (api.clone(), core.clone(), voice.clone());
                    tokio::spawn(async move { handle(&api, &core, &voice, &u).await });
                }
            }
            Ok(_) => {}
            Err(e) => {
                set_status(&status, true, format!("Reconnecting: {e}"));
                if e.contains("token") {
                    return set_status(&status, false, e);
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bots::tests::FakeRunner;
    use crate::bots::voice::{Transcriber, VoiceSettings};
    use crate::bots::PlatformSettings;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ok(result: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": result }))
    }

    #[tokio::test]
    async fn answers_allowed_users_and_ignores_others() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/botT0K/getMe")).respond_with(ok(json!({"username": "pf_bot"}))).mount(&server).await;
        let updates = json!([
            {"update_id": 10, "message": {"chat": {"id": 5}, "from": {"id": 42}, "text": "list files"}},
            {"update_id": 11, "message": {"chat": {"id": 6}, "from": {"id": 99}, "text": "rm -rf /"}},
            {"update_id": 12, "message": {"chat": {"id": 5}, "from": {"id": 42}, "text": "/stats"}}
        ]);
        Mock::given(method("POST")).and(path("/botT0K/getUpdates")).respond_with(ok(updates)).up_to_n_times(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/botT0K/getUpdates"))
            .respond_with(ok(json!([])).set_delay(Duration::from_millis(200))).mount(&server).await;
        Mock::given(method("POST")).and(path("/botT0K/sendMessage")).respond_with(ok(json!({}))).mount(&server).await;
        Mock::given(method("POST")).and(path("/botT0K/sendChatAction")).respond_with(ok(json!(true))).mount(&server).await;

        let runner = Arc::new(FakeRunner::default());
        let s = PlatformSettings { enabled: true, allowed_users: vec!["42".into()], allowed_channels: vec![] };
        let core = Arc::new(BotCore::new(runner.clone(), &s, 0));
        let voice = Arc::new(Transcriber::new(VoiceSettings::default(), reqwest::Client::new(), None));
        let status = Arc::new(Mutex::new(PlatformStatus::default()));
        let (tx, rx) = tokio::sync::watch::channel(false);
        let api = Api::new(reqwest::Client::new(), &server.uri(), "T0K");
        let task = tokio::spawn(run(api, core, voice, status.clone(), rx));
        tokio::time::sleep(Duration::from_millis(800)).await;
        tx.send(true).unwrap();
        task.await.unwrap();

        let sent: Vec<Value> = server.received_requests().await.unwrap().iter()
            .filter(|r| r.url.path().ends_with("/sendMessage"))
            .map(|r| serde_json::from_slice(&r.body).unwrap()).collect();
        let texts: Vec<&str> = sent.iter().map(|v| v["text"].as_str().unwrap()).collect();
        assert!(texts.contains(&"did: list files"), "{texts:?}");
        assert!(texts.contains(&"stats for 5"), "{texts:?}");
        assert!(sent.iter().all(|v| v["chat_id"] == 5), "never answers the stranger: {sent:?}");
        assert_eq!(runner.prompts.lock().unwrap().as_slice(), &[("5".to_string(), "list files".to_string())]);
        // The next poll acknowledges what was handled.
        let polls: Vec<Value> = server.received_requests().await.unwrap().iter()
            .filter(|r| r.url.path().ends_with("/getUpdates")).map(|r| serde_json::from_slice(&r.body).unwrap()).collect();
        assert!(polls.iter().any(|p| p["offset"] == 13), "{polls:?}");
        assert!(!status.lock().unwrap().running);
    }

    #[tokio::test]
    async fn bad_token_stops_with_a_plain_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/botBAD/getMe"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"})))
            .mount(&server).await;
        let s = PlatformSettings { enabled: true, allowed_users: vec!["1".into()], allowed_channels: vec![] };
        let core = Arc::new(BotCore::new(Arc::new(FakeRunner::default()), &s, 0));
        let voice = Arc::new(Transcriber::new(VoiceSettings::default(), reqwest::Client::new(), None));
        let status = Arc::new(Mutex::new(PlatformStatus::default()));
        let (_tx, rx) = tokio::sync::watch::channel(false);
        run(Api::new(reqwest::Client::new(), &server.uri(), "BAD"), core, voice, status.clone(), rx).await;
        let st = status.lock().unwrap().clone();
        assert!(!st.running && st.detail == "Telegram rejected the bot token.", "{st:?}");
    }
}
