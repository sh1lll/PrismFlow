//! The companion's HTTPS listener. Route allow-list (nothing else exists):
//!   GET  /                     installable web client (no data in it)
//!   GET  /manifest.webmanifest
//!   POST /pair                 one-time code → device token
//!   GET  /api/status           connections + gateway (read)
//!   GET  /api/diagnostics      recent sanitized events (read)
//!   POST /api/connections/{id}/verify   (action)
//!   POST /api/connections/{id}/model    (action) switch to a discovered model
//!   POST /api/gateway/restart           (action)
//! Every request must name our address in Host; a browser Origin must be our
//! own origin; /api needs a device token; actions need that device's
//! "Allow actions". Keys, tokens and the gateway are unreachable by design.

use super::{Registry, tls};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the companion may ask of the app. Implemented by `Service`.
pub trait Backend: Send + Sync + 'static {
    fn status(&self) -> Value;
    fn diagnostics(&self) -> Value;
    fn verify(&self, id: String) -> BoxFut<'_, Result<Value, String>>;
    fn switch_model(&self, id: String, model: String) -> BoxFut<'_, Result<Value, String>>;
    fn restart_gateway(&self) -> BoxFut<'_, Result<Value, String>>;
    fn record(&self, code: &str, message: &str);
}

const FAILED_AUTH_PER_MINUTE: u32 = 10;
const INDEX_HTML: &str = include_str!("../../companion-web/index.html");
const MANIFEST: &str = include_str!("../../companion-web/manifest.webmanifest");

#[derive(Clone)]
struct Ctx {
    registry: Arc<Registry>,
    backend: Arc<dyn Backend>,
    authority: String,
    origin: String,
    failures: Arc<Mutex<HashMap<IpAddr, (Instant, u32)>>>,
}

#[derive(Clone, Copy)]
struct Peer(SocketAddr);

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn now() -> u64 {
    crate::diagnostics::now_secs()
}

impl Ctx {
    fn guard(&self, headers: &HeaderMap) -> Result<(), Response> {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        if host != self.authority {
            return Err(err(StatusCode::FORBIDDEN, "wrong host"));
        }
        if let Some(o) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            if o != self.origin {
                return Err(err(StatusCode::FORBIDDEN, "cross-origin requests are not accepted"));
            }
        }
        Ok(())
    }

    fn limited(&self, ip: IpAddr) -> bool {
        let f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        f.get(&ip).is_some_and(|(t, n)| t.elapsed() < Duration::from_secs(60) && *n >= FAILED_AUTH_PER_MINUTE)
    }

    fn fail(&self, ip: IpAddr) {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        let e = f.entry(ip).or_insert((Instant::now(), 0));
        if e.0.elapsed() >= Duration::from_secs(60) {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
    }

    /// Authenticate a device; `action` additionally requires its permission.
    fn device(&self, headers: &HeaderMap, peer: Peer, action: bool) -> Result<super::Device, Response> {
        self.guard(headers)?;
        let ip = peer.0.ip();
        if self.limited(ip) {
            return Err(err(StatusCode::TOO_MANY_REQUESTS, "too many failed attempts; wait a minute"));
        }
        let token = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
        let Some(dev) = self.registry.authenticate(token.trim(), now()) else {
            self.fail(ip);
            self.backend.record("companion.unauthorized", &format!("Rejected a companion request from {ip}: unknown or revoked device token."));
            return Err(err(StatusCode::UNAUTHORIZED, "unknown or revoked device; pair again"));
        };
        if action && !dev.allow_actions {
            return Err(err(StatusCode::FORBIDDEN, "this device is read-only; allow actions for it on the desktop"));
        }
        Ok(dev)
    }
}

async fn index(State(c): State<Ctx>, headers: HeaderMap) -> Response {
    if let Err(r) = c.guard(&headers) {
        return r;
    }
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], INDEX_HTML).into_response()
}

async fn manifest(State(c): State<Ctx>, headers: HeaderMap) -> Response {
    if let Err(r) = c.guard(&headers) {
        return r;
    }
    ([(header::CONTENT_TYPE, "application/manifest+json")], MANIFEST).into_response()
}

#[derive(serde::Deserialize)]
struct PairReq {
    code: String,
    device_name: String,
}

async fn pair(State(c): State<Ctx>, Extension(peer): Extension<Peer>, headers: HeaderMap, Json(req): Json<PairReq>) -> Response {
    if let Err(r) = c.guard(&headers) {
        return r;
    }
    let ip = peer.0.ip();
    if c.limited(ip) {
        return err(StatusCode::TOO_MANY_REQUESTS, "too many failed attempts; wait a minute");
    }
    match c.registry.pair(&req.code, &req.device_name, now()) {
        Ok((dev, token)) => {
            c.backend.record("companion.paired", &format!("Paired device \"{}\" from {ip} (read-only).", dev.name));
            Json(json!({ "token": token, "device": { "id": dev.id, "name": dev.name, "allow_actions": dev.allow_actions } })).into_response()
        }
        Err(e) => {
            c.fail(ip);
            c.backend.record("companion.pair_failed", &format!("Pairing attempt from {ip} failed: {e}."));
            err(StatusCode::UNAUTHORIZED, &e.to_string())
        }
    }
}

