//! Backup and restore: your PrismFlow setup in one file, to keep or move to
//! another computer. Keys and tokens are never included (the file is plain
//! text); after restoring, add or import your keys and the connections come
//! back as they were.

use crate::service::Service;
use serde::{Deserialize, Serialize};
use serde_json::Value;

type R<T> = Result<T, String>;

pub const FORMAT: &str = "prismflow-backup-1";

#[derive(Debug, Serialize, Deserialize)]
pub struct Backup {
    pub format: String,
    pub created: u64,
    pub app_version: String,
    pub connections: Vec<crate::accounts::ProfileEntry>,
    pub settings: crate::gateway::RuntimeSettings,
    pub custom_providers: Vec<crate::providers::CustomProvider>,
    pub profiles: crate::accounts::Profiles,
    pub mcp_servers: Vec<crate::mcp::Server>,
    pub bots: Value,
}

#[derive(Debug, Default, Serialize)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    /// Plain notes, e.g. "Codex → OpenRouter: add an OpenRouter key, then connect".
    pub problems: Vec<String>,
}

impl Service {
    pub fn backup(&self) -> R<Backup> {
        let st = crate::mcp::load(self.state.dir());
        Ok(Backup {
            format: FORMAT.into(),
            created: crate::diagnostics::now_secs(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            connections: self.connections()?.into_iter()
                .map(|c| crate::accounts::ProfileEntry { app_id: c.app_id, provider_id: c.provider_id, model: c.model, routing: c.routing })
                .collect(),
            settings: self.settings(),
            custom_providers: self.state.load_custom_providers().map_err(|e| e.to_string())?,
            profiles: self.profiles(),
            mcp_servers: st.servers,
            bots: serde_json::to_value(crate::bots::BotSettings::load(self.state.dir())).unwrap_or_default(),
        })
    }

    pub async fn restore(&self, b: Backup) -> R<RestoreReport> {
        if b.format != FORMAT {
            return Err("That isn't a PrismFlow backup file.".into());
        }
        let mut rep = RestoreReport::default();
        if !b.custom_providers.is_empty() {
            self.state.save_custom_providers(&b.custom_providers).map_err(|e| e.to_string())?;
            crate::providers::set_custom(&b.custom_providers);
            rep.restored.push(format!("{} custom provider(s)", b.custom_providers.len()));
        }
        self.set_settings(b.settings).await?;
        rep.restored.push("Settings and spending limits".into());
        let profile_count = b.profiles.list.len();
        crate::config_engine::atomic_write(&self.state.dir().join("profiles.json"), &serde_json::to_string_pretty(&b.profiles).unwrap_or_default())
            .map_err(|e| e.to_string())?;
        if profile_count > 0 {
            rep.restored.push(format!("{profile_count} profile(s)"));
        }
        let mut st = crate::mcp::load(self.state.dir());
        st.servers = b.mcp_servers;
        crate::mcp::save(self.state.dir(), &st)?;
        rep.restored.push(format!("{} MCP server(s) (press Apply on the Tools page to add them to your apps)", st.servers.len()));
        if let Ok(bots) = serde_json::from_value::<crate::bots::BotSettings>(b.bots) {
            let mut bots = bots;
            // Bots need their tokens again; keep them off until then.
            bots.telegram.enabled = false;
            bots.discord.enabled = false;
            bots.save(self.state.dir())?;
            rep.restored.push("Chat bot settings (turned off until you add the bot tokens)".into());
        }
        for e in b.connections {
            match self.connect(&e.app_id, &e.provider_id, &e.model).await {
                Ok(rec) => {
                    if e.routing != Default::default() {
                        let _ = self.set_routing(&rec.id, e.routing).await;
                    }
                    rep.restored.push(format!("{} → {}", crate::service::app_name(&e.app_id), e.model));
                }
                Err(err) => rep.problems.push(format!("{} → {} ({}): {err}", crate::service::app_name(&e.app_id), e.model, e.provider_id)),
            }
        }
        self.diag.record(crate::diagnostics::Level::Info, "backup.restored", &format!("Restored a backup: {} item(s), {} problem(s).", rep.restored.len(), rep.problems.len()));
        Ok(rep)
    }
}

