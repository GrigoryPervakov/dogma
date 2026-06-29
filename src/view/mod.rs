//! View trait — the future-tab seam.

pub mod chat;
pub mod list_detail;
pub mod notifications;
pub mod plans;
pub mod skills;
pub mod stub;
pub mod tasks;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::api::types::{HttpReq, WsClientMsg};
use crate::app::action::Action;
use crate::instance::InstanceId;

/// Per-key/per-event context — where views push instance-routed Actions, and
/// the set of instances they can fan a request out to (e.g. initial list loads).
pub struct ViewCtx<'a> {
    pub app_actions: &'a mut Vec<Action>,
    pub instances: &'a [InstanceId],
}

impl<'a> ViewCtx<'a> {
    pub fn push(&mut self, a: Action) {
        self.app_actions.push(a);
    }

    /// Route a WS frame to one instance.
    pub fn ws(&mut self, instance: InstanceId, msg: WsClientMsg) {
        self.app_actions.push(Action::Ws { instance, msg });
    }

    /// Route an HTTP request to one instance.
    pub fn http(&mut self, instance: InstanceId, req: HttpReq) {
        self.app_actions.push(Action::Http { instance, req });
    }

    /// Issue the same HTTP request to every connected instance (list loads).
    pub fn http_all(&mut self, req: HttpReq) {
        for &instance in self.instances {
            self.app_actions.push(Action::Http {
                instance,
                req: req.clone(),
            });
        }
    }
}

pub trait View: std::any::Any {
    fn id(&self) -> &'static str;
    fn title(&self) -> &str;

    /// Whether the view currently owns every key, so global single-key
    /// shortcuts (`q` quit, `?` help, `:` command) are suppressed and passed
    /// through to the view — e.g. while typing in an insert tier or answering
    /// a poll. Default: global shortcuts stay active.
    fn consumes_global_shortcuts(&self) -> bool {
        false
    }

    /// Whether Tab/BackTab cycling stops on this view. Unimplemented stub
    /// tabs return false so cycling skips straight past them.
    fn selectable(&self) -> bool {
        true
    }

    /// Called when the view becomes the active tab (e.g. via Tab cycling
    /// or `:tasks` command). Default: no-op. Use to dispatch lazy fetches.
    fn on_focus(&mut self, _ctx: &mut ViewCtx) {}

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ViewCtx);

    fn render(&mut self, area: Rect, frame: &mut Frame, app_state: ViewRenderCtx<'_>);

    /// Convenience for downcasting.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Read-only slice of App-level state that views need at render time.
pub struct ViewRenderCtx<'a> {
    pub mode: crate::app::state::Mode,
    pub command_buffer: &'a str,
    /// All connected instances — used to render per-instance badges (color +
    /// sigil) when there's more than one.
    pub instances: &'a [crate::app::state::InstanceMeta],
}

impl ViewRenderCtx<'_> {
    /// Whether to show per-instance badges (more than one instance connected).
    pub fn multi_instance(&self) -> bool {
        self.instances.len() > 1
    }

    pub fn instance(
        &self,
        id: crate::instance::InstanceId,
    ) -> Option<&crate::app::state::InstanceMeta> {
        self.instances.get(id.index())
    }
}
