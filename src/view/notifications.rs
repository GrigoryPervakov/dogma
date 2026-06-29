//! Notifications tab — list + detail with answerable polls.
//!
//! `question` and `propose_action` notifications carry `options`; while
//! pending, the highlighted one is answered by pressing its number (1-9).
//! Any pending notification can be dismissed with `d`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::api::types::HttpReq;
use crate::instance::InstanceId;
use crate::model::Notification;
use crate::view::list_detail::{
    ListDetailRender, NavOutcome, Pane, handle_nav, meta_line, truncate,
};
use crate::view::{View, ViewCtx, ViewRenderCtx};

pub struct NotificationsView {
    items: Vec<Notification>,
    selected: usize,
    focused_pane: Pane,
    loaded: bool,
    loading: bool,
    last_error: Option<String>,
    /// When false (default), only pending notifications are shown; `a` toggles.
    show_all: bool,
}

impl NotificationsView {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            focused_pane: Pane::List,
            loaded: false,
            loading: false,
            last_error: None,
            show_all: false,
        }
    }

    /// The notifications currently on screen — pending-only unless `show_all`.
    fn shown(&self) -> Vec<&Notification> {
        self.items
            .iter()
            .filter(|n| self.show_all || n.is_pending())
            .collect()
    }

    fn clamp_selection(&mut self) {
        let n = self.shown().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }

    fn current(&self) -> Option<&Notification> {
        self.shown().get(self.selected).copied()
    }

    /// Any unanswered/undismissed notification — drives the red nav-rail tab.
    pub fn has_pending(&self) -> bool {
        self.items.iter().any(|n| n.is_pending())
    }

    pub fn apply_list_loaded(
        &mut self,
        instance: InstanceId,
        r: std::result::Result<Vec<Notification>, String>,
    ) {
        self.loading = false;
        match r {
            Ok(mut list) => {
                for n in &mut list {
                    n.instance = instance;
                }
                self.items.retain(|n| n.instance != instance);
                self.items.append(&mut list);
                // Newest first across instances, not grouped by instance.
                self.items.sort_by(|a, b| {
                    b.created_at
                        .as_deref()
                        .unwrap_or("")
                        .cmp(a.created_at.as_deref().unwrap_or(""))
                });
                self.loaded = true;
                self.last_error = None;
                self.clamp_selection();
            }
            Err(e) => self.last_error = Some(e),
        }
    }

    pub fn apply_answered(
        &mut self,
        instance: InstanceId,
        id: &str,
        answer: &str,
        result: std::result::Result<(), String>,
    ) {
        match result {
            Ok(()) => {
                if let Some(n) = self
                    .items
                    .iter_mut()
                    .find(|n| n.instance == instance && n.id == id)
                {
                    n.status = "answered".into();
                    n.answer = Some(answer.to_string());
                }
            }
            Err(e) => self.last_error = Some(format!("answer: {e}")),
        }
    }

    pub fn apply_dismissed(
        &mut self,
        instance: InstanceId,
        id: &str,
        result: std::result::Result<(), String>,
    ) {
        match result {
            Ok(()) => {
                if let Some(n) = self
                    .items
                    .iter_mut()
                    .find(|n| n.instance == instance && n.id == id)
                {
                    n.status = "dismissed".into();
                }
            }
            Err(e) => self.last_error = Some(format!("dismiss: {e}")),
        }
    }

    fn refresh(&mut self, ctx: &mut ViewCtx) {
        self.loading = true;
        ctx.http_all(HttpReq::ListNotifications);
    }

    /// Answer the highlighted pending poll with option `opt_idx`.
    fn answer_selected(&mut self, opt_idx: usize, ctx: &mut ViewCtx) {
        let (id, instance, answer) = {
            let Some(n) = self.shown().get(self.selected).copied() else {
                return;
            };
            if !n.answerable() {
                return;
            }
            let Some(answer) = n.options.get(opt_idx).cloned() else {
                return;
            };
            (n.id.clone(), n.instance, answer)
        };
        ctx.http(
            instance,
            HttpReq::AnswerNotification {
                id: id.clone(),
                answer: answer.clone(),
            },
        );
        // Optimistic — confirmed by the result handler, reverted on error.
        if let Some(n) = self
            .items
            .iter_mut()
            .find(|x| x.instance == instance && x.id == id)
        {
            n.status = "answered".into();
            n.answer = Some(answer);
        }
        self.clamp_selection();
    }

    fn dismiss_selected(&mut self, ctx: &mut ViewCtx) {
        let (id, instance) = {
            let Some(n) = self.shown().get(self.selected).copied() else {
                return;
            };
            if !n.is_pending() {
                return;
            }
            (n.id.clone(), n.instance)
        };
        ctx.http(instance, HttpReq::DismissNotification { id: id.clone() });
        if let Some(n) = self
            .items
            .iter_mut()
            .find(|x| x.instance == instance && x.id == id)
        {
            n.status = "dismissed".into();
        }
        self.clamp_selection();
    }
}

