//! More than one key per provider, importing keys you already have, and
//! profiles (save and switch a whole set of connections at once).
//!
//! - Extra keys live in the secure store next to the main one
//!   (`provider/k2`, `provider/k3`, …); the gateway spreads requests across
//!   them. `key_slots.json` only records how many there are.
//! - Import looks at your login shell's environment and another tool's
//!   `.env` files for the usual variable names (`OPENAI_API_KEY`, …). The
//!   scan returns masked keys only; the real values go straight from the
//!   source into the secure store when you choose Import.
//! - A profile is a saved list of (app, provider, model, routing); switching
//!   connects that list and disconnects the rest.

use crate::credentials::account_key;
use crate::service::Service;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

type R<T> = Result<T, String>;

pub const MAX_KEYS: usize = 5;

/// Environment variable → PrismFlow provider id (another tool's names plus the common ones).
pub const ENV_KEYS: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "openai"), ("ANTHROPIC_API_KEY", "anthropic"), ("OPENROUTER_API_KEY", "openrouter"),
    ("GROQ_API_KEY", "groq"), ("DEEPSEEK_API_KEY", "deepseek"), ("GEMINI_API_KEY", "gemini"), ("GOOGLE_API_KEY", "gemini"),
    ("MISTRAL_API_KEY", "mistral"), ("CODESTRAL_API_KEY", "mistral_codestral"), ("XAI_API_KEY", "xai"),
    ("TOGETHER_API_KEY", "together"), ("FIREWORKS_API_KEY", "fireworks"), ("NVIDIA_NIM_API_KEY", "nvidia_nim"), ("NVIDIA_API_KEY", "nvidia_nim"),
    ("HUGGINGFACE_API_KEY", "huggingface"), ("HF_TOKEN", "huggingface"), ("CEREBRAS_API_KEY", "cerebras"), ("DEEPINFRA_API_KEY", "deepinfra"),
    ("PERPLEXITY_API_KEY", "perplexity"), ("COHERE_API_KEY", "cohere"), ("MOONSHOT_API_KEY", "moonshot"), ("KIMI_API_KEY", "moonshot"),
    ("KIMI_CODE_API_KEY", "kimi_code"), ("ZAI_API_KEY", "zai_coding"), ("SAMBANOVA_API_KEY", "sambanova"), ("NOVITA_API_KEY", "novita"),
    ("SILICONFLOW_API_KEY", "siliconflow"), ("MINIMAX_API_KEY", "minimax"), ("OLLAMA_API_KEY", "ollama_cloud"),
    ("AI_GATEWAY_API_KEY", "vercel"), ("CHUTES_API_KEY", "chutes"), ("FEATHERLESS_API_KEY", "featherless"), ("SCW_SECRET_KEY", "scaleway"),
    ("LLM7_API_KEY", "llm7"), ("AWS_BEARER_TOKEN_BEDROCK", "bedrock"), ("ALIBABA_CLOUD_API_KEY", "alibaba_cloud"), ("DASHSCOPE_API_KEY", "alibaba_cloud"),
    ("QWENCLOUD_API_KEY", "qwencloud"), ("QWENCLOUD_CODING_API_KEY", "qwencloud_coding"), ("NEBIUS_API_KEY", "nebius"),
    ("WANDB_API_KEY", "wandb"), ("ZENMUX_API_KEY", "zenmux"), ("AGNES_API_KEY", "agnes"), ("CLINE_API_KEY", "cline_pass"),
    ("OPENCODE_API_KEY", "opencode_zen"), ("WAFER_API_KEY", "wafer"), ("KILO_API_KEY", "kilo"), ("TOKENROUTER_API_KEY", "tokenrouter"),
    ("NARAROUTE_API_KEY", "nararoute"), ("POOLSIDE_API_KEY", "poolside"), ("LIGHTNING_API_KEY", "lightning"), ("EXPLABS_API_KEY", "experiential"),
    ("CHEAPER_INFERENCE_API_KEY", "cheaperinference"), ("ORCAROUTER_API_KEY", "orcarouter"), ("ATLASCLOUD_API_KEY", "atlascloud"),
    ("XKIRO_API_KEY", "xkiro"), ("OPPER_API_KEY", "opper"), ("VENICE_API_KEY", "venice"), ("AI21_API_KEY", "ai21"),
    ("REKA_API_KEY", "reka"), ("WRITER_API_KEY", "writer"), ("POE_API_KEY", "poe"), ("REQUESTY_API_KEY", "requesty"),
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Found {
    pub provider_id: String,
    pub provider_name: String,
    pub variable: String,
    /// Where it was found ("Your shell", "another tool").
    pub source: String,
    /// e.g. `sk-or…a1b2`; the real value is never sent to the window.
    pub masked: String,
    /// The same key is already saved in PrismFlow.
    pub already_saved: bool,
}