async fn status(State(c): State<Ctx>, Extension(peer): Extension<Peer>, headers: HeaderMap) -> Response {
    match c.device(&headers, peer, false) {
        Err(r) => r,
        Ok(dev) => {
            let mut v = c.backend.status();
            v["device"] = json!({ "id": dev.id, "name": dev.name, "allow_actions": dev.allow_actions });
            Json(v).into_response()
        }
    }
}

async fn diagnostics(State(c): State<Ctx>, Extension(peer): Extension<Peer>, headers: HeaderMap) -> Response {
    match c.device(&headers, peer, false) {
        Err(r) => r,
        Ok(_) => Json(c.backend.diagnostics()).into_response(),
    }
}

fn done(r: Result<Value, String>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::UNPROCESSABLE_ENTITY, &e),
    }
}

async fn verify(State(c): State<Ctx>, Extension(peer): Extension<Peer>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    match c.device(&headers, peer, true) {
        Err(r) => r,
        Ok(dev) => {
            c.backend.record("companion.action", &format!("Device \"{}\" ran verification for {id}.", dev.name));
            done(c.backend.verify(id).await)
        }
    }
}

#[derive(serde::Deserialize)]
struct ModelReq {
    model: String,
}

async fn switch_model(State(c): State<Ctx>, Extension(peer): Extension<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<ModelReq>) -> Response {
    match c.device(&headers, peer, true) {
        Err(r) => r,
        Ok(dev) => {
            c.backend.record("companion.action", &format!("Device \"{}\" switched {id} to {}.", dev.name, req.model));
            done(c.backend.switch_model(id, req.model).await)
        }
    }
}

async fn restart(State(c): State<Ctx>, Extension(peer): Extension<Peer>, headers: HeaderMap) -> Response {
    match c.device(&headers, peer, true) {
        Err(r) => r,
        Ok(dev) => {
            c.backend.record("companion.action", &format!("Device \"{}\" restarted the gateway.", dev.name));
            done(c.backend.restart_gateway().await)
        }
    }
}

fn router(ctx: Ctx) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/manifest.webmanifest", get(manifest))
        .route("/pair", post(pair))
        .route("/api/status", get(status))
        .route("/api/diagnostics", get(diagnostics))
        .route("/api/connections/{id}/verify", post(verify))
        .route("/api/connections/{id}/model", post(switch_model))
        .route("/api/gateway/restart", post(restart))
        .with_state(ctx)
}

pub struct Running {
    pub url: String,
    pub fingerprint: String,
    stop: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl Running {
    pub async fn stop(mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), &mut self.handle).await;
    }
}

