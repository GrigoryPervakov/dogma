//! Tasks tab — read-only list + detail (see [`crate::view::list_detail`]).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::api::types::HttpReq;
use crate::model::Task;
use crate::view::list_detail::{ListDetail, ListDetailModel, meta_line, truncate};

pub type TasksView = ListDetail<TaskModel>;

pub struct TaskModel;

impl ListDetailModel for TaskModel {
    type Item = Task;
    const ID: &'static str = "tasks";
    const TITLE: &'static str = "Tasks";
    const EMPTY_TITLE: &'static str = "(no task selected)";

    fn list_req() -> HttpReq {
        HttpReq::ListTasks
    }
    fn detail_req(id: &str) -> HttpReq {
        HttpReq::GetTask {
            task_id: id.to_string(),
        }
    }
    fn item_id(t: &Task) -> &str {
        &t.id
    }
    fn detail_title(t: &Task) -> &str {
        &t.title
    }
    fn detail_body(t: &Task) -> Option<&str> {
        t.content.as_deref()
    }

    fn list_line(t: &Task) -> Line<'static> {
        let (glyph, glyph_style) = match t.status.as_str() {
            "done" | "completed" => ("✓", Style::default().fg(Color::Green)),
            "in_progress" | "active" => ("❯", Style::default().fg(Color::Yellow)),
            "blocked" => ("⨯", Style::default().fg(Color::Red)),
            _ => ("•", Style::default().add_modifier(Modifier::DIM)),
        };
        Line::from(vec![
            Span::styled(glyph.to_string(), glyph_style),
            Span::raw(" "),
            Span::styled(truncate(&t.title, 32), Style::default().fg(Color::White)),
        ])
    }

    fn detail_meta(t: &Task) -> Vec<Line<'static>> {
        let mut out = vec![meta_line("status", &t.status, 10)];
        if let Some(d) = t.deadline.as_deref() {
            out.push(meta_line("deadline", d, 10));
        }
        if let Some(s) = t.source.as_deref() {
            out.push(meta_line("source", s, 10));
        }
        if let Some(u) = t.source_url.as_deref() {
            out.push(meta_line("url", u, 10));
        }
        if let Some(c) = t.created_at.as_deref() {
            out.push(meta_line("created", c, 10));
        }
        out
    }
}
