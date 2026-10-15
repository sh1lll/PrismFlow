//! Local web tools for gateway connections (ADR-018): search through a
//! user-chosen backend with a sanctioned API, and fetch pages with SSRF
//! protection. Off by default.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

pub const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESULT_CHARS: usize = 20_000;
const MAX_REDIRECTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackend {
    #[default]
    None,
    /// A SearXNG instance's JSON API (self-hosted or one the user trusts).
    Searxng,
    /// Brave Search API (key in the secure store).
    Brave,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebToolsSettings {
    pub enabled: bool,
    pub backend: SearchBackend,
    pub searxng_url: String,
    /// Allow fetching private-network addresses (off: SSRF protection).
    pub allow_private: bool,
}

pub const BRAVE_ACCOUNT: &str = "prismflow:web_search:brave";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("PrismFlow/", env!("CARGO_PKG_VERSION"), " (local web tools)"))
        .build()
        .expect("static client config")
}

pub async fn search(s: &WebToolsSettings, brave_key: Option<&str>, query: &str) -> Result<Vec<SearchResult>, String> {
    let q = query.trim();
    if q.is_empty() || q.len() > 400 {
        return Err("The search query must be 1–400 characters.".into());
    }
    match s.backend {
        SearchBackend::None => Err("Web search isn't set up. Turn it on in Settings.".into()),
        SearchBackend::Searxng => {
            let base = s.searxng_url.trim_end_matches('/');
            if !(base.starts_with("https://") || base.starts_with("http://127.0.0.1") || base.starts_with("http://localhost")) {
                return Err("The search address must start with https://.".into());
            }
            let v: serde_json::Value = client().get(format!("{base}/search")).query(&[("q", q), ("format", "json")])
                .send().await.map_err(|e| format!("search failed: {}", e.without_url()))?
                .error_for_status().map_err(|e| format!("search failed: {}", e.without_url()))?
                .json().await.map_err(|_| "the search backend did not return JSON (enable the JSON format in SearXNG)".to_string())?;
            Ok(v["results"].as_array().into_iter().flatten().take(8).map(|r| SearchResult {
                title: r["title"].as_str().unwrap_or_default().into(),
                url: r["url"].as_str().unwrap_or_default().into(),
                snippet: r["content"].as_str().unwrap_or_default().into(),
            }).collect())
        }
        SearchBackend::Brave => {
            let key = brave_key.ok_or("Add a Brave Search API key in Settings → Web tools.")?;
            let v: serde_json::Value = client().get("https://api.search.brave.com/res/v1/web/search")
                .query(&[("q", q), ("count", "8")]).header("X-Subscription-Token", key).header("Accept", "application/json")
                .send().await.map_err(|e| format!("search failed: {}", e.without_url()))?
                .error_for_status().map_err(|e| format!("search failed: {}", e.without_url()))?
                .json().await.map_err(|e| format!("search failed: {e}"))?;
            Ok(v["web"]["results"].as_array().into_iter().flatten().take(8).map(|r| SearchResult {
                title: r["title"].as_str().unwrap_or_default().into(),
                url: r["url"].as_str().unwrap_or_default().into(),
                snippet: r["description"].as_str().unwrap_or_default().into(),
            }).collect())
        }
    }
}

/// Resolve and vet a URL before any request (SPEC §6.3).
pub fn vet_url(raw: &str, allow_private: bool) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| format!("not a valid URL: {raw}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs can be fetched".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with credentials are not fetched".into());
    }
    let host = url.host_str().ok_or("the URL has no host")?.trim_matches(['[', ']']).to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => (host.as_str(), port).to_socket_addrs().map_err(|_| format!("{host} could not be resolved"))?.map(|a| a.ip()).collect(),
    };
    if addrs.is_empty() {
        return Err(format!("{host} could not be resolved"));
    }
    if !allow_private && addrs.iter().any(|ip| ip.is_loopback() || crate::providers::is_private_ip(*ip)) {
        return Err(format!("{host} is on a private or local network; fetching it is blocked (Settings → Web tools)"));
    }
    Ok(url)
}

