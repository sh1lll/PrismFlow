//! Ollama on this computer: context size and model management.
//!
//! Ollama runs every model at its server's default context (4,096 tokens
//! unless changed), far below what coding agents send (~11k+ tokens), and its
//! OpenAI-compatible API has no way to ask for more per request. PrismFlow
//! creates a variant of the chosen model with a larger `num_ctx`
//! (`POST /api/create {model, from, parameters}`), which reuses the same
//! weights (no extra disk), and routes requests to that variant.
//! Verified 2026-10-15 with Ollama 0.40.4.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// What coding agents need: room for the system prompt, tools and history.
pub const AGENT_CONTEXT: u64 = 32_768;

/// `http://127.0.0.1:11434/v1` → `http://127.0.0.1:11434`.
pub fn api_root(base_url: &str) -> String {
    base_url.trim_end_matches('/').trim_end_matches("/v1").to_string()
}

/// The variant PrismFlow uses for `model` at `ctx` tokens.
pub fn variant_name(model: &str, ctx: u64) -> String {
    let k = ctx / 1024;
    if model.contains(&format!("-pf{k}k")) {
        return model.to_string();
    }
    if model.contains(':') {
        format!("{model}-pf{k}k")
    } else {
        format!("{model}:latest-pf{k}k")
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ModelDetails {
    /// The most the model itself supports.
    pub max_context: Option<u64>,
    /// `num_ctx` set on the model (None = the server default, usually 4,096).
    pub num_ctx: Option<u64>,
}

pub async fn details(client: &reqwest::Client, root: &str, model: &str) -> Result<ModelDetails, String> {
    let v: Value = client.post(format!("{root}/api/show")).json(&json!({ "model": model }))
        .timeout(Duration::from_secs(20)).send().await.map_err(|_| "Ollama isn't running.".to_string())?
        .json().await.map_err(|e| e.to_string())?;
    if let Some(e) = v["error"].as_str() {
        return Err(format!("Ollama: {e}"));
    }
    let max_context = v["model_info"].as_object()
        .and_then(|m| m.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| v.as_u64()));
    let num_ctx = v["parameters"].as_str().and_then(|p| {
        p.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            (it.next() == Some("num_ctx")).then(|| it.next()?.parse().ok()).flatten()
        })
    });
    Ok(ModelDetails { max_context, num_ctx })
}

static MADE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Makes sure a large-context variant of `model` exists and returns its name.
/// A model that already has a big enough `num_ctx` is used as is.
pub async fn ensure_context(client: &reqwest::Client, root: &str, model: &str, want: u64) -> Result<String, String> {
    let key = format!("{root}|{model}|{want}");
    if let Some(v) = MADE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&key).cloned()) {
        return Ok(v);
    }
    let d = details(client, root, model).await?;
    let ctx = d.max_context.map(|m| m.min(want)).unwrap_or(want);
    let name = if d.num_ctx.is_some_and(|n| n >= ctx) {
        model.to_string()
    } else {
        let name = variant_name(model, ctx);
        let made = details(client, root, &name).await.ok().and_then(|x| x.num_ctx) == Some(ctx);
        if !made {
            let r: Value = client.post(format!("{root}/api/create"))
                .json(&json!({ "model": name, "from": model, "parameters": { "num_ctx": ctx }, "stream": false }))
                .timeout(Duration::from_secs(120)).send().await.map_err(|e| format!("Ollama: {e}"))?
                .json().await.map_err(|e| e.to_string())?;
            if let Some(e) = r["error"].as_str() {
                return Err(format!("Ollama couldn't set a larger context: {e}"));
            }
        }
        name
    };
    MADE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, name.clone());
    Ok(name)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Installed {
    pub name: String,
    pub size_gb: f64,
    pub parameters: String,
    /// One of PrismFlow's large-context copies (same weights, no extra disk).
    pub prismflow_copy: bool,
    pub running: bool,
}

pub async fn list(client: &reqwest::Client, root: &str) -> Result<Vec<Installed>, String> {
    let tags: Value = client.get(format!("{root}/api/tags")).timeout(Duration::from_secs(10)).send().await
        .map_err(|_| "Ollama isn't running. Open the Ollama app, or run `ollama serve`.".to_string())?.json().await.map_err(|e| e.to_string())?;
    let ps: Value = match client.get(format!("{root}/api/ps")).timeout(Duration::from_secs(10)).send().await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => Value::Null,
    };
    let running: Vec<String> = ps["models"].as_array().map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect()).unwrap_or_default();
    Ok(tags["models"].as_array().map(|a| a.iter().filter_map(|m| {
        let name = m["name"].as_str()?.to_string();
        Some(Installed {
            prismflow_copy: name.contains("-pf") && name.ends_with('k'),
            running: running.contains(&name),
            size_gb: m["size"].as_f64().unwrap_or(0.0) / 1e9,
            parameters: m["details"]["parameter_size"].as_str().unwrap_or("").to_string(),
            name,
        })
    }).collect()).unwrap_or_default())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Pull {
    pub status: String,
    pub done: bool,
    pub error: Option<String>,
    pub percent: Option<f64>,
}

static PULLS: Mutex<Option<HashMap<String, Pull>>> = Mutex::new(None);

