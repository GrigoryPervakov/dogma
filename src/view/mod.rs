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

use crate::app::action::Action;

/// Per-render context — where views push Actions to be dispatched.
pub struct ViewCtx<'a> {
    pub app_actions: &'a mut Vec<Action>,
}

impl<'a> ViewCtx<'a> {
    pub fn push(&mut self, a: Action) {
        self.app_actions.push(a);
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
    pub ws: &'a crate::app::state::WsConnState,
    pub server: &'a str,
}
