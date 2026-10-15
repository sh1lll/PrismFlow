//! The embedded translation engine: `@the-next-ai/ai-gateway` (MIT, pinned in
//! `src-tauri/gateway/`) run as a child process (ADR-005).
//!
//! Hardening applied at spawn, each closing an observed gap (plan E3–E5):
//! - `HOST=127.0.0.1`, because the engine defaults to `0.0.0.0`.
//! - `MANAGER_API_KEY` set to a random value that is never shared. Without it,
//!   `/manager/*` lets any local process rewrite the provider config.
//! - Client auth by a random internal key known only to the broker.
//! - Logs at `warn` with access logs off, since info logs carry prompt text.
//! - `env_clear()`: the child sees only the variables listed here, so no
//!   ambient `OPENAI_API_KEY`, proxy setting or `GATEWAY_*` override leaks in.
//! - A preload script ends the child when PrismFlow's end of stdin closes.

use super::tokens;
use crate::diagnostics::redact_known;
use crate::providers::{Protocol, ProviderDefinition};
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

pub const ENGINE: &str = "@the-next-ai/ai-gateway";
pub const ENGINE_VERSION: &str = "1.0.21";
pub const NODE_MIN_MAJOR: u32 = 20;
const LOG_TAIL_LINES: usize = 40;
const CLIENT_KEY_ENV: &str = "PRISMFLOW_GATEWAY_CLIENT_KEY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRuntime {
    pub node: PathBuf,
    pub node_version: String,
    pub package_dir: PathBuf,
}

/// Where the pinned package lives: `PRISMFLOW_GATEWAY_DIR`, else the
/// source tree's `src-tauri/gateway` (dev builds; WS-G bundles it as a
/// sidecar resource for releases).
pub fn package_dir() -> PathBuf {
    std::env::var_os("PRISMFLOW_GATEWAY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("gateway"))
}

fn engine_dir(package_dir: &Path) -> PathBuf {
    package_dir.join("node_modules").join("@the-next-ai").join("ai-gateway")
}

/// Point the runtime at bundled resources when they exist. Explicit
/// `PRISMFLOW_*` overrides win (dev and tests).
pub fn use_bundled_runtime(resource_dir: &Path) {
    let (gw, node) = bundled_paths(resource_dir);
    if let (Some(gw), None) = (gw, std::env::var_os("PRISMFLOW_GATEWAY_DIR")) {
        std::env::set_var("PRISMFLOW_GATEWAY_DIR", gw);
    }
    if let (Some(node), None) = (node, std::env::var_os("PRISMFLOW_NODE_BIN")) {
        // Only adopt a bundled runtime that actually starts; otherwise the
        // system Node (if any) is still found and the failure is diagnosable.
        let runs = std::process::Command::new(&node).arg("--version").output().is_ok_and(|o| o.status.success());
        if runs {
            std::env::set_var("PRISMFLOW_NODE_BIN", node);
        }
    }
}

/// Bundled engine dir and Node binary, each only if actually present.
pub fn bundled_paths(resource_dir: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let gw = resource_dir.join("gateway");
    let node = bundled_node(resource_dir);
    (
        engine_dir(&gw).join("package.json").is_file().then_some(gw),
        node.is_file().then_some(node),
    )
}

pub fn bundled_node(resource_dir: &Path) -> PathBuf {
    let base = resource_dir.join("runtime").join("node");
    if cfg!(windows) { base.join("node.exe") } else { base.join("bin").join("node") }
}

pub fn find_node() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PRISMFLOW_NODE_BIN") {
        return Some(PathBuf::from(p));
    }
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(crate::adapters::extra_bin_dirs());
    dirs.iter().find_map(|d| crate::adapters::exe_in(d, "node"))
}

/// `"v22.23.1"` → 22.
pub fn node_major(version: &str) -> Option<u32> {
    version.trim().trim_start_matches('v').split('.').next()?.parse().ok()
}

