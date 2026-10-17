//! PrismFlow's gateway: a loopback-only Rust broker that
//! owns the trust boundary, plus a supervised translation engine behind it.
//!
//! Agents → broker (local token) → engine (internal key, `x-target-provider`)
//! → provider (upstream key). See the Slice 2 plan for the threat model.

pub mod server;
pub mod shortcuts;
pub mod webloop;
pub mod supervisor;
pub mod tokens;

use crate::diagnostics::{DiagnosticsLog, Level};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use supervisor::{EngineChild, GatewayRuntime, UpstreamSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

/// Fixed so agent configs keep a stable URL.
pub const DEFAULT_PORT: u16 = 47821;
const MAX_RESTARTS: u32 = 5;
const STABLE_AFTER: Duration = Duration::from_secs(60);
const READY_TIMEOUT: Duration = Duration::from_secs(20);

pub fn configured_port() -> u16 {
    std::env::var("PRISMFLOW_BROKER_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// User-tunable gateway limits. Persisted by
/// the service; `0` disables a limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RuntimeSettings {
    /// Requests per minute per connection, enforced by the broker.
    pub rate_limit_per_minute: u32,
    /// Concurrent upstream requests per provider, enforced by the engine.
    pub max_concurrency_per_provider: u32,
    /// Transport retries per upstream attempt (engine), for idempotent failures.
    pub upstream_retries: u32,
    /// Local WebSearch/WebFetch for Claude Code (off by default).
    pub web_tools: crate::webtools::WebToolsSettings,
    /// Answer Claude Code's background requests (titles, suggestions) locally.
    pub shortcuts: bool,
    /// Monthly spending limits in whole US dollars per provider id (estimated cost).
    pub budgets: std::collections::BTreeMap<String, u32>,
    /// Context size PrismFlow gives local Ollama models (tokens).
    pub ollama_context: u32,
    /// Seconds a provider may stay silent mid-request before PrismFlow gives up.
    pub provider_timeout_secs: u32,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self { rate_limit_per_minute: 0, max_concurrency_per_provider: 0, upstream_retries: 1, web_tools: Default::default(), shortcuts: true, budgets: Default::default(), ollama_context: 32_768, provider_timeout_secs: 300 }
    }
}

/// What a valid local token is allowed to reach.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Binding {
    pub connection_id: String,
    pub provider_id: String,
    /// The connection's own model; empty = use whatever the client sends.
    pub model: String,
    /// Tiers, reasoning policy and fallbacks for this connection.
    pub routing: Arc<crate::routing::Routing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Stopped,
    Starting,
    Ready,
    Restarting,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrokerStatus {
    pub phase: Phase,
    /// Base URL agents use; empty while stopped.
    pub url: String,
    pub port: u16,
    pub engine: String,
    pub node: Option<String>,
    /// Human-readable reason for Failed/Restarting, sanitized.
    pub detail: Option<String>,
    pub restarts: u32,
    pub requests: u64,
    pub rejected: u64,
    pub upstream_errors: u64,
    /// Background requests answered locally (see `shortcuts`).
    pub shortcuts: u64,
    pub connections: usize,
    pub upstreams: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BrokerError {
    #[error("Another PrismFlow instance is already running the gateway on 127.0.0.1:{0}. Close it, or use that instance.")]
    OtherInstance(u16),
    #[error("Another program is using the port PrismFlow needs ({0}). Quit that program or restart your computer, then try again.")]
    PortInUse(u16),
    #[error("{0}")]
    Engine(String),
    #[error("Could not start the broker: {0}")]
    Io(String),
}

#[derive(Default)]
struct Counters {
    requests: u64,
    rejected: u64,
    upstream_errors: u64,
    shortcuts: u64,
}

/// Where requests go right now. Only the supervisor writes it.
#[derive(Clone)]
pub(crate) struct Upstream {
    port: u16,
    client_key: String,
}

struct EngineState {
    phase: Phase,
    upstream: Option<Upstream>,
    detail: Option<String>,
    restarts: u32,
    node: Option<String>,
}

pub(crate) struct Inner {
    port: Mutex<u16>,
    /// Brave Search key, in memory only (from the secure store).
    pub(crate) web_key: RwLock<Option<String>>,
    pub(crate) settings: RwLock<RuntimeSettings>,
    /// Fixed one-minute windows per connection: (window start, count).
    limiter: Mutex<HashMap<String, (std::time::Instant, u32)>>,
    bindings: RwLock<HashMap<tokens::TokenDigest, Binding>>,
    engine: RwLock<EngineState>,
    counters: Mutex<Counters>,
    diag: Option<Arc<DiagnosticsLog>>,
    client: RwLock<reqwest::Client>,
    work_dir: PathBuf,
    /// Request log, token usage and spend (see `crate::usage`).
    pub(crate) usage: Arc<crate::usage::Store>,
}

impl Inner {
    pub(crate) fn record(&self, level: Level, code: &str, message: &str) {
        if let Some(d) = &self.diag {
            d.record(level, code, message);
        }
    }

    fn set_engine(&self, f: impl FnOnce(&mut EngineState)) {
        f(&mut self.engine.write().unwrap_or_else(|e| e.into_inner()));
    }

    pub(crate) fn upstream(&self) -> Result<Upstream, Phase> {
        let e = self.engine.read().unwrap_or_else(|e| e.into_inner());
        e.upstream.clone().ok_or(e.phase)
    }

    pub(crate) fn lookup(&self, token: &str) -> Option<Binding> {
        self.bindings.read().unwrap_or_else(|e| e.into_inner()).get(&tokens::digest(token)).cloned()
    }

    /// True when `connection` may send another request this minute.
    pub(crate) fn admit(&self, connection: &str) -> bool {
        let limit = self.settings.read().unwrap_or_else(|e| e.into_inner()).rate_limit_per_minute;
        if limit == 0 {
            return true;
        }
        let mut l = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        let slot = l.entry(connection.to_string()).or_insert((now, 0));
        if now.duration_since(slot.0) >= Duration::from_secs(60) {
            *slot = (now, 0);
        }
        if slot.1 >= limit {
            return false;
        }
        slot.1 += 1;
        true
    }

    pub(crate) fn count(&self, f: impl FnOnce(&mut Counters)) {
        f(&mut self.counters.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

struct Running {
    server_stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
    supervisor: Option<(watch::Sender<bool>, JoinHandle<()>)>,
    specs: Vec<UpstreamSpec>,
    settings: RuntimeSettings,
}

/// The client the broker sends requests on with. Its limit is on silence
/// (no bytes for that long), not total time: long streams are fine.
fn provider_client(timeout_secs: u32) -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .read_timeout(Duration::from_secs(u64::from(timeout_secs.clamp(30, 3600))))
        .no_proxy()
        .build()
        .expect("reqwest client")
}

impl Inner {
    pub(crate) fn http(&self) -> reqwest::Client {
        self.client.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The broker. Cheap to share; all methods take `&self`.
pub struct Broker {
    inner: Arc<Inner>,
    running: tokio::sync::Mutex<Option<Running>>,
    requested_port: u16,
}

impl Broker {
    /// `port` 0 picks an ephemeral port (tests); production uses
    /// [`configured_port`].
    pub fn new(port: u16, work_dir: PathBuf, diag: Option<Arc<DiagnosticsLog>>) -> Self {
        let client = provider_client(RuntimeSettings::default().provider_timeout_secs);
        Self {
            inner: Arc::new(Inner {
                port: Mutex::new(port),
                web_key: RwLock::new(None),
                settings: RwLock::new(RuntimeSettings::default()),
                limiter: Mutex::new(HashMap::new()),
                bindings: RwLock::new(HashMap::new()),
                engine: RwLock::new(EngineState {
                    phase: Phase::Stopped,
                    upstream: None,
                    detail: None,
                    restarts: 0,
                    node: None,
                }),
                counters: Mutex::new(Counters::default()),
                diag,
                client: RwLock::new(client),
                usage: Arc::new(crate::usage::Store::open(&work_dir)),
                work_dir,
            }),
            running: tokio::sync::Mutex::new(None),
            requested_port: port,
        }
    }

    pub fn usage(&self) -> Arc<crate::usage::Store> {
        self.inner.usage.clone()
    }

    pub fn port(&self) -> u16 {
        *self.inner.port.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The URL written into agent configs. Stable across restarts because
    /// the production port is fixed.
    pub fn url_for_config(&self) -> String {
        self.url()
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    pub fn status(&self) -> BrokerStatus {
        let e = self.inner.engine.read().unwrap_or_else(|e| e.into_inner());
        let c = self.inner.counters.lock().unwrap_or_else(|e| e.into_inner());
        let bindings = self.inner.bindings.read().unwrap_or_else(|e| e.into_inner());
        let mut upstreams: Vec<String> = bindings.values().map(|b| b.provider_id.clone()).collect();
        upstreams.sort();
        upstreams.dedup();
        BrokerStatus {
            phase: e.phase,
            url: if e.phase == Phase::Stopped { String::new() } else { self.url() },
            port: self.port(),
            engine: format!("{} {}", supervisor::ENGINE, supervisor::ENGINE_VERSION),
            node: e.node.clone(),
            detail: e.detail.clone(),
            restarts: e.restarts,
            requests: c.requests,
            rejected: c.rejected,
            upstream_errors: c.upstream_errors,
            shortcuts: c.shortcuts,
            connections: bindings.len(),
            upstreams,
        }
    }

    /// Update limits; engine-side ones apply on the next `start`.
    pub fn set_settings(&self, settings: RuntimeSettings) {
        let old = self.settings().provider_timeout_secs;
        if settings.provider_timeout_secs != old {
            *self.inner.client.write().unwrap_or_else(|e| e.into_inner()) = provider_client(settings.provider_timeout_secs);
        }
        *self.inner.settings.write().unwrap_or_else(|e| e.into_inner()) = settings;
    }

    pub fn set_web_key(&self, key: Option<String>) {
        *self.inner.web_key.write().unwrap_or_else(|e| e.into_inner()) = key;
    }

    pub fn settings(&self) -> RuntimeSettings {
        self.inner.settings.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the token table. Tokens missing from `bindings` stop working
    /// on the next request (revocation).
    pub fn set_bindings(&self, bindings: Vec<(String, Binding)>) {
        let table = bindings.into_iter().map(|(t, b)| (tokens::digest(&t), b)).collect();
        *self.inner.bindings.write().unwrap_or_else(|e| e.into_inner()) = table;
    }

    /// Start (or reconfigure) the broker and the engine, then wait until the
    /// engine is ready. Unchanged upstreams keep the running engine.
    pub async fn start(&self, specs: Vec<UpstreamSpec>, bindings: Vec<(String, Binding)>) -> Result<BrokerStatus, BrokerError> {
        let runtime = GatewayRuntime::discover().map_err(|e| {
            self.inner.set_engine(|s| {
                s.phase = Phase::Failed;
                s.detail = Some(e.clone());
            });
            self.inner.record(Level::Error, "gateway.unavailable", &e);
            BrokerError::Engine(e)
        })?;
        self.set_bindings(bindings);
        let mut running = self.running.lock().await;
        if running.is_none() {
            let (stop_tx, server) = self.bind().await?;
            *running = Some(Running { server_stop: Some(stop_tx), server, supervisor: None, specs: Vec::new(), settings: self.settings() });
        }
        let r = running.as_mut().expect("just set");
        let engine_alive = r.supervisor.as_ref().is_some_and(|(_, h)| !h.is_finished());
        let settings = self.settings();
        if !engine_alive || r.specs != specs || r.settings != settings {
            if let Some((stop, handle)) = r.supervisor.take() {
                let _ = stop.send(true);
                let _ = handle.await;
            }
            let (stop_tx, stop_rx) = watch::channel(false);
            self.inner.set_engine(|s| {
                s.phase = Phase::Starting;
                s.upstream = None;
                s.detail = None;
                s.restarts = 0;
                s.node = Some(runtime.node_version.clone());
            });
            let handle = tokio::spawn(supervise(self.inner.clone(), runtime, specs.clone(), stop_rx));
            r.supervisor = Some((stop_tx, handle));
            r.specs = specs;
            r.settings = settings;
        }
        drop(running);
        self.wait_settled(READY_TIMEOUT + Duration::from_secs(5)).await;
        let status = self.status();
        match status.phase {
            Phase::Ready => Ok(status),
            _ => Err(BrokerError::Engine(status.detail.unwrap_or_else(|| "The gateway engine did not start.".into()))),
        }
    }

    async fn wait_settled(&self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if matches!(self.status().phase, Phase::Ready | Phase::Failed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn bind(&self) -> Result<(oneshot::Sender<()>, JoinHandle<()>), BrokerError> {
        let want = if self.requested_port == 0 { 0 } else { self.port() };
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", want)).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                let err = if server::is_prismflow_broker(&self.inner.http(), want).await {
                    BrokerError::OtherInstance(want)
                } else {
                    BrokerError::PortInUse(want)
                };
                self.inner.set_engine(|s| {
                    s.phase = Phase::Failed;
                    s.detail = Some(err.to_string());
                });
                self.inner.record(Level::Error, "gateway.port_conflict", &err.to_string());
                return Err(err);
            }
            Err(e) => return Err(BrokerError::Io(e.to_string())),
        };
        let port = listener.local_addr().map_err(|e| BrokerError::Io(e.to_string()))?.port();
        *self.inner.port.lock().unwrap_or_else(|e| e.into_inner()) = port;
        let (tx, rx) = oneshot::channel();
        let app = server::router(self.inner.clone());
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });
        self.inner.record(Level::Info, "gateway.listening", &format!("Broker listening on 127.0.0.1:{port} (loopback only)."));
        Ok((tx, handle))
    }

    /// Stop the engine and the listener. Tokens stay in the secure store.
    pub async fn stop(&self) {
        let Some(mut r) = self.running.lock().await.take() else { return };
        if let Some((stop, handle)) = r.supervisor.take() {
            let _ = stop.send(true);
            let _ = handle.await;
        }
        if let Some(tx) = r.server_stop.take() {
            let _ = tx.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), r.server).await;
        self.set_bindings(Vec::new());
        self.inner.set_engine(|s| {
            s.phase = Phase::Stopped;
            s.upstream = None;
            s.detail = None;
        });
        self.inner.record(Level::Info, "gateway.stopped", "Gateway stopped (no gateway connections remain).");
    }

    pub async fn is_running(&self) -> bool {
        self.running.lock().await.is_some()
    }
}

/// Keep the engine alive: start, wait for readiness, publish the upstream,
/// restart with backoff on exit. Gives up after `MAX_RESTARTS` quick failures.
async fn supervise(inner: Arc<Inner>, rt: GatewayRuntime, specs: Vec<UpstreamSpec>, mut stop: watch::Receiver<bool>) {
    let mut failures: u32 = 0;
    loop {
        let settings = inner.settings.read().unwrap_or_else(|e| e.into_inner()).clone();
        let attempt = supervisor::spawn(&rt, &specs, &settings, &inner.work_dir.join("gateway"));
        let mut child: Option<EngineChild> = None;
        let error = match attempt {
            Err(e) => e,
            Ok(mut c) => match c.wait_ready(&inner.http(), READY_TIMEOUT).await {
                Err(e) => {
                    c.shutdown().await;
                    e
                }
                Ok(()) => {
                    inner.set_engine(|s| {
                        s.phase = Phase::Ready;
                        s.upstream = Some(Upstream { port: c.port, client_key: c.client_key.clone() });
                        s.detail = None;
                    });
                    inner.record(Level::Info, "gateway.ready", &format!(
                        "Gateway engine {} {} ready (pid {}).",
                        supervisor::ENGINE, supervisor::ENGINE_VERSION,
                        c.pid().map(|p| p.to_string()).unwrap_or_default()
                    ));
                    child = Some(c);
                    String::new()
                }
            },
        };
        if let Some(mut c) = child {
            tokio::select! {
                status = c.wait() => {
                    let ran = c.started.elapsed();
                    if ran >= STABLE_AFTER {
                        failures = 0;
                    }
                    let msg = format!(
                        "The gateway engine exited ({}). Last output:\n{}",
                        status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string()),
                        supervisor::tail_text(&c.log_tail, 8)
                    );
                    inner.record(Level::Error, "gateway.crashed", &msg);
                    inner.set_engine(|s| { s.upstream = None; s.detail = Some(msg); });
                }
                _ = stop.changed() => {
                    inner.set_engine(|s| s.upstream = None);
                    c.shutdown().await;
                    return;
                }
            }
        } else {
            inner.record(Level::Error, "gateway.start_failed", &error);
            inner.set_engine(|s| s.detail = Some(error.clone()));
        }
        failures += 1;
        if failures > MAX_RESTARTS {
            inner.set_engine(|s| {
                s.phase = Phase::Failed;
                s.upstream = None;
            });
            inner.record(Level::Error, "gateway.gave_up", &format!(
                "The gateway engine failed {MAX_RESTARTS} times in a row; restart it under More → Activity."
            ));
            return;
        }
        inner.set_engine(|s| {
            s.phase = Phase::Restarting;
            s.restarts += 1;
        });
        let backoff = Duration::from_millis(500u64.saturating_mul(1 << failures.min(5))).min(Duration::from_secs(10));
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = stop.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests;
