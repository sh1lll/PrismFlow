//! `Backend` for the real app: a thin, scoped view of `Service`.

use super::server::{Backend, BoxFut};
use crate::diagnostics::Level;
use crate::service::Service;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ServiceBackend(pub Arc<Service>);

fn label(s: crate::state::ConnectionStatus) -> &'static str {
    use crate::state::ConnectionStatus::*;
    match s {
        NotConfigured => "Not configured",
        NeedsCredentials => "Needs credentials",
        ConfigurationInvalid => "Configuration invalid",
        AuthenticationFailed => "Authentication failed",
        ProviderUnavailable => "Provider unavailable",
        ModelUnavailable => "Model unavailable",
        PartiallyCompatible => "Partially compatible",
        ConnectedAndVerified => "Connected and verified",
        Disconnected => "Disconnected",
        RequiresAttention => "Requires attention",
        Unverified => "Configured — not verified",
    }
}

impl Backend for ServiceBackend {
    /// Status only: no file paths, keys, tokens or routing internals.
    fn status(&self) -> Value {
        let conns: Vec<Value> = self.0.connections().unwrap_or_default().into_iter().map(|c| json!({
            "id": c.id,
            "app_id": c.app_id,
            "app_name": crate::service::app_name(&c.app_id),
            "provider_id": c.provider_id,
            "model": c.model,
            "status": c.status,
            "status_label": label(c.status),
            "last_verified": c.last_report.as_ref().map(|r| r.finished_at),
        })).collect();
        let g = self.0.gateway_status();
        json!({ "connections": conns, "gateway": { "phase": g.phase, "requests": g.requests, "connections": g.connections } })
    }

    fn diagnostics(&self) -> Value {
        json!(self.0.diagnostics(50))
    }

    fn verify(&self, id: String) -> BoxFut<'_, Result<Value, String>> {
        Box::pin(async move {
            let r = self.0.verify(&id).await?;
            Ok(json!({ "status": label(r.status) }))
        })
    }

    /// Only a model already discovered for the connection's provider.
    fn switch_model(&self, id: String, model: String) -> BoxFut<'_, Result<Value, String>> {
        Box::pin(async move {
            let rec = self.0.connections()?.into_iter().find(|c| c.id == id).ok_or("Unknown connection.")?;
            let listed = self.0.discover_models(&rec.provider_id).await?;
            if !listed.iter().any(|m| m.id == model) {
                return Err(format!("{model} is not in {}'s model list. Remote devices can only pick listed models.", rec.provider_id));
            }
            let r = self.0.connect(&rec.app_id, &rec.provider_id, &model).await?;
            Ok(json!({ "id": r.id, "model": r.model, "status": label(r.status) }))
        })
    }

    fn restart_gateway(&self) -> BoxFut<'_, Result<Value, String>> {
        Box::pin(async move {
            let s = self.0.restart_gateway().await?;
            Ok(json!({ "phase": s.phase }))
        })
    }

    fn record(&self, code: &str, message: &str) {
        let level = if code.ends_with("unauthorized") || code.ends_with("failed") { Level::Warn } else { Level::Info };
        self.0.diag.record(level, code, message);
    }
}
