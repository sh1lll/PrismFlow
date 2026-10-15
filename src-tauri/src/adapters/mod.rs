//! Application adapters (SPEC §4). Provider and application support stay
//! independent: an adapter consumes a `ProviderDefinition`, never owns one.

pub mod codex;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AppDetection {
    pub app_id: String,
    pub display_name: String,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// True only when the installed version is inside the adapter's
    /// verified range (SPEC §21.2).
    pub version_verified: bool,
    pub notes: Vec<String>,
}

pub fn detect_all() -> Vec<AppDetection> {
    vec![codex::detect()]
}
