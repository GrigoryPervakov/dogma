//! Minimal multi-line text input widget.
//!
//! Replaces `tui-textarea` so we can stay on the latest ratatui / crossterm
//! without waiting for an upstream release. v1 covers what the chat input
//! actually needs: typing, navigation, basic editing, line wrapping, and a
//! configurable cursor style.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

#[derive(Debug, Clone)]
pub struct TextArea {
    lines: Vec<String>,
    /// Cursor row (line index).
    row: usize,
    /// Cursor column in *characters*, not bytes.
    col: usize,
    cursor_style: Style,
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new(vec![String::new()])
    }
}

impl TextArea {
    pub fn new(lines: Vec<String>) -> Self {
        let lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        let row = lines.len() - 1;
        let col = char_count(&lines[row]);
        Self {
            lines,
            row,
            col,
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn set_cursor_style(&mut self, s: Style) {
        self.cursor_style = s;
    }

    /// Compatibility shim for the old tui-textarea API; we don't actively
    /// style the cursor line beyond the cursor cell.
    pub fn set_cursor_line_style(&mut self, _s: Style) {}

    /// Feed a key event. The chat-input owner intercepts Enter / Esc /
    /// Ctrl+C before this runs, so those don't show up here in the
    /// expected paths — but we still handle them defensively.
    pub fn input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let _ = shift;
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => self.insert_char(c),
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            KeyCode::Char('u') if ctrl => self.delete_to_line_start(),
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = char_count(&self.lines[self.row]),
            KeyCode::Char('k') if ctrl => self.delete_to_line_end(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = char_count(&self.lines[self.row]),
            KeyCode::PageUp => {
                for _ in 0..10 {
                    self.move_up();
                }
            }
            KeyCode::PageDown => {
                for _ in 0..10 {
                    self.move_down();
                }
            }
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Tab => {
                self.insert_char(' ');
                self.insert_char(' ');
            }
            _ => {}
        }
    }

    // ----- editing primitives ---------------------------------------------

    fn insert_char(&mut self, c: char) {
        let byte = byte_at(&self.lines[self.row], self.col);
        self.lines[self.row].insert(byte, c);
        self.col += 1;
    }

    fn insert_newline(&mut self) {
        let row = std::mem::take(&mut self.lines[self.row]);
        let byte = byte_at(&row, self.col);
        let (left, right) = row.split_at(byte);
        self.lines[self.row] = left.to_string();
        self.lines.insert(self.row + 1, right.to_string());
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            let prev = self.col - 1;
            let row = &mut self.lines[self.row];
            let from = byte_at(row, prev);
            let to = byte_at(row, self.col);
            row.replace_range(from..to, "");
            self.col = prev;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = char_count(&self.lines[self.row]);
            self.lines[self.row].push_str(&cur);
        }
    }

    fn delete_forward(&mut self) {
        let line_chars = char_count(&self.lines[self.row]);
        if self.col < line_chars {
            let row = &mut self.lines[self.row];
            let from = byte_at(row, self.col);
            let to = byte_at(row, self.col + 1);
            row.replace_range(from..to, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn delete_word_back(&mut self) {
        if self.col == 0 {
            self.backspace();
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut new_col = self.col;
        while new_col > 0 && chars[new_col - 1].is_whitespace() {
            new_col -= 1;
        }
        while new_col > 0 && !chars[new_col - 1].is_whitespace() {
            new_col -= 1;
        }
        let row = &mut self.lines[self.row];
        let from = byte_at(row, new_col);
        let to = byte_at(row, self.col);
        row.replace_range(from..to, "");
        self.col = new_col;
    }

    fn delete_to_line_start(&mut self) {
        let row = &mut self.lines[self.row];
        let to = byte_at(row, self.col);
        row.replace_range(0..to, "");
        self.col = 0;
    }

    fn delete_to_line_end(&mut self) {
        let row = &mut self.lines[self.row];
        let from = byte_at(row, self.col);
        row.truncate(from);
    }

    // ----- cursor motion --------------------------------------------------

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = char_count(&self.lines[self.row]);
        }
    }

    fn move_right(&mut self) {
        let line_chars = char_count(&self.lines[self.row]);
        if self.col < line_chars {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(char_count(&self.lines[self.row]));
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(char_count(&self.lines[self.row]));
        }
    }
}

impl Widget for &TextArea {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line<'_>> = Vec::with_capacity(self.lines.len());
        for (r, line) in self.lines.iter().enumerate() {
            if r == self.row {
                lines.push(line_with_cursor(line, self.col, self.cursor_style));
            } else {
                lines.push(Line::raw(line.clone()));
            }
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn line_with_cursor(line: &str, col: usize, cursor_style: Style) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    if col > 0 {
        let prefix: String = chars[..col.min(chars.len())].iter().collect();
        spans.push(Span::raw(prefix));
    }
    if col < chars.len() {
        spans.push(Span::styled(chars[col].to_string(), cursor_style));
        if col + 1 < chars.len() {
            let suffix: String = chars[col + 1..].iter().collect();
            spans.push(Span::raw(suffix));
        }
    } else {
        spans.push(Span::styled(" ".to_string(), cursor_style));
    }
    Line::from(spans)
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

fn byte_at(s: &str, char_pos: usize) -> usize {
    s.char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn type_then_backspace() {
        let mut ta = TextArea::default();
        ta.input(k(KeyCode::Char('h')));
        ta.input(k(KeyCode::Char('i')));
        assert_eq!(ta.lines(), &["hi".to_string()]);
        ta.input(k(KeyCode::Backspace));
        assert_eq!(ta.lines(), &["h".to_string()]);
    }

    #[test]
    fn enter_splits_line() {
        let mut ta = TextArea::new(vec!["hello world".into()]);
        // Move to position 5 (after "hello").
        for _ in 0..6 {
            ta.input(k(KeyCode::Left));
        }
        ta.input(k(KeyCode::Enter));
        assert_eq!(ta.lines(), &["hello".to_string(), " world".to_string()]);
    }

    #[test]
    fn unicode_round_trip() {
        let mut ta = TextArea::default();
        ta.input(k(KeyCode::Char('ä')));
        ta.input(k(KeyCode::Char('日')));
        assert_eq!(ta.lines(), &["ä日".to_string()]);
        ta.input(k(KeyCode::Backspace));
        assert_eq!(ta.lines(), &["ä".to_string()]);
    }
}
