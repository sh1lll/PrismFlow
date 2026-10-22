//! Request log, token usage, cost estimates and spending limits.
//!
//! Every request through the gateway is recorded once it finishes: app,
//! provider, model, status, time, tokens, estimated cost, and a plain
//! error. Tokens are read from the response itself (JSON `usage`, or the
//! streamed events that carry it). Prices come from LiteLLM's MIT price
//! table (`catalog/prices.json`); local models are free; unknown prices
//! show as "unknown", never guessed.
//!
//! Files (state dir): `requests.jsonl` (last 5,000 requests) and
//! `usage.json` (daily totals per connection + provider, kept for 400 days).
//! No prompts or replies are stored.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const KEEP_REQUESTS: usize = 5_000;
const KEEP_DAYS: usize = 400;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub ts: u64,
    pub connection: String,
    pub provider: String,
    pub model: String,
    pub path: String,
    pub status: u16,
    pub ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// USD; None when the price isn't known.
    pub cost: Option<f64>,
    /// A plain-language error, when it failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    /// Requests whose price wasn't known (cost excludes them).
    pub unpriced: u64,
}

impl Totals {
    fn add(&mut self, r: &Request) {
        self.requests += 1;
        if r.status >= 400 {
            self.errors += 1;
        }
        self.input_tokens += r.input_tokens;
        self.output_tokens += r.output_tokens;
        match r.cost {
            Some(c) => self.cost += c,
            None if r.status < 400 => self.unpriced += 1,
            None => {}
        }
    }
}

/// date (YYYY-MM-DD) → "conn:<id>" / "prov:<id>" → totals
type Days = BTreeMap<String, BTreeMap<String, Totals>>;

pub struct Store {
    dir: PathBuf,
    lock: Mutex<()>,
}

