//! Markdown renderer — pulldown-cmark walk → ratatui Lines, with syntect
//! integration for fenced code blocks.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::highlight;

pub fn render_markdown(src: &str) -> Vec<Line<'static>> {
    // GitHub-flavoured extensions: without these, tables / ~~strike~~ / task
    // lists never parse and render as raw text.
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let parser = Parser::new_ext(src, opts);
    let mut walker = Walker::default();
    for ev in parser {
        walker.handle(ev);
    }
    walker.flush();
    walker.lines
}

/// Buffered table state — cells stream in as text, rendered as an aligned grid
/// on `TagEnd::Table`.
#[derive(Default)]
struct TableState {
    alignments: Vec<Alignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    cur_row: Vec<String>,
    cur_cell: String,
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
    table: Option<TableState>,
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
                if let Some(tbl) = self.table.as_mut() {
                    tbl.cur_cell.push_str(&t);
                } else if self.in_code_block {
                    self.code_buf.push_str(&t);
                } else {
                    let style = self.cur_style();
                    self.cur.push(Span::styled(t.into_string(), style));
                }
            }
            Event::Code(t) => {
                if let Some(tbl) = self.table.as_mut() {
                    tbl.cur_cell.push('`');
                    tbl.cur_cell.push_str(&t);
                    tbl.cur_cell.push('`');
                } else {
                    let s = self
                        .cur_style()
                        .patch(Style::default().fg(Color::Yellow).bg(Color::Reset));
                    self.cur.push(Span::styled(format!("`{t}`"), s));
                }
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
            Tag::Table(aligns) => {
                self.flush();
                self.table = Some(TableState {
                    alignments: aligns,
                    ..Default::default()
                });
            }
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.cur_row.clear();
                }
            }
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.cur_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(t) = self.table.as_mut() {
                    t.cur_cell.clear();
                }
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
                self.lines.extend(render_code_block(lang.as_deref(), &code));
            }
            TagEnd::List(_) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                self.ordered_counters.pop();
            }
            TagEnd::Item => {
                self.flush();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::TableCell => {
                if let Some(t) = self.table.as_mut() {
                    let cell = std::mem::take(&mut t.cur_cell);
                    t.cur_row.push(cell.trim().to_string());
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.header = std::mem::take(&mut t.cur_row);
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.cur_row);
                    if !row.is_empty() {
                        t.rows.push(row);
                    }
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.lines.extend(render_table(&t));
                    self.lines.push(Line::raw(""));
                }
            }
            _ => {}
        }
    }
}

/// Render a fenced code block as a closed box: a `lang`-labelled top border, a
/// header separator, syntax-highlighted lines with left+right borders, and a
/// bottom border. The box grows to the widest line.
fn render_code_block(lang: Option<&str>, code: &str) -> Vec<Line<'static>> {
    let highlighted = highlight::highlight_code(code, lang);
    let label = lang.unwrap_or("code");
    let line_w = |l: &Line| {
        l.spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
    };
    // Inner content width — the widest code line, but wide enough for the label.
    let mut width = label.chars().count() + 1;
    for l in &highlighted {
        width = width.max(line_w(l));
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut out = Vec::new();
    // Top border with the embedded language label: ┌─ rust ───┐
    let fill = (width + 2).saturating_sub(label.chars().count() + 3);
    out.push(Line::from(Span::styled(
        format!("┌─ {label} {}┐", "─".repeat(fill)),
        dim,
    )));
    // Separator line after the header.
    out.push(Line::from(Span::styled(
        format!("├{}┤", "─".repeat(width + 2)),
        dim,
    )));
    for l in &highlighted {
        let pad = width.saturating_sub(line_w(l));
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(l.spans.len() + 3);
        spans.push(Span::styled("│ ", dim));
        for sp in &l.spans {
            spans.push(Span::styled(sp.content.clone().into_owned(), sp.style));
        }
        spans.push(Span::raw(" ".repeat(pad + 1)));
        spans.push(Span::styled("│", dim));
        out.push(Line::from(spans));
    }
    out.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(width + 2)),
        dim,
    )));
    out
}