pub fn pulls() -> HashMap<String, Pull> {
    PULLS.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn set_pull(name: &str, p: Pull) {
    PULLS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(name.to_string(), p);
}

/// Downloads `name` in the background; progress via `pulls()`.
pub fn start_pull(_shared: reqwest::Client, root: String, name: String) {
    set_pull(&name, Pull { status: "Starting".into(), ..Default::default() });
    // A download takes minutes; the shared client's 30 s limit covers the
    // whole body and cut every download short. Only a stall ends this one.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default();
    tokio::spawn(async move {
        use futures_util::StreamExt;
        let r = client.post(format!("{root}/api/pull")).json(&json!({ "model": name, "stream": true })).send().await;
        let mut stream = match r {
            Ok(r) => r.bytes_stream(),
            Err(_) => return set_pull(&name, Pull { done: true, error: Some("Ollama isn't running.".into()), ..Default::default() }),
        };
        let mut buf = String::new();
        let mut last = Pull::default();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(i) = buf.find('\n') {
                let line: String = buf.drain(..=i).collect();
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
                if let Some(e) = v["error"].as_str() {
                    return set_pull(&name, Pull { done: true, error: Some(e.to_string()), ..Default::default() });
                }
                last.status = v["status"].as_str().unwrap_or("").to_string();
                if let (Some(c), Some(t)) = (v["completed"].as_f64(), v["total"].as_f64()) {
                    if t > 0.0 {
                        last.percent = Some((c / t * 100.0).min(100.0));
                    }
                }
                last.done = last.status == "success";
                set_pull(&name, last.clone());
            }
        }
        if !last.done {
            set_pull(&name, Pull { done: true, error: Some(if last.status.is_empty() { "The download stopped.".into() } else { last.status.clone() }), ..last });
        }
    });
}

pub async fn delete(client: &reqwest::Client, root: &str, name: &str) -> Result<(), String> {
    let r = client.delete(format!("{root}/api/delete")).json(&json!({ "model": name })).send().await.map_err(|_| "Ollama isn't running.".to_string())?;
    if r.status().is_success() { Ok(()) } else { Err(format!("Ollama couldn't delete {name}.")) }
}

/// Good coding models to download (name, what it is, rough download size).
pub const SUGGESTED: &[(&str, &str, &str)] = &[
    ("qwen3-coder:30b", "Strong coding model (needs 32 GB+ memory)", "19 GB"),
    ("qwen2.5-coder:7b", "Coding model for most laptops", "4.7 GB"),
    ("devstral:24b", "Mistral's agent coding model (needs 32 GB+ memory)", "14 GB"),
    ("gpt-oss:20b", "OpenAI's open model, good with tools (16 GB+ memory)", "14 GB"),
    ("qwen3:4b-instruct-2507-q4_K_M", "Small and fast, for light tasks", "2.5 GB"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn lists_models_and_tracks_a_download() {
        let s = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/tags")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [
            {"name": "qwen3:4b", "size": 2_500_000_000u64, "details": {"parameter_size": "4.0B"}},
            {"name": "qwen3:4b-pf32k", "size": 2_500_000_000u64, "details": {"parameter_size": "4.0B"}}]}))).mount(&s).await;
        Mock::given(method("GET")).and(path("/api/ps")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [{"name": "qwen3:4b"}]}))).mount(&s).await;
        let progress = "{\"status\":\"pulling manifest\"}\n{\"status\":\"downloading\",\"completed\":50,\"total\":100}\n{\"status\":\"success\"}\n";
        Mock::given(method("POST")).and(path("/api/pull")).respond_with(ResponseTemplate::new(200).set_body_string(progress)).mount(&s).await;
        let c = reqwest::Client::new();
        let l = list(&c, &s.uri()).await.unwrap();
        assert_eq!(l.len(), 2);
        assert!(l[0].running && !l[0].prismflow_copy && l[1].prismflow_copy);
        start_pull(c, s.uri(), "tiny:1".into());
        for _ in 0..50 {
            if pulls().get("tiny:1").is_some_and(|p| p.done) { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let p = pulls()["tiny:1"].clone();
        assert!(p.done && p.error.is_none(), "{p:?}");
    }

    #[test]
    fn variant_names() {
        assert_eq!(variant_name("qwen3:4b", 32_768), "qwen3:4b-pf32k");
        assert_eq!(variant_name("llama3", 32_768), "llama3:latest-pf32k");
        assert_eq!(variant_name("qwen3:4b-pf32k", 32_768), "qwen3:4b-pf32k");
        assert_eq!(api_root("http://127.0.0.1:11434/v1"), "http://127.0.0.1:11434");
    }

    #[tokio::test]
    async fn creates_a_variant_only_when_needed() {
        let s = MockServer::start().await;
        Mock::given(method("POST")).and(path("/api/show")).and(body_partial_json(json!({"model": "m:1"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"parameters": "temperature 0.7", "model_info": {"qwen3.context_length": 262144}})))
            .mount(&s).await;
        Mock::given(method("POST")).and(path("/api/show")).and(body_partial_json(json!({"model": "m:1-pf32k"})))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
            .mount(&s).await;
        Mock::given(method("POST")).and(path("/api/create")).and(body_partial_json(json!({"model": "m:1-pf32k", "from": "m:1", "parameters": {"num_ctx": 32768}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "success"})))
            .expect(1).mount(&s).await;
        Mock::given(method("POST")).and(path("/api/show")).and(body_partial_json(json!({"model": "big:1"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"parameters": "num_ctx 65536", "model_info": {"x.context_length": 131072}})))
            .mount(&s).await;
        let c = reqwest::Client::new();
        assert_eq!(ensure_context(&c, &s.uri(), "m:1", AGENT_CONTEXT).await.unwrap(), "m:1-pf32k");
        assert_eq!(ensure_context(&c, &s.uri(), "big:1", AGENT_CONTEXT).await.unwrap(), "big:1", "already large enough");
    }
}
