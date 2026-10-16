//! Terminal coding agents launched through PrismFlow (Aider, Cline, Pi,
//! Hermes, OpenCode, DeepSeek Harness, Grok Build, Muse Code). Mechanisms verified 2026-10-14 (research/agent-mechanisms-…md).
//!
//! Each connection owns a directory in PrismFlow's state dir holding a
//! launch script and, where the agent needs one, an isolated config. The
//! user's own agent config is never read or written. No secret is stored:
//! the script fetches the connection's revocable gateway token at launch
//! (or the agent runs the token helper itself, for Pi).

use super::AppDetection;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MARKER: &str = "# Managed by PrismFlow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Aider,
    Cline,
    Pi,
    Hermes,
    OpenCode,
    Dsh,
    Grok,
    Muse,
    Qwen,
    Goose,
    Crush,
    Droid,
    ContinueCli,
    CopilotCli,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentSpec {
    pub id: &'static str,
    pub display_name: &'static str,
    pub binary: &'static str,
    pub kind: Kind,
    /// Version verified on 2026-10-14 (shown when detection differs).
    pub verified_version: &'static str,
    /// The agent works through tool calls (Stage 6 applies).
    pub tools: bool,
}

pub const AGENTS: &[AgentSpec] = &[
    AgentSpec { id: "aider", display_name: "Aider", binary: "aider", kind: Kind::Aider, verified_version: "0.86.2", tools: false },
    AgentSpec { id: "cline", display_name: "Cline CLI", binary: "cline", kind: Kind::Cline, verified_version: "3.0.70", tools: true },
    AgentSpec { id: "pi", display_name: "Pi", binary: "pi", kind: Kind::Pi, verified_version: "1.1.0", tools: true },
    AgentSpec { id: "hermes", display_name: "Hermes Agent", binary: "hermes", kind: Kind::Hermes, verified_version: "0.21.6", tools: true },
    // OpenCode 1.19.0 and 2.0.29 both verified 2026-10-15; the script picks the config format at launch.
    AgentSpec { id: "opencode", display_name: "OpenCode", binary: "opencode", kind: Kind::OpenCode, verified_version: "2.0.29", tools: true },
    AgentSpec { id: "dsh", display_name: "DeepSeek Harness", binary: "dsh", kind: Kind::Dsh, verified_version: "0.2.0-rc.2", tools: true },
    AgentSpec { id: "grok", display_name: "Grok Build", binary: "grok", kind: Kind::Grok, verified_version: "1.0.52", tools: true },
    AgentSpec { id: "muse", display_name: "Muse Code", binary: "muse", kind: Kind::Muse, verified_version: "1.4.8", tools: true },
    // Verified live 2026-10-15 through the broker (nonce read with a tool), except Copilot CLI (see its notes).
    AgentSpec { id: "qwen", display_name: "Qwen Code", binary: "qwen", kind: Kind::Qwen, verified_version: "0.25.2", tools: true },
    AgentSpec { id: "goose", display_name: "Goose", binary: "goose", kind: Kind::Goose, verified_version: "1.54.0", tools: true },
    AgentSpec { id: "crush", display_name: "Crush", binary: "crush", kind: Kind::Crush, verified_version: "0.98.1", tools: true },
    AgentSpec { id: "droid", display_name: "Factory Droid", binary: "droid", kind: Kind::Droid, verified_version: "0.242.1", tools: true },
    AgentSpec { id: "continue_cli", display_name: "Continue CLI", binary: "cn", kind: Kind::ContinueCli, verified_version: "1.5.47", tools: true },
    AgentSpec { id: "copilot_cli", display_name: "GitHub Copilot CLI", binary: "copilot", kind: Kind::CopilotCli, verified_version: "1.0.98", tools: true },
];

pub fn spec(app_id: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|a| a.id == app_id)
}

pub fn find_binary(spec: &AgentSpec) -> Option<PathBuf> {
    let env = format!("PRISMFLOW_{}_BIN", spec.id.to_ascii_uppercase());
    if let Some(p) = std::env::var_os(env) {
        return Some(PathBuf::from(p));
    }
    super::find_exe(spec.binary)
}

pub fn detect(spec: &AgentSpec) -> AppDetection {
    let mut d = AppDetection {
        app_id: spec.id.into(),
        display_name: spec.display_name.into(),
        installed: false,
        path: None,
        version: None,
        version_verified: false,
        notes: vec!["PrismFlow opens it for you. Your own settings for it don't change.".into()],
    };
    let Some(bin) = find_binary(spec) else {
        d.notes = vec![format!("{} was not found on PATH or in common install locations.", spec.display_name)];
        return d;
    };
    d.path = Some(bin.display().to_string());
    let out = Command::new(&bin).arg("--version").env("PATH", super::codex::child_path(&bin)).output();
    if let Some(v) = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string()) {
        d.installed = true;
        d.version_verified = v.contains(spec.verified_version);
        if !d.version_verified {
            d.notes.push(format!("Tested with {} {}. Press Test after connecting.", spec.display_name, spec.verified_version));
        }
        d.version = Some(v);
    } else {
        d.notes.push(format!("Found {}, but it didn't start.", bin.display()));
    }
    d
}

