//! End-to-end smoke test against a real running Nerve daemon.
//!
//! This test is **gated by `#[ignore]`** so it does not run on default
//! `cargo test`. To run it:
//!
//! ```bash
//! DOGMA_TEST_PASSWORD=<password> \
//!   cargo test --test smoke -- --ignored --nocapture
//! ```
//!
//! Configuration via env vars:
//! - `DOGMA_TEST_PASSWORD` (required) — Nerve daemon password.
//! - `DOGMA_TEST_URL` (optional; default `http://127.0.0.1:8900`).
//! - `DOGMA_TEST_LIMIT` (optional; default 10) — number of recent sessions to
//!   probe.
//!
//! What it does:
//! 1. Authenticates against `/api/auth/login`.
//! 2. Lists sessions via `/api/sessions` and decodes them through the same
//!    `Session` type the TUI uses. Aborts on decode failure with the raw
//!    response saved to `~/.dogma/smoke-failures/sessions.json`.
//! 3. For each of the top N sessions (by API order, which is `updated_at`
//!    DESC), fetches `/api/sessions/{id}/messages?limit=500` and decodes
//!    through the same `Message` type. On decode failure, saves the raw
//!    JSON to `~/.dogma/smoke-failures/<session_id>.json` and continues —
//!    final assertion fails with the list of broken sessions.
//!
//! Logging: structured tracing output is appended to `~/.dogma/smoke.log`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use dogma::config;
use dogma::model::{Message, Session};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Debug, Deserialize)]
struct LoginResp {
    token: String,
}

#[derive(Debug, Deserialize)]
struct SessionsResp {
    sessions: Vec<Session>,
}

#[derive(Debug, Deserialize)]
struct MessagesResp {
    messages: Vec<Message>,
}

fn init_smoke_logging() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let path = config::smoke_log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open smoke log file");
        let filter = EnvFilter::try_from_env("RUST_LOG")
            .unwrap_or_else(|_| EnvFilter::new("debug,hyper=info,reqwest=info"));
        let file_layer = fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_target(false);
        let stderr_layer = fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_target(false);
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(stderr_layer)
            .init();
        info!(log = %path.display(), "smoke log initialized");
    });
}

