//! Claude Desktop (third-party inference), guided — never file-edited.
//!
//! Anthropic sanctions third-party inference for Claude Desktop through the
//! app's own Developer Mode → Configure Third-Party Inference flow, which
//! validates and tests the connection itself (COMPATIBILITY-MATRIX, V-2).
//! The app keeps that configuration in its own profile, and the user may be
//! running Claude Desktop while connecting (this very app can be the host),
//! so PrismFlow does not write it. It creates a gateway connection and shows
//! the values to enter; disconnect revokes the token.

use super::AppDetection;
use std::path::PathBuf;
use std::process::Command;

pub const APP_ID: &str = "claude_desktop";
pub const BUNDLE_ID: &str = "com.anthropic.claudefordesktop";

pub fn find_app() -> Option<PathBuf> {
    ["/Applications/Claude.app"]
        .iter()
        .map(PathBuf::from)
        .chain(dirs::home_dir().map(|h| h.join("Applications/Claude.app")))
        .find(|p| {
            Command::new("/usr/bin/defaults")
                .args(["read", &p.join("Contents/Info").display().to_string(), "CFBundleIdentifier"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == BUNDLE_ID)
        })
}

pub fn detect() -> AppDetection {
    let app = find_app();
    let version = app.as_ref().and_then(|p| {
        Command::new("/usr/bin/defaults")
            .args(["read", &p.join("Contents/Info").display().to_string(), "CFBundleShortVersionString"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    AppDetection {
        app_id: APP_ID.into(),
        display_name: "Claude Desktop".into(),
        installed: app.is_some(),
        path: app.map(|p| p.display().to_string()),
        version,
        version_verified: false,
        notes: vec![
            "Guided setup: PrismFlow shows the gateway address and a token to enter in Claude Desktop's own Developer Mode → Configure Third-Party Inference screen. It does not edit Claude Desktop's files.".into(),
            "Switching Claude Desktop to third-party inference signs it out of your Claude account mode until you switch back in the same screen.".into(),
        ],
    }
}

/// What the user enters in Claude Desktop. The token is the connection's
/// revocable local token, valid only on this machine's loopback gateway.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GuidedSetup {
    pub gateway_url: String,
    pub auth_scheme: &'static str,
    pub token: String,
    pub model: String,
    pub steps: Vec<String>,
}

pub fn guided_setup(gateway_url: &str, token: String, model: &str) -> GuidedSetup {
    GuidedSetup {
        gateway_url: gateway_url.into(),
        auth_scheme: "Bearer",
        token,
        model: model.into(),
        steps: vec![
            "Open Claude Desktop and enable Developer Mode (Help → Troubleshooting → Enable Developer Mode).".into(),
            "Open Developer → Configure Third-Party Inference and choose Gateway.".into(),
            format!("Set the gateway base URL to {gateway_url}."),
            "Paste the token below as the API key (Bearer).".into(),
            format!("Use model {model} (PrismFlow routes whatever model Claude Desktop asks for to this connection's model)."),
            "Click Test connection, then Save, and restart Claude Desktop when asked.".into(),
            "To undo: choose your Claude account again in the same screen, then Disconnect here to revoke the token.".into(),
        ],
    }
}