pub fn agent_dir(state_dir: &Path, connection_id: &str) -> PathBuf {
    state_dir.join("agents").join(connection_id.replace(':', "-"))
}

/// Which kind of launch script to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// POSIX `sh` (macOS, Linux).
    Posix,
    /// Windows PowerShell (`powershell -File launch.ps1`).
    PowerShell,
}

impl Shell {
    pub const HERE: Shell = if cfg!(windows) { Shell::PowerShell } else { Shell::Posix };
}

pub fn launch_script(dir: &Path) -> PathBuf {
    launch_script_for(dir, Shell::HERE)
}

pub fn launch_script_for(dir: &Path, shell: Shell) -> PathBuf {
    dir.join(match shell { Shell::Posix => "launch.sh", Shell::PowerShell => "launch.ps1" })
}

/// How the app starts the script: the program and its leading arguments.
pub fn script_runner(script: &Path) -> (String, Vec<String>) {
    match Shell::HERE {
        Shell::Posix => ("/bin/sh".into(), vec![script.display().to_string()]),
        Shell::PowerShell => ("powershell".into(), vec!["-NoProfile".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), script.display().to_string()]),
    }
}

/// The command line a user types to start the agent (Open in Terminal).
pub fn launch_command_line(script: &Path) -> String {
    match Shell::HERE {
        Shell::Posix => q(&script.display().to_string()),
        Shell::PowerShell => format!("powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"", script.display()),
    }
}

/// POSIX single quotes.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A shell word: bare when it is plainly safe, quoted otherwise.
fn qw(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:".contains(c)) { s.to_string() } else { q(s) }
}

/// PowerShell single quotes.
fn pq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// How one agent starts, independent of the shell that runs it.
#[derive(Default)]
struct Launch {
    /// Environment variable that receives the gateway token (fetched at launch).
    token_env: Option<&'static str>,
    env: Vec<(String, String)>,
    files: Vec<(PathBuf, String)>,
    /// A file written at every launch with the token substituted for `{TOKEN}` (Droid).
    token_file: Option<(PathBuf, String)>,
    /// OpenCode: the config differs between 1.x and 2.x; picked from `--version`.
    version_switch: Option<VersionSwitch>,
    /// Special first arguments, checked in order.
    branches: Vec<Branch>,
    /// Arguments placed before the user's own for a normal launch.
    default_args: Vec<String>,
}

struct Branch {
    /// The first argument values that select this branch.
    on: Vec<&'static str>,
    /// Keep the matched argument as the first argument (`exec`, `resume`).
    keep_first: bool,
    env: Vec<(String, String)>,
    args: Vec<String>,
}

struct VersionSwitch {
    env_key: &'static str,
    v2_value: String,
    v1_value: String,
    /// With 2.x, `run` gets this flag right after it.
    v2_run_flag: &'static str,
}

fn s(x: &str) -> String {
    x.to_string()
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Files to write for a connection: (path, contents). The first is the
/// launch script (the connection's managed file).
pub fn plan_files(
    spec: &AgentSpec,
    dir: &Path,
    bin: &Path,
    helper: &Path,
    connection_id: &str,
    broker_url: &str,
    model: &str,
) -> Vec<(PathBuf, String)> {
    plan_files_for(Shell::HERE, spec, dir, bin, helper, connection_id, broker_url, model)
}

#[allow(clippy::too_many_arguments)]
pub fn plan_files_for(
    shell: Shell,
    spec: &AgentSpec,
    dir: &Path,
    bin: &Path,
    helper: &Path,
    connection_id: &str,
    broker_url: &str,
    model: &str,
) -> Vec<(PathBuf, String)> {
    let base = format!("{broker_url}/v1");
    let path_of = |p: PathBuf| p.display().to_string();
    // The token command as other programs run it (Pi runs it itself).
    let token_cmd = match shell {
        Shell::Posix => format!("{} token print --connection {}", q(&helper.display().to_string()), q(connection_id)),
        Shell::PowerShell => format!("\"{}\" token print --connection \"{}\"", helper.display(), connection_id),
    };
    let mut l = Launch::default();
    match spec.kind {
        Kind::Aider => {
            l.token_env = Some("OPENAI_API_KEY");
            l.env = env(&[("OPENAI_API_BASE", &base)]);
            l.default_args = vec![s("--model"), format!("openai/{model}"), s("--no-show-model-warnings"), s("--analytics-disable"), s("--no-check-update")];
        }
        Kind::Cline => {
            let providers = serde_json::json!({
                "version": 1,
                "lastUsedProvider": "openai-compatible",
                "modes": {},
                // Cline 3.0.70 silently ignores an entry without `updatedAt` and
                // falls back to api.openai.com (verified 2026-10-14).
                "providers": { "openai-compatible": { "settings": {
                    "provider": "openai-compatible", "model": model, "baseUrl": base
                }, "updatedAt": "2026-10-14T00:00:00.000Z", "tokenSource": "manual" } }
            });
            l.files.push((dir.join("cline-data/settings/providers.json"), serde_json::to_string_pretty(&providers).unwrap()));
            l.token_env = Some("OPENAI_API_KEY");
            // Defense in depth: the gateway is plain-http loopback, so a dead
            // HTTPS proxy keeps the token from ever reaching a remote host if
            // Cline falls back to a default endpoint.
            l.env = env(&[("HTTPS_PROXY", "http://127.0.0.1:9"), ("https_proxy", "http://127.0.0.1:9"), ("NO_PROXY", "127.0.0.1,localhost"), ("no_proxy", "127.0.0.1,localhost")]);
            l.default_args = vec![s("--data-dir"), path_of(dir.join("cline-data")), s("-P"), s("openai-compatible"), s("-m"), s(model)];
        }
        Kind::Pi => {
            let models = serde_json::json!({ "providers": { "prismflow": {
                "baseUrl": base, "api": "openai-completions", "apiKey": format!("!{token_cmd}"),
                "models": [{ "id": model }]
            } } });
            l.files.push((dir.join("pi-agent/models.json"), serde_json::to_string_pretty(&models).unwrap()));
            l.env = env(&[("PI_CODING_AGENT_DIR", &path_of(dir.join("pi-agent")))]);
            l.default_args = vec![s("--model"), format!("prismflow/{model}")];
        }
        Kind::Hermes => {
            let cfg = format!(
                "{MARKER}\nmodel:\n  provider: \"custom\"\n  base_url: {}\n  api_key: \"${{PRISMFLOW_AGENT_TOKEN}}\"\n  default: {}\n",
                serde_json::to_string(&base).unwrap(),
                serde_json::to_string(model).unwrap()
            );
            l.files.push((dir.join("hermes-home/config.yaml"), cfg));
            l.token_env = Some("PRISMFLOW_AGENT_TOKEN");
            l.env = env(&[("HERMES_HOME", &path_of(dir.join("hermes-home")))]);
        }
        Kind::OpenCode => {
            // Process-local config (OPENCODE_CONFIG_CONTENT); verified with
            // opencode-ai 1.19.0 and @opencode/cli 2.0.29, which use different keys.
            let model_def = |v2: bool| {
                let m = if v2 {
                    serde_json::json!({ "name": model, "capabilities": { "tools": true, "input": ["text"], "output": ["text"] }, "limit": { "context": 32768, "output": 4096 } })
                } else {
                    serde_json::json!({ "name": model, "tool_call": true, "limit": { "context": 32768, "output": 4096 } })
                };
                serde_json::json!({ "main": m })
            };
            let v1 = serde_json::json!({
                "provider": { "prismflow": { "npm": "@ai-sdk/openai-compatible", "name": "PrismFlow",
                    "options": { "baseURL": base, "apiKey": "{env:PRISMFLOW_API_KEY}", "timeout": 600000 },
                    "models": model_def(false) } },
                "enabled_providers": ["prismflow"], "model": "prismflow/main", "small_model": "prismflow/main",
                "autoupdate": false, "share": "disabled"
            });
            let v2 = serde_json::json!({
                "providers": { "prismflow": { "name": "PrismFlow", "package": "@opencode/ai/providers/openai-compatible",
                    "settings": { "baseURL": base, "apiKey": "{env:PRISMFLOW_API_KEY}" },
                    "models": model_def(true) } },
                "experimental": { "policies": [
                    { "action": "provider.use", "resource": "*", "effect": "deny" },
                    { "action": "provider.use", "resource": "prismflow", "effect": "allow" } ] },
                "model": "prismflow/main", "agents": { "title": { "model": "prismflow/main" } }
            });
            l.token_env = Some("PRISMFLOW_API_KEY");
            l.env = env(&[("OPENCODE_DISABLE_MODELS_FETCH", "1"), ("OPENCODE_DISABLE_AUTOUPDATE", "1"), ("OPENCODE_DISABLE_SHARE", "1"),
                ("OPENCODE_DISABLE_CLAUDE_CODE", "1"), ("OPENCODE_DISABLE_LSP_DOWNLOAD", "1")]);
            // One-shot runs on 2.x get a private server instead of the background service.
            l.version_switch = Some(VersionSwitch { env_key: "OPENCODE_CONFIG_CONTENT", v2_value: v2.to_string(), v1_value: v1.to_string(), v2_run_flag: "--standalone" });
        }
        Kind::Dsh => {
            // A launcher patch (verified with @deepseek-ai/dsh 0.2.0-rc.2):
            // PrismFlow's provider, DeepSeek's own account/web rows off.
            let creds = dir.join("dsh-credentials.yaml");
            let patch = format!(
                "{MARKER}\n- id: credentials\n  name: '@deepseek-ai/dsh-credentials-local'\n  config: {{ path: {}, watch: false }}\n\
                 - id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n  config:\n    providers:\n      prismflow:\n        displayName: PrismFlow\n\
                 \x20       apiKeyEnv: PRISMFLOW_API_KEY\n        api: openai-completions\n        baseURL: {}\n        defaultInput: [text]\n\
                 \x20       retryPolicy: {{ mode: normal, maxRetries: 0 }}\n        streamIdleTimeoutMs: 600000\n        models:\n\
                 \x20         - {{ id: main, name: {}, reasoningEfforts: false, input: [text], contextWindow: 32768, maxTokens: 4096 }}\n\
                 - id: agent-default-model\n  name: '@deepseek-ai/dsh-agent-default-model'\n  config: {{ provider: prismflow, model: main }}\n\
                 - {{ id: llm-deepseek, name: '@deepseek-ai/dsh-llm-deepseek-api-key', disabled: true }}\n\
                 - {{ id: llm-deepseek-account, name: '@deepseek-ai/dsh-llm-deepseek-account', disabled: true }}\n\
                 - {{ id: web-search-deepseek, name: '@deepseek-ai/dsh-web-search-deepseek', disabled: true }}\n\
                 - {{ id: tool-web, name: '@deepseek-ai/dsh-tool-web', disabled: true }}\n",
                serde_json::to_string(&creds.display().to_string()).unwrap(),
                serde_json::to_string(&base).unwrap(),
                serde_json::to_string(model).unwrap(),
            );
            l.files.push((dir.join("dsh-patch.yml"), patch));
            l.files.push((creds, "{}\n".into()));
            l.token_env = Some("PRISMFLOW_API_KEY");
            l.env = env(&[("DSH_HOME", &path_of(dir.join("dsh-home"))), ("DSH_TELEMETRY_DISABLED", "1")]);
            let patch_arg = path_of(dir.join("dsh-patch.yml"));
            // Launcher flags go before the app's own flags.
            l.branches.push(Branch { on: vec!["--prismflow-oneshot"], keep_first: false, env: vec![],
                args: vec![s("--profile"), s("headless"), s("--patch"), patch_arg.clone()] });
            l.default_args = vec![s("--patch"), patch_arg];
        }
        Kind::Grok => {
            // Environment only (verified with grok 1.0.52): no login, the
            // model list comes from PrismFlow, telemetry and feedback off.
            let cfg = serde_json::json!({ "cli": { "auto_update": false }, "features": { "telemetry": false, "feedback": false },
                "telemetry": { "trace_upload": false, "mixpanel_enabled": false } });
            l.token_env = Some("XAI_API_KEY");
            l.env = env(&[("GROK_XAI_API_BASE_URL", &base), ("GROK_MODELS_BASE_URL", &base), ("GROK_HOME", &path_of(dir.join("grok-home"))),
                ("GROK_TELEMETRY_ENABLED", "0"), ("GROK_FEEDBACK_ENABLED", "0"), ("GROK_DISABLE_AUTOUPDATER", "1"), ("GROK_CONFIG", &cfg.to_string())]);
            l.default_args = vec![s("--disable-web-search"), s("--no-leader")];
        }
        Kind::Muse => {
            // Verified with Muse Code 1.4.8: Meta provider mode pointed at
            // PrismFlow; PrismFlow answers its /muse-code/models catalog.
            l.token_env = Some("META_API_KEY");
            l.env = env(&[("MUSE_MODEL", model), ("MUSE_ENABLE_WEB_TOOLS", "0"), ("MUSE_WEB_SEARCH_MODE", "off"), ("TBH_DISABLE_FEATURE_CONFIG", "1")]);
            let conn = vec![s("--provider"), s("meta"), s("--base-url"), base.clone(), s("--model"), s(model)];
            let mut exec_args = conn.clone();
            exec_args.extend([s("--no-foreign-personal-context"), s("-c"), s("telemetry.enabled=false"), s("-c"), s("feature_config.enabled=false")]);
            l.branches.push(Branch { on: vec!["exec", "resume"], keep_first: true, env: vec![], args: exec_args });
            l.default_args = conn;
        }
        Kind::Qwen => {
            // OpenAI-compatible mode of Qwen Code (verified 0.25.2).
            l.token_env = Some("OPENAI_API_KEY");
            l.env = env(&[("OPENAI_BASE_URL", &base), ("OPENAI_MODEL", model)]);
            l.default_args = vec![s("--auth-type"), s("openai")];
        }
        Kind::Goose => {
            // Goose's OpenAI provider, env only; no keyring, telemetry off (verified 1.54.0).
            l.token_env = Some("OPENAI_API_KEY");
            l.env = env(&[("GOOSE_PROVIDER", "openai"), ("OPENAI_HOST", broker_url), ("OPENAI_BASE_PATH", "v1/chat/completions"), ("GOOSE_MODEL", model),
                ("GOOSE_DISABLE_KEYRING", "1"), ("GOOSE_TELEMETRY_OFF", "1")]);
            l.branches.push(Branch { on: vec!["--prismflow-oneshot"], keep_first: false, env: env(&[("GOOSE_MODE", "auto")]),
                args: vec![s("run"), s("--no-session"), s("--no-profile"), s("--with-builtin"), s("developer"), s("-t")] });
        }
        Kind::Crush => {
            // A config folder of its own: openai-compat provider, key from the environment (verified 0.98.1).
            let cfg = serde_json::json!({
                "options": { "disable_metrics": true, "disable_provider_auto_update": true, "disable_default_providers": true },
                "providers": { "prismflow": { "name": "PrismFlow", "type": "openai-compat", "base_url": base, "api_key": "$PRISMFLOW_TOKEN",
                    "models": [{ "id": model, "name": model, "context_window": 32768, "default_max_tokens": 4096 }] } },
                "models": { "large": { "provider": "prismflow", "model": model }, "small": { "provider": "prismflow", "model": model } }
            });
            l.files.push((dir.join("crush-xdg/crush/crush.json"), serde_json::to_string_pretty(&cfg).unwrap()));
            l.token_env = Some("PRISMFLOW_TOKEN");
            l.env = env(&[("XDG_CONFIG_HOME", &path_of(dir.join("crush-xdg"))), ("DO_NOT_TRACK", "1"), ("CRUSH_DISABLE_PROVIDER_AUTO_UPDATE", "1")]);
            l.branches.push(Branch { on: vec!["--prismflow-oneshot"], keep_first: false, env: vec![], args: vec![s("run"), s("-q"), s("-m"), format!("prismflow/{model}")] });
        }
        Kind::Droid => {
            // Droid takes a per-process settings file; its key field must hold the
            // value, so the script writes the local token there at launch.
            let settings = dir.join("droid-settings.json");
            let template = serde_json::json!({ "customModels": [{ "model": model, "displayName": "PrismFlow", "baseUrl": base,
                "apiKey": "{TOKEN}", "provider": "generic-chat-completion-api", "maxOutputTokens": 4096 }] }).to_string();
            l.token_file = Some((settings.clone(), template));
            l.env = env(&[("FACTORY_DISABLE_KEYRING", "1"), ("FACTORY_DROID_AUTO_UPDATE_ENABLED", "false"), ("FACTORY_OTEL_ENABLED", "false")]);
            let sp = path_of(settings);
            l.branches.push(Branch { on: vec!["--prismflow-oneshot"], keep_first: false, env: env(&[("FACTORY_AIRGAP_ENABLED", "1")]),
                args: vec![s("exec"), s("--settings"), sp.clone(), s("-m"), s("custom:PrismFlow-0")] });
            l.branches.push(Branch { on: vec!["exec"], keep_first: false, env: vec![], args: vec![s("exec"), s("--settings"), sp.clone()] });
            l.default_args = vec![s("--settings"), sp];
        }
        Kind::ContinueCli => {
            // Continue's config with the key from the environment (verified cn 1.5.47).
            let yaml = format!(
                "name: PrismFlow\nversion: 1.0.0\nschema: v1\nmodels:\n  - name: {m}\n    provider: openai\n    model: {m}\n    apiBase: {b}\n    apiKey: ${{{{ secrets.PRISMFLOW_TOKEN }}}}\n    roles: [chat, edit, apply]\n    capabilities: [tool_use]\n    defaultCompletionOptions: {{ contextLength: 32768, maxTokens: 4096 }}\n",
                m = serde_json::to_string(model).unwrap(), b = serde_json::to_string(&base).unwrap()
            );
            l.files.push((dir.join("continue-config.yaml"), yaml));
            l.token_env = Some("PRISMFLOW_TOKEN");
            l.env = env(&[("CONTINUE_GLOBAL_DIR", &path_of(dir.join("continue-home"))), ("CONTINUE_CLI_ENABLE_TELEMETRY", "0"), ("CONTINUE_METRICS_ENABLED", "0")]);
            l.default_args = vec![s("--config"), path_of(dir.join("continue-config.yaml"))];
        }
        Kind::CopilotCli => {
            // Copilot CLI's documented bring-your-own-key mode; no GitHub sign-in needed.
            l.token_env = Some("COPILOT_PROVIDER_API_KEY");
            l.env = env(&[("COPILOT_PROVIDER_BASE_URL", &base), ("COPILOT_PROVIDER_TYPE", "openai"), ("COPILOT_MODEL", model), ("COPILOT_OFFLINE", "true")]);
        }
    }
    let script = match shell {
        Shell::Posix => render_posix(spec, &l, bin, helper, connection_id),
        Shell::PowerShell => render_powershell(spec, &l, bin, helper, connection_id),
    };
    let mut files = l.files;
    files.insert(0, (launch_script_for(dir, shell), script));
    files
}

fn render_posix(spec: &AgentSpec, l: &Launch, bin: &Path, helper: &Path, connection_id: &str) -> String {
    let token_cmd = format!("{} token print --connection {}", q(&helper.display().to_string()), q(connection_id));
    let mut o = format!(
        "#!/bin/sh\n{MARKER} for {} ({connection_id}). Removed on disconnect; edits here are overwritten.\n\
         # The gateway token is fetched at launch from the secure store; nothing secret is stored here.\nset -e\n",
        spec.display_name
    );
    o.push_str(&format!("export PATH={}:\"$PATH\"\n", q(&bin.parent().unwrap_or(Path::new("/")).display().to_string())));
    let b = q(&bin.display().to_string());
    if let Some(var) = l.token_env {
        o.push_str(&format!("{var}=\"$({token_cmd})\"\nexport {var}\n"));
    }
    if let Some((file, template)) = &l.token_file {
        let (a, z) = template.split_once("{TOKEN}").unwrap_or((template, ""));
        o.push_str(&format!("TOKEN=\"$({token_cmd})\"\numask 077\nprintf '%s%s%s' {} \"$TOKEN\" {} > {}\n", q(a), q(z), q(&file.display().to_string())));
    }
    if !l.env.is_empty() {
        let parts: Vec<String> = l.env.iter().map(|(k, v)| format!("{k}={}", q(v))).collect();
        o.push_str(&format!("export {}\n", parts.join(" ")));
    }
    let words = |v: &[String]| v.iter().map(|a| qw(a)).collect::<Vec<_>>().join(" ");
    if let Some(vs) = &l.version_switch {
        o.push_str(&format!("case \"$({b} --version 2>/dev/null)\" in\n"));
        o.push_str(&format!("  *v2.*|2.*) export {}={}\n", vs.env_key, q(&vs.v2_value)));
        o.push_str(&format!("    if [ \"$1\" = run ]; then shift; set -- run {} \"$@\"; fi ;;\n", vs.v2_run_flag));
        o.push_str(&format!("  *) export {}={} ;;\nesac\n", vs.env_key, q(&vs.v1_value)));
    }
    for br in &l.branches {
        let test = br.on.iter().map(|w| format!("[ \"$1\" = {w} ]")).collect::<Vec<_>>().join(" || ");
        let envs = if br.env.is_empty() { String::new() } else { format!("export {}; ", br.env.iter().map(|(k, v)| format!("{k}={}", q(v))).collect::<Vec<_>>().join(" ")) };
        let first = if br.keep_first { "first=\"$1\"; shift; " } else { "shift; " };
        let lead = if br.keep_first { "\"$first\" " } else { "" };
        o.push_str(&format!("if {test}; then {first}{envs}exec {b} {lead}{} \"$@\"; fi\n", words(&br.args)));
    }
    let def = words(&l.default_args);
    o.push_str(&format!("exec {b}{}{} \"$@\"\n", if def.is_empty() { "" } else { " " }, def));
    o
}

fn render_powershell(spec: &AgentSpec, l: &Launch, bin: &Path, helper: &Path, connection_id: &str) -> String {
    let mut o = format!(
        "{MARKER} for {} ({connection_id}). Removed on disconnect; edits here are overwritten.\r\n\
         # The gateway token is fetched at launch from the secure store; nothing secret is stored here.\r\n\
         $ErrorActionPreference = 'Stop'\r\n",
        spec.display_name
    );
    o.push_str(&format!("$env:PATH = {} + ';' + $env:PATH\r\n", pq(&bin.parent().unwrap_or(Path::new("C:\\")).display().to_string())));
    let b = pq(&bin.display().to_string());
    if l.token_env.is_some() || l.token_file.is_some() {
        o.push_str(&format!("$token = ((& {} token print --connection {}) | Out-String).Trim()\r\n", pq(&helper.display().to_string()), pq(connection_id)));
        o.push_str("if ($LASTEXITCODE -ne 0 -or -not $token) { Write-Error 'PrismFlow could not read this connection''s key. Open PrismFlow and connect again.'; exit 1 }\r\n");
    }
    if let Some(var) = l.token_env {
        o.push_str(&format!("$env:{var} = $token\r\n"));
    }
    if let Some((file, template)) = &l.token_file {
        let (a, z) = template.split_once("{TOKEN}").unwrap_or((template, ""));
        o.push_str(&format!("[IO.File]::WriteAllText({}, {} + $token + {})\r\n", pq(&file.display().to_string()), pq(a), pq(z)));
    }
    for (k, v) in &l.env {
        o.push_str(&format!("$env:{k} = {}\r\n", pq(v)));
    }
    let list = |v: &[String]| v.iter().map(|a| pq(a)).collect::<Vec<_>>().join(", ");
    o.push_str("$rest = @($args)\r\n");
    if let Some(vs) = &l.version_switch {
        o.push_str(&format!("$version = ((& {b} --version 2>$null) | Out-String)\r\n"));
        o.push_str(&format!("if ($version -match 'v2\\.' -or $version -match '^\\s*2\\.') {{\r\n  $env:{} = {}\r\n", vs.env_key, pq(&vs.v2_value)));
        o.push_str(&format!("  if ($rest.Count -gt 0 -and $rest[0] -eq 'run') {{ $rest = @('run', {}) + @($rest | Select-Object -Skip 1) }}\r\n}} else {{\r\n  $env:{} = {}\r\n}}\r\n",
            pq(vs.v2_run_flag), vs.env_key, pq(&vs.v1_value)));
    }
    for br in &l.branches {
        let test = br.on.iter().map(|w| format!("$rest[0] -eq {}", pq(w))).collect::<Vec<_>>().join(" -or ");
        o.push_str(&format!("if ($rest.Count -gt 0 -and ({test})) {{\r\n"));
        for (k, v) in &br.env {
            o.push_str(&format!("  $env:{k} = {}\r\n", pq(v)));
        }
        let lead = if br.keep_first { "@($rest[0]) + " } else { "" };
        o.push_str(&format!("  $run = {lead}@({}) + @($rest | Select-Object -Skip 1)\r\n  & {b} @run\r\n  exit $LASTEXITCODE\r\n}}\r\n", list(&br.args)));
    }
    o.push_str(&format!("$run = @({}) + $rest\r\n& {b} @run\r\nexit $LASTEXITCODE\r\n", list(&l.default_args)));
    o
}

/// Arguments for a non-interactive one-shot run of `prompt`.
pub fn oneshot_args(spec: &AgentSpec, prompt: &str) -> Vec<String> {
    let p = prompt.to_string();
    match spec.kind {
        Kind::Aider => vec!["--message".into(), p, "--no-git".into(), "--yes-always".into(), "--no-pretty".into()],
        Kind::Cline => vec!["--retries".into(), "2".into(), p],
        Kind::Pi => vec!["--no-extensions".into(), "-p".into(), p],
        Kind::Hermes => vec!["--ignore-rules".into(), "-z".into(), p],
        Kind::OpenCode => vec!["run".into(), p],
        Kind::Dsh => vec!["--prismflow-oneshot".into(), p],
        Kind::Grok => vec!["--always-approve".into(), "-p".into(), p],
        Kind::Qwen => vec!["--bare".into(), "--yolo".into(), "-p".into(), p],
        Kind::Goose | Kind::Crush | Kind::Droid => vec!["--prismflow-oneshot".into(), p],
        Kind::ContinueCli => vec!["-p".into(), p],
        Kind::CopilotCli => vec!["--allow-all-tools".into(), "-p".into(), p],
        Kind::Muse => vec!["exec".into(), "--trust-workspace".into(), "--approval-mode".into(), "never".into(), "--approval-judge".into(), "off".into(),
            "--disable-web-tools".into(), "--disable-reminders".into(), p],
    }
}

/// Write the planned files (script made executable).
pub fn write(files: &[(PathBuf, String)]) -> Result<(), String> {
    for (path, body) in files {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        crate::config_engine::atomic_write(path, body).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(unix)]
    if let Some((script, _)) = files.first() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Remove a connection's directory, only if its script carries our marker.
pub fn remove(dir: &Path) -> Result<Option<PathBuf>, String> {
    let script = launch_script(dir);
    match std::fs::read_to_string(&script) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
        Ok(s) if !s.contains(MARKER) => Err(format!("{} is not PrismFlow-managed; left alone.", script.display())),
        Ok(_) => std::fs::remove_dir_all(dir).map(|_| Some(dir.to_path_buf())).map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(kind_id: &str) -> Vec<(PathBuf, String)> {
        plan_files_for(Shell::Posix, spec(kind_id).unwrap(), Path::new("/s/agents/x"), Path::new("/usr/local/bin/agent"),
            Path::new("/Apps/Prism Flow/prismflow"), "aider:ollama", "http://127.0.0.1:47821", "qwen3:4b")
    }

    #[test]
    fn every_agent_gets_a_marked_script_and_no_secret() {
        for a in AGENTS {
            let f = files(a.id);
            assert!(f[0].0.ends_with("launch.sh"));
            assert!(f[0].1.starts_with("#!/bin/sh\n# Managed by PrismFlow"));
            // The token helper is invoked from the script, or from the agent's own config (Pi).
            assert!(f.iter().any(|(_, b)| b.contains("'/Apps/Prism Flow/prismflow' token print --connection 'aider:ollama'")), "{}", a.id);
            for (_, body) in &f {
                assert!(!body.contains("pft_") && !body.contains("sk-"), "{}", a.id);
            }
        }
    }

    #[test]
    fn per_agent_mechanisms_match_the_verified_research() {
        let aider = files("aider");
        assert!(aider[0].1.contains("export OPENAI_API_BASE='http://127.0.0.1:47821/v1'"));
        assert!(aider[0].1.contains("--model openai/qwen3:4b"));

        let cline = files("cline");
        let providers: serde_json::Value = serde_json::from_str(&cline[1].1).unwrap();
        let s = &providers["providers"]["openai-compatible"]["settings"];
        assert_eq!(s["baseUrl"], "http://127.0.0.1:47821/v1");
        assert!(s.get("apiKey").is_none(), "Cline reads OPENAI_API_KEY when no key is stored");
        assert!(providers["providers"]["openai-compatible"]["updatedAt"].is_string(), "without it Cline ignores the entry");
        assert!(cline[0].1.contains("HTTPS_PROXY='http://127.0.0.1:9'"), "token must not be able to leave over https");

        let pi = files("pi");
        let models: serde_json::Value = serde_json::from_str(&pi[1].1).unwrap();
        assert!(models["providers"]["prismflow"]["apiKey"].as_str().unwrap().starts_with("!'/Apps/Prism Flow/prismflow' token print"));
        assert!(pi[0].1.contains("export PI_CODING_AGENT_DIR="));

        let hermes = files("hermes");
        assert!(hermes[1].1.contains("api_key: \"${PRISMFLOW_AGENT_TOKEN}\""));
        assert!(hermes[0].1.contains("export HERMES_HOME="));
    }

    fn ps_files(kind_id: &str) -> Vec<(PathBuf, String)> {
        plan_files_for(Shell::PowerShell, spec(kind_id).unwrap(), Path::new(r"C:\s\agents\x"), Path::new(r"C:\npm\agent.cmd"),
            Path::new(r"C:\Program Files\PrismFlow\prismflow.exe"), "aider:ollama", "http://127.0.0.1:47821", "qwen3:4b")
    }

    #[test]
    fn windows_launchers_for_every_agent() {
        for a in AGENTS {
            let f = ps_files(a.id);
            assert!(f[0].0.ends_with("launch.ps1"), "{}", a.id);
            assert!(f[0].1.starts_with("# Managed by PrismFlow"), "{}", a.id);
            assert!(f[0].1.contains("& 'C:\\npm\\agent.cmd' @run"), "{}", a.id);
            assert!(f.iter().any(|(_, b)| b.contains("token print --connection")), "{}", a.id);
            for (_, body) in &f {
                assert!(!body.contains("pft_") && !body.contains("sk-") && !body.contains("exec "), "{}", a.id);
            }
        }
        let aider = ps_files("aider");
        assert!(aider[0].1.contains("$env:OPENAI_API_KEY = $token"));
        assert!(aider[0].1.contains("$env:OPENAI_API_BASE = 'http://127.0.0.1:47821/v1'"));
        let pi = ps_files("pi");
        assert!(pi[1].1.contains(r#"\"C:\\Program Files\\PrismFlow\\prismflow.exe\" token print"#), "{}", pi[1].1);
        let goose = ps_files("goose");
        assert!(goose[0].1.contains("$rest[0] -eq '--prismflow-oneshot'") && goose[0].1.contains("$env:GOOSE_MODE = 'auto'"));
        let oc = ps_files("opencode");
        assert!(oc[0].1.contains("--standalone") && oc[0].1.contains("$env:OPENCODE_CONFIG_CONTENT"));
        let droid = ps_files("droid");
        assert!(droid[0].1.contains("[IO.File]::WriteAllText(") && !droid[0].1.contains("{TOKEN}"));
    }

    /// Runs a generated launcher with a fake agent that prints what it got.
    #[cfg(windows)]
    #[test]
    fn windows_launcher_runs() {
        let root = tempfile::tempdir().unwrap();
        let helper = root.path().join("helper.cmd");
        std::fs::write(&helper, "@echo tok-123\r\n").unwrap();
        let bin = root.path().join("agent.cmd");
        std::fs::write(&bin, "@echo key=%OPENAI_API_KEY% base=%OPENAI_BASE_URL% args=%*\r\n").unwrap();
        let dir = root.path().join("c");
        let f = plan_files(spec("qwen").unwrap(), &dir, &bin, &helper, "qwen:x", "http://127.0.0.1:1", "m");
        write(&f).unwrap();
        let (prog, lead) = script_runner(&launch_script(&dir));
        let out = std::process::Command::new(prog).args(lead).args(["-p", "hello world"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text} {}", String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("key=tok-123 base=http://127.0.0.1:1/v1"), "{text}");
        assert!(text.contains("--auth-type openai -p \"hello world\""), "{text}");
    }

    #[test]
    fn oneshot_args_are_non_interactive() {
        assert!(oneshot_args(spec("aider").unwrap(), "x").contains(&"--message".to_string()));
        assert_eq!(oneshot_args(spec("hermes").unwrap(), "x"), vec!["--ignore-rules", "-z", "x"]);
    }

    #[test]
    fn write_then_remove_only_ours() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("agents/c");
        let f = plan_files(spec("pi").unwrap(), &dir, Path::new("/bin/pi"), Path::new("/bin/echo"), "pi:x", "http://127.0.0.1:1", "m");
        write(&f).unwrap();
        assert!(launch_script(&dir).exists() && dir.join("pi-agent/models.json").exists());
        assert_eq!(remove(&dir).unwrap(), Some(dir.clone()));
        assert!(!dir.exists());
        assert_eq!(remove(&dir).unwrap(), None);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(launch_script(&dir), "echo mine\n").unwrap();
        assert!(remove(&dir).is_err());
        assert!(dir.exists());
    }
}

