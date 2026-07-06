//! Bottom 1-line statusbar — every transient bit of UI state lives here.
//!
//! Layout, left → right:
//!   <mode/tier> · <agent activity> · <session> · <ws conn> · <ctx>
//!     · <error>?
//!   …spacer…
//!   <right hint: command line / help />

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::state::{App, Mode, WsConnState};
use crate::ui::truncate;
use crate::view::chat::{AgentStatus, ChatView, FocusTier};

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    // Command mode takes over the whole row as a prompt + completion
    // menu.
    if matches!(app.mode, Mode::Command) {
        render_command_bar(app, frame, area);
        return;
    }

    let mut spans: Vec<Span<'_>> = Vec::new();

    // ----- left side ---------------------------------------------------------
    let mode_label = mode_label(app);
    spans.push(Span::styled(
        mode_label,
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Yellow),
    ));

    let on_chat = active_view_is_chat(app);
    if on_chat {
        if let Some(chat) = chat_view(app) {
            push_sep(&mut spans);
            spans.extend(agent_spans(chat));

            push_sep(&mut spans);
            spans.extend(session_spans(chat));

            push_sep(&mut spans);
            spans.extend(conn_spans(app));

            push_sep(&mut spans);
            spans.extend(ctx_spans(chat));

            if let Some(err) = chat.last_error.as_deref() {
                push_sep(&mut spans);
                spans.push(Span::styled(
                    format!("error: {}", truncate(err, 64)),
                    Style::default().fg(Color::Red),
                ));
            }
        }
    } else {
        push_sep(&mut spans);
        spans.extend(conn_spans(app));
    }

    // ----- right hint --------------------------------------------------------
    let line = Line::from(spans);
    let used = line_width(&line);
    let total = area.width as usize;
    let right_text = right_hint(app);
    let right_used = right_text.chars().count();
    let pad = total
        .saturating_sub(used)
        .saturating_sub(right_used)
        .saturating_sub(2);

    let mut composed: Vec<Span<'_>> = line.spans;
    composed.push(Span::raw(" ".repeat(pad)));
    composed.push(Span::raw(" "));
    composed.push(Span::styled(
        right_text,
        Style::default().add_modifier(Modifier::DIM),
    ));

    frame.render_widget(Paragraph::new(Line::from(composed)), area);
}

