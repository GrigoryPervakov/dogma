//! Block renderer — converts model::Block into ratatui Lines, with selection
//! styling for the focused block.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as RatBlock, BorderType, Borders, Paragraph, Wrap};

use crate::model::{Block, Role, ToolCall, ToolCallStatus};
use crate::ui::markdown::render_markdown;
use crate::ui::{theme, truncate};
use crate::view::chat::items::ChatItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockSelection {
    None,
    Selected,
    SelectedInterior,
}

/// Render a single chat item (one `model::Block` from a message) into the
/// given area. Each item is its own bordered card — text, thinking, and
/// tool_call all get their own visual unit, mirroring the web UI's
/// per-block rendering.
pub fn render_item(
    item: &ChatItem<'_>,
    selection: BlockSelection,
    area: Rect,
    max_block_h: u16,
    frame: &mut Frame,
) {
    // Collapsed tool/thinking blocks render as a single dense header line with
    // no border box — their body is hidden until expanded in BlockInterior, so
    // a full card would just be wasted chrome. The summary may run up to 80% of
    // the line width before truncation.
    if is_collapsible(item.block()) {
        let title = item_title(item, selection, area.width as usize);
        let style = match selection {
            BlockSelection::Selected | BlockSelection::SelectedInterior => {
                Style::default().bg(Color::DarkGray)
            }
            BlockSelection::None => Style::default(),
        };
        let row = Rect {
            height: area.height.min(1),
            ..area
        };
        frame.render_widget(Paragraph::new(title).style(style), row);
        return;
    }

    let title = item_title(item, selection, 0);
    let (border_type, border_style) = if is_plan_block(item.block()) {
        // Plan-mode blocks always wear a magenta frame so the pending decision
        // stands out from ordinary blocks (selection still shows via the ▶).
        (BorderType::Thick, Style::default().fg(Color::Magenta))
    } else {
        match selection {
            BlockSelection::Selected | BlockSelection::SelectedInterior => {
                (BorderType::Thick, Style::default().fg(Color::Cyan))
            }
            // User messages get a distinct colored frame so they stand out from
            // the assistant's blocks.
            BlockSelection::None if matches!(item.msg.role, Role::User) => {
                (BorderType::Plain, theme::role_user())
            }
            BlockSelection::None => (BorderType::Plain, Style::default()),
        }
    };

    let inner_w = area.width.saturating_sub(2).max(1);
    let max_rows = content_budget(max_block_h, item.is_streaming);
    let content_lines = compact_content_lines(item.block(), item.is_streaming, inner_w, max_rows);

    let block_widget = RatBlock::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(title);

    let inner = block_widget.inner(area);
    frame.render_widget(block_widget, area);

    if !content_lines.is_empty() {
        let p = Paragraph::new(content_lines).wrap(Wrap { trim: false });
        frame.render_widget(p, inner);
    }
}

/// Rows of content a non-collapsed card may show in the compact list: half the
/// window height, minus chrome (2 borders + 1 gap). Streaming blocks are
/// uncapped (`0`) so the live tail stays visible as it grows.
pub fn content_budget(max_block_h: u16, streaming: bool) -> usize {
    if streaming {
        return 0;
    }
    (max_block_h as usize).saturating_sub(3)
}

/// Compact-view content for a non-collapsed block. Text is capped at `max_rows`
/// wrapped rows (`0` = unlimited) with a "… N more — Enter to expand" footer,
/// so one long message can't dominate the list.
pub fn compact_content_lines(
    b: &Block,
    streaming: bool,
    inner_w: u16,
    max_rows: usize,
) -> Vec<Line<'static>> {
    let lines = render_block(b, streaming);
    if max_rows == 0 {
        return lines;
    }
    let w = inner_w.max(1) as usize;
    let rows_of = |l: &Line<'static>| -> usize {
        l.spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
            .max(1)
            .div_ceil(w)
    };
    let total_rows: usize = lines.iter().map(&rows_of).sum();
    if total_rows <= max_rows {
        return lines;
    }
    let budget = max_rows.saturating_sub(1).max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut used = 0usize;
    for l in &lines {
        let r = rows_of(l);
        if used + r > budget {
            break;
        }
        used += r;
        out.push(l.clone());
    }
    let hidden = lines.len().saturating_sub(out.len());
    out.push(Line::from(Span::styled(
        format!("… {hidden} more lines — Enter to expand"),
        Style::default().add_modifier(Modifier::DIM),
    )));
    out
}