/// Fetch a page as readable text, following at most three redirects, each
/// re-vetted. HTML is reduced to text.
pub async fn fetch(raw: &str, allow_private: bool) -> Result<String, String> {
    let c = client();
    let mut url = vet_url(raw, allow_private)?;
    for _ in 0..=MAX_REDIRECTS {
        let resp = c.get(url.clone()).send().await.map_err(|e| format!("fetch failed: {}", e.without_url()))?;
        if resp.status().is_redirection() {
            let loc = resp.headers().get("location").and_then(|v| v.to_str().ok()).ok_or("redirect without a location")?;
            let next = url.join(loc).map_err(|_| "bad redirect location".to_string())?;
            url = vet_url(next.as_str(), allow_private)?;
            continue;
        }
        if !resp.status().is_success() {
            return Err(format!("the page answered HTTP {}", resp.status().as_u16()));
        }
        let is_html = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.contains("html"));
        let mut body = Vec::new();
        let mut stream = resp;
        while let Some(chunk) = stream.chunk().await.map_err(|e| format!("fetch failed: {}", e.without_url()))? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_FETCH_BYTES {
                return Err("the page is larger than 2 MB".into());
            }
        }
        let text = String::from_utf8_lossy(&body).to_string();
        let head = text.trim_start().get(..15).unwrap_or("").to_lowercase();
        let sniffed = head.starts_with("<!doctype html") || head.starts_with("<html");
        let text = if is_html || sniffed { html_to_text(&text) } else { text };
        return Ok(text.chars().take(MAX_RESULT_CHARS).collect());
    }
    Err("too many redirects".into())
}

/// Minimal HTML → text: drop script/style, strip tags, decode a few
/// entities, collapse whitespace.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            for skip in ["<script", "<style", "<noscript"] {
                if lower[i..].starts_with(skip) {
                    let close = format!("</{}", &skip[1..]);
                    i = lower[i..].find(&close).map(|e| i + e).unwrap_or(bytes.len());
                    break;
                }
            }
            if i >= bytes.len() {
                break;
            }
            match html[i..].find('>') {
                Some(e) => {
                    let tag = &lower[i..i + e];
                    if ["<p", "<br", "<div", "<li", "<h1", "<h2", "<h3", "<tr", "</p", "</div"].iter().any(|t| tag.starts_with(t)) {
                        out.push('\n');
                    }
                    i += e + 1;
                }
                None => break,
            }
        } else {
            let next = html[i..].find('<').map(|e| i + e).unwrap_or(bytes.len());
            out.push_str(&html[i..next]);
            i = next;
        }
    }
    let decoded = out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'");
    decoded.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn ssrf_guard_blocks_private_and_odd_urls() {
        assert!(vet_url("file:///etc/passwd", false).is_err());
        assert!(vet_url("https://user:pw@example.com/", false).is_err());
        for u in ["http://127.0.0.1/", "http://10.0.0.1/", "http://169.254.169.254/latest/meta-data", "http://[::1]/", "http://localhost/"] {
            assert!(vet_url(u, false).is_err(), "{u} must be blocked");
        }
        assert!(vet_url("http://127.0.0.1/", true).is_ok(), "explicit opt-in allows local");
        assert!(vet_url("https://1.1.1.1/", false).is_ok());
    }

    #[test]
    fn html_becomes_readable_text() {
        let t = html_to_text("<html><head><style>p{}</style><script>evil()</script></head><body><h1>Title</h1><p>One &amp; two</p><div>Three</div></body></html>");
        assert_eq!(t, "Title\nOne & two\nThree");
    }

    #[tokio::test]
    async fn searxng_backend_parses_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/search")).and(query_param("format", "json")).and(query_param("q", "rust tls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": [{"title": "T", "url": "https://x", "content": "snip"}]})))
            .mount(&server).await;
        let s = WebToolsSettings { enabled: true, backend: SearchBackend::Searxng, searxng_url: server.uri().replace("http://127.0.0.1", "http://127.0.0.1"), allow_private: false };
        let r = search(&s, None, "rust tls").await.unwrap();
        assert_eq!(r, vec![SearchResult { title: "T".into(), url: "https://x".into(), snippet: "snip".into() }]);
        assert!(search(&WebToolsSettings::default(), None, "q").await.is_err(), "no backend configured");
        let insecure = WebToolsSettings { searxng_url: "http://search.example".into(), backend: SearchBackend::Searxng, ..Default::default() };
        assert!(search(&insecure, None, "q").await.is_err(), "remote plain http refused");
    }

    #[tokio::test]
    async fn fetch_follows_vetted_redirects_and_blocks_private_hops() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<p>Hello</p>", "text/html"))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/hop"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/page"))
            .mount(&server).await;
        // The mock is on loopback, so private access must be allowed explicitly here.
        assert_eq!(fetch(&format!("{}/hop", server.uri()), true).await.unwrap(), "Hello");
        assert!(fetch(&format!("{}/hop", server.uri()), false).await.unwrap_err().contains("private"));
    }
}
