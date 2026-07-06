//! UI — top-level frame: header, body, statusbar.

pub mod header;
pub mod highlight;
pub mod markdown;
pub mod statusbar;
pub mod textarea;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::state::App;
use crate::view::ViewRenderCtx;

/// Char-safe truncation with a trailing ellipsis — the single copy shared by
/// every list/title/label that needs to fit text into a fixed column budget.
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    let layout = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(0),    // body (delegated to active view)
        Constraint::Length(1), // statusbar
    ])
    .split(area);

    header::render(app, frame, layout[0]);
    render_body(app, frame, layout[1]);
    statusbar::render(app, frame, layout[2]);

    if matches!(app.mode, crate::app::state::Mode::Help) {
        render_help_overlay(frame, area);
    }
}

fn render_body(app: &mut App, frame: &mut Frame, area: Rect) {
    let mode = app.mode;
    let cmd_buf = app.command_buffer.clone();
    let instances = app.instances.clone();
    let ctx = ViewRenderCtx {
        mode,
        command_buffer: &cmd_buf,
        instances: &instances,
    };
    let idx = app.current_view;
    app.views[idx].render(area, frame, ctx);
}

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    let head = |s: &str| {
        Line::from(Span::styled(
            s.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let row = |s: &str| Line::from(s.to_string());

    let lines = vec![
        head("GLOBAL"),
        row("  Tab / S-Tab  cycle tabs    :  command    ?  help    q  quit"),
        row("  Ctrl+C  stop the running agent (or quit)"),
        head("COMMANDS   (Tab autocompletes; singular or plural)"),
        row("  tabs     :chat  :notifs  :tasks  :plans  :skills"),
        row("  actions  :new  :fork  :resume  :rename  :delete  :reload  :q"),
        head("CHAT"),
        row("  Sessions  ↑↓/jk walk · Enter/→ open · / search"),
        row("            multi-instance: rows tagged ‹sigil name›; Enter on"),
        row("            + new chat asks which instance it lands on"),
        row("  Messages  ↑↓/jk walk · g/G top/bot · PgUp/Dn · Enter expand"),
        row("            i input · Ctrl+B sidebar · Ctrl+P panel · Ctrl+F files"),
        row("  Block     ↑↓/jk scroll · g/G top/bot · PgUp/Dn · Esc/← back"),
        row("  Input     Enter → insert · any key types · ↑/Esc back"),
        row("  Insert    Enter send · Shift-Enter newline · Esc back"),
        row("            Alt+←/→ word · Alt+Bksp del word · Up/← at edge → blocks"),
        row("  Poll      ↑↓ option · ←→ question · Space select · Enter ok · Esc skip"),
        row("  Plan      shows as the latest block · a approve · d decline (a chat"),
        row("            or block view; navigate freely without answering)"),
        row("  Files     ↑↓ list · Enter diff · ↑↓ scroll · r refresh · Esc back"),
        head("NOTIFS"),
        row("  ↑↓/jk move · 1-9 answer poll · d dismiss · a all · r refresh"),
        row("  (pending only by default; a toggles answered/dismissed)"),
        head("TASKS · PLANS · SKILLS"),
        row("  ↑↓/jk move · Enter/→ detail · a all · r refresh"),
        row("  (lists are time-sorted; a shows done/declined items)"),
        row("  Plans   A approve · D decline a pending plan"),
        Line::from(""),
        Line::from(Span::styled(
            "press any key to close",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];

    let w = 76.min(area.width.saturating_sub(4));
    let h = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let centered = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    frame.render_widget(Clear, centered);
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" help ")
            .style(Style::default()),
    );
    frame.render_widget(p, centered);
}
