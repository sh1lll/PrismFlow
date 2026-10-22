//! Model routing for gateway connections: per-tier models, a reasoning
//! policy, and ordered fallbacks. Everything here is
//! pure; the broker applies it per request.
//!
//! Claude Code's model slots are set to stable aliases (`prismflow-opus`,
//! …). The broker resolves an alias to a provider + model, so tiers can use
//! different providers without the agent knowing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ALIAS_PREFIX: &str = "prismflow-";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider_id: String,
    pub model: String,
}

impl ModelRef {
    pub fn new(provider_id: &str, model: &str) -> Self {
        Self { provider_id: provider_id.into(), model: model.into() }
    }
}

/// Claude Code's model slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Default,
    Fable,
    Opus,
    Sonnet,
    Haiku,
    Subagent,
}

impl Tier {
    pub const ALL: [Tier; 6] = [Tier::Default, Tier::Fable, Tier::Opus, Tier::Sonnet, Tier::Haiku, Tier::Subagent];

    pub fn alias(self) -> String {
        format!("{ALIAS_PREFIX}{}", self.slug())
    }

    fn slug(self) -> &'static str {
        match self {
            Tier::Default => "default",
            Tier::Fable => "fable",
            Tier::Opus => "opus",
            Tier::Sonnet => "sonnet",
            Tier::Haiku => "haiku",
            Tier::Subagent => "subagent",
        }
    }

    pub fn from_alias(model: &str) -> Option<Tier> {
        let slug = model.strip_prefix(ALIAS_PREFIX)?;
        Tier::ALL.into_iter().find(|t| t.slug() == slug)
    }

    /// A Claude model name (`claude-opus-4-…`, `claude-3-5-haiku-…`) → its
    /// slot; Claude Desktop sends these names.
    pub fn from_claude_name(model: &str) -> Option<Tier> {
        let m = model.to_ascii_lowercase();
        if !m.starts_with("claude") {
            return None;
        }
        [Tier::Fable, Tier::Opus, Tier::Sonnet, Tier::Haiku].into_iter().find(|t| m.contains(t.slug()))
    }
}

