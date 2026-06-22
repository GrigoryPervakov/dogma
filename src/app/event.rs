//! AppEvent — everything that flows into the main loop's `update`.

use crate::api::types::{HttpResult, WsConnEvent, WsServerMsg};

#[derive(Debug)]
pub enum AppEvent {
    /// Terminal input.
    Term(crossterm::event::Event),
    /// A frame from the WS task (server → client).
    Wire(WsServerMsg),
    /// Connection-state event from the WS task.
    WireConn(WsConnEvent),
    /// Result of an HTTP request kicked off via Action::Http.
    Http(HttpResult),
    /// Periodic tick (for spinners / reconnect timer / dirty drain).
    Tick,
    /// Async fatal error — main loop will tear down.
    Fatal(String),
}
