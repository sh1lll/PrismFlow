//! Provider registry (SPEC §3). Built-in entries come from the versioned
//! catalog `catalog/providers.json` (the data layer of SPEC §21.2); custom
//! providers are added at runtime by the user, after URL validation and
//! explicit consent (SPEC §6.3).
//!
//! Entries are parsed once and leaked into `'static` so every caller keeps a
//! cheap `&'static ProviderDefinition`. Custom providers leak a few hundred
//! bytes per add or edit, bounded by user actions.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{OnceLock, RwLock};

/// Supported wire protocols (SPEC §5.2 — these are NOT interchangeable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

/// How the provider expects its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` (OpenAI family).
    #[default]
    Bearer,
    /// `x-api-key` + `anthropic-version` (Anthropic family).
    Anthropic,
}

/// Where the provider's credential comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// A key the user pastes; stored in the system secure store.
    #[default]
    Keychain,
    /// The GitHub CLI's own sign-in (`gh auth token`, read at use).
    GhCli,
    /// The sign-in Codex made with OpenAI (`codex login`), read at use.
    CodexLogin,
    /// The Google Cloud CLI's sign-in (`gcloud auth print-access-token`);
    /// tokens last an hour, so PrismFlow refreshes them (Vertex AI).
    Gcloud,
    /// GitHub's official Copilot CLI sign-in (`copilot login`); PrismFlow
    /// asks the CLI for the endpoint and a short-lived token.
    CopilotCli,
}

/// A provider definition. Only verified facts live here (SPEC §3.3):
/// identity, base URL, protocol family, docs. Capabilities are discovered
/// and verified at runtime, never asserted statically.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
    pub base_url: &'static str,
    pub protocols: &'static [Protocol],
    pub model_list_path: Option<&'static str>,
    /// A documented, side-effect-free endpoint that fails on a bad key
    /// (SPEC §9 Stage 2). None = no safe check known; Stage 2 is skipped.
    pub auth_check_path: Option<&'static str>,
    pub auth_style: AuthStyle,
    /// Empty = no public page; the UI renders "—" rather than a dead link.
    pub docs_url: &'static str,
    pub keys_url: &'static str,
    /// Runs on this machine (loopback); plain http is acceptable.
    pub local: bool,
    pub requires_key: bool,
    /// Added by the user, not part of the shipped catalog.
    pub custom: bool,
    /// Date the facts were last checked (SPEC §21.3); empty for custom.
    pub verified_at: &'static str,
    pub credential_source: CredentialSource,
}

#[derive(Deserialize)]
struct CatalogFile {
    catalog_version: String,
    providers: Vec<CatalogEntry>,
}

#[derive(Deserialize)]
struct CatalogEntry {
    id: String,
    display_name: String,
    base_url: String,
    protocols: Vec<Protocol>,
    model_list_path: Option<String>,
    auth_check_path: Option<String>,
    #[serde(default)]
    auth_style: AuthStyle,
    #[serde(default)]
    docs_url: String,
    #[serde(default)]
    keys_url: String,
    #[serde(default)]
    local: bool,
    #[serde(default = "yes")]
    requires_key: bool,
    #[serde(default)]
    verified_at: String,
    #[serde(default)]
    credential_source: CredentialSource,
}

fn yes() -> bool {
    true
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn leak_opt(s: Option<String>) -> Option<&'static str> {
    s.filter(|s| !s.is_empty()).map(leak)
}

impl CatalogEntry {
    fn into_def(self, custom: bool) -> ProviderDefinition {
        ProviderDefinition {
            id: leak(self.id),
            display_name: leak(self.display_name),
            base_url: leak(self.base_url),
            protocols: Box::leak(self.protocols.into_boxed_slice()),
            model_list_path: leak_opt(self.model_list_path),
            auth_check_path: leak_opt(self.auth_check_path),
            auth_style: self.auth_style,
            docs_url: leak(self.docs_url),
            keys_url: leak(self.keys_url),
            local: self.local,
            requires_key: self.requires_key,
            custom,
            verified_at: leak(self.verified_at),
            credential_source: self.credential_source,
        }
    }
}

const BUILTIN_JSON: &str = include_str!("../../catalog/providers.json");

struct Builtin {
    version: &'static str,
    defs: Vec<&'static ProviderDefinition>,
}