/// Reasoning policy (Off / From client / Low…Max, per-tier Inherit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Inherit,
    Off,
    #[default]
    FromClient,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    fn level(self) -> Option<&'static str> {
        match self {
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::Xhigh => Some("xhigh"),
            Effort::Max => Some("max"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Routing {
    /// Tier → model. Missing tiers use the connection's own model.
    #[serde(default)]
    pub tiers: BTreeMap<Tier, ModelRef>,
    #[serde(default)]
    pub reasoning: Effort,
    #[serde(default)]
    pub tier_reasoning: BTreeMap<Tier, Effort>,
    /// Tried in order when the chosen model fails before output starts.
    #[serde(default)]
    pub fallbacks: Vec<ModelRef>,
    /// The user accepted that one request may be billed by several
    /// providers. Fallbacks are ignored without it.
    #[serde(default)]
    pub fallback_consent: bool,
    /// Claude Code only: pass shell output through RTK before the model sees it.
    #[serde(default)]
    pub output_filter: bool,
}

impl Routing {
    /// Every provider + model this routing can reach (engine config needs them).
    pub fn refs<'a>(&'a self, primary: &'a ModelRef) -> Vec<&'a ModelRef> {
        let mut v = vec![primary];
        v.extend(self.tiers.values());
        if self.fallback_consent {
            v.extend(self.fallbacks.iter());
        }
        v
    }

    pub fn effort_for(&self, tier: Tier) -> Effort {
        match self.tier_reasoning.get(&tier).copied().unwrap_or(Effort::Inherit) {
            Effort::Inherit => self.reasoning,
            e => e,
        }
    }

    /// The ordered attempts for a request whose body names `requested`.
    /// A model the app picked from its menu: that model first, then the
    /// backups (tier mappings don't apply).
    pub fn plan_exact(&self, primary: &ModelRef) -> Vec<ModelRef> {
        let mut chain = vec![primary.clone()];
        if self.fallback_consent {
            for f in &self.fallbacks {
                if !chain.contains(f) {
                    chain.push(f.clone());
                }
            }
        }
        chain
    }

    pub fn plan(&self, primary: &ModelRef, requested: Option<&str>) -> (Tier, Vec<ModelRef>) {
        let tier = requested.and_then(|m| Tier::from_alias(m).or_else(|| Tier::from_claude_name(m))).unwrap_or(Tier::Default);
        let first = self.tiers.get(&tier).cloned().unwrap_or_else(|| primary.clone());
        let mut chain = vec![first];
        if self.fallback_consent {
            for f in &self.fallbacks {
                if !chain.contains(f) {
                    chain.push(f.clone());
                }
            }
        }
        (tier, chain)
    }
}

/// Which front a request came in on; decides where reasoning lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Front {
    Messages,
    Responses,
    Chat,
    Other,
}

impl Front {
    pub fn of(path: &str) -> Front {
        match path {
            "/v1/messages" => Front::Messages,
            "/v1/responses" => Front::Responses,
            "/v1/chat/completions" => Front::Chat,
            _ => Front::Other,
        }
    }
}

/// Apply the reasoning policy to a request body in place. `FromClient`
/// leaves it alone; providers translate only controls they support (the
/// engine maps effort levels to the closest one the target accepts).
pub fn apply_effort(body: &mut serde_json::Value, front: Front, effort: Effort) {
    let Some(obj) = body.as_object_mut() else { return };
    match (front, effort) {
        (_, Effort::FromClient | Effort::Inherit) | (Front::Other, _) => {}
        (Front::Messages, Effort::Off) => {
            obj.insert("thinking".into(), serde_json::json!({"type": "disabled"}));
            if let Some(oc) = obj.get_mut("output_config").and_then(|v| v.as_object_mut()) {
                oc.remove("effort");
            }
        }
        (Front::Messages, e) => {
            let oc = obj.entry("output_config").or_insert_with(|| serde_json::json!({}));
            if let Some(oc) = oc.as_object_mut() {
                oc.insert("effort".into(), e.level().unwrap_or("medium").into());
            }
        }
        (Front::Responses, Effort::Off) => {
            obj.remove("reasoning");
        }
        (Front::Responses, e) => {
            let r = obj.entry("reasoning").or_insert_with(|| serde_json::json!({}));
            if let Some(r) = r.as_object_mut() {
                r.insert("effort".into(), e.level().unwrap_or("medium").into());
            }
        }
        (Front::Chat, Effort::Off) => {
            obj.remove("reasoning_effort");
        }
        (Front::Chat, e) => {
            obj.insert("reasoning_effort".into(), e.level().unwrap_or("medium").into());
        }
    }
}

/// Whether an upstream answer should move on to the next fallback. Only
/// failures before any output qualify; the broker checks this on the status
/// line, before streaming the body.
pub fn should_fall_back(status: u16) -> bool {
    !(200..300).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn primary() -> ModelRef {
        ModelRef::new("nvidia_nim", "meta/llama")
    }

    #[test]
    fn claude_desktop_names_use_their_slot() {
        let primary = ModelRef::new("ollama", "qwen3");
        let mut r = Routing::default();
        r.tiers.insert(Tier::Opus, ModelRef::new("openrouter", "big"));
        assert_eq!(r.plan(&primary, Some("claude-opus-4-20250514")).1[0], ModelRef::new("openrouter", "big"));
        assert_eq!(r.plan(&primary, Some("claude-3-opus-20240229")).1[0], ModelRef::new("openrouter", "big"));
        // Unset slot: the connection's own model.
        assert_eq!(r.plan(&primary, Some("claude-3-5-haiku-20241022")).1[0], primary);
    }

    #[test]
    fn aliases_round_trip() {
        for t in Tier::ALL {
            assert_eq!(Tier::from_alias(&t.alias()), Some(t));
        }
        assert_eq!(Tier::from_alias("claude-opus-4"), None);
        assert_eq!(Tier::from_claude_name("claude-opus-4-20250514"), Some(Tier::Opus));
        assert_eq!(Tier::from_claude_name("claude-3-5-haiku-20241022"), Some(Tier::Haiku));
        assert_eq!(Tier::from_claude_name("claude-fable-5"), Some(Tier::Fable));
        assert_eq!(Tier::from_claude_name("gpt-opus"), None);
        assert_eq!(Tier::from_alias("prismflow-nope"), None);
    }

    #[test]
    fn tiers_route_and_unknown_models_use_the_primary() {
        let mut r = Routing::default();
        r.tiers.insert(Tier::Haiku, ModelRef::new("groq", "small"));
        assert_eq!(r.plan(&primary(), Some("prismflow-haiku")).1, vec![ModelRef::new("groq", "small")]);
        assert_eq!(r.plan(&primary(), Some("prismflow-opus")).1, vec![primary()]);
        // Claude names map to their slot; an unset slot uses the primary.
        assert_eq!(r.plan(&primary(), Some("claude-sonnet-x")), (Tier::Sonnet, vec![primary()]));
        assert_eq!(r.plan(&primary(), Some("gpt-4o")), (Tier::Default, vec![primary()]));
        assert_eq!(r.plan(&primary(), None).1, vec![primary()]);
    }

    #[test]
    fn fallbacks_need_consent_and_skip_duplicates() {
        let mut r = Routing { fallbacks: vec![ModelRef::new("groq", "a"), primary()], ..Default::default() };
        assert_eq!(r.plan(&primary(), None).1.len(), 1, "no consent, no fallbacks");
        assert_eq!(r.refs(&primary()).len(), 1);
        r.fallback_consent = true;
        assert_eq!(r.plan(&primary(), None).1, vec![primary(), ModelRef::new("groq", "a")]);
    }

    #[test]
    fn tier_reasoning_inherits_the_root_policy() {
        let mut r = Routing { reasoning: Effort::High, ..Default::default() };
        r.tier_reasoning.insert(Tier::Haiku, Effort::Off);
        r.tier_reasoning.insert(Tier::Opus, Effort::Inherit);
        assert_eq!(r.effort_for(Tier::Haiku), Effort::Off);
        assert_eq!(r.effort_for(Tier::Opus), Effort::High);
        assert_eq!(r.effort_for(Tier::Default), Effort::High);
    }

    #[test]
    fn effort_lands_where_each_protocol_expects_it() {
        let mut m = json!({"model": "x", "thinking": {"type": "enabled", "budget_tokens": 1000}});
        apply_effort(&mut m, Front::Messages, Effort::Low);
        assert_eq!(m["output_config"]["effort"], "low");
        apply_effort(&mut m, Front::Messages, Effort::Off);
        assert_eq!(m["thinking"]["type"], "disabled");
        assert!(m["output_config"].get("effort").is_none());

        let mut r = json!({"model": "x", "reasoning": {"summary": "auto"}});
        apply_effort(&mut r, Front::Responses, Effort::Max);
        assert_eq!(r["reasoning"], json!({"summary": "auto", "effort": "max"}));
        apply_effort(&mut r, Front::Responses, Effort::Off);
        assert!(r.get("reasoning").is_none());

        let mut c = json!({"model": "x"});
        apply_effort(&mut c, Front::Chat, Effort::Xhigh);
        assert_eq!(c["reasoning_effort"], "xhigh");

        let before = json!({"model": "x", "reasoning": {"effort": "high"}});
        let mut same = before.clone();
        apply_effort(&mut same, Front::Responses, Effort::FromClient);
        assert_eq!(same, before);
    }

    #[test]
    fn only_success_stops_the_chain() {
        assert!(!should_fall_back(200));
        for s in [400, 401, 404, 429, 500, 502, 503] {
            assert!(should_fall_back(s));
        }
    }

    #[test]
    fn routing_deserializes_from_partial_json() {
        let r: Routing = serde_json::from_value(json!({"reasoning": "high"})).unwrap();
        assert_eq!(r.reasoning, Effort::High);
        assert!(r.tiers.is_empty() && !r.fallback_consent);
    }
}
