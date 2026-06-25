//! ChatView renderer — top-level layout.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as RatBlock, Borders, Paragraph, Wrap};

use super::blocks::{BlockSelection, render_item};
use super::items;
use super::state::{ChatView, FocusTier, SessionKey};
use crate::view::ViewRenderCtx;

pub fn render(view: &mut ChatView, area: Rect, frame: &mut Frame, _ctx: &ViewRenderCtx<'_>) {
    let show_sidebar = view.sidebar_visible && area.width >= 80;
    let show_panel =
        view.side_panel.visible && !view.side_panel.panels.is_empty() && area.width >= 100;

    let constraints: Vec<Constraint> = match (show_sidebar, show_panel) {
        (true, true) => vec![
            Constraint::Length(28),
            Constraint::Min(40),
            Constraint::Length(36),
        ],
        (true, false) => vec![Constraint::Length(28), Constraint::Min(40)],
        (false, true) => vec![Constraint::Min(40), Constraint::Length(36)],
        (false, false) => vec![Constraint::Min(40)],
    };
    let cols = Layout::horizontal(constraints).split(area);

    let mut col_idx = 0;
    if show_sidebar {
        super::sidebar::render(view, frame, cols[col_idx]);
        col_idx += 1;
    }
    let main = cols[col_idx];
    col_idx += 1;
    render_main(view, frame, main);

    if show_panel {
        let panel_area = cols[col_idx];
        render_panel(view, frame, panel_area);
    }
}

// ---------------------------------------------------------------------------
// Main column: title bar + message list + input
// ---------------------------------------------------------------------------

fn render_main(view: &mut ChatView, frame: &mut Frame, area: Rect) {
    // Owned line buffers — computed up front so the immutable borrows release
    // before the &mut renders below.
    let poll_lines = view.active_poll().map(super::poll::render_lines);
    let task_lines = {
        let tasks = view.current_tasks();
        let bg = view.current_background();
        super::tasks_panel::panel_lines(&tasks, bg)
    };
    // While the active session streams, the input box is removed and the
    // whole chat gets a green frame instead.
    let streaming = view.is_current_streaming();
    let input_height = compute_input_height(view, area.width);

    let task_height = task_lines
        .as_ref()
        .map(|l| ((l.len() as u16) + 2).clamp(3, (area.height / 3).max(3)));
    let poll_height = poll_lines
        .as_ref()
        .map(|l| ((l.len() as u16) + 2).clamp(3, (area.height / 2).max(3)));

    let mut constraints = vec![Constraint::Length(1), Constraint::Min(0)];
    if let Some(h) = task_height {
        constraints.push(Constraint::Length(h));
    }
    if let Some(h) = poll_height {
        constraints.push(Constraint::Length(h));
    }
    if !streaming {
        constraints.push(Constraint::Length(input_height));
    }
    let layout = Layout::vertical(constraints).split(area);

    render_title(view, frame, layout[0]);
    render_messages(view, frame, layout[1], streaming);
    let mut idx = 2;
    if let (Some(lines), Some(_)) = (task_lines, task_height) {
        render_side_block(frame, layout[idx], lines, " tasks ");
        idx += 1;
    }
    if let (Some(lines), Some(_)) = (poll_lines, poll_height) {
        render_poll_card(frame, layout[idx], lines);
        idx += 1;
    }
    if !streaming {
        render_input(view, frame, layout[idx]);
    }
}

