//! Top header — just the nav rail (one line). Connection state and context
//! usage live in the bottom statusbar.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::state::App;
use crate::ui::theme;
use crate::view::notifications::NotificationsView;

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let mut spans: Vec<Span<'_>> = Vec::new();
    spans.push(Span::styled("dogma ", theme::bold()));

    for (i, v) in app.views.iter().enumerate() {
        let active = i == app.current_view;
        // The notifications tab burns red while anything is pending.
        let alert = v
            .as_any()
            .downcast_ref::<NotificationsView>()
            .map(|n| n.has_pending())
            .unwrap_or(false);
        let style = match (alert, active) {
            (true, _) => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            (false, true) => theme::nav_active(),
            (false, false) => theme::nav_inactive(),
        };
        if active {
            spans.push(Span::raw(" ["));
            spans.push(Span::styled(v.title().to_string(), style));
            spans.push(Span::raw("] "));
        } else {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(v.title().to_string(), style));
            spans.push(Span::raw("  "));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