impl GatewayRuntime {
    pub fn discover() -> Result<Self, String> {
        let package_dir = package_dir();
        let pkg_json = engine_dir(&package_dir).join("package.json");
        if !pkg_json.is_file() && option_env!("PRISMFLOW_RELEASE").is_some() {
            return Err("Part of PrismFlow is missing. Reinstall PrismFlow.".into());
        }
        let pkg: serde_json::Value = std::fs::read_to_string(&pkg_json)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or_else(|| format!(
                "The gateway engine is not installed in {}. Run scripts/fetch-gateway.sh.",
                package_dir.display()
            ))?;
        if pkg["version"].as_str() != Some(ENGINE_VERSION) || pkg["license"].as_str() != Some("MIT") {
            return Err(format!(
                "The installed gateway engine is not the reviewed {ENGINE} {ENGINE_VERSION} (MIT). Run scripts/fetch-gateway.sh."
            ));
        }
        let node = find_node().ok_or_else(|| format!(
            "Node.js {NODE_MIN_MAJOR} or newer is required to run the gateway and was not found. Install it from nodejs.org or with Homebrew."
        ))?;
        let out = std::process::Command::new(&node)
            .arg("--version")
            .output()
            .map_err(|e| format!("Could not run {}: {e}", node.display()))?;
        let node_version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match node_major(&node_version) {
            Some(m) if m >= NODE_MIN_MAJOR => Ok(Self { node, node_version, package_dir }),
            _ => Err(format!(
                "{} is Node.js {node_version:?}; the gateway needs {NODE_MIN_MAJOR} or newer.",
                node.display()
            )),
        }
    }

    fn entry(&self) -> PathBuf {
        engine_dir(&self.package_dir).join("bin").join("next-ai-gateway.js")
    }

    fn preload(&self) -> PathBuf {
        self.package_dir.join("prismflow-preload.cjs")
    }
}

/// The engine's upstream protocol for a provider (plan D5).
pub fn engine_type(p: &ProviderDefinition) -> &'static str {
    if p.protocols.contains(&Protocol::AnthropicMessages) {
        "anthropic_messages"
    } else if p.protocols.contains(&Protocol::OpenAiChat) {
        "openai_chat_completions"
    } else {
        "openai_responses"
    }
}

/// One upstream the engine may call. The key stays in memory and reaches
/// the child only through its environment.
#[derive(Clone, PartialEq, Eq)]
pub struct UpstreamSpec {
    pub provider_id: String,
    pub engine_type: &'static str,
    pub base_url: String,
    pub models: Vec<String>,
    pub key: String,
    /// The key is a ChatGPT access token from a Codex sign-in; the engine's
    /// Codex OAuth plugin injects it (refresh disabled, see signin.rs).
    pub codex_oauth: bool,
    /// Extra headers every request to this upstream needs (GitHub Copilot's
    /// endpoint headers and short-lived session token).
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for UpstreamSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamSpec")
            .field("provider_id", &self.provider_id)
            .field("engine_type", &self.engine_type)
            .field("base_url", &self.base_url)
            .field("models", &self.models)
            .field("codex_oauth", &self.codex_oauth)
            .finish_non_exhaustive()
    }
}

pub fn key_env(provider_id: &str) -> String {
    format!("PRISMFLOW_UPSTREAM_{}", provider_id.to_ascii_uppercase())
}

