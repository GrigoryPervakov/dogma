//! App — the owned state container.

use crate::api::types::Token;
use crate::view::View;
use crate::view::chat::ChatView;
use crate::view::notifications::NotificationsView;
use crate::view::plans::PlansView;
use crate::view::skills::SkillsView;
use crate::view::stub::StubView;
use crate::view::tasks::TasksView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Default mode — keys are dispatched into the active view.
    Normal,
    /// `:` command line.
    Command,
    /// Help overlay (`?`).
    Help,
}

#[derive(Debug, Clone, Default)]
pub enum WsConnState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Reconnecting {
        retry_in_ms: u64,
        reason: String,
    },
    AuthRejected,
}

pub struct AuthState {
    pub server: String,
    pub token: Token,
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState")
            .field("server", &self.server)
            .field("token", &self.token)
            .finish()
    }
}

pub struct App {
    pub mode: Mode,
    pub command_buffer: String,
    pub auth: AuthState,
    pub ws: WsConnState,
    pub views: Vec<Box<dyn View>>,
    pub current_view: usize,
    pub fatal: Option<String>,
    pub should_quit: bool,
    pub dirty: bool,
}

impl App {
    pub fn new(server: String, token: Token) -> Self {
        let mut views: Vec<Box<dyn View>> = vec![
            Box::new(ChatView::new()),
            Box::new(NotificationsView::new()),
            Box::new(TasksView::new()),
            Box::new(PlansView::new()),
            Box::new(SkillsView::new()),
        ];
        for (id, t) in [
            ("cron", "cron"),
            ("sources", "sources"),
            ("memory", "memory"),
            ("diag", "diag"),
        ] {
            views.push(Box::new(StubView::new(id, t)));
        }

        Self {
            mode: Mode::Normal,
            command_buffer: String::new(),
            auth: AuthState { server, token },
            ws: WsConnState::default(),
            views,
            current_view: 0,
            fatal: None,
            should_quit: false,
            dirty: true,
        }
    }

    pub fn current_view_idx(&self) -> usize {
        self.current_view
    }

    pub fn view_titles(&self) -> Vec<&str> {
        self.views.iter().map(|v| v.title()).collect()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}
