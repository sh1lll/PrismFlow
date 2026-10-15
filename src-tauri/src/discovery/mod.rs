//! Provider HTTP probes: model discovery (SPEC §8) and credential checks
//! (SPEC §9 Stage 2). Redirects are never followed, so a credential is
//! never forwarded to a host the user did not choose (SPEC §6.3).

use crate::providers::AuthStyle;
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum ProbeError {
    /// 401/403 — the key was rejected.
    Unauthorized(u16),
    NotFound,
    RateLimited,
    /// Connection, TLS, timeout, 5xx, or a redirect.
    Unavailable(String),
    /// The endpoint answered, but not in the expected format.
    BadResponse(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Unauthorized(code) => write!(f, "the provider rejected the credential (HTTP {code})"),
            ProbeError::NotFound => write!(f, "the endpoint was not found (HTTP 404)"),
            ProbeError::RateLimited => write!(f, "the provider is rate-limiting requests (HTTP 429)"),
            ProbeError::Unavailable(d) => write!(f, "the provider could not be reached: {d}"),
            ProbeError::BadResponse(d) => write!(f, "unexpected response: {d}"),
        }
    }
}

/// A discovered model. Listing is not availability (SPEC §8): these fields
/// are provider-reported metadata, never verified capabilities.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<u64>,
    /// `Some(true)` when the provider reports tool support for this model.
    pub reports_tools: Option<bool>,
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("PrismFlow/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("static client config is valid")
}

fn join(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

async fn get(
    client: &reqwest::Client,
    url: &str,
    key: Option<&str>,
    style: AuthStyle,
) -> Result<serde_json::Value, ProbeError> {
    let mut req = client.get(url);
    if style == AuthStyle::Anthropic {
        req = req.header("anthropic-version", "2023-06-01");
    }
    if let Some(k) = key {
        req = match style {
            AuthStyle::Bearer => req.bearer_auth(k),
            AuthStyle::Anthropic => req.header("x-api-key", k),
        };
    }
    let resp = req.send().await.map_err(|e| {
        // reqwest errors can embed the URL but never headers; still sanitize.
        ProbeError::Unavailable(crate::diagnostics::redact(&e.without_url().to_string()))
    })?;
    let status = resp.status().as_u16();
    match status {
        200..=299 => resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| ProbeError::BadResponse(e.without_url().to_string())),
        300..=399 => Err(ProbeError::Unavailable(format!(
            "the endpoint redirected (HTTP {status}); PrismFlow does not follow redirects with credentials"
        ))),
        401 | 403 => Err(ProbeError::Unauthorized(status)),
        404 => Err(ProbeError::NotFound),
        429 => Err(ProbeError::RateLimited),
        _ => Err(ProbeError::Unavailable(format!("HTTP {status}"))),
    }
}

/// `GET {base}{path}` in the OpenAI list shape: `{"data":[{"id":…},…]}`.
pub async fn list_models(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    key: Option<&str>,
) -> Result<Vec<ModelInfo>, ProbeError> {
    list_models_as(client, base_url, path, key, AuthStyle::Bearer).await
}

pub async fn list_models_as(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    key: Option<&str>,
    style: AuthStyle,
) -> Result<Vec<ModelInfo>, ProbeError> {
    let body = get(client, &join(base_url, path), key, style).await?;
    let data = body
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| ProbeError::BadResponse("no `data` array in model list".into()))?;
    let mut models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let reports_tools = m
                .get("supported_parameters")
                .and_then(|p| p.as_array())
                .map(|p| p.iter().any(|v| v.as_str() == Some("tools")));
            Some(ModelInfo {
                id,
                name: m.get("name").and_then(|n| n.as_str()).map(String::from),
                context_length: m
                    .get("context_length")
                    .or_else(|| m.get("context_window"))
                    .and_then(|c| c.as_u64()),
                reports_tools,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.to_lowercase().cmp(&b.id.to_lowercase()));
    Ok(models)
}

/// Stage 2: a side-effect-free authenticated GET that fails on a bad key.
pub async fn check_credential(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    key: &str,
) -> Result<(), ProbeError> {
    check_credential_as(client, base_url, path, key, AuthStyle::Bearer).await
}

pub async fn check_credential_as(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    key: &str,
    style: AuthStyle,
) -> Result<(), ProbeError> {
    get(client, &join(base_url, path), Some(key), style).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn parses_openrouter_style_model_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "z/model", "name": "Z", "context_length": 8192, "supported_parameters": ["tools", "temperature"]},
                    {"id": "a/model", "supported_parameters": ["temperature"]},
                    {"no_id": true}
                ]
            })))
            .mount(&server)
            .await;
        let models = list_models(&client(), &format!("{}/v1", server.uri()), "/models", None)
            .await
            .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "a/model");
        assert_eq!(models[0].reports_tools, Some(false));
        assert_eq!(models[1].context_length, Some(8192));
        assert_eq!(models[1].reports_tools, Some(true));
    }

    #[tokio::test]
    async fn credential_check_sends_bearer_and_maps_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/key"))
            .and(header("authorization", "Bearer good-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {}})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/key"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        assert!(check_credential(&client(), &base, "/key", "good-key").await.is_ok());
        assert_eq!(
            check_credential(&client(), &base, "/key", "bad-key").await,
            Err(ProbeError::Unauthorized(401))
        );
    }

    #[tokio::test]
    async fn anthropic_style_sends_x_api_key_and_version() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-ant-good"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [{"id": "claude-x", "display_name": "X"}]})))
            .mount(&server)
            .await;
        let m = list_models_as(&client(), &server.uri(), "/v1/models", Some("sk-ant-good"), AuthStyle::Anthropic).await.unwrap();
        assert_eq!(m[0].id, "claude-x");
        assert!(check_credential_as(&client(), &server.uri(), "/v1/models", "sk-ant-bad", AuthStyle::Anthropic).await.is_err());
    }

    #[tokio::test]
    async fn redirects_are_refused_not_followed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "https://evil.example/steal"))
            .mount(&server)
            .await;
        let err = check_credential(&client(), &server.uri(), "/key", "k").await.unwrap_err();
        assert!(matches!(err, ProbeError::Unavailable(ref d) if d.contains("redirect")));
    }

    #[tokio::test]
    async fn maps_rate_limit_and_bad_shape() {
        let server = MockServer::start().await;
        Mock::given(path("/limited")).respond_with(ResponseTemplate::new(429)).mount(&server).await;
        Mock::given(path("/odd"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"models": []})))
            .mount(&server)
            .await;
        assert_eq!(
            list_models(&client(), &server.uri(), "/limited", None).await,
            Err(ProbeError::RateLimited)
        );
        assert!(matches!(
            list_models(&client(), &server.uri(), "/odd", None).await,
            Err(ProbeError::BadResponse(_))
        ));
    }

    #[tokio::test]
    async fn unreachable_host_is_unavailable() {
        let err = list_models(&client(), "http://127.0.0.1:9", "/models", None).await.unwrap_err();
        assert!(matches!(err, ProbeError::Unavailable(_)));
    }
}
