//! Interactive poll (`AskUserQuestion`) — parsing, selection state, and card
//! rendering. Mirrors `web/src/components/Chat/tools/QuestionBlock.tsx`.
//!
//! A poll arrives as a pending `Interaction` of type `question`; its
//! `tool_input` carries `{ questions: [{ question, header, options, multiSelect }] }`.
//! The answer sent back is `{ <question text>: "<label[, label]>" }` — the
//! shape the backend folds into the tool input as `answers`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct PollOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct PollQuestion {
    pub question: String,
    pub header: String,
    pub options: Vec<PollOption>,
    pub multi_select: bool,
}

#[derive(Debug, Clone)]
pub struct Poll {
    pub questions: Vec<PollQuestion>,
}

impl Poll {
    /// Parse from an `AskUserQuestion` `tool_input`. Returns `None` if there
    /// are no usable questions.
    pub fn from_tool_input(input: &Value) -> Option<Poll> {
        let arr = input.get("questions")?.as_array()?;
        let mut questions = Vec::new();
        for q in arr {
            let options = q
                .get("options")
                .and_then(Value::as_array)
                .map(|opts| {
                    opts.iter()
                        .map(|o| PollOption {
                            label: str_field(o, "label"),
                            description: str_field(o, "description"),
                        })
                        .filter(|o| !o.label.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if options.is_empty() {
                continue;
            }
            questions.push(PollQuestion {
                question: str_field(q, "question"),
                header: str_field(q, "header"),
                options,
                multi_select: q
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
        if questions.is_empty() {
            return None;
        }
        Some(Poll { questions })
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Live selection state for the focused poll.
#[derive(Debug, Clone)]
pub struct PollUiState {
    pub interaction_id: String,
    pub poll: Poll,
    pub cursor_q: usize,
    pub cursor_o: usize,
    /// `selections[question][option]` — chosen flags.
    pub selections: Vec<Vec<bool>>,
}

impl PollUiState {
    pub fn new(interaction_id: String, poll: Poll) -> Self {
        let selections = poll
            .questions
            .iter()
            .map(|q| vec![false; q.options.len()])
            .collect();
        Self {
            interaction_id,
            poll,
            cursor_q: 0,
            cursor_o: 0,
            selections,
        }
    }

    pub fn move_option(&mut self, delta: isize) {
        let n = self.poll.questions[self.cursor_q].options.len();
        if n == 0 {
            return;
        }
        self.cursor_o = (self.cursor_o as isize + delta).rem_euclid(n as isize) as usize;
    }

    pub fn move_question(&mut self, delta: isize) {
        let n = self.poll.questions.len();
        if n <= 1 {
            return;
        }
        self.cursor_q = (self.cursor_q as isize + delta).rem_euclid(n as isize) as usize;
        self.cursor_o = 0;
    }

    /// Toggle (multi-select) or set (single-select) the highlighted option.
    pub fn select_current(&mut self) {
        let multi = self.poll.questions[self.cursor_q].multi_select;
        let sel = &mut self.selections[self.cursor_q];
        if multi {
            sel[self.cursor_o] = !sel[self.cursor_o];
        } else {
            sel.iter_mut().for_each(|s| *s = false);
            sel[self.cursor_o] = true;
        }
    }

    pub fn question_answered(&self, qi: usize) -> bool {
        self.selections
            .get(qi)
            .map(|s| s.iter().any(|&b| b))
            .unwrap_or(false)
    }

    pub fn all_answered(&self) -> bool {
        (0..self.poll.questions.len()).all(|i| self.question_answered(i))
    }

    /// One question, single-select — pick-and-submit immediately (web parity).
    pub fn is_single_simple(&self) -> bool {
        self.poll.questions.len() == 1 && !self.poll.questions[0].multi_select
    }

    /// Build the `{ question: "label[, label]" }` answers map for the wire.
    pub fn build_answers(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (qi, q) in self.poll.questions.iter().enumerate() {
            let labels: Vec<String> = self.selections[qi]
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(oi, _)| q.options[oi].label.clone())
                .collect();
            if !labels.is_empty() {
                map.insert(q.question.clone(), Value::String(labels.join(", ")));
            }
        }
        Value::Object(map)
    }
}

/// Render the poll card body. The caller wraps it in a bordered block.
pub fn render_lines(ui: &PollUiState) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let multi_q = ui.poll.questions.len() > 1;
    for (qi, q) in ui.poll.questions.iter().enumerate() {
        let active_q = qi == ui.cursor_q;
        let mut head: Vec<Span<'static>> = Vec::new();
        if multi_q {
            let marker = if active_q { "▾ " } else { "  " };
            head.push(Span::styled(marker, Style::default().fg(Color::Cyan)));
        }
        if !q.header.is_empty() {
            head.push(Span::styled(
                format!("[{}] ", q.header),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        head.push(Span::styled(
            q.question.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if q.multi_select {
            head.push(Span::styled(
                "  (select multiple)",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        out.push(Line::from(head));

        for (oi, opt) in q.options.iter().enumerate() {
            let selected = ui.selections[qi][oi];
            let on_cursor = active_q && oi == ui.cursor_o;
            let marker = match (q.multi_select, selected) {
                (true, true) => "[x]",
                (true, false) => "[ ]",
                (false, true) => "(•)",
                (false, false) => "( )",
            };
            let style = if on_cursor {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let prefix = if on_cursor { "▸ " } else { "  " };
            out.push(Line::from(Span::styled(
                format!("{prefix}{marker} {}", opt.label),
                style,
            )));
            if on_cursor && !opt.description.is_empty() {
                out.push(Line::from(Span::styled(
                    format!("       {}", opt.description),
                    Style::default().add_modifier(Modifier::DIM),
                )));
            }
        }
        out.push(Line::raw(""));
    }

    let hint = if ui.is_single_simple() {
        "↑↓ move · Enter pick · Esc skip"
    } else {
        "↑↓ move · ←→ question · Space select · Enter submit · Esc skip"
    };
    out.push(Line::from(Span::styled(
        hint,
        Style::default().add_modifier(Modifier::DIM),
    )));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Poll {
        Poll::from_tool_input(&json!({
            "questions": [
                {"question": "Pick colors", "header": "Colors", "multiSelect": true,
                 "options": [{"label": "Red", "description": "r"},
                             {"label": "Green", "description": ""},
                             {"label": "Blue", "description": "b"}]},
                {"question": "Ship?", "header": "Ship", "multiSelect": false,
                 "options": [{"label": "Yes", "description": ""},
                             {"label": "No", "description": ""}]}
            ]
        }))
        .expect("parse")
    }

    #[test]
    fn parses_questions_options_and_multiselect() {
        let p = sample();
        assert_eq!(p.questions.len(), 2);
        assert!(p.questions[0].multi_select);
        assert!(!p.questions[1].multi_select);
        assert_eq!(p.questions[0].options.len(), 3);
        assert_eq!(p.questions[1].options[0].label, "Yes");
    }

    #[test]
    fn empty_or_optionless_questions_yield_none() {
        assert!(Poll::from_tool_input(&json!({"questions": []})).is_none());
        assert!(
            Poll::from_tool_input(&json!({"questions": [{"question": "q", "options": []}]}))
                .is_none()
        );
        assert!(Poll::from_tool_input(&json!({})).is_none());
    }

    #[test]
    fn single_select_replaces_multi_select_toggles() {
        let mut ui = PollUiState::new("i".into(), sample());
        // Q0 multi-select: toggle Red and Blue.
        ui.select_current(); // Red
        ui.cursor_o = 2;
        ui.select_current(); // Blue
        assert_eq!(ui.selections[0], vec![true, false, true]);
        // Q1 single-select: choosing replaces.
        ui.cursor_q = 1;
        ui.cursor_o = 1;
        ui.select_current(); // No
        ui.cursor_o = 0;
        ui.select_current(); // Yes — clears No
        assert_eq!(ui.selections[1], vec![true, false]);
    }

    #[test]
    fn build_answers_joins_multiselect_labels_keyed_by_question() {
        let mut ui = PollUiState::new("i".into(), sample());
        ui.selections[0] = vec![true, false, true]; // Red, Blue
        ui.selections[1] = vec![false, true]; // No
        let ans = ui.build_answers();
        assert_eq!(ans["Pick colors"], json!("Red, Blue"));
        assert_eq!(ans["Ship?"], json!("No"));
    }

    #[test]
    fn all_answered_requires_every_question() {
        let mut ui = PollUiState::new("i".into(), sample());
        assert!(!ui.all_answered());
        ui.selections[0][0] = true;
        assert!(!ui.all_answered());
        ui.selections[1][0] = true;
        assert!(ui.all_answered());
    }
}