fn builtin() -> &'static Builtin {
    static B: OnceLock<Builtin> = OnceLock::new();
    B.get_or_init(|| {
        let file: CatalogFile = serde_json::from_str(BUILTIN_JSON).expect("shipped catalog is valid (tested)");
        Builtin {
            version: leak(file.catalog_version),
            defs: file.providers.into_iter().map(|e| &*Box::leak(Box::new(e.into_def(false)))).collect(),
        }
    })
}

fn custom_list() -> &'static RwLock<Vec<&'static ProviderDefinition>> {
    static C: OnceLock<RwLock<Vec<&'static ProviderDefinition>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(Vec::new()))
}

pub fn catalog_version() -> &'static str {
    builtin().version
}

/// Built-in providers first (catalog order), then custom ones.
pub fn registry() -> Vec<&'static ProviderDefinition> {
    let mut all = builtin().defs.clone();
    all.extend(custom_list().read().unwrap_or_else(|e| e.into_inner()).iter().copied());
    all
}

pub fn get(id: &str) -> Option<&'static ProviderDefinition> {
    builtin().defs.iter().copied().find(|p| p.id == id).or_else(|| {
        custom_list().read().unwrap_or_else(|e| e.into_inner()).iter().copied().find(|p| p.id == id)
    })
}

// ---- Custom providers ----------------------------------------------------

/// What the user saved (persisted in the state dir; no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomProvider {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub requires_key: bool,
    /// The host the user explicitly consented to send requests to.
    pub consented_host: String,
    /// Keychain key, or a sign-in (Vertex AI uses `gcloud`).
    #[serde(default)]
    pub credential_source: CredentialSource,
}

impl CustomProvider {
    fn to_def(&self) -> ProviderDefinition {
        let anthropic = self.protocol == Protocol::AnthropicMessages;
        let host = host_of(&self.base_url).unwrap_or_default();
        CatalogEntry {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            base_url: self.base_url.trim_end_matches('/').to_string(),
            protocols: vec![self.protocol],
            model_list_path: Some(if anthropic { "/v1/models" } else { "/models" }.into()),
            auth_check_path: None,
            auth_style: if anthropic { AuthStyle::Anthropic } else { AuthStyle::Bearer },
            docs_url: String::new(),
            keys_url: String::new(),
            local: is_loopback_host(&host),
            requires_key: self.requires_key,
            verified_at: String::new(),
            credential_source: self.credential_source,
        }
        .into_def(true)
    }
}

/// Serializes tests that touch the process-wide custom list.
#[cfg(test)]
pub(crate) static TEST_CUSTOM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Replace the runtime custom list (on startup and after every change).
pub fn set_custom(list: &[CustomProvider]) {
    let defs = list.iter().map(|c| &*Box::leak(Box::new(c.to_def()))).collect();
    *custom_list().write().unwrap_or_else(|e| e.into_inner()) = defs;
}

/// `"My Server!"` → `"custom_my_server"`.
pub fn custom_id(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("custom_{}", if slug.is_empty() { "provider".into() } else { slug })
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok()?.host_str().map(|h| h.trim_matches(['[', ']']).to_lowercase())
}

pub fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Private, link-local, CGNAT, unique-local and unspecified addresses: the
/// SSRF-sensitive ranges (SPEC §6.3). Loopback is reported separately.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
                || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || v6.to_ipv4_mapped().is_some_and(|v4| is_private_ip(IpAddr::V4(v4)))
        }
    }
}

/// The result of checking a custom base URL. `warnings` must be shown and
/// acknowledged before saving; `errors` block saving outright.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UrlCheck {
    pub normalized: String,
    pub host: String,
    pub loopback: bool,
    pub resolves_private: bool,
    pub warnings: Vec<String>,
}

