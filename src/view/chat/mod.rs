//! ChatView — the only fully-featured view in v1.

pub mod blocks;
pub mod blocks_height_estimator;
mod files;
pub mod items;
pub mod poll;
mod reducer;
mod render;
mod sidebar;
pub mod state;
pub mod tasks_panel;

pub use state::{AgentStatus, ChatCommand, ChatView, FocusTier};
