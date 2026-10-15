//! Gateway performance harness for the SPEC §21.4 budgets:
//! - added time-to-first-token ≤ 150 ms at p95,
//! - streaming throughput degradation ≤ 5 %,
//! - idle memory (this process + engine child) ≤ 250 MB.
//!
//! The upstream is a paced streaming server (first token after 200 ms, then
//! one chunk every 5 ms), so the comparison reflects a realistic provider
//! instead of an in-memory mock that answers in microseconds. Each request is
//! measured direct and through the broker on identical traffic.
//!
//! Run: `PRISMFLOW_PERF=1 cargo test --release --test perf`
//! Writes `perf-report.json` next to the crate and exits non-zero on a breach.

use axum::body::Body;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use prismflow_lib::gateway::supervisor::UpstreamSpec;
use prismflow_lib::gateway::{tokens, Binding, Broker};
use std::time::{Duration, Instant};

const FIRST_TOKEN_DELAY: Duration = Duration::from_millis(200);
const CHUNK_INTERVAL: Duration = Duration::from_millis(5);
const CHUNKS: usize = 200;
const SAMPLES: usize = 40;
const MODEL: &str = "perf-model";

const BUDGET_ADDED_TTFT_P95_MS: f64 = 150.0;
const BUDGET_THROUGHPUT_LOSS: f64 = 0.05;
const BUDGET_IDLE_MB: f64 = 250.0;

fn chunk(text: &str, finish: Option<&str>) -> String {
    let c = serde_json::json!({
        "id": "p", "object": "chat.completion.chunk", "created": 1, "model": MODEL,
        "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": finish}]
    });
    format!("data: {c}\n\n")
}

async fn paced_stream() -> Response {
    let stream = async_stream(CHUNKS);
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn async_stream(n: usize) -> impl futures_util::Stream<Item = Result<String, std::io::Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(16);
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_TOKEN_DELAY).await;
        for i in 0..n {
            let text = format!("token{i:04} lorem ipsum dolor sit amet ");
            if tx.send(Ok(chunk(&text, None))).await.is_err() {
                return;
            }
            tokio::time::sleep(CHUNK_INTERVAL).await;
        }
        let _ = tx.send(Ok(chunk("", Some("stop")))).await;
        let _ = tx.send(Ok("data: [DONE]\n\n".into())).await;
    });
    tokio_stream_from(rx)
}

fn tokio_stream_from(
    mut rx: tokio::sync::mpsc::Receiver<Result<String, std::io::Error>>,
) -> impl futures_util::Stream<Item = Result<String, std::io::Error>> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// Time to the first byte that carries model text, and total time.
async fn measure(client: &reqwest::Client, url: &str, token: Option<&str>) -> (Duration, Duration, usize) {
    let mut body = serde_json::json!({"model": MODEL, "stream": true, "messages": [{"role": "user", "content": "go"}]});
    if url.ends_with("/v1/messages") {
        body["max_tokens"] = 4096.into();
    }
    let mut req = client.post(url).json(&body);
    if let Some(t) = token {
        req = req.header("x-api-key", t);
    }
    let start = Instant::now();
    let mut resp = req.send().await.expect("request");
    assert!(resp.status().is_success(), "status {}", resp.status());
    let mut first = None;
    let mut bytes = 0usize;
    while let Some(c) = resp.chunk().await.expect("chunk") {
        if first.is_none() && String::from_utf8_lossy(&c).contains("token0000") {
            first = Some(start.elapsed());
        }
        bytes += c.len();
    }
    (first.expect("saw first token"), start.elapsed(), bytes)
}

