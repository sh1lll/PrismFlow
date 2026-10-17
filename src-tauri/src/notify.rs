//! System notifications for things the user should know without opening
//! PrismFlow: a rejected key, a provider failing over to a backup, a
//! sign-in that ran out. Built from Activity events; each kind is shown at
//! most once every 10 minutes so a broken provider doesn't spam.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

static LAST: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

pub fn send<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

/// A plain notification for an Activity event, or None if it isn't worth one.
pub fn for_event(code: &str, message: &str) -> Option<(String, String)> {
    let m = message;
    Some(match code {
        "broker.request" if m.contains("→ 401") || m.contains("→ 403") => ("A key was rejected".into(), "A provider turned down its key. Check it on the Providers page.".into()),
        "broker.request" if m.contains("→ 429") => ("A provider is busy".into(), "A provider said you're over its limit. Try again soon, or add a backup model.".into()),
        "broker.fallback" => ("Using a backup model".into(), "Your main model failed, so PrismFlow used your backup.".into()),
        // Offline is the usual cause, and PrismFlow retries every minute.
        "gateway.copilot" | "gateway.copilot_refresh" if m.contains("didn't answer in time") || m.contains("trying again") => ("GitHub Copilot is unreachable".into(), "PrismFlow keeps trying. If you're online and it still fails, run `copilot login`.".into()),
        "gateway.copilot" | "gateway.copilot_refresh" => ("GitHub Copilot needs attention".into(), "Copilot isn't available. Run `copilot login`, then reopen PrismFlow.".into()),
        "chatgpt.renew" if m.contains("Sign in again") => ("ChatGPT sign-in ended".into(), "Open PrismFlow → Providers and sign in to ChatGPT again.".into()),
        "gateway.refresh" => ("Google Cloud sign-in expired".into(), "Run `gcloud auth login` to keep using Vertex AI.".into()),
        "gateway.startup" | "connect.gateway" => ("PrismFlow's background service stopped".into(), "Connected apps can't reach their models. Open PrismFlow to restart it.".into()),
        "usage.budget" => ("Spending limit reached".into(), m.to_string()),
        _ => return None,
    })
}

/// True when a notification of this kind wasn't shown in the last 10 minutes.
pub fn due(kind: &str) -> bool {
    let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    if map.get(kind).is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
        return false;
    }
    map.insert(kind.to_string(), Instant::now());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_events_and_throttles() {
        assert!(for_event("broker.request", "claude_code:openrouter POST /v1/messages → 401 after 120 ms").unwrap().0.contains("rejected"));
        assert!(for_event("broker.request", "x → 500 after 1 ms").is_none());
        assert!(for_event("connect.applied", "Wrote …").is_none());
        assert!(due("test-kind"));
        assert!(!due("test-kind"));
    }
}