/// Start the HTTPS listener on exactly `address:port` (never 0.0.0.0).
pub async fn start(
    address: IpAddr,
    port: u16,
    state_dir: &std::path::Path,
    registry: Arc<Registry>,
    backend: Arc<dyn Backend>,
) -> Result<Running, String> {
    if address.is_unspecified() {
        return Err("Choose one network address; listening on all interfaces is not allowed.".into());
    }
    let id = tls::load_or_create(state_dir, &[address.to_string()])?;
    let cfg = tls::server_config(&id)?;
    let listener = tokio::net::TcpListener::bind((address, port)).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::AddrInUse => format!("Port {port} on {address} is in use; choose another port."),
        std::io::ErrorKind::AddrNotAvailable => format!("{address} is not an address of this computer."),
        _ => e.to_string(),
    })?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let authority = match address {
        IpAddr::V6(a) => format!("[{a}]:{port}"),
        IpAddr::V4(a) => format!("{a}:{port}"),
    };
    let url = format!("https://{authority}");
    let ctx = Ctx {
        registry,
        backend,
        authority: authority.clone(),
        origin: url.clone(),
        failures: Arc::new(Mutex::new(HashMap::new())),
    };
    let app = router(ctx);
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let (tx, mut rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => {
                    let Ok((tcp, peer)) = accepted else { continue };
                    let acceptor = acceptor.clone();
                    let app = app.clone().layer(Extension(Peer(peer)));
                    tokio::spawn(async move {
                        let Ok(Ok(tls)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await else { return };
                        let svc = hyper_util::service::TowerToHyperService::new(app);
                        let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                            .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                            .await;
                    });
                }
            }
        }
    });
    Ok(Running { url, fingerprint: id.fingerprint, stop: Some(tx), handle })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        events: Mutex<Vec<String>>,
    }

    impl Backend for Fake {
        fn status(&self) -> Value {
            json!({ "connections": [{ "id": "codex_cli:ollama", "status": "ConnectedAndVerified" }], "gateway": { "phase": "ready" } })
        }
        fn diagnostics(&self) -> Value {
            json!([])
        }
        fn verify(&self, id: String) -> BoxFut<'_, Result<Value, String>> {
            Box::pin(async move { Ok(json!({ "verified": id })) })
        }
        fn switch_model(&self, id: String, model: String) -> BoxFut<'_, Result<Value, String>> {
            Box::pin(async move { Ok(json!({ "id": id, "model": model })) })
        }
        fn restart_gateway(&self) -> BoxFut<'_, Result<Value, String>> {
            Box::pin(async { Ok(json!({ "phase": "ready" })) })
        }
        fn record(&self, code: &str, _m: &str) {
            self.events.lock().unwrap().push(code.into());
        }
    }

    /// A client that trusts only the pinned fingerprint (checked via TLS info).
    fn client() -> reqwest::Client {
        reqwest::Client::builder().danger_accept_invalid_certs(true).tls_info(true).no_proxy().build().unwrap()
    }

    async fn up() -> (Running, Arc<Registry>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(Registry::open(dir.path()).unwrap());
        let backend: Arc<dyn Backend> = Arc::new(Fake { events: Mutex::new(vec![]) });
        let run = start("127.0.0.1".parse().unwrap(), 0, dir.path(), reg.clone(), backend).await.unwrap();
        (run, reg, dir)
    }

    #[tokio::test]
    async fn tls_pairing_auth_and_action_permissions() {
        let (run, reg, _d) = up().await;
        let c = client();
        // TLS 1.3 with the pinned certificate.
        let r = c.get(format!("{}/", run.url)).send().await.unwrap();
        let info = r.extensions().get::<reqwest::tls::TlsInfo>().unwrap();
        assert_eq!(tls::fingerprint(info.peer_certificate().unwrap()), run.fingerprint);
        assert!(r.text().await.unwrap().contains("PrismFlow"));

        assert_eq!(c.get(format!("{}/api/status", run.url)).send().await.unwrap().status(), 401);
        let code = reg.start_pairing();
        let paired: Value = c.post(format!("{}/pair", run.url)).json(&json!({"code": code, "device_name": "phone"}))
            .send().await.unwrap().json().await.unwrap();
        let token = paired["token"].as_str().unwrap().to_string();
        let st: Value = c.get(format!("{}/api/status", run.url)).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
        assert_eq!(st["device"]["allow_actions"], false);
        assert_eq!(st["connections"][0]["id"], "codex_cli:ollama");

        let url = format!("{}/api/gateway/restart", run.url);
        assert_eq!(c.post(&url).bearer_auth(&token).send().await.unwrap().status(), 403, "read-only by default");
        reg.set_allow_actions(paired["device"]["id"].as_str().unwrap(), true).unwrap();
        assert_eq!(c.post(&url).bearer_auth(&token).send().await.unwrap().status(), 200);
        let m: Value = c.post(format!("{}/api/connections/codex_cli:ollama/model", run.url)).bearer_auth(&token)
            .json(&json!({"model": "m2"})).send().await.unwrap().json().await.unwrap();
        assert_eq!(m["model"], "m2");

        reg.revoke(paired["device"]["id"].as_str().unwrap()).unwrap();
        assert_eq!(c.get(format!("{}/api/status", run.url)).bearer_auth(&token).send().await.unwrap().status(), 401);
        run.stop().await;
    }

    #[tokio::test]
    async fn host_origin_and_hidden_routes_are_refused() {
        let (run, reg, _d) = up().await;
        let c = client();
        let code = reg.start_pairing();
        let token: Value = c.post(format!("{}/pair", run.url)).json(&json!({"code": code, "device_name": "p"}))
            .send().await.unwrap().json().await.unwrap();
        let token = token["token"].as_str().unwrap();
        let st = format!("{}/api/status", run.url);
        assert_eq!(c.get(&st).bearer_auth(token).header("host", "evil.example").send().await.unwrap().status(), 403);
        assert_eq!(c.get(&st).bearer_auth(token).header("origin", "https://evil.example").send().await.unwrap().status(), 403);
        for hidden in ["/api/keys", "/v1/messages", "/api/tokens", "/api/providers"] {
            let s = c.get(format!("{}{hidden}", run.url)).bearer_auth(token).send().await.unwrap().status();
            assert_eq!(s, 404, "{hidden} must not exist");
        }
        run.stop().await;
    }

    #[tokio::test]
    async fn failed_auth_is_rate_limited_and_plain_http_is_refused() {
        let (run, _reg, _d) = up().await;
        let c = client();
        let st = format!("{}/api/status", run.url);
        for _ in 0..FAILED_AUTH_PER_MINUTE {
            assert_eq!(c.get(&st).bearer_auth("pfd_bad").send().await.unwrap().status(), 401);
        }
        assert_eq!(c.get(&st).bearer_auth("pfd_bad").send().await.unwrap().status(), 429);
        let plain = run.url.replace("https://", "http://");
        assert!(reqwest::Client::new().get(format!("{plain}/")).send().await.is_err(), "no plaintext listener");
        run.stop().await;
    }

    #[tokio::test]
    async fn refuses_all_interfaces() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(Registry::open(dir.path()).unwrap());
        let backend: Arc<dyn Backend> = Arc::new(Fake { events: Mutex::new(vec![]) });
        assert!(start("0.0.0.0".parse().unwrap(), 0, dir.path(), reg, backend).await.is_err());
    }
}
