//! Chat-item flattening — each `model::Block` becomes its own selectable
//! "chat item" (mirrors what `web/src/components/Chat/BlockRenderer.tsx`
//! does: text / thinking / tool_call render as independent units).
//!
//! `selected_block` (poorly named — it's now an *item* index) refers into
//! the order produced by `flatten`. Streaming buffer items append after
//! all history items, in chronological order.

use crate::model::{Block, Message};

#[derive(Debug, Clone, Copy)]
pub struct ChatItem<'a> {
    pub msg: &'a Message,
    pub block_idx: usize,
    /// First block of its parent message — render the role+timestamp
    /// header above it, but not on subsequent blocks of the same message.
    pub is_first_in_msg: bool,
    /// Belongs to the in-flight streaming buffer (not history).
    pub is_streaming: bool,
}

impl<'a> ChatItem<'a> {
    pub fn block(&self) -> &Block {
        &self.msg.blocks[self.block_idx]
    }
}

pub fn flatten<'a>(history: &'a [Message], streaming: Option<&'a Message>) -> Vec<ChatItem<'a>> {
    let mut out = Vec::with_capacity(count(history, streaming));
    for msg in history {
        for i in 0..msg.blocks.len() {
            out.push(ChatItem {
                msg,
                block_idx: i,
                is_first_in_msg: i == 0,
                is_streaming: false,
            });
        }
    }
    if let Some(s) = streaming {
        for i in 0..s.blocks.len() {
            out.push(ChatItem {
                msg: s,
                block_idx: i,
                is_first_in_msg: i == 0,
                is_streaming: true,
            });
        }
    }
    out
}

pub fn count(history: &[Message], streaming: Option<&Message>) -> usize {
    let h: usize = history.iter().map(|m| m.blocks.len()).sum();
    let s = streaming.map(|m| m.blocks.len()).unwrap_or(0);
    h + s
}

pub fn count_for(history: &[Message]) -> usize {
    history.iter().map(|m| m.blocks.len()).sum()
}