fn render_poll_card(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let block = RatBlock::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Thick)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" question ");
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// A bordered bottom panel (tasks/jobs) with a plain cyan frame.
fn render_side_block(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>, title: &str) {
    let block = RatBlock::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title.to_string());
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn render_title(view: &ChatView, frame: &mut Frame, area: Rect) {
    let title = match &view.current {
        SessionKey::Real(id) => view
            .sessions
            .iter()
            .find(|s| &s.id == id)
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| id.to_string()),
        SessionKey::NewChat => "+ new chat".to_string(),
    };
    let line = Line::from(vec![
        Span::styled("# ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_messages(view: &mut ChatView, frame: &mut Frame, area: Rect, streaming: bool) {
    // The changed-files browser takes over the whole message area.
    if matches!(view.focus, FocusTier::Files) {
        super::files::render(view, frame, area);
        return;
    }
    // Blocks render identically in every state; streaming only adds the
    // green whole-chat frame (and removes the input box, handled by caller).
    let area = if streaming {
        let frame_block = RatBlock::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Thick)
            .border_style(Style::default().fg(Color::Green))
            .title(Span::styled(
                " ▶ streaming… ",
                Style::default().fg(Color::Green),
            ));
        let inner = frame_block.inner(area);
        frame.render_widget(frame_block, area);
        inner
    } else {
        area
    };

    let items = items::flatten(view.current_history(), view.current_streaming());
    let total = items.len();
    drop(items);

    if total == 0 {
        let placeholder = if view.is_loading_history() {
            "loading messages…"
        } else {
            "no messages yet — type below and press Enter to start"
        };
        let empty = Paragraph::new(vec![
            Line::raw(""),
            Line::raw(""),
            Line::from(Span::styled(
                placeholder,
                Style::default().add_modifier(Modifier::DIM),
            )),
        ])
        .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(empty, area);
        return;
    }

    if matches!(view.focus, FocusTier::BlockInterior) {
        render_block_interior(view, frame, area);
        return;
    }

    view.adjust_viewport(area);

    // Re-flatten after adjust_viewport (cheap; bounded by item count).
    let items = items::flatten(view.current_history(), view.current_streaming());
    let total = items.len();
    let key = view.current.clone();
    let ui = view.ui_state(&key);
    let selected_idx = ui.selected_block.unwrap_or(total - 1).min(total - 1);
    let top = ui.viewport_top.min(total - 1);

    let show_load_more_hint = top == 0
        && view
            .current_session_id()
            .map(|id| view.can_load_more(id))
            .unwrap_or(false);
    let hint_height: u16 = if show_load_more_hint || view.is_loading_history() {
        1
    } else {
        0
    };
    if hint_height > 0 {
        let hint_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: hint_height,
        };
        let label = if view.is_loading_history() {
            "  ⠋ loading older messages…"
        } else {
            "  ↑ press up to load older messages"
        };
        let p = Paragraph::new(Line::from(Span::styled(
            label,
            Style::default().add_modifier(Modifier::DIM),
        )));
        frame.render_widget(p, hint_area);
    }

    let body = Rect {
        x: area.x,
        y: area.y + hint_height,
        width: area.width,
        height: area.height.saturating_sub(hint_height),
    };
    let mut y = body.y;
    let bottom = body.y + body.height;
    let mut idx = top;
    let inner_w = body.width.saturating_sub(2).max(1);
    let max_block_h = super::state::block_height_cap(area.height);
    while idx < total && y < bottom {
        let item = &items[idx];
        let est_h = super::blocks_height_estimator::item_height(item, inner_w, max_block_h);
        let h = est_h.min(bottom - y);
        if h == 0 {
            break;
        }
        let sel = block_selection(view.focus, idx, selected_idx);
        let r = Rect {
            x: body.x,
            y,
            width: body.width,
            height: h,
        };
        render_item(item, sel, r, max_block_h, frame);
        y = y.saturating_add(h);
        idx += 1;
    }
}

/// Render a single block in zoomed-in mode (BlockInterior tier).
fn render_block_interior(view: &mut ChatView, frame: &mut Frame, area: Rect) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Span;
    use ratatui::widgets::{Block as RatBlock, BorderType, Borders};

    let items = items::flatten(view.current_history(), view.current_streaming());
    let total = items.len();
    let key = view.current.clone();
    let ui = view.ui_state(&key);
    let selected_idx = ui.selected_block.unwrap_or(total.saturating_sub(1));
    if selected_idx >= total {
        return;
    }
    let item = items[selected_idx];

    // Render block content into Lines. Use the "full" variant so tool
    // diffs/results aren't truncated — the cursor + scroll let the user
    // walk through every line.
    let mut content_lines: Vec<Line<'static>> =
        super::blocks::render_block_full(item.block(), item.is_streaming);
    content_lines = super::blocks::strip_trailing_blank_lines(content_lines);
    if content_lines.is_empty() {
        content_lines.push(Line::raw(""));
    }
    let total_lines = content_lines.len();

    // Title bar: who + when + cursor position.
    let role = match item.msg.role {
        crate::model::Role::User => "user",
        crate::model::Role::Assistant => "assistant",
        crate::model::Role::System => "system",
        crate::model::Role::Other => "?",
    };
    let role_style = match item.msg.role {
        crate::model::Role::User => super::super::super::ui::theme::role_user(),
        crate::model::Role::Assistant => super::super::super::ui::theme::role_assistant(),
        _ => Style::default().add_modifier(Modifier::DIM),
    };
    let block_widget = RatBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(vec![
            Span::styled(" ▶ ", Style::default().fg(Color::Cyan)),
            Span::styled(role.to_string(), role_style),
            Span::raw("  ·  "),
            Span::styled(
                format!(
                    "block {}/{}  L{}/{}",
                    selected_idx + 1,
                    total,
                    cursor_line_clamped(&ui.block_cursor, total_lines) + 1,
                    total_lines.max(1),
                ),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]));
    let inner = block_widget.inner(area);
    frame.render_widget(block_widget, area);

    // Resolve cursor + scroll relative to the inner viewport's height.
    let view_h = inner.height as usize;
    let cursor = cursor_line_clamped(&ui.block_cursor, total_lines);
    let mut scroll = ui.block_cursor.scroll.min(total_lines.saturating_sub(1));
    if cursor < scroll {
        scroll = cursor;
    } else if view_h > 0 && cursor >= scroll + view_h {
        scroll = cursor + 1 - view_h;
    }

    // Persist normalized cursor + scroll back to state.
    if let Some(ui_mut) = view.ui_mut_for_current() {
        ui_mut.block_cursor.line = cursor;
        ui_mut.block_cursor.scroll = scroll;
    }

    // Highlight the cursor line — pad it to the full inner width so empty /
    // short lines still show a solid bar (so you can see where you are).
    let inner_w = inner.width as usize;
    let highlighted: Vec<Line<'static>> = content_lines
        .into_iter()
        .enumerate()
        .map(|(i, mut line)| {
            if i == cursor {
                let highlight = Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
                for sp in line.spans.iter_mut() {
                    sp.style = sp.style.patch(highlight);
                }
                let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
                if w < inner_w {
                    line.spans
                        .push(Span::styled(" ".repeat(inner_w - w), highlight));
                }
                line = line.style(highlight);
            }
            line
        })
        .collect();

    // Wrap long lines so they don't fall off the right edge — we don't
    // support horizontal scroll inside a block. Vertical scroll is in
    // logical-line units; ratatui's wrap can make wrapped rows differ from
    // logical lines, so the cursor row may sit a row or two off when lines
    // wrap. Acceptable for v1.
    let p = Paragraph::new(highlighted)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((scroll as u16, 0));
    frame.render_widget(p, inner);
}

