//! Claude Code task panel — the agent's live to-do list (TaskCreate /
//! TaskUpdate / TaskList tool calls) plus run_in_background jobs, rendered as
//! a pinned block at the bottom of the chat. Mirrors the web
//! `stores/helpers/ccTasks.ts` reducer, reconstructed by folding over the
//! session's tool-call blocks.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::model::{Block, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcStatus {
    Pending,
    InProgress,
    Completed,
}

impl CcStatus {
    fn parse(s: &str) -> Option<CcStatus> {
        match s {
            "pending" => Some(CcStatus::Pending),
            "in_progress" => Some(CcStatus::InProgress),
            "completed" => Some(CcStatus::Completed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CcTask {
    pub id: String,
    pub subject: String,
    pub active_form: Option<String>,
    pub status: CcStatus,
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Reconstruct the current task list by replaying every Task* tool call in
/// chronological order across history + the in-flight streaming turn.
pub fn extract(history: &[Message], streaming: Option<&Message>) -> Vec<CcTask> {
    let mut tasks: Vec<CcTask> = Vec::new();
    for m in history.iter().chain(streaming) {
        for b in &m.blocks {
            let Block::ToolCall(tc) = b else { continue };
            match tc.tool.as_str() {
                "TaskCreate" => {
                    apply_create_input(&mut tasks, &tc.input, &tc.tool_use_id);
                    if let Some(r) = tc.result.as_deref() {
                        parse_create_result(&mut tasks, r, &tc.tool_use_id);
                    }
                }
                "TaskUpdate" => apply_update_input(&mut tasks, &tc.input),
                "TaskList" => {
                    if let Some(r) = tc.result.as_deref() {
                        tasks = parse_list_result(r, &tasks);
                    }
                }
                _ => {}
            }
        }
    }
    sort_tasks(&mut tasks);
    tasks
}

fn sort_tasks(tasks: &mut [CcTask]) {
    tasks.sort_by(|a, b| {
        let pa = a.id.starts_with("pending:");
        let pb = b.id.starts_with("pending:");
        match (pa, pb) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => match (a.id.parse::<u64>(), b.id.parse::<u64>()) {
                (Ok(na), Ok(nb)) => na.cmp(&nb),
                _ => a.id.cmp(&b.id),
            },
        }
    });
}

fn apply_create_input(tasks: &mut Vec<CcTask>, input: &Value, tool_use_id: &str) {
    let Some(subject) = str_field(input, "subject") else {
        return;
    };
    if subject.is_empty() {
        return;
    }
    let id = format!("pending:{tool_use_id}");
    if tasks.iter().any(|t| t.id == id) {
        return;
    }
    tasks.push(CcTask {
        id,
        subject,
        active_form: str_field(input, "activeForm"),
        status: CcStatus::Pending,
    });
}

/// `Task #N created successfully: SUBJECT` — swap the placeholder row for the
/// real numeric id.
fn parse_create_result(tasks: &mut [CcTask], text: &str, tool_use_id: &str) {
    let Some((id, subject)) = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Task #"))
        .and_then(|body| body.split_once(' '))
        .and_then(|(id, tail)| {
            let subject = tail.strip_prefix("created successfully:")?.trim();
            if !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()) {
                Some((id.to_string(), subject.to_string()))
            } else {
                None
            }
        })
    else {
        return;
    };
    let placeholder = format!("pending:{tool_use_id}");
    if let Some(t) = tasks.iter_mut().find(|t| t.id == placeholder) {
        t.id = id;
        t.subject = subject;
    }
}

fn apply_update_input(tasks: &mut Vec<CcTask>, input: &Value) {
    let task_id = input.get("taskId").and_then(|v| {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    });
    let Some(task_id) = task_id else { return };
    let status_raw = input.get("status").and_then(Value::as_str);
    if status_raw == Some("deleted") {
        tasks.retain(|t| t.id != task_id);
        return;
    }
    let status = status_raw.and_then(CcStatus::parse);
    let subject = str_field(input, "subject");
    let active_form = str_field(input, "activeForm");
    if let Some(t) = tasks.iter_mut().find(|t| t.id == task_id) {
        if let Some(s) = status {
            t.status = s;
        }
        if let Some(s) = subject {
            t.subject = s;
        }
        if active_form.is_some() {
            t.active_form = active_form;
        }
    } else if subject.is_some() || status.is_some() {
        tasks.push(CcTask {
            id: task_id.clone(),
            subject: subject.unwrap_or_else(|| format!("Task #{task_id}")),
            active_form,
            status: status.unwrap_or(CcStatus::Pending),
        });
    }
}

/// `#N [status] subject (owner) [blocked by #X]` per line, or `No tasks found`.
fn parse_list_result(text: &str, current: &[CcTask]) -> Vec<CcTask> {
    if text.trim().is_empty() || text.trim().eq_ignore_ascii_case("no tasks found") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((id, status, subject)) = parse_list_line(line) else {
            continue;
        };
        // TaskList omits activeForm — keep any we already had.
        let active_form = current
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.active_form.clone());
        out.push(CcTask {
            id,
            subject,
            active_form,
            status,
        });
    }
    out
}

fn parse_list_line(line: &str) -> Option<(String, CcStatus, String)> {
    let rest = line.trim().strip_prefix('#')?;
    let (id, after) = rest.split_once(' ')?;
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let after = after.trim_start().strip_prefix('[')?;
    let (status_s, subject_part) = after.split_once(']')?;
    let status = CcStatus::parse(status_s.trim())?;
    let mut subject = subject_part.trim().to_string();
    // Drop the optional " [blocked by ...]" suffix; keep "(owner)" inline.
    if let Some(i) = subject.find(" [blocked by ") {
        subject.truncate(i);
    }
    Some((id.to_string(), status, subject.trim().to_string()))
}

/// A background job the backend still considers live. Finished jobs linger in
/// the registry marked `done`/`failed`, so the panel filters them out.
fn job_running(j: &Value) -> bool {
    !matches!(
        j.get("status").and_then(Value::as_str),
        Some("done" | "completed" | "stopped" | "failed" | "timeout" | "cancelled" | "error")
    )
}

/// Body lines for the bottom tasks/jobs panel, or `None` when there is nothing
/// *active* to show — every task completed and no job running. The panel is a
/// live tracker, so it collapses (and the input reclaims the row) once work is
/// done rather than pinning a stale all-completed list over the editor.
pub fn panel_lines(tasks: &[CcTask], bg: &[Value]) -> Option<Vec<Line<'static>>> {
    let show_tasks = tasks.iter().any(|t| t.status != CcStatus::Completed);
    let running: Vec<&Value> = bg.iter().filter(|j| job_running(j)).collect();
    if !show_tasks && running.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    // While any task is incomplete, show the full list (completed included) so
    // progress reads clearly; once all are done the task section disappears.
    if show_tasks {
        for t in tasks {
            let (glyph, glyph_style) = match t.status {
                CcStatus::Completed => ("✓", Style::default().fg(Color::Green)),
                CcStatus::InProgress => ("◐", Style::default().fg(Color::Yellow)),
                CcStatus::Pending => ("○", Style::default().add_modifier(Modifier::DIM)),
            };
            let label = if t.status == CcStatus::InProgress {
                t.active_form.clone().unwrap_or_else(|| t.subject.clone())
            } else {
                t.subject.clone()
            };
            let label_style = match t.status {
                CcStatus::Completed => {
                    Style::default().add_modifier(Modifier::DIM | Modifier::CROSSED_OUT)
                }
                CcStatus::InProgress => Style::default().add_modifier(Modifier::BOLD),
                CcStatus::Pending => Style::default(),
            };
            out.push(Line::from(vec![
                Span::styled(format!(" {glyph} "), glyph_style),
                Span::styled(label, label_style),
            ]));
        }
    }

