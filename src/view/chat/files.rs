//! Changed-files browser — the session's modified-file list plus a scrollable
//! per-file diff. Driven by the `Files` focus tier.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::state::ChatView;
use crate::model::FileDiff;

pub fn render(view: &ChatView, frame: &mut Frame, area: Rect) {
    match view.open_diff.as_ref() {
        Some(diff) => render_diff(diff, view.diff_scroll, frame, area),
        None => render_list(view, frame, area),
    }
}

fn framed(title: String, area: Rect, frame: &mut Frame) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn status_glyph(status: &str) -> (&'static str, Style) {
    match status {
        "created" => ("A", Style::default().fg(Color::Green)),
        "deleted" => ("D", Style::default().fg(Color::Red)),
        _ => ("M", Style::default().fg(Color::Yellow)),
    }
}

fn render_list(view: &ChatView, frame: &mut Frame, area: Rect) {
    let files = view.current_modified_files();
    let title = if view.files_loading {
        " changed files · loading… ".to_string()
    } else {
        format!(" changed files ({}) · Enter diff · Esc back ", files.len())
    };
    let inner = framed(title, area, frame);

    if files.is_empty() {
        let msg = if view.files_loading {
            "loading…"
        } else {
            "no files changed in this session yet"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().add_modifier(Modifier::DIM),
            ))),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = files
        .iter()
        .map(|f| {
            let (g, gs) = status_glyph(&f.status);
            let name = if f.short_path.is_empty() {
                &f.path
            } else {
                &f.short_path
            };
            let mut spans = vec![
                Span::styled(format!("{g} "), gs),
                Span::styled(name.clone(), Style::default().fg(Color::White)),
            ];
            if f.stats.additions > 0 {
                spans.push(Span::styled(
                    format!("  +{}", f.stats.additions),
                    Style::default().fg(Color::Green),
                ));
            }
            if f.stats.deletions > 0 {
                spans.push(Span::styled(
                    format!(" -{}", f.stats.deletions),
                    Style::default().fg(Color::Red),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).highlight_symbol("▶ ").highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    state.select(Some(view.files_selected.min(files.len() - 1)));
    frame.render_stateful_widget(list, inner, &mut state);
}

fn render_diff(diff: &FileDiff, scroll: usize, frame: &mut Frame, area: Rect) {
    let name = if diff.short_path.is_empty() {
        &diff.path
    } else {
        &diff.short_path
    };
    let title = format!(" {name} · ↑↓ scroll · Esc back ");
    let inner = framed(title, area, frame);

    let lines = diff_lines(diff);
    let max_scroll = lines.len().saturating_sub(1) as u16;
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll(((scroll as u16).min(max_scroll), 0));
    frame.render_widget(para, inner);
}

fn diff_lines(diff: &FileDiff) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(Line::from(vec![
        Span::styled(
            format!("+{}", diff.stats.additions),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" "),
        Span::styled(
            format!("-{}", diff.stats.deletions),
            Style::default().fg(Color::Red),
        ),
        Span::styled(
            format!("  ({})", diff.status),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]));
    if diff.binary {
        out.push(Line::from(Span::styled(
            "binary file — no diff",
            Style::default().add_modifier(Modifier::DIM),
        )));
        return out;
    }
    out.push(Line::raw(""));
    for hunk in &diff.hunks {
        out.push(Line::from(Span::styled(
            hunk.header.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
        )));
        for l in &hunk.lines {
            let (prefix, style) = match l.kind.as_str() {
                "addition" => ("+", Style::default().fg(Color::Green)),
                "deletion" => ("-", Style::default().fg(Color::Red)),
                "info" => (" ", Style::default().add_modifier(Modifier::DIM)),
                _ => (" ", Style::default()),
            };
            out.push(Line::from(Span::styled(
                format!("{prefix}{}", l.content),
                style,
            )));
        }
    }
    if diff.truncated {
        out.push(Line::from(Span::styled(
            "… diff truncated",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    out
}
