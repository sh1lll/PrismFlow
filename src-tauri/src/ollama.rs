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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
