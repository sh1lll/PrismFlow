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

/// Launch scripts are POSIX shell; Windows support for terminal agents is
/// not built yet (stated in the UI rather than half-working).
pub const SUPPORTED_HERE: bool = cfg!(unix);

pub fn detect(spec: &AgentSpec) -> AppDetection {
    if !SUPPORTED_HERE {
        return AppDetection {
            app_id: spec.id.into(), display_name: spec.display_name.into(), installed: false, path: None,
            version: None, version_verified: false,
            notes: vec![format!("{} with PrismFlow isn't available on Windows yet.", spec.display_name)],
        };
    }
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

pub fn launch_script(dir: &Path) -> PathBuf {
    dir.join("launch.sh")
}

fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
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
    let base = format!("{broker_url}/v1");
    let token_cmd = format!("{} token print --connection {}", q(&helper.display().to_string()), q(connection_id));
    let mut files = Vec::new();
    let mut script = format!(
        "#!/bin/sh\n{MARKER} for {} ({connection_id}). Removed on disconnect; edits here are overwritten.\n\
         # The gateway token is fetched at launch from the secure store; nothing secret is stored here.\nset -e\n",
        spec.display_name
    );
    script.push_str(&format!("export PATH={}:\"$PATH\"\n", q(&bin.parent().unwrap_or(Path::new("/")).display().to_string())));
    let bin = q(&bin.display().to_string());
    match spec.kind {
        Kind::Aider => {
            script.push_str(&format!("OPENAI_API_KEY=\"$({token_cmd})\"\nexport OPENAI_API_KEY\n"));
            script.push_str(&format!("export OPENAI_API_BASE={}\n", q(&base)));
            script.push_str(&format!(
                "exec {bin} --model {} --no-show-model-warnings --analytics-disable --no-check-update \"$@\"\n",
                q(&format!("openai/{model}"))
            ));
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
            files.push((dir.join("cline-data/settings/providers.json"), serde_json::to_string_pretty(&providers).unwrap()));
            script.push_str(&format!("OPENAI_API_KEY=\"$({token_cmd})\"\nexport OPENAI_API_KEY\n"));
            // Defense in depth: the gateway is plain-http loopback, so a dead
            // HTTPS proxy keeps the token from ever reaching a remote host if
            // Cline falls back to a default endpoint.
            script.push_str("export HTTPS_PROXY=http://127.0.0.1:9 https_proxy=http://127.0.0.1:9 NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost\n");
            script.push_str(&format!(
                "exec {bin} --data-dir {} -P openai-compatible -m {} \"$@\"\n",
                q(&dir.join("cline-data").display().to_string()),
                q(model)
            ));
        }
        Kind::Pi => {
            let models = serde_json::json!({ "providers": { "prismflow": {
                "baseUrl": base, "api": "openai-completions", "apiKey": format!("!{token_cmd}"),
                "models": [{ "id": model }]
            } } });
            files.push((dir.join("pi-agent/models.json"), serde_json::to_string_pretty(&models).unwrap()));
            script.push_str(&format!("export PI_CODING_AGENT_DIR={}\n", q(&dir.join("pi-agent").display().to_string())));
            script.push_str(&format!("exec {bin} --model {} \"$@\"\n", q(&format!("prismflow/{model}"))));
        }
        Kind::Hermes => {
            let cfg = format!(
                "{MARKER}\nmodel:\n  provider: \"custom\"\n  base_url: {}\n  api_key: \"${{PRISMFLOW_AGENT_TOKEN}}\"\n  default: {}\n",
                serde_json::to_string(&base).unwrap(),
                serde_json::to_string(model).unwrap()
            );
            files.push((dir.join("hermes-home/config.yaml"), cfg));
            script.push_str(&format!("PRISMFLOW_AGENT_TOKEN=\"$({token_cmd})\"\nexport PRISMFLOW_AGENT_TOKEN\n"));
            script.push_str(&format!("export HERMES_HOME={}\n", q(&dir.join("hermes-home").display().to_string())));
            script.push_str(&format!("exec {bin} \"$@\"\n"));
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
            script.push_str(&format!("PRISMFLOW_API_KEY=\"$({token_cmd})\"\nexport PRISMFLOW_API_KEY\n"));
            script.push_str("export OPENCODE_DISABLE_MODELS_FETCH=1 OPENCODE_DISABLE_AUTOUPDATE=1 OPENCODE_DISABLE_SHARE=1 OPENCODE_DISABLE_CLAUDE_CODE=1 OPENCODE_DISABLE_LSP_DOWNLOAD=1\n");
            script.push_str(&format!("case \"$({bin} --version 2>/dev/null)\" in\n"));
            script.push_str(&format!("  *v2.*|2.*) export OPENCODE_CONFIG_CONTENT={}\n", q(&v2.to_string())));
            script.push_str("    # One-shot runs get a private server instead of the background service.\n");
            script.push_str("    if [ \"$1\" = run ]; then shift; set -- run --standalone \"$@\"; fi ;;\n");
            script.push_str(&format!("  *) export OPENCODE_CONFIG_CONTENT={} ;;\nesac\n", q(&v1.to_string())));
            script.push_str(&format!("exec {bin} \"$@\"\n"));
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
            files.push((dir.join("dsh-patch.yml"), patch));
            files.push((creds, "{}\n".into()));
            script.push_str(&format!("PRISMFLOW_API_KEY=\"$({token_cmd})\"\nexport PRISMFLOW_API_KEY\n"));
            script.push_str(&format!("export DSH_HOME={} DSH_TELEMETRY_DISABLED=1\n", q(&dir.join("dsh-home").display().to_string())));
            let patch_arg = q(&dir.join("dsh-patch.yml").display().to_string());
            // Launcher flags go before the app's own flags.
            script.push_str(&format!("if [ \"$1\" = --prismflow-oneshot ]; then shift; exec {bin} --profile headless --patch {patch_arg} \"$@\"; fi\n"));
            script.push_str(&format!("exec {bin} --patch {patch_arg} \"$@\"\n"));
        }
        Kind::Grok => {
            // Environment only (verified with grok 1.0.52): no login, the
            // model list comes from PrismFlow, telemetry and feedback off.
            let cfg = serde_json::json!({ "cli": { "auto_update": false }, "features": { "telemetry": false, "feedback": false },
                "telemetry": { "trace_upload": false, "mixpanel_enabled": false } });
            script.push_str(&format!("XAI_API_KEY=\"$({token_cmd})\"\nexport XAI_API_KEY\n"));
            script.push_str(&format!("export GROK_XAI_API_BASE_URL={b} GROK_MODELS_BASE_URL={b}\n", b = q(&base)));
            script.push_str(&format!("export GROK_HOME={}\n", q(&dir.join("grok-home").display().to_string())));
            script.push_str("export GROK_TELEMETRY_ENABLED=0 GROK_FEEDBACK_ENABLED=0 GROK_DISABLE_AUTOUPDATER=1\n");
            script.push_str(&format!("export GROK_CONFIG={}\n", q(&cfg.to_string())));
            script.push_str(&format!("exec {bin} --disable-web-search --no-leader \"$@\"\n"));
        }
        Kind::Muse => {
            // Verified with Muse Code 1.4.8: Meta provider mode pointed at
            // PrismFlow; PrismFlow answers its /muse-code/models catalog.
            script.push_str(&format!("META_API_KEY=\"$({token_cmd})\"\nexport META_API_KEY\n"));
            script.push_str(&format!("export MUSE_MODEL={} MUSE_ENABLE_WEB_TOOLS=0 MUSE_WEB_SEARCH_MODE=off TBH_DISABLE_FEATURE_CONFIG=1\n", q(model)));
            let conn = format!("--provider meta --base-url {} --model {}", q(&base), q(model));
            script.push_str(&format!(
                "if [ \"$1\" = exec ] || [ \"$1\" = resume ]; then sub=\"$1\"; shift; exec {bin} \"$sub\" {conn} --no-foreign-personal-context -c telemetry.enabled=false -c feature_config.enabled=false \"$@\"; fi\n"
            ));
            script.push_str(&format!("exec {bin} {conn} \"$@\"\n"));
        }
        Kind::Qwen => {
            // OpenAI-compatible mode of Qwen Code (verified 0.25.2).
            script.push_str(&format!("OPENAI_API_KEY=\"$({token_cmd})\"\nexport OPENAI_API_KEY\n"));
            script.push_str(&format!("export OPENAI_BASE_URL={} OPENAI_MODEL={}\n", q(&base), q(model)));
            script.push_str(&format!("exec {bin} --auth-type openai \"$@\"\n"));
        }
        Kind::Goose => {
            // Goose's OpenAI provider, env only; no keyring, telemetry off (verified 1.54.0).
            script.push_str(&format!("OPENAI_API_KEY=\"$({token_cmd})\"\nexport OPENAI_API_KEY\n"));
            script.push_str(&format!("export GOOSE_PROVIDER=openai OPENAI_HOST={} OPENAI_BASE_PATH=v1/chat/completions GOOSE_MODEL={}\n", q(broker_url), q(model)));
            script.push_str("export GOOSE_DISABLE_KEYRING=1 GOOSE_TELEMETRY_OFF=1\n");
            script.push_str(&format!("if [ \"$1\" = --prismflow-oneshot ]; then shift; export GOOSE_MODE=auto; exec {bin} run --no-session --no-profile --with-builtin developer -t \"$@\"; fi\n"));
            script.push_str(&format!("exec {bin} \"$@\"\n"));
        }
        Kind::Crush => {
            // A config folder of its own: openai-compat provider, key from the environment (verified 0.98.1).
            let cfg = serde_json::json!({
                "options": { "disable_metrics": true, "disable_provider_auto_update": true, "disable_default_providers": true },
                "providers": { "prismflow": { "name": "PrismFlow", "type": "openai-compat", "base_url": base, "api_key": "$PRISMFLOW_TOKEN",
                    "models": [{ "id": model, "name": model, "context_window": 32768, "default_max_tokens": 4096 }] } },
                "models": { "large": { "provider": "prismflow", "model": model }, "small": { "provider": "prismflow", "model": model } }
            });
            files.push((dir.join("crush-xdg/crush/crush.json"), serde_json::to_string_pretty(&cfg).unwrap()));
            script.push_str(&format!("PRISMFLOW_TOKEN=\"$({token_cmd})\"\nexport PRISMFLOW_TOKEN\n"));
            script.push_str(&format!("export XDG_CONFIG_HOME={} DO_NOT_TRACK=1 CRUSH_DISABLE_PROVIDER_AUTO_UPDATE=1\n", q(&dir.join("crush-xdg").display().to_string())));
            script.push_str(&format!("if [ \"$1\" = --prismflow-oneshot ]; then shift; exec {bin} run -q -m {} \"$@\"; fi\n", q(&format!("prismflow/{model}"))));
            script.push_str(&format!("exec {bin} \"$@\"\n"));
        }
        Kind::Droid => {
            // Droid takes a per-process settings file; its key field must hold the
            // value, so the script writes the local token there at launch (0600).
            let settings = dir.join("droid-settings.json");
            script.push_str(&format!("TOKEN=\"$({token_cmd})\"\n"));
            script.push_str("umask 077\n");
            script.push_str(&format!(
                "printf '{{\"customModels\":[{{\"model\":%s,\"displayName\":\"PrismFlow\",\"baseUrl\":%s,\"apiKey\":\"%s\",\"provider\":\"generic-chat-completion-api\",\"maxOutputTokens\":4096}}]}}' {} {} \"$TOKEN\" > {}\n",
                q(&serde_json::to_string(model).unwrap()), q(&serde_json::to_string(&base).unwrap()), q(&settings.display().to_string())
            ));
            script.push_str("export FACTORY_DISABLE_KEYRING=1 FACTORY_DROID_AUTO_UPDATE_ENABLED=false FACTORY_OTEL_ENABLED=false\n");
            let s = q(&settings.display().to_string());
            script.push_str(&format!("if [ \"$1\" = --prismflow-oneshot ]; then shift; export FACTORY_AIRGAP_ENABLED=1; exec {bin} exec --settings {s} -m custom:PrismFlow-0 \"$@\"; fi\n"));
            script.push_str(&format!("if [ \"$1\" = exec ]; then shift; exec {bin} exec --settings {s} \"$@\"; fi\n"));
            script.push_str(&format!("exec {bin} --settings {s} \"$@\"\n"));
        }
        Kind::ContinueCli => {
            // Continue's config with the key from the environment (verified cn 1.5.47).
            let yaml = format!(
                "name: PrismFlow\nversion: 1.0.0\nschema: v1\nmodels:\n  - name: {m}\n    provider: openai\n    model: {m}\n    apiBase: {b}\n    apiKey: ${{{{ secrets.PRISMFLOW_TOKEN }}}}\n    roles: [chat, edit, apply]\n    capabilities: [tool_use]\n    defaultCompletionOptions: {{ contextLength: 32768, maxTokens: 4096 }}\n",
                m = serde_json::to_string(model).unwrap(), b = serde_json::to_string(&base).unwrap()
            );
            files.push((dir.join("continue-config.yaml"), yaml));
            script.push_str(&format!("PRISMFLOW_TOKEN=\"$({token_cmd})\"\nexport PRISMFLOW_TOKEN\n"));
            script.push_str(&format!("export CONTINUE_GLOBAL_DIR={} CONTINUE_CLI_ENABLE_TELEMETRY=0 CONTINUE_METRICS_ENABLED=0\n", q(&dir.join("continue-home").display().to_string())));
            script.push_str(&format!("exec {bin} --config {} \"$@\"\n", q(&dir.join("continue-config.yaml").display().to_string())));
        }
        Kind::CopilotCli => {
            // Copilot CLI's documented bring-your-own-key mode; no GitHub sign-in needed.
            script.push_str(&format!("COPILOT_PROVIDER_API_KEY=\"$({token_cmd})\"\nexport COPILOT_PROVIDER_API_KEY\n"));
            script.push_str(&format!("export COPILOT_PROVIDER_BASE_URL={} COPILOT_PROVIDER_TYPE=openai COPILOT_MODEL={} COPILOT_OFFLINE=true\n", q(&base), q(model)));
            script.push_str(&format!("exec {bin} \"$@\"\n"));
        }
    }
    files.insert(0, (launch_script(dir), script));
    files
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
        plan_files(spec(kind_id).unwrap(), Path::new("/s/agents/x"), Path::new("/usr/local/bin/agent"),
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
        assert!(aider[0].1.contains("--model 'openai/qwen3:4b'"));

        let cline = files("cline");
        let providers: serde_json::Value = serde_json::from_str(&cline[1].1).unwrap();
        let s = &providers["providers"]["openai-compatible"]["settings"];
        assert_eq!(s["baseUrl"], "http://127.0.0.1:47821/v1");
        assert!(s.get("apiKey").is_none(), "Cline reads OPENAI_API_KEY when no key is stored");
        assert!(providers["providers"]["openai-compatible"]["updatedAt"].is_string(), "without it Cline ignores the entry");
        assert!(cline[0].1.contains("HTTPS_PROXY=http://127.0.0.1:9"), "token must not be able to leave over https");

        let pi = files("pi");
        let models: serde_json::Value = serde_json::from_str(&pi[1].1).unwrap();
        assert!(models["providers"]["prismflow"]["apiKey"].as_str().unwrap().starts_with("!'/Apps/Prism Flow/prismflow' token print"));
        assert!(pi[0].1.contains("export PI_CODING_AGENT_DIR="));

        let hermes = files("hermes");
        assert!(hermes[1].1.contains("api_key: \"${PRISMFLOW_AGENT_TOKEN}\""));
        assert!(hermes[0].1.contains("export HERMES_HOME="));
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
        std::fs::write(launch_script(&dir), "#!/bin/sh\necho mine\n").unwrap();
        assert!(remove(&dir).is_err());
        assert!(dir.exists());
    }
}