/// The engine config. Keys are env-var names, never values. The one
/// exception: GitHub Copilot's short-lived session headers (the engine has
/// no env templating for headers); the file is private (0600) and rewritten
/// on every refresh.
pub fn render_config(specs: &[UpstreamSpec], settings: &super::RuntimeSettings) -> serde_json::Value {
    let providers: Vec<_> = specs
        .iter()
        .map(|s| json!({
            "name": s.provider_id,
            "type": s.engine_type,
            "apiKeyEnv": key_env(&s.provider_id),
            "baseurl": s.base_url,
            "models": s.models,
            "extraHeaders": { "default": s.headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<String, serde_json::Value>>(), "byModel": {} },
        }))
        .collect();
    // Env references only: the file never holds a token.
    let plugins: Vec<_> = specs
        .iter()
        .filter(|s| s.codex_oauth)
        .map(|s| json!({
            "key": format!("prismflow-codex-oauth-{}", s.provider_id),
            "match": { "providerName": s.provider_id },
            "providerHooks": { "codexOauth": {
                "accessToken": format!("{{{{ env.{} }}}}", key_env(&s.provider_id)),
                "required": true,
                "refreshIfMissingAccessToken": false,
                "forceRefresh": false,
                "requiredScopes": []
            } }
        }))
        .collect();
    json!({
        "host": "127.0.0.1",
        "plugins": plugins,
        "providers": providers,
        "defaultTargetProvider": specs.first().map(|s| s.provider_id.clone()),
        "auth": {
            "enabled": true,
            "mode": "static_api_key",
            "required": true,
            "staticApiKeys": { "keysEnv": CLIENT_KEY_ENV, "keyHeader": "authorization", "keyBearerOnly": true }
        },
        "logging": { "enabled": true, "level": "warn", "accessLog": false },
        "metrics": { "enabled": false },
        "cors": { "enabled": false },
        "providerHealthCheck": { "enabled": false },
        "mcpGateway": { "enabled": false, "websocket": { "enabled": false } },
        "rawTrace": { "enabled": false },
        "billing": { "enabled": false },
        "upstreamConcurrency": {
            "enabled": settings.max_concurrency_per_provider > 0,
            "maxInFlightPerProvider": settings.max_concurrency_per_provider.max(1),
            "queueTimeoutMs": 30_000
        },
        "upstreamRetry": {
            "enabled": settings.upstream_retries > 0,
            "maxAttempts": settings.upstream_retries + 1
        }
    })
}

pub type LogTail = Arc<Mutex<VecDeque<String>>>;

/// A running engine. Dropping it closes stdin and kills the process.
pub struct EngineChild {
    child: Child,
    stdin: Option<ChildStdin>,
    pub port: u16,
    pub client_key: String,
    pub started: Instant,
    pub log_tail: LogTail,
}

fn free_loopback_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn pump(stream: impl tokio::io::AsyncRead + Unpin + Send + 'static, tail: LogTail, secrets: Vec<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
            let clean = redact_known(&line, &refs);
            let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
            if t.len() == LOG_TAIL_LINES {
                t.pop_front();
            }
            t.push_back(clean.chars().take(500).collect());
        }
    });
}

pub fn tail_text(tail: &LogTail, lines: usize) -> String {
    let t = tail.lock().unwrap_or_else(|e| e.into_inner());
    t.iter().skip(t.len().saturating_sub(lines)).cloned().collect::<Vec<_>>().join("\n")
}

/// Write the config into `work_dir` and start the engine.
pub fn spawn(rt: &GatewayRuntime, specs: &[UpstreamSpec], settings: &super::RuntimeSettings, work_dir: &Path) -> Result<EngineChild, String> {
    std::fs::create_dir_all(work_dir).map_err(|e| format!("Cannot create {}: {e}", work_dir.display()))?;
    let config_path = work_dir.join("gateway.config.json");
    let body = serde_json::to_string_pretty(&render_config(specs, settings)).map_err(|e| e.to_string())?;
    crate::config_engine::atomic_write(&config_path, &body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600));
    }
    let port = free_loopback_port().map_err(|e| format!("No free loopback port for the gateway: {e}"))?;
    let client_key = tokens::generate();
    let manager_key = tokens::generate();

    let mut cmd = Command::new(&rt.node);
    cmd.arg("--require").arg(rt.preload()).arg(rt.entry());
    cmd.current_dir(work_dir).env_clear();
    if cfg!(windows) {
        // Node on Windows needs the system directories and these variables.
        for var in ["SystemRoot", "windir", "SystemDrive", "TEMP", "TMP", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
            if let Some(v) = std::env::var_os(var) {
                cmd.env(var, v);
            }
        }
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        cmd.env("PATH", format!(r"{root}\System32;{root}"));
    } else {
        cmd.env("PATH", "/usr/bin:/bin");
    }
    if let Some(home) = dirs::home_dir() {
        cmd.env("HOME", home);
    }
    cmd.env("NODE_ENV", "production")
        .env("HOST", "127.0.0.1")
        .env("PORT", port.to_string())
        .env("GATEWAY_CONFIG_PATH", &config_path)
        .env(CLIENT_KEY_ENV, &client_key)
        .env("MANAGER_API_KEY", &manager_key)
        // The engine's default is a fixed 60 s for the whole upstream request,
        // which cuts off any long streamed answer (found by an end-to-end app
        // test, 2026-10-15). Disable it; the broker enforces an idle timeout.
        .env("UPSTREAM_TIMEOUT_MS", "0");
    for s in specs {
        cmd.env(key_env(&s.provider_id), &s.key);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("Could not start the gateway engine: {e}"))?;

    let mut secrets: Vec<String> = specs.iter().map(|s| s.key.clone()).collect();
    secrets.push(client_key.clone());
    secrets.push(manager_key);
    let log_tail: LogTail = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(out) = child.stdout.take() {
        pump(out, log_tail.clone(), secrets.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump(err, log_tail.clone(), secrets);
    }
    let stdin = child.stdin.take();
    Ok(EngineChild { child, stdin, port, client_key, started: Instant::now(), log_tail })
}

