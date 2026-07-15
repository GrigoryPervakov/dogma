//! Session — what the sidebar shows.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    // SQLite stores BOOLEAN as INTEGER 0/1 — accept both via serde_json::Value
    // would require a custom deserializer. The aiosqlite Row converts it to
    // an `int` so the JSON arrives as 0/1, not a bool. Default to false on
    // any deserialization failure by using a flexible deserializer.
    #[serde(default, deserialize_with = "de_flex_bool")]
    pub starred: bool,
    // Nerve serializes SQLite TIMESTAMPs as plain strings ("2026-04-29 ...")
    // without timezone. Keep as Option<String> for v0.1 — v0.2 parses to chrono.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default, deserialize_with = "de_flex_bool")]
    pub is_running: bool,
    #[serde(default)]
    pub status: SessionStatus,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub message_count: u32,
    #[serde(default)]
    pub total_cost_usd: f64,
    /// Resolved model bound to the session's SDK client (set at connect time).
    #[serde(default)]
    pub model: Option<String>,
    /// Agent backend serving this session (`claude` / `codex`). Sticky —
    /// chosen at creation, never changes.
    #[serde(default)]
    pub backend: Option<String>,
    /// Which Nerve instance this session came from. Stamped at ingest, never
    /// on the wire.
    #[serde(default, skip)]
    pub instance: crate::instance::InstanceId,
}

fn de_flex_bool<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Bool(b) => Ok(b),
        serde_json::Value::Number(n) => Ok(n.as_i64().map(|i| i != 0).unwrap_or(false)),
        serde_json::Value::Null => Ok(false),
        other => Err(D::Error::custom(format!("expected bool/int, got {other}"))),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Active,
    Idle,
    Paused,
    Stopped,
    Error,
    Archived,
    #[serde(other)]
    Other,
}