fn cursor_line_clamped(c: &super::state::BlockCursor, total_lines: usize) -> usize {
    if total_lines == 0 {
        0
    } else {
        c.line.min(total_lines - 1)
    }
}

fn block_selection(focus: FocusTier, idx: usize, selected_idx: usize) -> BlockSelection {
    if idx != selected_idx {
        return BlockSelection::None;
    }
    match focus {
        FocusTier::BlockInterior => BlockSelection::SelectedInterior,
        FocusTier::ChatBlocks => BlockSelection::Selected,
        _ => BlockSelection::None,
    }
}

fn render_input(view: &mut ChatView, frame: &mut Frame, area: Rect) {
    let focused = matches!(view.focus, FocusTier::Input | FocusTier::Insert);
    let active_color = match view.focus {
        FocusTier::Insert => Color::Yellow,
        FocusTier::Input => Color::Cyan,
        _ => Color::DarkGray,
    };
    let title = match view.focus {
        FocusTier::Insert => " input · INSERT — Enter send · Shift-Enter \\n · Esc ",
        FocusTier::Input => " input · Enter to type · ↑/Esc back ",
        _ => " input · ↓ or i to focus ",
    };

    let blk = RatBlock::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(active_color))
        .title(Span::styled(
            title,
            if focused {
                Style::default()
                    .fg(active_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            },
        ));
    let inner = blk.inner(area);
    frame.render_widget(blk, area);

    // Caret visibility — show only when the input box is focused.
    let cursor_style = if matches!(view.focus, FocusTier::Insert) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if matches!(view.focus, FocusTier::Input) {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::REVERSED)
    } else {
        // Render an invisible caret (no inversion, no background) so the
        // textarea widget doesn't paint a stray block over the text.
        Style::default()
    };
    view.input.set_cursor_style(cursor_style);
    view.input.set_cursor_line_style(Style::default());

    frame.render_widget(&view.input, inner);
}

fn compute_input_height(view: &ChatView, width: u16) -> u16 {
    let w = width.saturating_sub(2).max(1) as usize;
    let mut wrapped: u16 = 0;
    for l in view.input.lines() {
        let chars = l.chars().count().max(1);
        wrapped = wrapped.saturating_add(chars.div_ceil(w) as u16);
    }
    wrapped = wrapped.max(1);
    let max = 8;
    wrapped.min(max).saturating_add(2)
}

// ---------------------------------------------------------------------------
// Side panel
// ---------------------------------------------------------------------------

fn render_panel(view: &ChatView, frame: &mut Frame, area: Rect) {
    let blk = RatBlock::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Sub-agents ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = blk.inner(area);
    frame.render_widget(blk, area);

    let mut lines: Vec<Line<'_>> = Vec::new();
    for p in &view.side_panel.panels {
        let head = format!(
            "{} · {} {}",
            if p.running { "⠋" } else { "✓" },
            p.kind,
            p.description.chars().take(48).collect::<String>(),
        );
        lines.push(Line::from(Span::styled(
            head,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for b in &p.blocks {
            let r = super::blocks::render_block(b, p.running);
            for l in r {
                lines.push(l);
            }
        }
        lines.push(Line::raw(""));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no active sub-agents",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(p, inner);
}
