//! Color palette.

use ratatui::style::{Color, Modifier, Style};

use crate::instance::InstanceId;

/// Per-instance accent colors and sigils, used to visibly distinguish resources
/// from different Nerve servers when more than one is connected. Indexed by
/// `InstanceId`, wrapping if there are somehow more instances than entries.
const INSTANCE_COLORS: [Color; 6] = [
    Color::Cyan,
    Color::Magenta,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::LightRed,
];
const INSTANCE_SIGILS: [char; 6] = ['●', '◆', '▲', '■', '◇', '★'];

pub fn instance_color(id: InstanceId) -> Color {
    INSTANCE_COLORS[id.index() % INSTANCE_COLORS.len()]
}

pub fn instance_sigil(id: InstanceId) -> char {
    INSTANCE_SIGILS[id.index() % INSTANCE_SIGILS.len()]
}

pub fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub fn role_user() -> Style {
    Style::default()
        .fg(Color::LightGreen)
        .add_modifier(Modifier::BOLD)
}

pub fn role_assistant() -> Style {
    Style::default()
        .fg(Color::LightCyan)
        .add_modifier(Modifier::BOLD)
}

/// Session runtime text colors (sidebar + chat frame).
pub fn session_streaming() -> Style {
    Style::default().fg(Color::Green)
}

pub fn session_waiting() -> Style {
    Style::default().fg(Color::Yellow)
}

/// A session paused on a plan-mode approval — distinct from a question poll.
pub fn session_waiting_plan() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

pub fn nav_active() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

pub fn nav_inactive() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