impl EngineChild {
    /// Poll `/ready` until the engine answers, it exits, or `timeout` passes.
    pub async fn wait_ready(&mut self, client: &reqwest::Client, timeout: Duration) -> Result<(), String> {
        let url = format!("http://127.0.0.1:{}/ready", self.port);
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "The gateway engine exited during startup ({status}). Last output:\n{}",
                    tail_text(&self.log_tail, 8)
                ));
            }
            if let Ok(r) = client.get(&url).timeout(Duration::from_secs(2)).send().await {
                if r.status().is_success() {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "The gateway engine did not become ready within {}s. Last output:\n{}",
                    timeout.as_secs(),
                    tail_text(&self.log_tail, 8)
                ));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Close stdin (the preload exits cleanly), then kill if still running.
    pub async fn shutdown(mut self) {
        drop(self.stdin.take());
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::*;

    #[test]
    fn bundled_paths_only_when_present() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(bundled_paths(dir.path()), (None, None));
        let pkg = engine_dir(&dir.path().join("gateway"));
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), "{}").unwrap();
        let node = bundled_node(dir.path());
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::write(&node, "").unwrap();
        assert_eq!(bundled_paths(dir.path()), (Some(dir.path().join("gateway")), Some(node)));
    }

    #[test]
    fn bundled_node_path_per_platform() {
        let p = bundled_node(Path::new("/r"));
        if cfg!(windows) {
            assert!(p.ends_with("runtime/node/node.exe"));
        } else {
            assert_eq!(p, PathBuf::from("/r/runtime/node/bin/node"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers;

    #[test]
    fn node_versions_parse() {
        assert_eq!(node_major("v22.23.1\n"), Some(22));
        assert_eq!(node_major("v18.0.0"), Some(18));
        assert_eq!(node_major("garbage"), None);
    }

    #[test]
    fn engine_type_follows_protocols() {
        assert_eq!(engine_type(providers::get("nvidia_nim").unwrap()), "openai_chat_completions");
        assert_eq!(engine_type(providers::get("openrouter").unwrap()), "openai_chat_completions");
        assert_eq!(engine_type(providers::get("anthropic").unwrap()), "anthropic_messages");
    }

    #[test]
    fn config_is_loopback_authenticated_quiet_and_secret_free() {
        let spec = UpstreamSpec {
            provider_id: "nvidia_nim".into(),
            engine_type: "openai_chat_completions",
            base_url: "https://integrate.api.nvidia.com/v1".into(),
            models: vec!["meta/llama-3.3-70b-instruct".into()],
            key: "nvapi-supersecretvalue".into(), codex_oauth: false, headers: vec![] };
        let cfg = render_config(std::slice::from_ref(&spec), &crate::gateway::RuntimeSettings::default());
        let text = cfg.to_string();
        assert!(!text.contains("supersecret"));
        assert_eq!(cfg["host"], "127.0.0.1");
        assert_eq!(cfg["auth"]["required"], true);
        assert_eq!(cfg["logging"]["level"], "warn");
        assert_eq!(cfg["mcpGateway"]["enabled"], false);
        assert_eq!(cfg["providers"][0]["apiKeyEnv"], "PRISMFLOW_UPSTREAM_NVIDIA_NIM");
        assert!(!format!("{spec:?}").contains("supersecret"), "Debug must not print keys");
    }
}
