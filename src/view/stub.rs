//! Stub view — placeholder for tabs not yet implemented in v1.

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::view::{View, ViewCtx, ViewRenderCtx};

pub struct StubView {
    id: &'static str,
    title: String,
}

impl StubView {
    pub fn new(id: &'static str, title: &str) -> Self {
        Self {
            id,
            title: title.into(),
        }
    }
}

impl View for StubView {
    fn id(&self) -> &'static str {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn selectable(&self) -> bool {
        false
    }

    fn handle_key(&mut self, _key: KeyEvent, _ctx: &mut ViewCtx) {}

    fn render(&mut self, area: Rect, frame: &mut Frame, _ctx: ViewRenderCtx<'_>) {
        let lines = vec![
            Line::from(""),
            Line::from(""),
            Line::from(Span::styled(
                format!("{} — not yet implemented", self.title),
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from("press : then `chat` to go back"),
        ];
        let p = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::NONE));
        frame.render_widget(p, area);
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
