//! Telling a short rate limit ("slow down, try again in 40 s") apart from a
//! used-up quota or plan ("out of credit until it resets") and from
//! PrismFlow's own limits, so the app and the user see which one it is.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// Too many requests right now; it lifts by itself shortly.
    RateLimit,
    /// The plan's or account's allowance (credits, daily/monthly use) is used up.
    Quota,
    /// PrismFlow's own requests-per-minute limit (Settings).
    PrismflowLimit,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitState {
    pub provider_id: String,
    pub kind: LimitKind,
    /// What the provider said, shortened.
    pub provider_message: String,
    /// Unix seconds when it should lift, when known.
    pub until: Option<u64>,
    /// Unix seconds when it was seen.
    pub at: u64,
}

/// The provider's own error, out of the engine's "all target providers
/// failed" wrapper (last attempt), or the body itself.
fn provider_error(body: &Value) -> (&Value, Option<u64>) {
    let details = body["error"]["attempts"].as_array().and_then(|a| a.last()).map(|a| &a["details"]).unwrap_or(body);
    let retry = details["prismflow_retry_after"].as_f64().map(|s| s.ceil() as u64);
    let err = if details["error"].is_object() { &details["error"] } else { details };
    (err, retry)
}

/// "try again in 37s", "retry after 2 minutes", "Please retry in 1m30s".
fn wait_in_text(text: &str) -> Option<u64> {
    let t = text.to_lowercase();
    let at = ["try again in", "retry in", "retry after", "wait", "reset in", "resets in"].iter().find_map(|k| t.find(k).map(|i| i + k.len()))?;
    let rest: String = t[at..].chars().take(30).collect();
    let mut total = 0f64;
    let mut num = String::new();
    let mut found = false;
    let mut chars = rest.trim_start().chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || (c == '.' && !num.is_empty()) {
            num.push(c);
            continue;
        }
        if num.is_empty() {
            if c == ' ' { continue; } else { break; }
        }
        let n: f64 = num.parse().unwrap_or(0.0);
        num.clear();
        let mut unit = String::from(c);
        while let Some(&d) = chars.peek() {
            if d.is_ascii_alphabetic() { unit.push(d); chars.next(); } else { break; }
        }
        let unit = unit.trim().to_string();
        let mult = if unit.is_empty() { while chars.peek() == Some(&' ') { chars.next(); } let mut u = String::new(); while let Some(&d) = chars.peek() { if d.is_ascii_alphabetic() { u.push(d); chars.next(); } else { break; } } unit_secs(&u) } else { unit_secs(&unit) };
        match mult {
            Some(m) => { total += n * m; found = true; }
            None => break,
        }
    }
    found.then(|| total.ceil() as u64)
}

fn unit_secs(u: &str) -> Option<f64> {
    match u {
        "ms" => Some(0.001),
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1.0),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60.0),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(3600.0),
        _ => None,
    }
}

const QUOTA_WORDS: &[&str] = &[
    "quota", "insufficient", "billing", "credit", "usage limit", "usage_limit", "daily limit", "per day",
    "monthly", "plan limit", "exceeded your current", "upgrade", "subscription", "balance",
];

/// What kind of limit a 429/402 answer is, how long to wait if known, and
/// the provider's own words.
pub fn classify(status: u16, body: &Value, raw: &str) -> Option<(LimitKind, Option<u64>, String)> {
    if status != 429 && status != 402 {
        return None;
    }
    let (err, retry) = provider_error(body);
    let mut message = err["message"].as_str().or_else(|| body["message"].as_str()).unwrap_or("").trim().to_string();
    if message.is_empty() && !raw.trim_start().starts_with('{') {
        message = raw.trim().chars().take(200).collect();
    }
    let code = format!("{} {}", err["code"].as_str().unwrap_or(""), err["type"].as_str().unwrap_or("")).to_lowercase();
    let lower = format!("{code} {}", message.to_lowercase());
    let quota = status == 402 || QUOTA_WORDS.iter().any(|w| lower.contains(w));
    let wait = retry.or_else(|| wait_in_text(&message));
    let kind = if quota { LimitKind::Quota } else { LimitKind::RateLimit };
    Some((kind, wait, message.chars().take(200).collect()))
}

/// The message the app (and the user) sees.
pub fn plain(kind: LimitKind, provider: &str, wait: Option<u64>, provider_message: &str) -> String {
    let said = if provider_message.is_empty() { String::new() } else { format!(" {provider} said: \"{provider_message}\"") };
    match kind {
        LimitKind::RateLimit => match wait {
            Some(s) => format!("Rate limited by {provider}: too many requests right now. Try again in {}.{said}", human(s)),
            None => format!("Rate limited by {provider}: too many requests right now. Try again in a minute.{said}"),
        },
        LimitKind::Quota => match wait {
            Some(s) => format!("Out of quota at {provider}: your plan's or account's allowance is used up. It resets in {}.{said}", human(s)),
            None => format!("Out of quota at {provider}: your plan's or account's allowance is used up. It works again when it resets or you add credit.{said}"),
        },
        LimitKind::PrismflowLimit => "PrismFlow's own limit: this app sent more requests per minute than allowed in PrismFlow's Settings. Try again shortly, or raise the limit.".into(),
    }
}

/// 37 → "37 s", 90 → "2 min", 7200 → "2 h".
pub fn human(secs: u64) -> String {
    match secs {
        0..=89 => format!("{} s", secs.max(1)),
        90..=3599 => format!("{} min", (secs + 30) / 60),
        _ => format!("{} h", (secs + 1800) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engine(details: Value) -> Value {
        json!({ "error": { "message": "All target providers failed.", "attempts": [{ "status": 429, "details": details }] } })
    }

    #[test]
    fn quota_and_rate_limits_are_told_apart() {
        let q = engine(json!({ "error": { "message": "You exceeded your current quota, please check your plan and billing details.", "code": "insufficient_quota" }, "prismflow_retry_after": 37 }));
        assert_eq!(classify(429, &q, "").map(|c| (c.0, c.1)), Some((LimitKind::Quota, Some(37))));
        let r = engine(json!({ "error": { "message": "Rate limit reached for requests. Please try again in 1m30s." } }));
        assert_eq!(classify(429, &r, "").map(|c| (c.0, c.1)), Some((LimitKind::RateLimit, Some(90))));
        let plain_429 = engine(json!({ "message": "Too Many Requests" }));
        assert_eq!(classify(429, &plain_429, "").map(|c| (c.0, c.1)), Some((LimitKind::RateLimit, None)));
        assert_eq!(classify(402, &json!({}), "").map(|c| c.0), Some(LimitKind::Quota));
        assert!(classify(500, &json!({}), "").is_none());
    }

    #[test]
    fn waits_are_read_from_words() {
        assert_eq!(wait_in_text("Please try again in 20s."), Some(20));
        assert_eq!(wait_in_text("retry after 2 minutes"), Some(120));
        assert_eq!(wait_in_text("try again in 1.5 seconds"), Some(2));
        assert_eq!(wait_in_text("no hint here"), None);
    }

    #[test]
    fn messages_say_which_limit_it_is() {
        assert!(plain(LimitKind::RateLimit, "Groq", Some(37), "").starts_with("Rate limited by Groq") );
        assert!(plain(LimitKind::RateLimit, "Groq", Some(37), "").contains("Try again in 37 s"));
        assert!(plain(LimitKind::Quota, "NVIDIA NIM", None, "daily limit").contains("Out of quota at NVIDIA NIM"));
        assert_eq!(human(3600), "1 h");
    }
}
