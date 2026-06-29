//! Task — `/api/tasks` items.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Populated only by `GET /api/tasks/{id}` (the detail endpoint).
    #[serde(default)]
    pub content: Option<String>,
    /// Which Nerve instance this task came from. Stamped at ingest.
    #[serde(default, skip)]
    pub instance: crate::instance::InstanceId,
}