#[tokio::test]
#[ignore = "requires running Nerve daemon at DOGMA_TEST_URL with DOGMA_TEST_PASSWORD"]
async fn smoke_decode_recent_sessions() {
    init_smoke_logging();

    let url = std::env::var("DOGMA_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:8900".into());
    let password = std::env::var("DOGMA_TEST_PASSWORD")
        .expect("DOGMA_TEST_PASSWORD env var is required to run the smoke test");
    let limit: usize = std::env::var("DOGMA_TEST_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    info!(server = %url, limit, "starting smoke test");

    if let Err(e) = run(&url, &password, limit).await {
        error!(error = %format!("{e:#}"), "smoke FAILED");
        panic!("{e:#}");
    }
    info!("smoke OK");
}

async fn run(url: &str, password: &str, limit: usize) -> Result<()> {
    let client = Client::builder()
        .user_agent("dogma-smoke")
        .timeout(Duration::from_secs(30))
        .build()
        .context("build http client")?;

    // ---- 1. Auth ----
    let token = login(&client, url, password).await?;
    info!("authenticated");

    // ---- 2. List sessions, decode through the Session type ----
    let sessions = fetch_sessions(&client, url, &token).await?;
    info!(count = sessions.len(), "decoded sessions list");

    // ---- 3. Fetch messages for the top N sessions ----
    let top: Vec<&Session> = sessions.iter().take(limit).collect();
    info!(count = top.len(), "probing recent sessions");

    let mut failures: Vec<(String, String)> = Vec::new();

    for s in top {
        let id = s.id.clone();
        let title = s.title.clone().unwrap_or_else(|| "(untitled)".into());
        match fetch_messages(&client, url, &token, &id).await {
            Ok(n) => {
                info!(session = %id, %title, messages = n, "ok");
            }
            Err(e) => {
                let msg = format!("{e:#}");
                error!(session = %id, %title, error = %msg, "decode FAILED");
                failures.push((id, msg));
            }
        }
    }

    if !failures.is_empty() {
        warn!(broken = failures.len(), "some sessions failed to decode");
        bail!(
            "decode failures ({}):\n{}",
            failures.len(),
            failures
                .iter()
                .map(|(id, e)| format!("  - {id}: {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

async fn login(client: &Client, url: &str, password: &str) -> Result<String> {
    let endpoint = format!("{}/api/auth/login", url.trim_end_matches('/'));
    let resp = client
        .post(&endpoint)
        .json(&serde_json::json!({ "password": password }))
        .send()
        .await
        .with_context(|| format!("POST {endpoint}"))?;
    let status = resp.status();
    let body = resp.text().await.context("read login body")?;
    if !status.is_success() {
        bail!("login failed: HTTP {status} — {body}");
    }
    let parsed: LoginResp =
        serde_json::from_str(&body).with_context(|| format!("parse login response: {body}"))?;
    Ok(parsed.token)
}

async fn fetch_sessions(client: &Client, url: &str, token: &str) -> Result<Vec<Session>> {
    let endpoint = format!("{}/api/sessions", url.trim_end_matches('/'));
    let raw = get_text(client, &endpoint, token).await?;
    match serde_json::from_str::<SessionsResp>(&raw) {
        Ok(parsed) => Ok(parsed.sessions),
        Err(e) => {
            let path = config::smoke_failures_dir().join("sessions.json");
            let _ = std::fs::write(&path, &raw);
            error!(saved = %path.display(), "raw sessions response saved for inspection");
            log_decode_context(&raw, &e);
            bail!("decode /api/sessions failed: {e}");
        }
    }
}

async fn fetch_messages(
    client: &Client,
    url: &str,
    token: &str,
    session_id: &str,
) -> Result<usize> {
    let endpoint = format!(
        "{}/api/sessions/{}/messages?limit=500",
        url.trim_end_matches('/'),
        urlencode(session_id),
    );
    let raw = get_text(client, &endpoint, token).await?;

    // First, parse as Value to confirm the wire shape is JSON. If THAT fails,
    // we have a server-side problem, not a client decoder problem.
    if let Err(e) = serde_json::from_str::<Value>(&raw) {
        let path = save_failure_dump(session_id, &raw, "raw");
        bail!(
            "response is not valid JSON ({}); saved to {}",
            e,
            path.display()
        );
    }

    match serde_json::from_str::<MessagesResp>(&raw) {
        Ok(parsed) => Ok(parsed.messages.len()),
        Err(e) => {
            let path = save_failure_dump(session_id, &raw, "decode");
            log_decode_context(&raw, &e);
            bail!("decode failed: {e}; raw saved to {}", path.display());
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct MessagesRespMin {
    messages: Vec<Message>,
}

async fn get_text(client: &Client, endpoint: &str, token: &str) -> Result<String> {
    let resp = client
        .get(endpoint)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("GET {endpoint}"))?;
    let status = resp.status();
    let body = resp.text().await.context("read response body")?;
    if !status.is_success() {
        bail!("HTTP {status} for {endpoint}");
    }
    Ok(body)
}

fn save_failure_dump(session_id: &str, raw: &str, tag: &str) -> std::path::PathBuf {
    let safe = session_id.replace(
        |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_',
        "_",
    );
    let path = config::smoke_failures_dir().join(format!("{safe}.{tag}.json"));
    if let Err(e) = std::fs::write(&path, raw) {
        error!(?e, path = %path.display(), "failed to write failure dump");
    } else {
        warn!(saved = %path.display(), "raw response saved for inspection");
    }
    path
}

/// When serde_json reports "at line N column C", log a 200-byte window of the
/// raw text around that position so the failing field is human-eyeballable
/// without opening the saved file.
fn log_decode_context(raw: &str, err: &serde_json::Error) {
    let line = err.line();
    let col = err.column();
    if line == 0 || col == 0 {
        return;
    }
    let abs = byte_offset(raw, line, col);
    let from = abs.saturating_sub(80);
    let to = (abs + 120).min(raw.len());
    let snippet = &raw[from..to];
    warn!(
        line,
        col,
        abs,
        byte_window = format!("[{from}..{to}]"),
        "decode-fail context: …{}…",
        snippet.replace('\n', "\\n"),
    );
}

fn byte_offset(s: &str, line: usize, col: usize) -> usize {
    let mut cur_line = 1usize;
    let mut cur_col = 1usize;
    for (i, ch) in s.char_indices() {
        if cur_line == line && cur_col == col {
            return i;
        }
        if ch == '\n' {
            cur_line += 1;
            cur_col = 1;
        } else {
            cur_col += 1;
        }
    }
    s.len()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(ch),
            _ => {
                let mut buf = [0u8; 4];
                for &b in ch.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}
