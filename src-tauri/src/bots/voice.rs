//! Voice notes to text.
//!
//! - **On this computer**: ffmpeg turns the note (Telegram/Discord send
//!   Ogg/Opus) into 16 kHz mono WAV, then whisper.cpp's `whisper-cli`
//!   transcribes it with a model file the user picks. Nothing leaves the Mac.
//! - **With a provider**: the OpenAI-style `/audio/transcriptions` endpoint
//!   (Groq or OpenAI), using the key already saved for that provider.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceBackend {
    #[default]
    Off,
    Local,
    Provider,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    pub backend: VoiceBackend,
    /// whisper.cpp model file (e.g. ggml-base.en.bin).
    pub model_path: String,
    /// `groq` or `openai`.
    pub provider_id: String,
    /// e.g. `whisper-large-v3-turbo` (Groq) or `gpt-4o-mini-transcribe` (OpenAI).
    pub model: String,
}

/// Providers with an OpenAI-style transcription endpoint.
pub fn provider_base(id: &str) -> Option<&'static str> {
    match id {
        "groq" => Some("https://api.groq.com/openai/v1"),
        "openai" => Some("https://api.openai.com/v1"),
        _ => None,
    }
}

const MAX_BYTES: usize = 20 * 1024 * 1024;

pub struct Transcriber {
    settings: VoiceSettings,
    client: reqwest::Client,
    key: Option<String>,
    /// Overrides the provider address (tests).
    pub base_override: Option<String>,
}

impl Transcriber {
    pub fn new(settings: VoiceSettings, client: reqwest::Client, key: Option<String>) -> Self {
        Self { settings, client, key, base_override: None }
    }

    pub fn enabled(&self) -> bool {
        self.settings.backend != VoiceBackend::Off
    }

    pub async fn transcribe(&self, audio: Vec<u8>, filename: &str) -> Result<String, String> {
        if audio.len() > MAX_BYTES {
            return Err("That voice note is too long.".into());
        }
        let text = match self.settings.backend {
            VoiceBackend::Off => return Err("Voice notes are turned off. Turn them on in PrismFlow.".into()),
            VoiceBackend::Local => self.local(audio).await?,
            VoiceBackend::Provider => self.remote(audio, filename).await?,
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("I couldn't hear any words in that voice note.".into());
        }
        Ok(text)
    }

    async fn local(&self, audio: Vec<u8>) -> Result<String, String> {
        let ffmpeg = crate::adapters::find_exe("ffmpeg").ok_or("Install ffmpeg to use voice notes on this computer.")?;
        let whisper = find_whisper().ok_or("Install whisper.cpp (whisper-cli) to use voice notes on this computer.")?;
        let model = PathBuf::from(self.settings.model_path.trim());
        if !model.is_file() {
            return Err("Pick a whisper model file in PrismFlow's bot settings.".into());
        }
        let dir = TempDir::new()?;
        let input = dir.0.join("note.audio");
        let wav = dir.0.join("note.wav");
        std::fs::write(&input, &audio).map_err(|e| e.to_string())?;
        run(&ffmpeg, &["-nostdin", "-loglevel", "error", "-i", &s(&input), "-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le", &s(&wav)], 120)
            .await
            .map_err(|e| format!("Couldn't read the voice note: {e}"))?;
        run(&whisper, &["-m", &s(&model), "-f", &s(&wav), "-nt", "-np", "-l", "auto"], 300)
            .await
            .map_err(|e| format!("Couldn't transcribe the voice note: {e}"))
    }