    if !running.is_empty() {
        out.push(Line::from(Span::styled(
            "background jobs",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for j in running {
            let label = j
                .get("label")
                .and_then(Value::as_str)
                .or_else(|| j.get("tool").and_then(Value::as_str))
                .unwrap_or("job");
            out.push(Line::from(vec![
                Span::styled(" ⠿ ", Style::default().fg(Color::Yellow)),
                Span::raw(label.to_string()),
                Span::styled("  [running]", Style::default().add_modifier(Modifier::DIM)),
            ]));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Role, ToolCall, ToolCallStatus};
    use serde_json::json;

    fn tool(name: &str, id: &str, input: Value, result: Option<&str>) -> Block {
        Block::ToolCall(ToolCall {
            tool_use_id: id.into(),
            tool: name.into(),
            input,
            result: result.map(str::to_string),
            is_error: false,
            status: ToolCallStatus::Complete,
            parent_tool_use_id: None,
        })
    }

    fn msg(blocks: Vec<Block>) -> Message {
        Message {
            id: None,
            session_id: "s".into(),
            role: Role::Assistant,
            blocks,
            content: None,
            thinking: None,
            created_at: None,
            channel: None,
        }
    }

    #[test]
    fn create_then_update_resolves_id_and_status() {
        let h = vec![msg(vec![
            tool(
                "TaskCreate",
                "tu1",
                json!({"subject": "Build it", "activeForm": "Building it"}),
                Some("Task #1 created successfully: Build it"),
            ),
            tool(
                "TaskUpdate",
                "tu2",
                json!({"taskId": "1", "status": "in_progress"}),
                None,
            ),
        ])];
        let tasks = extract(&h, None);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[0].subject, "Build it");
        assert_eq!(tasks[0].status, CcStatus::InProgress);
        assert_eq!(tasks[0].active_form.as_deref(), Some("Building it"));
    }

    #[test]
    fn task_list_replaces_and_sorts_numerically() {
        let h = vec![msg(vec![tool(
            "TaskList",
            "tl",
            json!({}),
            Some("#2 [completed] Second\n#1 [pending] First\ngarbage"),
        )])];
        let tasks = extract(&h, None);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[1].id, "2");
        assert_eq!(tasks[1].status, CcStatus::Completed);
    }

    #[test]
    fn no_tasks_found_clears_the_list() {
        let h = vec![msg(vec![
            tool(
                "TaskCreate",
                "tu1",
                json!({"subject": "x"}),
                Some("Task #1 created successfully: x"),
            ),
            tool("TaskList", "tl", json!({}), Some("No tasks found")),
        ])];
        assert!(extract(&h, None).is_empty());
    }

    #[test]
    fn create_without_result_shows_placeholder() {
        let h = vec![msg(vec![tool(
            "TaskCreate",
            "tu1",
            json!({"subject": "Pending one"}),
            None,
        )])];
        let tasks = extract(&h, None);
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].id.starts_with("pending:"));
        assert_eq!(tasks[0].status, CcStatus::Pending);
    }

    fn task(status: CcStatus) -> CcTask {
        CcTask {
            id: "1".into(),
            subject: "x".into(),
            active_form: None,
            status,
        }
    }

    #[test]
    fn panel_collapses_when_all_done_and_no_running_jobs() {
        assert!(panel_lines(&[task(CcStatus::Completed)], &[]).is_none());
        assert!(panel_lines(&[], &[]).is_none());
        // A done job must not keep the panel open.
        let done = json!([{"label": "sleep", "status": "done"}]);
        assert!(panel_lines(&[task(CcStatus::Completed)], done.as_array().unwrap()).is_none());
    }

    #[test]
    fn panel_shows_while_work_is_active() {
        assert!(panel_lines(&[task(CcStatus::InProgress)], &[]).is_some());
        assert!(panel_lines(&[task(CcStatus::Pending)], &[]).is_some());
        let running = json!([{"label": "sleep", "status": "running"}]);
        assert!(panel_lines(&[task(CcStatus::Completed)], running.as_array().unwrap()).is_some());
    }
}