pub fn mask(k: &str) -> String {
    let k = k.trim();
    if k.len() <= 10 {
        return "••••".into();
    }
    format!("{}…{}", &k[..5], &k[k.len() - 4..])
}

/// `KEY=value` lines (with optional `export`, quotes and comments).
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("export ").unwrap_or(l);
            if l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
                .unwrap_or_else(|| v.split(" #").next().unwrap_or(v).trim());
            (!k.trim().is_empty() && !v.is_empty()).then(|| (k.trim().to_string(), v.to_string()))
        })
        .collect()
}

/// Sources to scan: (label, variables).
fn sources() -> Vec<(String, BTreeMap<String, String>)> {
    let mut out = Vec::new();
    if let Ok(dir) = std::env::var("PRISMFLOW_IMPORT_TEST_DIR") {
        // Tests: fixed files instead of the real shell and home.
        for (label, f) in [("Your shell", "shell.env"), ("another tool", "other.env")] {
            if let Ok(t) = std::fs::read_to_string(PathBuf::from(&dir).join(f)) {
                out.push((label.to_string(), parse_env(&t)));
            }
        }
        return out;
    }
    // A GUI app doesn't see your shell's variables; ask a login shell.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(o) = std::process::Command::new(&shell).args(["-ilc", "env"]).stdin(std::process::Stdio::null()).output() {
        out.push(("Your shell".into(), parse_env(&String::from_utf8_lossy(&o.stdout))));
    }
    if let Some(home) = dirs::home_dir() {
        for f in [home.join(".other/.env"), home.join("other-tool/.env"), home.join(".config/other-tool/.env")] {
            if let Ok(t) = std::fs::read_to_string(&f) {
                out.push(("another tool".into(), parse_env(&t)));
            }
        }
    }
    out
}

/// (found, secret) pairs; the secret never leaves this module except into the store.
fn scan_with_secrets(svc: &Service) -> Vec<(Found, String)> {
    let mut out: Vec<(Found, String)> = Vec::new();
    for (label, vars) in sources() {
        for (var, provider) in ENV_KEYS {
            let Some(value) = vars.get(*var).map(|v| v.trim().to_string()).filter(|v| v.len() >= 8 && !v.contains(char::is_whitespace)) else { continue };
            let Some(p) = crate::providers::get(provider) else { continue };
            if out.iter().any(|(f, s)| f.provider_id == p.id && *s == value) {
                continue;
            }
            let saved = svc.all_keys(p.id).contains(&value);
            out.push((Found {
                provider_id: p.id.into(),
                provider_name: p.display_name.into(),
                variable: (*var).into(),
                source: label.clone(),
                masked: mask(&value),
                already_saved: saved,
            }, value));
        }
    }
    out
}