pub fn day(ts: u64) -> String {
    // Civil date from days since epoch (UTC).
    let z = (ts / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

impl Store {
    pub fn open(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf(), lock: Mutex::new(()) }
    }

    fn days(&self) -> Days {
        std::fs::read_to_string(self.dir.join("usage.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn record(&self, r: Request) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::create_dir_all(&self.dir);
        let log = self.dir.join("requests.jsonl");
        let mut lines: Vec<String> = std::fs::read_to_string(&log).unwrap_or_default().lines().map(String::from).collect();
        lines.push(serde_json::to_string(&r).unwrap_or_default());
        if lines.len() > KEEP_REQUESTS {
            lines.drain(..lines.len() - KEEP_REQUESTS);
        }
        let _ = crate::config_engine::atomic_write(&log, &(lines.join("\n") + "\n"));
        let mut days = self.days();
        let today = days.entry(day(r.ts)).or_default();
        today.entry(format!("conn:{}", r.connection)).or_default().add(&r);
        today.entry(format!("prov:{}", r.provider)).or_default().add(&r);
        while days.len() > KEEP_DAYS {
            let first = days.keys().next().cloned().unwrap_or_default();
            days.remove(&first);
        }
        let _ = crate::config_engine::atomic_write(&self.dir.join("usage.json"), &serde_json::to_string(&days).unwrap_or_default());
    }

    pub fn recent(&self, limit: usize) -> Vec<Request> {
        let s = std::fs::read_to_string(self.dir.join("requests.jsonl")).unwrap_or_default();
        let mut v: Vec<Request> = s.lines().rev().take(limit).filter_map(|l| serde_json::from_str(l).ok()).collect();
        v.truncate(limit);
        v
    }

    /// Daily totals from `since` (YYYY-MM-DD) on.
    pub fn days_since(&self, since: &str) -> Days {
        self.days().into_iter().filter(|(d, _)| d.as_str() >= since).collect()
    }

    /// This calendar month's estimated spend with `provider`, in USD.
    pub fn month_spend(&self, provider: &str, now: u64) -> f64 {
        let month = &day(now)[..7];
        self.days().iter().filter(|(d, _)| d.starts_with(month)).filter_map(|(_, m)| m.get(&format!("prov:{provider}"))).map(|t| t.cost).sum()
    }
}

#[derive(Deserialize)]
struct PriceFile {
    providers: BTreeMap<String, BTreeMap<String, (f64, f64, u64)>>,
}

fn prices() -> &'static BTreeMap<String, BTreeMap<String, (f64, f64, u64)>> {
    static P: OnceLock<BTreeMap<String, BTreeMap<String, (f64, f64, u64)>>> = OnceLock::new();
    P.get_or_init(|| serde_json::from_str::<PriceFile>(include_str!("../catalog/prices.json")).map(|f| f.providers).unwrap_or_default())
}

/// USD per 1M tokens (input, output) and context, for `provider`'s `model`.
pub fn price(provider: &str, model: &str) -> Option<(f64, f64, u64)> {
    if crate::providers::get(provider).is_some_and(|p| p.local) {
        return Some((0.0, 0.0, 0));
    }
    let t = prices().get(provider)?;
    let m = model.trim();
    t.get(m)
        .or_else(|| m.rsplit_once('/').and_then(|(_, tail)| t.get(tail)))
        .or_else(|| t.get(&m.to_ascii_lowercase()))
        .copied()
}

pub fn cost(provider: &str, model: &str, input: u64, output: u64) -> Option<f64> {
    let (i, o, _) = price(provider, model)?;
    Some((input as f64 * i + output as f64 * o) / 1_000_000.0)
}

/// Plain words for a failed status.
pub fn plain_error(status: u16, body: &str) -> String {
    let detail: String = serde_json::from_str::<Value>(body).ok()
        .and_then(|v| v["error"]["message"].as_str().or_else(|| v["message"].as_str()).map(String::from))
        .unwrap_or_default().chars().take(200).collect();
    let what = match status {
        401 | 403 => "The provider rejected the key.",
        402 => "Out of credit, or over the spending limit.",
        404 => "The model or address wasn't found.",
        408 | 504 => "The provider took too long.",
        413 => "The request was too big for the model.",
        429 => "Over the provider's limit. Try again soon.",
        500..=599 => "The provider had a problem.",
        _ => "The request failed.",
    };
    // Limit answers already carry PrismFlow's plain explanation.
    if matches!(status, 402 | 429) && (detail.starts_with("Rate limited") || detail.starts_with("Out of quota") || detail.starts_with("PrismFlow's own limit")) {
        return detail;
    }
    if detail.is_empty() { what.to_string() } else { format!("{what} ({detail})") }
}

/// Reads token counts out of a response as it streams by.
#[derive(Default)]
pub struct Tap {
    pending: String,
    body: String,
    pub input: u64,
    pub output: u64,
}

impl Tap {
    pub fn feed(&mut self, chunk: &[u8]) {
        let s = String::from_utf8_lossy(chunk);
        if self.body.len() < 64 * 1024 {
            self.body.push_str(&s);
        }
        self.pending.push_str(&s);
        while let Some(i) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=i).collect();
            if let Some(data) = line.trim().strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    self.read(&v);
                }
            }
        }
    }

    /// Call at the end: a plain JSON body is read whole.
    pub fn finish(&mut self) {
        if let Ok(v) = serde_json::from_str::<Value>(self.body.trim()) {
            self.read(&v);
        }
    }

    /// The start of the body (for error messages).
    pub fn head(&self) -> &str {
        &self.body
    }

    fn read(&mut self, v: &Value) {
        for u in [&v["usage"], &v["message"]["usage"], &v["response"]["usage"]] {
            if !u.is_object() {
                continue;
            }
            let input = u["input_tokens"].as_u64().or_else(|| u["prompt_tokens"].as_u64()).unwrap_or(0)
                + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
                + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            let output = u["output_tokens"].as_u64().or_else(|| u["completion_tokens"].as_u64()).unwrap_or(0);
            self.input = self.input.max(input);
            self.output = self.output.max(output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_prices() {
        assert_eq!(day(1_792_081_965), "2026-10-15");
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(price("ollama", "anything"), Some((0.0, 0.0, 0)));
        let (i, o, _) = price("anthropic", "claude-haiku-4-5").unwrap();
        assert!(i > 0.0 && o > i);
        assert!((cost("anthropic", "claude-haiku-4-5", 1_000_000, 0).unwrap() - i).abs() < 1e-9);
        assert_eq!(price("anthropic", "no-such-model"), None);
    }

    #[test]
    fn taps_anthropic_openai_and_json() {
        let mut t = Tap::default();
        t.feed(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":120,\"output_tokens\":1}}}\n\n");
        t.feed(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n");
        t.finish();
        assert_eq!((t.input, t.output), (120, 42));
        let mut c = Tap::default();
        c.feed(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":9}}\n\ndata: [DONE]\n");
        assert_eq!((c.input, c.output), (7, 9));
        let mut j = Tap::default();
        j.feed(br#"{"id":"x","usage":{"input_tokens":3,"output_tokens":4}}"#);
        j.finish();
        assert_eq!((j.input, j.output), (3, 4));
    }

    #[test]
    fn store_totals_and_month_spend() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path());
        let now = 1_792_081_965;
        for status in [200, 200, 401] {
            s.record(Request { ts: now, connection: "claude_code:anthropic".into(), provider: "anthropic".into(), model: "m".into(),
                status, input_tokens: 10, output_tokens: 5, cost: (status == 200).then_some(0.5), ..Default::default() });
        }
        assert_eq!(s.recent(10).len(), 3);
        assert_eq!(s.recent(10)[0].status, 401, "newest first");
        let d = s.days_since("2026-10-01");
        let t = &d["2026-10-15"]["prov:anthropic"];
        assert_eq!((t.requests, t.errors, t.input_tokens), (3, 1, 30));
        assert!((s.month_spend("anthropic", now) - 1.0).abs() < 1e-9);
        assert_eq!(plain_error(401, r#"{"error":{"message":"bad key"}}"#), "The provider rejected the key. (bad key)");
    }
}
