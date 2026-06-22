//! Standalone height estimators — used by both the renderer and the
//! viewport-fitting logic. Item-level (block-per-card) layout.

use crate::view::chat::blocks::{compact_content_lines, content_budget, is_collapsible};
use crate::view::chat::items::ChatItem;

/// Total cells the chat item occupies in the message list at the given inner
/// width. `max_block_h` is the half-window cap used to truncate long text; it
/// must match what `render_item` is handed so viewport math and rendering agree.
///
/// Tool/thinking blocks collapse to a single header line (no box).
pub fn item_height(item: &ChatItem<'_>, inner_w: u16, max_block_h: u16) -> u16 {
    let iw = inner_w.max(1);

    // Collapsed tool/thinking blocks render as one borderless header line.
    if is_collapsible(item.block()) {
        return 1;
    }

    // Block content lines, capped the same way render_item caps them.
    let max_rows = content_budget(max_block_h, item.is_streaming);
    let lines = compact_content_lines(item.block(), item.is_streaming, iw, max_rows);
    let w = iw as usize;
    let mut total: u32 = 2; // top + bottom borders
    for l in &lines {
        let n: usize = l
            .spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
            .max(1);
        total = total.saturating_add(n.div_ceil(w) as u32);
    }
    // 1-line gap between cards so they don't visually merge.
    total = total.saturating_add(1);
    total.min(u16::MAX as u32) as u16
}

#[cfg(test)]
mod tests {
    use super::item_height;
    use crate::model::{Block, Message, ToolCall, ToolCallStatus};
    use crate::view::chat::items::ChatItem;

    fn msg_with(blocks: Vec<Block>) -> Message {
        let mut m = Message::new_streaming_assistant("s".into());
        m.blocks = blocks;
        m
    }

    fn long_text() -> Block {
        Block::Text {
            content: (0..50)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn collapsed_tool_block_is_one_row() {
        let m = msg_with(vec![Block::ToolCall(ToolCall {
            tool_use_id: "t".into(),
            tool: "Bash".into(),
            input: serde_json::json!({ "command": "ls -la" }),
            result: Some("ok".into()),
            is_error: false,
            status: ToolCallStatus::Complete,
            parent_tool_use_id: None,
        })]);
        let item = ChatItem {
            msg: &m,
            block_idx: 0,
            is_first_in_msg: true,
            is_streaming: false,
        };
        assert_eq!(item_height(&item, 80, 12), 1);
    }

    #[test]
    fn long_text_block_capped_at_half_window() {
        let m = msg_with(vec![long_text()]);
        let item = ChatItem {
            msg: &m,
            block_idx: 0,
            is_first_in_msg: true,
            is_streaming: false,
        };
        // max_block_h = 12 → total card height must not exceed it.
        let h = item_height(&item, 80, 12);
        assert!(h > 1 && h <= 12, "height {h} not within (1, 12]");
    }

    #[test]
    fn streaming_text_block_is_uncapped() {
        let m = msg_with(vec![long_text()]);
        let item = ChatItem {
            msg: &m,
            block_idx: 0,
            is_first_in_msg: true,
            is_streaming: true,
        };
        // Streaming blocks ignore the cap so the live tail stays visible.
        assert!(item_height(&item, 80, 12) > 12);
    }
}