impl Service {
    fn slots(&self) -> BTreeMap<String, usize> {
        std::fs::read_to_string(self.state.dir().join("key_slots.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    fn save_slots(&self, s: &BTreeMap<String, usize>) -> R<()> {
        crate::config_engine::atomic_write(&self.state.dir().join("key_slots.json"), &serde_json::to_string(s).unwrap_or_default()).map_err(|e| e.to_string())
    }

    /// The extra keys (beyond the main one) for the gateway.
    pub fn extra_keys(&self, provider_id: &str) -> Vec<String> {
        let n = self.slots().get(provider_id).copied().unwrap_or(0);
        (2..n + 2).filter_map(|i| self.store.get(&account_key(provider_id, &format!("k{i}"))).ok()).collect()
    }

    /// Main key plus extras.
    pub fn all_keys(&self, provider_id: &str) -> Vec<String> {
        let mut v: Vec<String> = self.store.get(&account_key(provider_id, "default")).ok().into_iter().collect();
        v.extend(self.extra_keys(provider_id));
        v
    }

    pub fn key_count(&self, provider_id: &str) -> usize {
        let main = usize::from(self.store.get(&account_key(provider_id, "default")).is_ok());
        if main == 0 { 0 } else { 1 + self.slots().get(provider_id).copied().unwrap_or(0) }
    }

    /// Adds a key: the main one if none yet, otherwise an extra.
    pub fn add_key(&self, provider_id: &str, secret: &str) -> R<usize> {
        let p = crate::providers::get(provider_id).ok_or("Unknown provider.")?;
        let secret = secret.trim();
        if self.all_keys(p.id).iter().any(|k| k == secret) {
            return Err("That key is already saved.".into());
        }
        if self.key_count(p.id) == 0 {
            self.set_credential(p.id, secret)?;
            return Ok(1);
        }
        let mut slots = self.slots();
        let n = slots.get(p.id).copied().unwrap_or(0);
        if n + 1 >= MAX_KEYS {
            return Err(format!("Up to {MAX_KEYS} keys per provider."));
        }
        if secret.is_empty() || secret.chars().any(char::is_whitespace) {
            return Err("Paste the key exactly as given, with no spaces.".into());
        }
        self.store.set(&account_key(p.id, &format!("k{}", n + 2)), secret).map_err(|e| format!("Couldn't save the key: {e}"))?;
        slots.insert(p.id.into(), n + 1);
        self.save_slots(&slots)?;
        Ok(n + 2)
    }

    /// Removes the extra keys, keeping the main one.
    pub fn remove_extra_keys(&self, provider_id: &str) -> R<()> {
        let mut slots = self.slots();
        let n = slots.remove(provider_id).unwrap_or(0);
        for i in 2..n + 2 {
            let _ = self.store.delete(&account_key(provider_id, &format!("k{i}")));
        }
        self.save_slots(&slots)
    }

    pub fn scan_keys(&self) -> Vec<Found> {
        scan_with_secrets(self).into_iter().map(|(f, _)| f).collect()
    }

    /// Imports the chosen (provider, variable, source) entries.
    pub fn import_keys(&self, chosen: &[(String, String, String)]) -> R<usize> {
        let mut n = 0;
        for (f, secret) in scan_with_secrets(self) {
            if f.already_saved || !chosen.iter().any(|(p, v, s)| *p == f.provider_id && *v == f.variable && *s == f.source) {
                continue;
            }
            self.add_key(&f.provider_id, &secret)?;
            n += 1;
        }
        self.diag.record(crate::diagnostics::Level::Info, "keys.imported", &format!("Imported {n} key(s)."));
        Ok(n)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub app_id: String,
    pub provider_id: String,
    pub model: String,
    #[serde(default)]
    pub routing: crate::routing::Routing,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profiles {
    pub active: Option<String>,
    pub list: BTreeMap<String, Vec<ProfileEntry>>,
}

impl Service {
    pub fn profiles(&self) -> Profiles {
        std::fs::read_to_string(self.state.dir().join("profiles.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    fn save_profiles(&self, p: &Profiles) -> R<()> {
        crate::config_engine::atomic_write(&self.state.dir().join("profiles.json"), &serde_json::to_string_pretty(p).unwrap_or_default()).map_err(|e| e.to_string())
    }

    /// Saves the current connections as `name`.
    pub fn save_profile(&self, name: &str) -> R<Profiles> {
        let name = name.trim();
        if name.is_empty() || name.len() > 40 {
            return Err("Give the profile a name (up to 40 characters).".into());
        }
        let entries: Vec<ProfileEntry> = self.connections()?.into_iter()
            .map(|c| ProfileEntry { app_id: c.app_id, provider_id: c.provider_id, model: c.model, routing: c.routing })
            .collect();
        let mut p = self.profiles();
        p.list.insert(name.into(), entries);
        p.active = Some(name.into());
        self.save_profiles(&p)?;
        Ok(p)
    }

    pub fn delete_profile(&self, name: &str) -> R<Profiles> {
        let mut p = self.profiles();
        p.list.remove(name);
        if p.active.as_deref() == Some(name) {
            p.active = None;
        }
        self.save_profiles(&p)?;
        Ok(p)
    }

    /// Switches to `name`: connects its entries, disconnects everything else.
    pub async fn switch_profile(&self, name: &str) -> R<Profiles> {
        let mut p = self.profiles();
        let entries = p.list.get(name).cloned().ok_or("Unknown profile.")?;
        let mut keep = Vec::new();
        let mut problems = Vec::new();
        for e in &entries {
            match self.connect(&e.app_id, &e.provider_id, &e.model).await {
                Ok(rec) => {
                    if e.routing != Default::default() {
                        let _ = self.set_routing(&rec.id, e.routing.clone()).await;
                    }
                    keep.push(rec.id);
                }
                Err(err) => problems.push(format!("{} → {}: {err}", crate::service::app_name(&e.app_id), e.provider_id)),
            }
        }
        for c in self.connections()? {
            if !keep.contains(&c.id) {
                if let Err(err) = self.disconnect(&c.id).await {
                    problems.push(err);
                }
            }
        }
        p.active = Some(name.into());
        self.save_profiles(&p)?;
        if problems.is_empty() { Ok(p) } else { Err(format!("Switched, with problems:\n{}", problems.join("\n"))) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_files() {
        let m = parse_env("# keys\nexport OPENAI_API_KEY=\"sk-abc123456789\"\nGROQ_API_KEY='gsk_1234567890' \nEMPTY=\nNVIDIA_API_KEY=nvapi-xyz12345 # mine\n");
        assert_eq!(m["OPENAI_API_KEY"], "sk-abc123456789");
        assert_eq!(m["GROQ_API_KEY"], "gsk_1234567890");
        assert_eq!(m["NVIDIA_API_KEY"], "nvapi-xyz12345");
        assert!(!m.contains_key("EMPTY"));
        assert_eq!(mask("sk-or-v1-abcdef123456"), "sk-or…3456");
        for (_, p) in ENV_KEYS {
            assert!(crate::providers::get(p).is_some(), "{p} is not a provider id");
        }
    }
}