/// Validate a custom provider base URL (SPEC §6.3). DNS is resolved so a
/// public-looking name that points into the LAN is caught.
pub fn check_custom_url(raw: &str) -> Result<UrlCheck, String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| "Enter the full address, like https://ai.example.com/v1".to_string())?;
    match url.scheme() {
        "https" | "http" => {}
        s => return Err(format!("Only http and https are supported (got {s}:).")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Remove the name or password from the address. Add the key separately.".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Remove everything after ? or # from the address.".into());
    }
    let host = url.host_str().ok_or("The URL has no host.")?.trim_matches(['[', ']']).to_lowercase();
    let loopback = is_loopback_host(&host);
    if url.scheme() == "http" && !loopback {
        return Err("Use an address starting with https:// (http:// only works for servers on this computer).".into());
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<IpAddr> = if loopback {
        vec![]
    } else {
        (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| format!("Couldn't find {host}. Check the address and your internet connection."))?
            .map(|a| a.ip())
            .collect()
    };
    if addrs.iter().any(|ip| ip.is_loopback()) && !loopback {
        return Err(format!("{host} points to this computer. Use localhost instead."));
    }
    let resolves_private = addrs.iter().any(|ip| is_private_ip(*ip));
    let mut warnings = vec![format!(
        "Your prompts and code will be sent to {host}. Only continue if you trust it."
    )];
    if resolves_private {
        warnings.push(format!("{host} is on a home or work network. Make sure it's the right server."));
    }
    let mut normalized = url.to_string();
    while normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(UrlCheck { normalized, host, loopback, resolves_private, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses_with_unique_ids_and_safe_urls() {
        let all = registry();
        assert!(all.len() >= 25, "catalog shrank: {}", all.len());
        let mut seen = std::collections::HashSet::new();
        for p in all {
            assert!(seen.insert(p.id), "duplicate provider id {}", p.id);
            let host = host_of(p.base_url).expect("parseable base url");
            if p.local {
                assert!(is_loopback_host(&host), "{} is local but not loopback", p.id);
                assert!(!p.requires_key);
            } else {
                assert!(p.base_url.starts_with("https://"), "{} must be https", p.id);
            }
            assert!(!p.protocols.is_empty());
            assert!(!p.verified_at.is_empty(), "{} lacks verified_at", p.id);
            assert!(p.docs_url.is_empty() || p.docs_url.starts_with("https://"));
            // These publish no model list; the user types the model name.
            assert!(p.model_list_path.is_some() || p.credential_source == CredentialSource::CodexLogin || ["mistral_codestral", "netmind", "qianfan"].contains(&p.id));
        }
        assert!(!catalog_version().is_empty());
    }

    #[test]
    fn openrouter_entry_matches_verified_facts() {
        let p = get("openrouter").expect("openrouter must exist");
        assert_eq!(p.base_url, "https://openrouter.ai/api/v1");
        assert!(p.protocols.contains(&Protocol::OpenAiResponses));
        assert_eq!(p.model_list_path, Some("/models"));
        assert_eq!(get("anthropic").unwrap().auth_style, AuthStyle::Anthropic);
    }

    #[test]
    fn unknown_id_returns_none() {
        assert!(get("definitely-not-a-provider").is_none());
    }

    #[test]
    fn custom_ids_are_slugs() {
        assert_eq!(custom_id("My Server!"), "custom_my_server");
        assert_eq!(custom_id("  "), "custom_provider");
    }

    #[test]
    fn custom_providers_join_the_registry() {
        let _g = TEST_CUSTOM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let c = CustomProvider {
            id: "custom_unit_test_srv".into(),
            display_name: "Unit".into(),
            base_url: "http://localhost:9999/v1/".into(),
            protocol: Protocol::OpenAiChat,
            requires_key: false,
            consented_host: "localhost".into(),
            credential_source: CredentialSource::Keychain,
        };
        set_custom(std::slice::from_ref(&c));
        let p = get("custom_unit_test_srv").unwrap();
        assert!(p.custom && p.local);
        assert_eq!(p.base_url, "http://localhost:9999/v1");
        set_custom(&[]);
        assert!(get("custom_unit_test_srv").is_none());
    }

    #[test]
    fn url_checks_block_unsafe_shapes() {
        assert!(check_custom_url("ftp://x.example").is_err());
        assert!(check_custom_url("https://user:pw@api.example.com/v1").is_err());
        assert!(check_custom_url("https://api.example.com/v1?key=1").is_err());
        assert!(check_custom_url("http://api.example.com/v1").unwrap_err().contains("https"));
        let local = check_custom_url("http://localhost:11434/v1/").unwrap();
        assert!(local.loopback);
        assert_eq!(local.normalized, "http://localhost:11434/v1");
        let lit = check_custom_url("https://10.0.0.5/v1").unwrap();
        assert!(lit.resolves_private && lit.warnings.len() == 2);
    }

    #[test]
    fn private_ranges() {
        for ip in ["10.1.2.3", "192.168.0.1", "172.16.5.4", "169.254.169.254", "100.64.0.1", "0.0.0.0", "fd00::1", "fe80::1", "::ffff:10.0.0.1"] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2606:4700::1111"] {
            assert!(!is_private_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}