    async fn remote(&self, audio: Vec<u8>, filename: &str) -> Result<String, String> {
        let base = self
            .base_override
            .clone()
            .or_else(|| provider_base(&self.settings.provider_id).map(String::from))
            .ok_or("Pick Groq or OpenAI for voice notes.")?;
        let key = self.key.clone().ok_or("Add a key for the voice provider on the Providers page.")?;
        let model = if self.settings.model.trim().is_empty() { "whisper-large-v3-turbo" } else { self.settings.model.trim() };
        let part = reqwest::multipart::Part::bytes(audio).file_name(filename.to_string()).mime_str("audio/ogg").map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new().text("model", model.to_string()).text("response_format", "json").part("file", part);
        let r = self
            .client
            .post(format!("{base}/audio/transcriptions"))
            .bearer_auth(key)
            .multipart(form)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| format!("Couldn't reach the voice provider: {e}"))?;
        let status = r.status();
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        match v["text"].as_str() {
            Some(t) if status.is_success() => Ok(t.to_string()),
            _ => Err(format!("The voice provider said no (HTTP {}): {}", status.as_u16(), v["error"]["message"].as_str().unwrap_or(""))),
        }
    }
}

/// A private scratch folder, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, String> {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).map_err(|e| e.to_string())?;
        let p = std::env::temp_dir().join(format!("prismflow-voice-{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()));
        std::fs::create_dir(&p).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

pub fn find_whisper() -> Option<PathBuf> {
    std::env::var_os("PRISMFLOW_WHISPER_BIN").map(PathBuf::from).or_else(|| crate::adapters::find_exe("whisper-cli"))
}

async fn run(bin: &Path, args: &[&str], secs: u64) -> Result<String, String> {
    let out = tokio::time::timeout(
        Duration::from_secs(secs),
        tokio::process::Command::new(bin).args(args).stdin(std::process::Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| "took too long".to_string())?
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn provider_transcription_sends_multipart_with_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/audio/transcriptions")).and(header("authorization", "Bearer gsk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": " list the files "})))
            .mount(&server).await;
        let mut t = Transcriber::new(
            VoiceSettings { backend: VoiceBackend::Provider, provider_id: "groq".into(), ..Default::default() },
            reqwest::Client::new(),
            Some("gsk-test".into()),
        );
        t.base_override = Some(server.uri());
        assert_eq!(t.transcribe(b"OggS....".to_vec(), "voice.ogg").await.unwrap(), "list the files");
        let req = &server.received_requests().await.unwrap()[0];
        let body = String::from_utf8_lossy(&req.body);
        assert!(body.contains("name=\"model\"") && body.contains("whisper-large-v3-turbo") && body.contains("filename=\"voice.ogg\""));
    }

    #[tokio::test]
    async fn off_by_default() {
        let t = Transcriber::new(VoiceSettings::default(), reqwest::Client::new(), None);
        assert!(!t.enabled());
        assert!(t.transcribe(vec![1], "a.ogg").await.is_err());
    }

    /// Live, local: macOS `say` → Ogg/Opus (like a voice note) → ffmpeg →
    /// whisper-cli. Needs PRISMFLOW_WHISPER_MODEL pointing at a ggml model.
    #[tokio::test]
    #[ignore]
    async fn live_local_voice_note() {
        let Ok(model) = std::env::var("PRISMFLOW_WHISPER_MODEL") else { return };
        let dir = tempfile::tempdir().unwrap();
        let aiff = dir.path().join("a.aiff");
        let ogg = dir.path().join("a.ogg");
        assert!(std::process::Command::new("say").args(["-o", &s(&aiff), "Please list the files in this folder."]).status().unwrap().success());
        let ff = crate::adapters::find_exe("ffmpeg").unwrap();
        assert!(std::process::Command::new(ff).args(["-loglevel", "error", "-i", &s(&aiff), "-c:a", "libopus", &s(&ogg)]).status().unwrap().success());
        let t = Transcriber::new(VoiceSettings { backend: VoiceBackend::Local, model_path: model, ..Default::default() }, reqwest::Client::new(), None);
        let text = t.transcribe(std::fs::read(&ogg).unwrap(), "a.ogg").await.unwrap();
        eprintln!("transcribed: {text}");
        assert!(text.to_lowercase().contains("list the files"), "{text}");
    }
}
