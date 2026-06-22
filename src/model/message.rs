//! Persisted message + block model.
//!
//! Mirrors `nerve/db/messages.py`: one row = one whole turn,
//! `blocks` is a JSON array of discriminated blocks.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub session_id: String,
    pub role: Role,
    /// `blocks` may be `null` for legacy rows where only `content` was written.
    /// Treat null as an empty Vec; `Message::hydrate` will synthesize a Text
    /// block from the legacy `content` column.
    #[serde(default, deserialize_with = "null_to_default")]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    /// SQLite-formatted ISO timestamp string (no TZ). v0.2 parses to chrono.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

fn null_to_default<'de, T, D>(d: D) -> std::result::Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(|o| o.unwrap_or_default())
}

impl Message {
    pub fn new_user(session_id: String, content: String) -> Self {
        Self {
            id: None,
            session_id,
            role: Role::User,
            blocks: vec![Block::Text {
                content: content.clone(),
            }],
            content: Some(content),
            thinking: None,
            created_at: Some(now_string()),
            channel: Some("tui".into()),
        }
    }

    pub fn new_streaming_assistant(session_id: String) -> Self {
        Self {
            id: None,
            session_id,
            role: Role::Assistant,
            blocks: Vec::new(),
            content: None,
            thinking: None,
            created_at: Some(now_string()),
            channel: Some("tui".into()),
        }
    }

    /// Hydrate after fetching from /api/sessions/{id}/messages.
    /// The legacy `content` column may be the source of truth for old rows
    /// without a `blocks` array — synthesize a single Text block in that case.
    pub fn hydrate(mut self) -> Self {
        if self.blocks.is_empty()
            && let Some(text) = self.content.as_ref()
            && !text.is_empty()
        {
            self.blocks.push(Block::Text {
                content: text.clone(),
            });
        }
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        content: String,
    },
    Thinking {
        content: String,
    },
    ToolCall(ToolCall),
    File {
        #[serde(default)]
        url: String,
        #[serde(default)]
        filename: String,
        #[serde(default)]
        media_type: Option<String>,
    },
    Image {
        #[serde(default)]
        url: String,
        #[serde(default)]
        filename: String,
        #[serde(default)]
        media_type: Option<String>,
    },
    /// Any block type the TUI doesn't model (`auto`, `wakeup`, future kinds).
    /// Caught here so one unknown block can't fail the whole history parse.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub tool_use_id: String,
    pub tool: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub is_error: bool,
    #[serde(default, deserialize_with = "null_to_default")]
    pub status: ToolCallStatus,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    #[default]
    Streaming,
    Complete,
}

fn now_string() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::{Block, Message};

    #[test]
    fn history_with_attachment_and_marker_blocks_parses() {
        // A turn mixing text + file + image + an unmodeled marker (`auto`)
        // must not fail the whole parse.
        let json = r#"[{
            "id": 1, "session_id": "s", "role": "user",
            "blocks": [
                {"type": "text", "content": "hi"},
                {"type": "file", "url": "/api/files/x", "filename": "a.pdf", "media_type": "application/pdf"},
                {"type": "image", "url": "/api/files/y", "filename": "b.jpg", "media_type": "image/jpeg"},
                {"type": "auto"}
            ]
        }]"#;
        let msgs: Vec<Message> = serde_json::from_str(json).expect("mixed blocks must parse");
        let blocks = &msgs[0].blocks;
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], Block::Text { .. }));
        assert!(matches!(&blocks[1], Block::File { filename, .. } if filename == "a.pdf"));
        assert!(matches!(&blocks[2], Block::Image { filename, .. } if filename == "b.jpg"));
        assert!(matches!(blocks[3], Block::Unknown));
    }

    #[test]
    fn unknown_block_type_does_not_fail_parse() {
        let json = r#"[{"id":1,"session_id":"s","role":"assistant",
            "blocks":[{"type":"some_future_kind","data":42}]}]"#;
        let msgs: Vec<Message> = serde_json::from_str(json).unwrap();
        assert!(matches!(msgs[0].blocks[0], Block::Unknown));
    }
}