/// Command prompt: `:buffer` + a dim menu of matching commands.
fn render_command_bar(app: &App, frame: &mut Frame, area: Rect) {
    let buf = app.command_buffer.as_str();
    let mut spans: Vec<Span<'_>> = vec![
        Span::styled(
            ":",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            buf.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ];

    // Only complete the command token — once a space (argument) is typed the
    // command is chosen, so suppress the menu.
    let matches = if buf.contains(' ') {
        Vec::new()
    } else {
        crate::app::command::completions(buf)
    };
    if matches.is_empty() && !buf.contains(' ') && !buf.is_empty() {
        spans.push(Span::styled(
            "  no match".to_string(),
            Style::default().fg(Color::Red),
        ));
    } else if !matches.is_empty() {
        spans.push(Span::raw("   "));
        for (i, m) in matches.iter().take(12).enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            // Bright matched prefix, dim remainder.
            let cut = buf.len().min(m.len());
            spans.push(Span::styled(
                m[..cut].to_string(),
                Style::default().fg(Color::Cyan),
            ));
            spans.push(Span::styled(
                m[cut..].to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if matches.len() > 12 {
            spans.push(Span::styled(
                format!("  +{}", matches.len() - 12),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }

    spans.push(Span::styled(
        "   Tab complete · Enter run · Esc cancel",
        Style::default().add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------------------------------------------------------------------------
// Field helpers
// ---------------------------------------------------------------------------

fn agent_spans(chat: &ChatView) -> Vec<Span<'static>> {
    match chat.current_agent_status() {
        AgentStatus::Idle => vec![Span::styled(
            "idle".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        )],
        AgentStatus::Thinking => vec![Span::styled(
            "⠋ thinking".to_string(),
            Style::default().fg(Color::Magenta),
        )],
        AgentStatus::Writing => vec![Span::styled(
            "⠋ writing".to_string(),
            Style::default().fg(Color::Cyan),
        )],
        AgentStatus::Tool(t) => vec![Span::styled(
            format!("⠋ {}", t),
            Style::default().fg(Color::Cyan),
        )],
    }
}

fn session_spans(chat: &ChatView) -> Vec<Span<'static>> {
    let label = match chat.current_session_ref() {
        Some(sref) => {
            let title = chat
                .sessions
                .iter()
                .find(|s| s.instance == sref.instance && s.id == sref.id)
                .and_then(|s| s.title.clone())
                .unwrap_or_else(|| sref.id.clone());
            format!("session: {}", truncate(&title, 40))
        }
        None => "session: + new chat".to_string(),
    };
    vec![Span::raw(label)]
}

/// Connection summary: a single `ws` dot when one instance is connected, or a
/// per-instance `● label · ◆ label` strip (each in its instance color, dimmed
/// when offline) when several are.
fn conn_spans(app: &App) -> Vec<Span<'static>> {
    if app.instances.len() <= 1 {
        let ws = app
            .instances
            .first()
            .map(|i| i.ws.clone())
            .unwrap_or(WsConnState::Disconnected);
        return ws_spans(&ws);
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    for (i, meta) in app.instances.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled(
                " · ",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        let mut style = Style::default().fg(crate::ui::theme::instance_color(meta.id));
        if !matches!(meta.ws, WsConnState::Connected) {
            style = style.add_modifier(Modifier::DIM);
        }
        out.push(Span::styled(
            format!(
                "{} {}",
                crate::ui::theme::instance_sigil(meta.id),
                meta.label
            ),
            style,
        ));
    }
    out
}

fn ws_spans(ws: &WsConnState) -> Vec<Span<'static>> {
    let (dot, dot_style, label, label_style) = match ws {
        WsConnState::Connected => (
            "●",
            Style::default().fg(Color::Green),
            "ws".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        WsConnState::Connecting => (
            "⠋",
            Style::default().fg(Color::Yellow),
            "ws connecting".to_string(),
            Style::default().fg(Color::Yellow),
        ),
        WsConnState::Reconnecting { retry_in_ms, .. } => (
            "⚠",
            Style::default().fg(Color::Yellow),
            format!("ws retry {}s", retry_in_ms / 1000),
            Style::default().fg(Color::Yellow),
        ),
        WsConnState::AuthRejected => (
            "✗",
            Style::default().fg(Color::Red),
            "ws auth rejected".to_string(),
            Style::default().fg(Color::Red),
        ),
        WsConnState::Disconnected => (
            "○",
            Style::default().add_modifier(Modifier::DIM),
            "ws offline".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ),
    };
    vec![
        Span::styled(dot.to_string(), dot_style),
        Span::raw(" "),
        Span::styled(label, label_style),
    ]
}

fn ctx_spans(chat: &ChatView) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    let ctx = chat.current_context();
    let estimated = ctx.and_then(|c| c.estimated_context_tokens()).unwrap_or(0);
    let max_tokens = ctx.and_then(|c| c.max_context_tokens).unwrap_or(0);

    let label = if max_tokens > 0 && estimated > 0 {
        let pct = ((estimated as f64 / max_tokens as f64) * 100.0)
            .round()
            .min(999.0) as u64;
        format!("ctx ~{}/{} ({}%)", fmt_k(estimated), fmt_k(max_tokens), pct)
    } else if max_tokens > 0 {
        format!("ctx —/{}", fmt_k(max_tokens))
    } else if estimated > 0 {
        format!("ctx ~{}", fmt_k(estimated))
    } else {
        "ctx —".to_string()
    };
    let style = if max_tokens > 0 && estimated > 0 {
        let frac = estimated as f64 / max_tokens as f64;
        if frac > 0.8 {
            Style::default().fg(Color::Red)
        } else if frac > 0.6 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Cyan)
        }
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    spans.push(Span::styled(label, style));

    let cost = chat.current_cost_usd();
    if cost > 0.0001 {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("${:.3}", cost),
            Style::default().fg(Color::Gray),
        ));
    }

    spans
}

fn right_hint(app: &App) -> String {
    match app.mode {
        Mode::Command => format!(":{}", app.command_buffer),
        Mode::Help => "press any key".into(),
        Mode::Normal => {
            if active_view_is_chat(app) {
                let plan_pending = chat_view(app)
                    .map(|c| c.active_plan().is_some())
                    .unwrap_or(false);
                match chat_view(app).map(|c| c.focus) {
                    Some(FocusTier::Sessions) => "Enter open · / search · ? help · q quit".into(),
                    Some(FocusTier::ChatBlocks) if plan_pending => {
                        "a approve · d decline · ↑↓ scroll plan · Enter expand".into()
                    }
                    Some(FocusTier::ChatBlocks) => {
                        "Enter expand · Esc back · / find · i input".into()
                    }
                    Some(FocusTier::BlockInterior) if plan_pending => {
                        "a approve · d decline · ↑↓/jk scroll · Esc back".into()
                    }
                    Some(FocusTier::BlockInterior) => "↑↓/jk move · g/G top/bot · Esc back".into(),
                    Some(FocusTier::Input) => "Enter type · ↑/Esc back".into(),
                    Some(FocusTier::Insert) => "Enter send · Shift-Enter \\n · Esc".into(),
                    Some(FocusTier::Poll) => {
                        "↑↓ move · Space select · Enter submit · Esc skip".into()
                    }
                    Some(FocusTier::Files) => "↑↓ files · Enter diff · Esc back".into(),
                    Some(FocusTier::NewChatPicker) => {
                        "↑↓ pick instance · Enter ok · Esc cancel".into()
                    }
                    None => "press : for command, ? for help".into(),
                }
            } else {
                match active_view_id(app) {
                    Some("plans") => "↑↓ move · A approve · D decline · a all · r refresh".into(),
                    Some("tasks") | Some("skills") => {
                        "↑↓/jk move · Enter open · a all · r refresh".into()
                    }
                    Some("notifs") => "↑↓ move · 1-9 answer · d dismiss · a all · r refresh".into(),
                    _ => "Tab switch tab · ? help · q quit".into(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting + lookups
// ---------------------------------------------------------------------------

fn mode_label(app: &App) -> String {
    match app.mode {
        Mode::Command => "COMMAND".into(),
        Mode::Help => "HELP".into(),
        Mode::Normal => {
            if active_view_is_chat(app)
                && let Some(chat) = chat_view(app)
            {
                let tier = match chat.focus {
                    FocusTier::Sessions => "sessions",
                    FocusTier::ChatBlocks => "chat",
                    FocusTier::BlockInterior => "block",
                    FocusTier::Input => "input",
                    FocusTier::Insert => return "INSERT".into(),
                    FocusTier::Poll => return "POLL".into(),
                    FocusTier::Files => return "FILES".into(),
                    FocusTier::NewChatPicker => return "NEW CHAT".into(),
                };
                return format!("NORMAL · {tier}");
            }
            match active_view_id(app) {
                Some(id) => format!("NORMAL · {id}"),
                None => "NORMAL".into(),
            }
        }
    }
}

fn active_view_id(app: &App) -> Option<&'static str> {
    app.views.get(app.current_view).map(|v| v.id())
}

fn active_view_is_chat(app: &App) -> bool {
    active_view_id(app) == Some("chat")
}

fn chat_view(app: &App) -> Option<&ChatView> {
    app.views
        .iter()
        .find_map(|v| v.as_any().downcast_ref::<ChatView>())
}

fn fmt_k(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

fn push_sep(spans: &mut Vec<Span<'_>>) {
    spans.push(Span::styled(
        " │ ".to_string(),
        Style::default().add_modifier(Modifier::DIM),
    ));
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.chars().count()).sum()
}
