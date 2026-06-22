//! Markdown renderer — pulldown-cmark walk → ratatui Lines, with syntect
//! integration for fenced code blocks.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::highlight;

pub fn render_markdown(src: &str) -> Vec<Line<'static>> {
    let parser = Parser::new(src);
    let mut walker = Walker::default();
    for ev in parser {
        walker.handle(ev);
    }
    walker.flush();
    walker.lines
}

#[derive(Default)]
struct Walker {
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    in_code_block: bool,
    code_lang: Option<String>,
    code_buf: String,
    list_depth: u8,
    ordered_counters: Vec<u64>,
}

impl Walker {
    fn cur_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, s: Style) {
        let merged = self.cur_style().patch(s);
        self.style_stack.push(merged);
    }

    fn pop_style(&mut self) {
        self.style_stack.pop();
    }

    fn newline(&mut self) {
        let line = std::mem::take(&mut self.cur);
        self.lines.push(Line::from(line));
    }

    fn flush(&mut self) {
        if !self.cur.is_empty() {
            self.newline();
        }
    }

    fn handle(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag_end) => self.end(tag_end),
            Event::Text(t) => {
                if self.in_code_block {
                    self.code_buf.push_str(&t);
                } else {
                    let style = self.cur_style();
                    self.cur.push(Span::styled(t.into_string(), style));
                }
            }
            Event::Code(t) => {
                let s = self
                    .cur_style()
                    .patch(Style::default().fg(Color::Yellow).bg(Color::Reset));
                self.cur.push(Span::styled(format!("`{t}`"), s));
            }
            Event::SoftBreak | Event::HardBreak => {
                self.newline();
            }
            Event::Rule => {
                self.flush();
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().add_modifier(Modifier::DIM),
                )));
            }
            Event::Html(_) | Event::InlineHtml(_) | Event::FootnoteReference(_) => {}
            Event::TaskListMarker(checked) => {
                let m = if checked { "[x] " } else { "[ ] " };
                self.cur.push(Span::raw(m));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush();
                let style = match level {
                    HeadingLevel::H1 => Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    HeadingLevel::H2 => Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    _ => Style::default().add_modifier(Modifier::BOLD),
                };
                self.push_style(style);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.cur.push(Span::styled(
                    "│ ",
                    Style::default().add_modifier(Modifier::DIM),
                ));
                self.push_style(Style::default().add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.in_code_block = true;
                self.code_buf.clear();
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(s) => {
                        let s = s.into_string();
                        if s.is_empty() { None } else { Some(s) }
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Tag::List(start) => {
                self.flush();
                self.list_depth = self.list_depth.saturating_add(1);
                self.ordered_counters.push(start.unwrap_or(0));
            }
            Tag::Item => {
                self.flush();
                let indent = "  ".repeat(self.list_depth.saturating_sub(1) as usize);
                let bullet = if let Some(last) = self.ordered_counters.last_mut() {
                    if *last > 0 {
                        let n = *last;
                        *last += 1;
                        format!("{n}. ")
                    } else {
                        "• ".into()
                    }
                } else {
                    "• ".into()
                };
                self.cur.push(Span::raw(format!("{indent}{bullet}")));
            }
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                let s = self.cur_style().patch(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                );
                self.cur.push(Span::styled(format!("({dest_url})"), s));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag_end: TagEnd) {
        match tag_end {
            TagEnd::Paragraph => {
                self.flush();
                self.lines.push(Line::raw(""));
            }
            TagEnd::Heading(_) => {
                self.flush();
                self.pop_style();
                self.lines.push(Line::raw(""));
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.pop_style();
            }
            TagEnd::CodeBlock => {
                let lang = self.code_lang.take();
                let code = std::mem::take(&mut self.code_buf);
                self.in_code_block = false;

                self.lines.push(Line::from(Span::styled(
                    format!("┌─ {} ─", lang.as_deref().unwrap_or("code")),
                    Style::default().add_modifier(Modifier::DIM),
                )));
                let highlighted = highlight::highlight_code(&code, lang.as_deref());
                for line in highlighted {
                    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
                    spans.push(Span::styled(
                        "│ ".to_string(),
                        Style::default().add_modifier(Modifier::DIM),
                    ));
                    for sp in line.spans {
                        spans.push(Span::styled(sp.content.into_owned(), sp.style));
                    }
                    self.lines.push(Line::from(spans));
                }
                self.lines.push(Line::from(Span::styled(
                    "└".to_string() + &"─".repeat(40),
                    Style::default().add_modifier(Modifier::DIM),
                )));
            }
            TagEnd::List(_) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                self.ordered_counters.pop();
            }
            TagEnd::Item => {
                self.flush();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            _ => {}
        }
    }
}