/// Tool-call and thinking blocks collapse to a header-only card in the
/// compact view — except plan-mode blocks, which stay expanded so the plan and
/// its approve/decline prompt are always visible in the stream.
pub fn is_collapsible(b: &Block) -> bool {
    match b {
        Block::Thinking { .. } => true,
        Block::ToolCall(tc) => !is_plan_tool(&tc.tool),
        _ => false,
    }
}

fn is_plan_tool(tool: &str) -> bool {
    matches!(tool, "ExitPlanMode" | "EnterPlanMode")
}

/// A plan-mode approval block — gets a distinct frame in the message list.
pub fn is_plan_block(b: &Block) -> bool {
    matches!(b, Block::ToolCall(tc) if is_plan_tool(&tc.tool))
}

/// A line with no visible glyphs (empty or all-whitespace spans).
pub fn line_is_blank(l: &Line<'_>) -> bool {
    l.spans.iter().all(|s| s.content.trim().is_empty())
}

/// Drop trailing blank lines, keeping at least one — used by the zoomed block
/// view so the cursor can't park on empty space below the last real content.
pub fn strip_trailing_blank_lines(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    while lines.len() > 1 && lines.last().map(line_is_blank).unwrap_or(false) {
        lines.pop();
    }
    lines
}

/// Block type shown in the header, e.g. `tool:Read`, `thinking`, `text`.
fn block_type_label(b: &Block) -> String {
    match b {
        Block::Text { .. } => "text".into(),
        Block::Thinking { .. } => "thinking".into(),
        Block::ToolCall(tc) => format!("tool:{}", tc.tool),
        Block::File { .. } => "file".into(),
        Block::Image { .. } => "image".into(),
        Block::Unknown => "block".into(),
    }
}

fn block_type_style(b: &Block) -> Style {
    match b {
        Block::Thinking { .. } => Style::default().fg(Color::Magenta),
        Block::ToolCall(_) | Block::File { .. } | Block::Image { .. } => {
            Style::default().fg(Color::Cyan)
        }
        _ => Style::default().add_modifier(Modifier::DIM),
    }
}

/// One-line summary appended to a collapsed block's header (the tool args or
/// the first words of a thought), truncated to `max` display columns.
fn collapsed_summary(b: &Block, max: usize) -> Option<String> {
    let s = match b {
        Block::ToolCall(tc) => first_line_of_input(&tc.input),
        Block::Thinking { content } => content.lines().find(|l| !l.trim().is_empty())?.to_string(),
        _ => return None,
    };
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(truncate(s, max.max(1)))
}

/// The tool's primary command/argument as a single line, untruncated — the
/// header truncates it to its width budget. Falls back to compact (single-line)
/// JSON, never the pretty/multi-line form used by the full view.
fn first_line_of_input(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            for k in [
                "command",
                "cmd",
                "path",
                "file_path",
                "pattern",
                "url",
                "description",
                "prompt",
            ] {
                if let Some(s) = map.get(k).and_then(|v| v.as_str()) {
                    return s.lines().next().unwrap_or("").to_string();
                }
            }
            serde_json::to_string(v).unwrap_or_default()
        }
        serde_json::Value::String(s) => s.lines().next().unwrap_or("").to_string(),
        other => other.to_string(),
    }
}

/// Title line — uniform for every block regardless of streaming state: the
/// block type (`tool:Read`), with the role/timestamp prepended on a message's
/// first block and a short summary appended for collapsed blocks. The
/// streaming state is signalled by the whole-chat frame, not per-block.
/// `width` is the line width available to the collapsed header (`0` = don't
/// append a summary, e.g. the bordered card path). The summary is allowed up to
/// 80% of `width`, further bounded so the prefix + summary never overflow.
fn item_title(item: &ChatItem<'_>, selection: BlockSelection, width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    if matches!(
        selection,
        BlockSelection::Selected | BlockSelection::SelectedInterior
    ) {
        spans.push(Span::styled(" ▶ ", theme::accent()));
    } else {
        spans.push(Span::raw(" "));
    }

    if item.is_first_in_msg {
        let role = match item.msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Other => "?",
        };
        let role_style = match item.msg.role {
            Role::User => theme::role_user(),
            Role::Assistant => theme::role_assistant(),
            _ => Style::default().add_modifier(Modifier::DIM),
        };
        let ts = item
            .msg
            .created_at
            .as_deref()
            .map(format_timestamp)
            .unwrap_or_else(|| "--:--".into());
        spans.push(Span::styled(role.to_string(), role_style));
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            ts,
            Style::default().add_modifier(Modifier::DIM),
        ));
        spans.push(Span::raw(" · "));
    }
    spans.push(Span::styled(
        block_type_label(item.block()),
        block_type_style(item.block()),
    ));
    if width > 0 {
        let prefix_w: usize = spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
            + 3; // " · " separator before the summary
        let budget = (width * 8 / 10).min(width.saturating_sub(prefix_w));
        if budget > 0
            && let Some(sum) = collapsed_summary(item.block(), budget)
        {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                sum,
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }

    Line::from(spans)
}

