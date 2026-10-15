/// Supported wire protocols (SPEC §5.2 — these are NOT interchangeable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Protocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

/// A provider definition. Only verified facts live here (SPEC §3.3):
/// identity, base URL, protocol family, docs. Capabilities are discovered
/// and verified at runtime, never asserted statically.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Empty = the user must supply one (custom providers).
    pub base_url: &'static str,
    pub protocols: &'static [Protocol],
    pub model_list_path: Option<&'static str>,
    /// A documented, side-effect-free endpoint that fails on a bad key
    /// (SPEC §9 Stage 2). None = no safe check known; Stage 2 is skipped.
    pub auth_check_path: Option<&'static str>,
    /// Empty = no public page yet; the UI renders "—" rather than a dead link.
    pub docs_url: &'static str,
}

pub static REGISTRY: &[ProviderDefinition] = &[
    ProviderDefinition {
        id: "openrouter",
        display_name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        protocols: &[Protocol::OpenAiChat, Protocol::OpenAiResponses],
        model_list_path: Some("/models"),
        auth_check_path: Some("/key"),
        docs_url: "https://openrouter.ai/docs",
    },
    ProviderDefinition {
        id: "nvidia_nim",
        display_name: "NVIDIA NIM",
        base_url: "https://integrate.api.nvidia.com/v1",
        protocols: &[Protocol::OpenAiChat],
        model_list_path: Some("/models"),
        auth_check_path: None,
        docs_url: "https://build.nvidia.com",
    },
    ProviderDefinition {
        id: "huggingface",
        display_name: "Hugging Face Inference Providers",
        base_url: "https://router.huggingface.co/v1",
        protocols: &[Protocol::OpenAiChat, Protocol::OpenAiResponses],
        model_list_path: Some("/models"),
        auth_check_path: None,
        docs_url: "https://huggingface.co/docs/inference-providers",
    },
    ProviderDefinition {
        id: "openai",
        display_name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        protocols: &[Protocol::OpenAiChat, Protocol::OpenAiResponses],
        model_list_path: Some("/models"),
        auth_check_path: Some("/models"),
        docs_url: "https://platform.openai.com/docs",
    },
    ProviderDefinition {
        id: "anthropic",
        display_name: "Anthropic",
        base_url: "https://api.anthropic.com",
        protocols: &[Protocol::AnthropicMessages],
        model_list_path: Some("/v1/models"),
        auth_check_path: None,
        docs_url: "https://docs.anthropic.com",
    },
    ProviderDefinition {
        id: "custom_openai",
        display_name: "Custom OpenAI-compatible API",
        base_url: "",
        protocols: &[Protocol::OpenAiChat, Protocol::OpenAiResponses],
        model_list_path: Some("/models"),
        auth_check_path: None,
        docs_url: "",
    },
    ProviderDefinition {
        id: "custom_anthropic",
        display_name: "Custom Anthropic-compatible API",
        base_url: "",
        protocols: &[Protocol::AnthropicMessages],
        model_list_path: Some("/v1/models"),
        auth_check_path: None,
        docs_url: "",
    },
];

pub fn registry() -> &'static [ProviderDefinition] {
    REGISTRY
}

pub fn get(id: &str) -> Option<&'static ProviderDefinition> {
    REGISTRY.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_unique_ids_and_urls_parse() {
        let mut seen = std::collections::HashSet::new();
        for p in registry() {
            assert!(seen.insert(p.id), "duplicate provider id {}", p.id);
            if !p.base_url.is_empty() {
                assert!(p.base_url.starts_with("https://"), "{} must be https", p.id);
            }
            if !p.docs_url.is_empty() {
                assert!(p.docs_url.starts_with("https://"), "{} docs must be https", p.id);
            }
            assert!(!p.protocols.is_empty());
        }
    }

    #[test]
    fn openrouter_entry_matches_verified_facts() {
        let p = get("openrouter").expect("openrouter must exist");
        assert_eq!(p.base_url, "https://openrouter.ai/api/v1");
        assert!(p.protocols.contains(&Protocol::OpenAiResponses));
        assert_eq!(p.model_list_path, Some("/models"));
    }

    #[test]
    fn custom_providers_require_user_base_url() {
        assert!(get("custom_openai").unwrap().base_url.is_empty());
        assert!(get("custom_anthropic").unwrap().base_url.is_empty());
    }

    #[test]
    fn unknown_id_returns_none() {
        assert!(get("definitely-not-a-provider").is_none());
    }
}