impl Default for NotificationsView {
    fn default() -> Self {
        Self::new()
    }
}

impl View for NotificationsView {
    fn id(&self) -> &'static str {
        "notifs"
    }
    fn title(&self) -> &str {
        "notifs"
    }

    fn on_focus(&mut self, ctx: &mut ViewCtx) {
        if !self.loaded && !self.loading {
            self.refresh(ctx);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        // Shared list nav first; notification-specific keys fall through.
        let len = self.shown().len();
        if let NavOutcome::Moved | NavOutcome::Switched =
            handle_nav(key.code, &mut self.focused_pane, &mut self.selected, len)
        {
            return;
        }
        match key.code {
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                self.answer_selected((c as usize) - ('1' as usize), ctx);
            }
            KeyCode::Char('d') => self.dismiss_selected(ctx),
            // Toggle showing answered/dismissed notifications.
            KeyCode::Char('a') => {
                self.show_all = !self.show_all;
                self.clamp_selection();
            }
            KeyCode::Char('r') => {
                self.last_error = None;
                self.refresh(ctx);
            }
            _ => {}
        }
    }

    fn render(&mut self, area: Rect, frame: &mut Frame, ctx: ViewRenderCtx<'_>) {
        let multi = ctx.multi_instance();
        let shown = self.shown();
        let hidden = self.items.len() - shown.len();
        let items: Vec<Line<'_>> = shown
            .iter()
            .map(|n| {
                let mut line = notif_list_line(n);
                if multi {
                    let label = ctx
                        .instance(n.instance)
                        .map(|m| m.label.as_str())
                        .unwrap_or("");
                    line.spans.insert(
                        0,
                        Span::styled(
                            format!("{} {label} ", crate::ui::theme::instance_sigil(n.instance)),
                            Style::default().fg(crate::ui::theme::instance_color(n.instance)),
                        ),
                    );
                }
                line
            })
            .collect();
        let n = self.current();
        let detail_title = n
            .and_then(|n| n.title.as_deref())
            .unwrap_or("(no notification)");
        let detail_meta: Vec<Line<'_>> = n.map(notif_meta).unwrap_or_default();
        let detail_body = n.and_then(|n| n.body.as_deref());

        crate::view::list_detail::render(
            area,
            frame,
            ListDetailRender {
                title: "Notifications",
                items,
                selected: self.selected,
                focused_pane: self.focused_pane,
                loading: self.loading,
                last_error: self.last_error.as_deref(),
                detail_title,
                detail_meta,
                detail_body,
                detail_loading: false,
                showing_all: self.show_all,
                hidden,
            },
        );
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn status_glyph(n: &Notification) -> Span<'static> {
    let (ch, style) = match n.status.as_str() {
        "answered" => ("✓", Style::default().fg(Color::Green)),
        "dismissed" => ("·", Style::default().add_modifier(Modifier::DIM)),
        _ if n.answerable() => ("?", Style::default().fg(Color::Yellow)),
        _ => ("•", priority_style(n)),
    };
    Span::styled(ch.to_string(), style)
}

fn priority_style(n: &Notification) -> Style {
    match n.priority.as_deref() {
        Some("urgent") => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Some("high") => Style::default().fg(Color::Yellow),
        Some("low") => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default(),
    }
}

