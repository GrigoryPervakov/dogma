//! App — the owned state container.

use crate::api::types::Token;
use crate::config::host_label;
use crate::instance::InstanceId;
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

/// One connected Nerve instance: its identity, friendly label, server URL, and
/// live WS connection state. dogma merges resources from every instance into
/// one UI, distinguishing them by `id` (color + sigil via `ui::theme`).
#[derive(Debug, Clone)]
pub struct InstanceMeta {
    pub id: InstanceId,
    pub label: String,
    pub server: String,
    pub ws: WsConnState,
}

impl InstanceMeta {
    pub fn new(id: InstanceId, label: String, server: String) -> Self {
        Self {
            id,
            label,
            server,
            ws: WsConnState::Disconnected,
        }
    }
}

pub struct App {
    pub mode: Mode,
    pub command_buffer: String,
    /// One per connected Nerve server, indexed by `InstanceId`.
    pub instances: Vec<InstanceMeta>,
    /// `instances` ids precomputed for fan-out (handed to views via `ViewCtx`).
    pub instance_ids: Vec<InstanceId>,
    pub views: Vec<Box<dyn View>>,
    pub current_view: usize,
    pub fatal: Option<String>,
    pub should_quit: bool,
    pub dirty: bool,
}

impl App {
    /// Single-instance constructor (tests, the screenshot example, and a plain
    /// one-server launch). Token is unused now that auth lives in the workers.
    pub fn new(server: String, _token: Token) -> Self {
        let label = host_label(&server).to_string();
        Self::with_instances(vec![InstanceMeta::new(InstanceId::PRIMARY, label, server)])
    }

    pub fn with_instances(instances: Vec<InstanceMeta>) -> Self {
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

        let instance_ids = instances.iter().map(|i| i.id).collect();
        Self {
            mode: Mode::Normal,
            command_buffer: String::new(),
            instances,
            instance_ids,
            views,
            current_view: 0,
            fatal: None,
            should_quit: false,
            dirty: true,
        }
    }

    /// Whether more than one instance is connected — gates per-instance badges
    /// so a single-server launch looks exactly as before.
    pub fn multi_instance(&self) -> bool {
        self.instances.len() > 1
    }

    /// Mutable access to an instance's metadata (e.g. to update its WS state).
    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut InstanceMeta> {
        self.instances.get_mut(id.index())
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