/// Public entrypoint used by the message-list renderer.
///
/// `full = false` is the compact view: tool results / diffs are truncated
/// with a "…N more lines" footer. `full = true` is the BlockInterior /
/// expanded view: every line is emitted so the user can scroll through.
pub fn render_block(b: &Block, streaming: bool) -> Vec<Line<'static>> {
    render_block_with(b, streaming, false)
}

pub fn render_block_full(b: &Block, streaming: bool) -> Vec<Line<'static>> {
    render_block_with(b, streaming, true)
}

fn render_block_with(b: &Block, streaming: bool, full: bool) -> Vec<Line<'static>> {
    match b {
        Block::Text { content } => render_markdown(content),
        Block::Thinking { content } => render_thinking(content, streaming),
        Block::ToolCall(tc) => render_toolcall(tc, streaming, full),
        Block::File { filename, .. } => render_attachment("file", filename),
        Block::Image { filename, .. } => render_attachment("image", filename),
        Block::Unknown => Vec::new(),
    }
}

fn render_attachment(kind: &str, filename: &str) -> Vec<Line<'static>> {
    let label = if filename.is_empty() {
        format!("▸ {kind}")
    } else {
        format!("▸ {kind} · {filename}")
    };
    vec![Line::from(Span::styled(
        label,
        Style::default().fg(Color::Cyan),
    ))]
}