fn p95(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * 0.95).ceil() as usize).saturating_sub(1).min(v.len() - 1)]
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn rss_mb(pid: u32) -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn children(pid: u32) -> Vec<u32> {
    std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

fn main() {
    if std::env::var("PRISMFLOW_PERF").as_deref() != Ok("1") {
        println!("perf: skipped (set PRISMFLOW_PERF=1; use --release for representative numbers)");
        return;
    }
    let report = tokio::runtime::Runtime::new().unwrap().block_on(run());
    let text = serde_json::to_string_pretty(&report).unwrap();
    println!("{text}");
    std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/perf-report.json"), &text).unwrap();
    assert!(report["pass"].as_bool().unwrap(), "performance budget breached");
}

async fn run() -> serde_json::Value {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let up = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let app = Router::new().route("/v1/chat/completions", post(paced_stream));
        axum::serve(listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::new(0, dir.path().to_path_buf(), None);
    let token = tokens::generate();
    let spec = UpstreamSpec {
        provider_id: "perf".into(),
        engine_type: "openai_chat_completions",
        base_url: format!("http://127.0.0.1:{up}/v1"),
        models: vec![MODEL.into()],
        key: "perf-upstream-key".into(), codex_oauth: false, headers: vec![], extra_keys: vec![] };
    let binding = Binding { connection_id: "perf".into(), provider_id: "perf".into(), ..Default::default() };
    broker.start(vec![spec], vec![(token.clone(), binding)]).await.expect("broker starts");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let direct_url = format!("http://127.0.0.1:{up}/v1/chat/completions");
    let gw_url = format!("{}/v1/chat/completions", broker.url());
    let mut scenarios = serde_json::Map::new();
    for (name, gw) in [("chat_front", gw_url.clone()), ("messages_front_translated", format!("{}/v1/messages", broker.url()))] {
        scenarios.insert(name.into(), scenario(&client, &direct_url, &gw, &token).await);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let me = std::process::id();
    let engine: f64 = children(me).into_iter().map(rss_mb).sum();
    let own = rss_mb(me);
    let idle = own + engine;
    broker.stop().await;
    let mem_ok = idle <= BUDGET_IDLE_MB;
    let pass = mem_ok && scenarios.values().all(|s| s["pass"].as_bool() == Some(true));
    serde_json::json!({
        "spec": "SPEC §21.4",
        "samples": SAMPLES,
        "upstream": {"first_token_ms": FIRST_TOKEN_DELAY.as_millis(), "chunk_interval_ms": CHUNK_INTERVAL.as_millis(), "chunks": CHUNKS},
        "scenarios": scenarios,
        "memory_mb": {"process": own, "engine": engine, "total": idle, "budget": BUDGET_IDLE_MB,
            "note": "process = this harness + broker + tokio; engine = the node child"},
        "pass": pass
    })
}

async fn scenario(client: &reqwest::Client, direct_url: &str, gw_url: &str, token: &str) -> serde_json::Value {

    // Warm-up (engine JIT, connection pools).
    for _ in 0..3 {
        measure(client, direct_url, None).await;
        measure(client, gw_url, Some(token)).await;
    }
    let (mut d_ttft, mut g_ttft, mut d_total, mut g_total) = (vec![], vec![], vec![], vec![]);
    let (mut d_bytes, mut g_bytes) = (0, 0);
    for _ in 0..SAMPLES {
        let (f, t, b) = measure(client, direct_url, None).await;
        d_ttft.push(f.as_secs_f64() * 1000.0);
        d_total.push(t.as_secs_f64() * 1000.0);
        d_bytes = b;
        let (f, t, b) = measure(client, gw_url, Some(token)).await;
        g_ttft.push(f.as_secs_f64() * 1000.0);
        g_total.push(t.as_secs_f64() * 1000.0);
        g_bytes = b;
    }
    let added: Vec<f64> = g_ttft.iter().zip(&d_ttft).map(|(g, d)| g - d).collect();
    let added_p95 = p95(added.clone());
    let loss = (median(g_total.clone()) - median(d_total.clone())) / median(d_total.clone());
    serde_json::json!({
        "ttft_ms": {"direct_median": median(d_ttft), "gateway_median": median(g_ttft), "added_median": median(added), "added_p95": added_p95, "budget_p95": BUDGET_ADDED_TTFT_P95_MS},
        "stream_total_ms": {"direct_median": median(d_total), "gateway_median": median(g_total), "throughput_loss": loss, "budget": BUDGET_THROUGHPUT_LOSS},
        "bytes": {"direct": d_bytes, "gateway": g_bytes},
        "pass": added_p95 <= BUDGET_ADDED_TTFT_P95_MS && loss <= BUDGET_THROUGHPUT_LOSS
    })
}
