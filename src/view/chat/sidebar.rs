//! Sidebar renderer — sessions list partitioned into User and System
//! sections. Uses ratatui's List widget so the selection highlight
//! extends across the full row width.
//!
//! `sessions_selected` is the *logical* index (0 = `+ new chat`,
//! 1..=user_count = user sessions, user_count+1.. = system sessions).
//! The renderer inserts a non-selectable `── System (N) ──` header row
//! between groups, so the displayed list-state selection shifts by one
//! once `sessions_selected` enters the system range.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use super::state::{ChatView, FocusTier, SessionKey, SessionRuntime, is_system_session};
use crate::ui::{theme, truncate};

pub fn render(view: &ChatView, frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);

    let active_focus = matches!(view.focus, FocusTier::Sessions);

    // ----- header (search input or hint) -----
    let header_text = if view.sidebar_search_active {
        format!("/ {}_", view.sidebar_search)
    } else if view.sidebar_search.is_empty() {
        "Sessions  · / search".to_string()
    } else {
        format!("Sessions · /{}", view.sidebar_search)
    };
    let header_style = if active_focus {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let header = Paragraph::new(Line::from(Span::styled(header_text, header_style))).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(if active_focus {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            }),
    );
    frame.render_widget(header, layout[0]);

    let q = view.sidebar_search.to_lowercase();
    let first_system_idx = view.first_system_session_idx();
    let user_count = first_system_idx.unwrap_or(view.sessions.len());
    let system_count = view.sessions.len() - user_count;

    // Build the displayed ListItem list. We need a mapping from logical
    // index (`sessions_selected`) → display index (position in `items`).
    let mut items: Vec<ListItem> = Vec::with_capacity(view.sessions.len() + 3);
    let mut header_indices: Vec<usize> = Vec::new(); // displayed-row indices that aren't selectable

    // Logical 0 — "+ new chat".
    let has_draft = view.drafts.has(&SessionKey::NewChat);
    let mut new_chat_spans: Vec<Span<'static>> = Vec::new();
    new_chat_spans.push(Span::styled(
        "+ new chat".to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    if has_draft {
        new_chat_spans.push(Span::styled(" ●".to_string(), theme::accent()));
    }
    items.push(ListItem::new(Line::from(new_chat_spans)));

    // Logical 1..=user_count — user sessions.
    for s in &view.sessions[..user_count] {
        items.push(ListItem::new(session_line(view, s, &q, active_focus)));
    }

    // System group header (only if any system sessions exist).
    if system_count > 0 {
        let header_idx = items.len();
        header_indices.push(header_idx);
        let label = format!(
            "── system ({}) {}",
            system_count,
            "─".repeat(area.width.saturating_sub(16) as usize)
        );
        items.push(ListItem::new(Line::from(Span::styled(
            label,
            Style::default().add_modifier(Modifier::DIM),
        ))));

        // user_count+1..= total — system sessions.
        for s in &view.sessions[user_count..] {
            items.push(ListItem::new(session_line(view, s, &q, active_focus)));
        }
    }

    if view.sessions.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  (no sessions yet)",
            Style::default().add_modifier(Modifier::DIM),
        ))));
    }

    // Map logical `sessions_selected` → displayed list index.
    // Anything past the system header gets +1 to skip the header row.
    let sel_logical = view.sessions_selected;
    let display_sel = if first_system_idx.is_some() {
        let first_system_logical = 1 + user_count;
        if sel_logical >= first_system_logical {
            // Skip the inserted header row.
            sel_logical + 1
        } else {
            sel_logical
        }
    } else {
        sel_logical
    };

    // Highlight style differs by sidebar focus.
    let (highlight_style, highlight_symbol) = if active_focus {
        (
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            "▶ ",
        )
    } else {
        (
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            "  ",
        )
    };

    // Cyan right border + corner when sidebar is focused — gives clear
    // "this pane is active" feedback regardless of selection state.
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(if active_focus {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        });

    let list = List::new(items)
        .highlight_style(highlight_style)
        .highlight_symbol(highlight_symbol)
        .block(block);

    let mut state = ListState::default();
    // Skip selection if it lands on a header row (shouldn't happen, but
    // be defensive in case of mis-mapping).
    if !header_indices.contains(&display_sel) {
        state.select(Some(display_sel));
    }
    frame.render_stateful_widget(list, layout[1], &mut state);
}

fn session_line(
    view: &ChatView,
    s: &crate::model::Session,
    q: &str,
    active_focus: bool,
) -> Line<'static> {
    let title_raw = s.title.clone().unwrap_or_else(|| s.id.clone());
    let dim = !q.is_empty() && !title_raw.to_lowercase().contains(q);
    let runtime = view.session_runtime(&s.id);

    // Leading glyph reflects runtime: streaming ●, waiting-on-poll ?, else star/blank.
    let mut spans: Vec<Span<'static>> = Vec::new();
    match runtime {
        SessionRuntime::Streaming => {
            spans.push(Span::styled("●".to_string(), theme::session_streaming()));
        }
        SessionRuntime::WaitingPoll => {
            spans.push(Span::styled("?".to_string(), theme::session_waiting()));
        }
        SessionRuntime::WaitingPlan => {
            spans.push(Span::styled("◆".to_string(), theme::session_waiting_plan()));
        }
        SessionRuntime::Idle if s.starred => {
            spans.push(Span::styled(
                "★".to_string(),
                Style::default().fg(Color::Yellow),
            ));
        }
        SessionRuntime::Idle => spans.push(Span::raw(" ")),
    }
    spans.push(Span::raw(" "));

    let is_current = matches!(&view.current, SessionKey::Real(id) if id == &s.id);
    // Streaming/waiting colors take precedence; idle falls back to the
    // system/current/default hierarchy.
    let mut title_style = if dim {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        match runtime {
            SessionRuntime::Streaming => theme::session_streaming(),
            SessionRuntime::WaitingPoll => theme::session_waiting(),
            SessionRuntime::WaitingPlan => theme::session_waiting_plan(),
            SessionRuntime::Idle if is_system_session(s) => {
                Style::default().fg(Color::LightMagenta)
            }
            SessionRuntime::Idle if is_current && !active_focus => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            SessionRuntime::Idle => Style::default().fg(Color::White),
        }
    };
    if !dim && is_current && !active_focus {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    spans.push(Span::styled(truncate(&title_raw, 32), title_style));
    Line::from(spans)
}