fn render_thinking(content: &str, _streaming: bool) -> Vec<Line<'static>> {
    let style = Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::ITALIC);
    let mut out = Vec::new();
    out.push(Line::from(Span::styled("∽ thinking", style)));
    for line in content.lines() {
        out.push(Line::from(Span::styled(
            format!("  {line}"),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    out
}

fn render_toolcall(tc: &ToolCall, streaming: bool, full: bool) -> Vec<Line<'static>> {
    // Specialized renderers — match the web UI dispatch in
    // web/src/components/Chat/ToolCallBlock.tsx.
    match tc.tool.as_str() {
        "Edit" => return render_edit_call(tc, full),
        "MultiEdit" => return render_multiedit_call(tc, full),
        "AskUserQuestion" => return render_question_call(tc, streaming, full),
        "ExitPlanMode" | "EnterPlanMode" => return render_plan_call(tc),
        _ => {}
    }
    render_default_call(tc, streaming, full)
}

/// A plan-mode block: the plan markdown plus an approve/decline prompt while the
/// interaction is still pending. Rendered expanded (not collapsed) so the plan
/// is always readable in the message stream — the user answers with `a`/`d`.
fn render_plan_call(tc: &ToolCall) -> Vec<Line<'static>> {
    let is_exit = tc.tool == "ExitPlanMode";
    let pending = tc.result.is_none();
    let mut out = vec![Line::from(Span::styled(
        if is_exit {
            "▸ plan ready for approval"
        } else {
            "▸ wants to enter plan mode"
        },
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ))];
    match tc.input.get("plan").and_then(|v| v.as_str()) {
        Some(plan) if !plan.is_empty() => {
            out.push(Line::raw(""));
            out.extend(render_markdown(plan));
        }
        _ if !is_exit => out.push(Line::from(Span::styled(
            "The agent will explore the codebase and design an approach for approval.",
            Style::default().add_modifier(Modifier::DIM),
        ))),
        _ => {}
    }
    out.push(Line::raw(""));
    if pending {
        out.push(Line::from(vec![
            Span::styled(
                "[a]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" approve    "),
            Span::styled(
                "[d]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" decline"),
        ]));
    } else {
        let (glyph, label, color) = if tc.is_error {
            ("✗", "declined", Color::Red)
        } else {
            ("✓", "approved", Color::Green)
        };
        out.push(Line::from(Span::styled(
            format!("{glyph} {label}"),
            Style::default().fg(color),
        )));
    }
    out
}

/// Read-only poll summary for an `AskUserQuestion` tool-call in history:
/// question(s), options, and the chosen answer if the call has completed.
fn render_question_call(tc: &ToolCall, streaming: bool, full: bool) -> Vec<Line<'static>> {
    let Some(poll) = super::poll::Poll::from_tool_input(&tc.input) else {
        return render_default_call(tc, streaming, full);
    };
    let mut out = vec![Line::from(Span::styled(
        "▸ question",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    for q in &poll.questions {
        let mut head: Vec<Span<'static>> = vec![Span::raw("  ")];
        if !q.header.is_empty() {
            head.push(Span::styled(
                format!("[{}] ", q.header),
                Style::default().fg(Color::Cyan),
            ));
        }
        head.push(Span::styled(
            q.question.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        out.push(Line::from(head));
        for opt in &q.options {
            out.push(Line::from(Span::styled(
                format!("    · {}", opt.label),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
    }
    if let Some(ans) = tc.result.as_deref().filter(|s| !s.is_empty()) {
        let first = ans.lines().next().unwrap_or(ans);
        out.push(Line::from(vec![
            Span::styled("  answer: ", Style::default().fg(Color::Green)),
            Span::raw(first.to_string()),
        ]));
    }
    out
}

fn render_default_call(tc: &ToolCall, streaming: bool, full: bool) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let glyph = tool_glyph(tc, streaming);
    let mut head_spans = vec![
        Span::styled(
            glyph,
            if tc.is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Cyan)
            },
        ),
        Span::raw(" tool · "),
        Span::styled(
            tc.tool.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    // Compact view keeps a one-line truncated preview on the header; the full
    // (BlockInterior) view shows the whole command on its own lines below.
    if !full {
        let summary = summarize_input(&tc.input);
        if !summary.is_empty() {
            head_spans.push(Span::raw(" "));
            head_spans.push(Span::styled(
                summary,
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }
    if tc.result.is_none() && (matches!(tc.status, ToolCallStatus::Streaming) || streaming) {
        head_spans.push(Span::raw("  "));
        head_spans.push(Span::styled("running", Style::default().fg(Color::Yellow)));
    } else if !full && let Some(r) = &tc.result {
        head_spans.push(Span::raw("  →  "));
        head_spans.push(Span::styled(
            preview_len(r, 60),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    out.push(Line::from(head_spans));

    // Full screen: the complete command / input, untruncated and wrapped.
    if full {
        for line in full_input_text(&tc.input).lines() {
            out.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default(),
            )));
        }
    }

    if let Some(r) = &tc.result {
        if full {
            out.push(Line::raw(""));
        }
        let lines: Vec<&str> = r.lines().collect();
        let cap = if full { lines.len() } else { 8 };
        for line in lines.iter().take(cap) {
            out.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        if !full && lines.len() > cap {
            out.push(Line::from(Span::styled(
                format!("  …{} more lines", lines.len() - cap),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
    }
    out
}

/// The tool's primary command/argument in full (no truncation, newlines kept).
/// Mirrors `summarize_input`'s key preference but returns the whole value.
fn full_input_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            for k in [
                "command",
                "cmd",
                "path",
                "file_path",
                "pattern",
                "url",
                "description",
                "prompt",
            ] {
                if let Some(s) = map.get(k).and_then(|v| v.as_str()) {
                    return s.to_string();
                }
            }
            serde_json::to_string_pretty(v).unwrap_or_default()
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn render_edit_call(tc: &ToolCall, full: bool) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let file_path = tc
        .input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let old_s = tc
        .input
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_s = tc
        .input
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    out.push(edit_head_line(tc, "Edit", file_path, old_s, new_s));
    if !full {
        return out;
    }
    push_diff_block(&mut out, old_s, new_s);
    push_error_tail(&mut out, tc);
    out
}

fn render_multiedit_call(tc: &ToolCall, full: bool) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let file_path = tc
        .input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let edits = tc
        .input
        .get("edits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Collapsed head: edit count + total line delta
    let mut total_old = 0usize;
    let mut total_new = 0usize;
    for e in &edits {
        if let Some(s) = e.get("old_string").and_then(|v| v.as_str()) {
            total_old += s.lines().count();
        }
        if let Some(s) = e.get("new_string").and_then(|v| v.as_str()) {
            total_new += s.lines().count();
        }
    }
    let glyph = tool_glyph(tc, false);
    out.push(Line::from(vec![
        Span::styled(
            glyph,
            if tc.is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Cyan)
            },
        ),
        Span::raw(" tool · "),
        Span::styled("MultiEdit", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(
            preview_len(file_path, 80),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw("  "),
        Span::styled(
            format!("({} edits)", edits.len()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::styled(
            format!("-{} +{}", total_old, total_new),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    if !full {
        return out;
    }
    for (i, e) in edits.iter().enumerate() {
        let old_s = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
        let new_s = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
        out.push(Line::from(Span::styled(
            format!("─── edit {} ───", i + 1),
            Style::default().add_modifier(Modifier::DIM),
        )));
        push_diff_block(&mut out, old_s, new_s);
    }
    push_error_tail(&mut out, tc);
    out
}

fn edit_head_line(
    tc: &ToolCall,
    label: &str,
    file_path: &str,
    old_s: &str,
    new_s: &str,
) -> Line<'static> {
    let glyph = tool_glyph(tc, false);
    Line::from(vec![
        Span::styled(
            glyph,
            if tc.is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Cyan)
            },
        ),
        Span::raw(" tool · "),
        Span::styled(
            label.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            preview_len(file_path, 80),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw("  "),
        Span::styled(
            format!("-{} +{}", old_s.lines().count(), new_s.lines().count()),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn push_diff_block(out: &mut Vec<Line<'static>>, old_s: &str, new_s: &str) {
    let del_marker = Style::default().fg(Color::Red).add_modifier(Modifier::DIM);
    let del_text = Style::default().fg(Color::LightRed);
    let add_marker = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::DIM);
    let add_text = Style::default().fg(Color::LightGreen);

    for line in old_s.lines() {
        out.push(Line::from(vec![
            Span::styled("- ".to_string(), del_marker),
            Span::styled(line.to_string(), del_text),
        ]));
    }
    for line in new_s.lines() {
        out.push(Line::from(vec![
            Span::styled("+ ".to_string(), add_marker),
            Span::styled(line.to_string(), add_text),
        ]));
    }
}

fn push_error_tail(out: &mut Vec<Line<'static>>, tc: &ToolCall) {
    if !tc.is_error {
        return;
    }
    if let Some(r) = &tc.result {
        out.push(Line::raw(""));
        for line in r.lines() {
            out.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(Color::Red),
            )));
        }
    }
}

fn tool_glyph(tc: &ToolCall, streaming: bool) -> &'static str {
    match tc.status {
        ToolCallStatus::Streaming => "⠋",
        ToolCallStatus::Complete => {
            if tc.is_error {
                "✗"
            } else if streaming {
                "⠋"
            } else {
                "▸"
            }
        }
    }
}

fn summarize_input(v: &serde_json::Value) -> String {
    if v.is_null() {
        return String::new();
    }
    match v {
        serde_json::Value::Object(map) => {
            // Pull a couple of common fields for a pithy summary.
            let keys = [
                "command",
                "cmd",
                "path",
                "file_path",
                "pattern",
                "url",
                "description",
                "prompt",
            ];
            for k in keys {
                if let Some(v) = map.get(k)
                    && let Some(s) = v.as_str()
                {
                    return preview_len(s, 60);
                }
            }
            // Fallback: show a short JSON-ish hint (char-safe truncation —
            // byte `String::truncate` panics mid-UTF-8).
            truncate(&serde_json::to_string(v).unwrap_or_default(), 60)
        }
        serde_json::Value::String(s) => preview_len(s, 60),
        other => preview_len(&other.to_string(), 60),
    }
}

fn preview_len(s: &str, n: usize) -> String {
    let one = s.lines().next().unwrap_or("");
    if one.chars().count() <= n {
        one.to_string()
    } else {
        let p: String = one.chars().take(n - 1).collect();
        format!("{p}…")
    }
}

/// Best-effort: pull "HH:MM" out of an ISO-ish "2026-04-29 14:32:18" or
/// "2026-04-29T14:32:18+00:00". Falls back to the trimmed input.
fn format_timestamp(s: &str) -> String {
    // Find the time portion after a space or 'T'.
    let after = s
        .split_once(' ')
        .map(|p| p.1)
        .or_else(|| s.split_once('T').map(|p| p.1));
    let part = after.unwrap_or(s);
    let hhmm: String = part.chars().take(5).collect();
    if hhmm.len() == 5 && hhmm.chars().nth(2) == Some(':') {
        hhmm
    } else {
        s.chars().take(8).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compact_content_lines, first_line_of_input, full_input_text, is_collapsible, is_plan_block,
        line_is_blank, render_default_call, render_toolcall, strip_trailing_blank_lines,
        summarize_input,
    };
    use crate::model::{Block, ToolCall, ToolCallStatus};
    use ratatui::text::Line;

    fn render_text(lines: &[ratatui::text::Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    fn bash(command: &str) -> ToolCall {
        ToolCall {
            tool_use_id: "t1".into(),
            tool: "Bash".into(),
            input: serde_json::json!({ "command": command }),
            result: Some("done".into()),
            is_error: false,
            status: ToolCallStatus::Complete,
            parent_tool_use_id: None,
        }
    }

    #[test]
    fn plan_block_is_expanded_and_renders_plan_and_prompt() {
        let mut tc = bash("ignored");
        tc.tool = "ExitPlanMode".into();
        tc.input = serde_json::json!({ "plan": "# Plan\n- step one" });
        tc.result = None; // still pending
        let block = Block::ToolCall(tc.clone());
        assert!(is_plan_block(&block));
        assert!(!is_collapsible(&block), "plan blocks render expanded");
        let text = render_text(&render_toolcall(&tc, false, false));
        assert!(text.contains("plan ready for approval"));
        assert!(text.contains("step one"), "plan markdown shown");
        assert!(text.contains("approve") && text.contains("decline"));
        // A regular tool call is neither a plan block nor expanded.
        assert!(!is_plan_block(&Block::ToolCall(bash("ls"))));
    }

    #[test]
    fn full_input_text_returns_whole_command() {
        let long = format!("echo {}", "abcd ".repeat(40));
        let v = serde_json::json!({ "command": long });
        assert_eq!(full_input_text(&v), v["command"].as_str().unwrap());
    }

    #[test]
    fn full_screen_call_keeps_command_untruncated() {
        let long = format!("echo {}", "x".repeat(200));
        let tc = bash(&long);
        // Full (BlockInterior) view: the whole command survives, no ellipsis.
        let full = render_text(&render_default_call(&tc, false, true));
        assert!(full.contains(&long));
        assert!(!full.contains('…'));
        // Compact view truncates the same command.
        let compact = render_text(&render_default_call(&tc, false, false));
        assert!(compact.contains('…'));
        assert!(!compact.contains(&long));
    }

    #[test]
    fn summarize_input_truncates_on_char_boundary() {
        // Object hits the JSON-fallback path; many multi-byte em dashes put a
        // non-ASCII char across the byte-60 cut. Byte `String::truncate` used
        // to panic here (is_char_boundary). Must be char-safe now.
        let v = serde_json::json!({ "weird": "—".repeat(70) });
        let s = summarize_input(&v); // must not panic
        assert!(s.chars().count() <= 61);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn summarize_input_handles_emoji() {
        let v = serde_json::json!({ "note": "🚀".repeat(40) });
        let _ = summarize_input(&v); // must not panic on 4-byte chars
    }

    #[test]
    fn first_line_of_input_prefers_compact_json_for_keyless_objects() {
        // No primary key → compact (single-line) JSON, never pretty-printed.
        let v = serde_json::json!({ "taskId": "1", "status": "in_progress" });
        let s = first_line_of_input(&v);
        assert!(!s.contains('\n'));
        assert!(s.starts_with('{'));
        assert!(s.contains("in_progress"));
    }

    #[test]
    fn compact_content_lines_caps_long_text_with_footer() {
        let body = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let b = Block::Text { content: body };
        let lines = compact_content_lines(&b, false, 40, 8);
        // Capped to the row budget (8), last line is the expand footer.
        assert!(lines.len() <= 8);
        let last = lines.last().unwrap();
        let txt: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(txt.contains("more lines"));
        assert!(txt.contains("Enter to expand"));
    }

    #[test]
    fn strip_trailing_blank_lines_drops_empties_keeps_one() {
        let lines = vec![
            Line::from("hello"),
            Line::from("world"),
            Line::raw(""),
            Line::from("   "),
        ];
        let out = strip_trailing_blank_lines(lines);
        assert_eq!(out.len(), 2);
        assert!(!line_is_blank(&out[1]));
        // All-blank input collapses to a single line, never empty.
        let allblank = strip_trailing_blank_lines(vec![Line::raw(""), Line::raw("")]);
        assert_eq!(allblank.len(), 1);
    }

    #[test]
    fn compact_content_lines_uncapped_when_streaming_or_zero() {
        let body = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let b = Block::Text { content: body };
        // max_rows == 0 → identical to the unbounded render, no footer.
        let full = compact_content_lines(&b, false, 40, 0);
        assert_eq!(full.len(), super::render_block(&b, false).len());
        let joined: String = full
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!joined.contains("more lines"));
    }
}