fn notif_list_line(n: &Notification) -> Line<'static> {
    let title = n.title.clone().unwrap_or_else(|| n.id.clone());
    let title_style = if n.status == "dismissed" {
        Style::default().add_modifier(Modifier::DIM)
    } else if n.answerable() {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    Line::from(vec![
        status_glyph(n),
        Span::raw(" "),
        Span::styled(truncate(&title, 30), title_style),
    ])
}

fn notif_meta(n: &Notification) -> Vec<Line<'static>> {
    let mut out = vec![
        meta_line("status", &n.status, 12),
        meta_line("type", &n.kind, 12),
    ];
    if let Some(p) = n.priority.as_deref() {
        out.push(meta_line("priority", p, 12));
    }
    if let Some(s) = n.session_title.as_deref().or(n.session_id.as_deref()) {
        out.push(meta_line("session", s, 12));
    }
    if let Some(ts) = n.created_at.as_deref() {
        out.push(meta_line("created", ts, 12));
    }
    if let Some(ans) = n.answer.as_deref() {
        out.push(Line::from(vec![
            Span::styled(
                format!("{:<12} ", "answer"),
                Style::default().fg(Color::Green),
            ),
            Span::raw(ans.to_string()),
        ]));
    }
    if n.answerable() {
        out.push(Line::raw(""));
        out.push(Line::from(Span::styled(
            "options — press the number to answer:",
            Style::default().add_modifier(Modifier::DIM),
        )));
        for (i, opt) in n.options.iter().enumerate().take(9) {
            out.push(Line::from(vec![
                Span::styled(format!("  {}. ", i + 1), Style::default().fg(Color::Cyan)),
                Span::raw(opt.clone()),
            ]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action::Action;
    use crate::instance::InstanceId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    const TEST_IDS: [InstanceId; 1] = [InstanceId::PRIMARY];

    fn poll(id: &str) -> Notification {
        serde_json::from_value(serde_json::json!({
            "id": id, "type": "question", "status": "pending",
            "title": "Pick", "body": "b",
            "options": "[\"Red\", \"Green\", \"Blue\"]"
        }))
        .unwrap()
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn number_key_answers_selected_poll() {
        let mut v = NotificationsView::new();
        v.apply_list_loaded(InstanceId::PRIMARY, Ok(vec![poll("ask-1")]));
        let mut acts = Vec::new();
        let mut ctx = ViewCtx {
            app_actions: &mut acts,
            instances: &TEST_IDS,
        };
        v.handle_key(key('2'), &mut ctx);

        let answered = acts.iter().any(|a| matches!(
            a,
            Action::Http { req: HttpReq::AnswerNotification { id, answer }, .. } if id == "ask-1" && answer == "Green"
        ));
        assert!(answered, "expected answer with Green, got {acts:?}");
        assert_eq!(v.items[0].status, "answered");
        assert_eq!(v.items[0].answer.as_deref(), Some("Green"));
    }

    #[test]
    fn dismiss_key_dismisses_pending() {
        let mut v = NotificationsView::new();
        v.apply_list_loaded(InstanceId::PRIMARY, Ok(vec![poll("ask-1")]));
        let mut acts = Vec::new();
        let mut ctx = ViewCtx {
            app_actions: &mut acts,
            instances: &TEST_IDS,
        };
        v.handle_key(key('d'), &mut ctx);

        assert!(acts.iter().any(|a| matches!(
            a,
            Action::Http { req: HttpReq::DismissNotification { id }, .. } if id == "ask-1"
        )));
        assert_eq!(v.items[0].status, "dismissed");
    }

    #[test]
    fn number_key_ignored_for_non_poll() {
        let mut v = NotificationsView::new();
        let n: Notification = serde_json::from_value(serde_json::json!({
            "id": "x", "type": "notify", "status": "pending", "options": null
        }))
        .unwrap();
        v.apply_list_loaded(InstanceId::PRIMARY, Ok(vec![n]));
        let mut acts = Vec::new();
        let mut ctx = ViewCtx {
            app_actions: &mut acts,
            instances: &TEST_IDS,
        };
        v.handle_key(key('1'), &mut ctx);
        assert!(
            acts.is_empty(),
            "non-poll notification must not answer on digit"
        );
    }
}