/// Render a buffered table as an aligned, box-drawn grid.
fn render_table(t: &TableState) -> Vec<Line<'static>> {
    let ncols = t
        .header
        .len()
        .max(t.rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if ncols == 0 {
        return Vec::new();
    }
    let mut widths = vec![0usize; ncols];
    let mut measure = |cells: &[String]| {
        for (i, c) in cells.iter().enumerate() {
            if i < ncols {
                widths[i] = widths[i].max(c.chars().count());
            }
        }
    };
    measure(&t.header);
    for r in &t.rows {
        measure(r);
    }

    let dim = Style::default().add_modifier(Modifier::DIM);
    let border = |left: char, mid: char, right: char| -> Line<'static> {
        let mut s = String::new();
        s.push(left);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push(if i + 1 < ncols { mid } else { right });
        }
        Line::from(Span::styled(s, dim))
    };
    let fmt_cell = |s: &str, i: usize| -> String {
        // Cells are never truncated — columns grow to their widest cell.
        let pad = widths[i].saturating_sub(s.chars().count());
        match t.alignments.get(i).copied().unwrap_or(Alignment::None) {
            Alignment::Right => format!("{}{s}", " ".repeat(pad)),
            Alignment::Center => {
                let l = pad / 2;
                format!("{}{s}{}", " ".repeat(l), " ".repeat(pad - l))
            }
            _ => format!("{s}{}", " ".repeat(pad)),
        }
    };
    let row_line = |cells: &[String], style: Style| -> Line<'static> {
        let mut spans = vec![Span::styled("│", dim)];
        for i in 0..ncols {
            let c = cells.get(i).map(String::as_str).unwrap_or("");
            spans.push(Span::styled(format!(" {} ", fmt_cell(c, i)), style));
            spans.push(Span::styled("│", dim));
        }
        Line::from(spans)
    };

    let mut out = vec![border('┌', '┬', '┐')];
    if !t.header.is_empty() {
        out.push(row_line(
            &t.header,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        out.push(border('├', '┼', '┤'));
    }
    for r in &t.rows {
        out.push(row_line(r, Style::default()));
    }
    out.push(border('└', '┴', '┘'));
    out
}

#[cfg(test)]
mod tests {
    use super::render_markdown;

    fn text(lines: &[ratatui::text::Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_gfm_table_as_box_grid() {
        let md = "| Name | Age |\n|------|----:|\n| Alice | 30 |\n| Bob | 5 |";
        let out = text(&render_markdown(md));
        assert!(out.contains('┌') && out.contains('┐'), "top border: {out}");
        assert!(out.contains('├') && out.contains('┼'), "header sep: {out}");
        assert!(out.contains("Name") && out.contains("Age"));
        assert!(out.contains("Alice") && out.contains("Bob"));
        // Right-aligned Age column keeps the digits flush right.
        assert!(out.contains("└"));
    }

    #[test]
    fn extensions_parse_strikethrough_and_tasklist() {
        // Without ENABLE_* options these render as raw markup.
        let strike = text(&render_markdown("~~gone~~"));
        assert!(strike.contains("gone") && !strike.contains("~~"));
        let task = text(&render_markdown("- [x] done\n- [ ] todo"));
        assert!(task.contains("[x]") && task.contains("[ ]"));
    }

    #[test]
    fn renders_code_block_as_closed_box() {
        let out = text(&render_markdown("```rust\nfn x() {}\nlet y = 1;\n```"));
        assert!(out.contains('┌') && out.contains('┐'), "top corners: {out}");
        assert!(out.contains('├') && out.contains('┤'), "header separator");
        assert!(out.contains('└') && out.contains('┘'), "bottom corners");
        assert!(out.contains("rust"));
        // Every box row is the same display width (closed, aligned).
        let widths: Vec<usize> = out
            .lines()
            .filter(|l| l.starts_with(['┌', '├', '│', '└']))
            .map(|l| l.chars().count())
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "all box rows equal width: {widths:?}"
        );
    }
}
