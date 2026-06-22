//! Plan — `/api/plans` items.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Plan content (markdown). Filled by detail endpoint.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub feedback: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
}
