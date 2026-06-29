//! AppEvent — everything that flows into the main loop's `update`.

use crate::api::types::{HttpResult, WsConnEvent, WsServerMsg};
use crate::instance::InstanceId;

#[derive(Debug)]
pub enum AppEvent {
    /// Terminal input.
    Term(crossterm::event::Event),
    /// A connection-origin event, tagged with the instance it came from.
    /// Boxed because `ConnEvent::Wire`/`Http` are large.
    Inst {
        instance: InstanceId,
        ev: Box<ConnEvent>,
    },
    /// Periodic tick (for spinners / reconnect timer / dirty drain).
    Tick,
    /// Async fatal error — main loop will tear down.
    Fatal(String),
}

/// The three connection-origin event kinds, all scoped to one instance.
#[derive(Debug)]
pub enum ConnEvent {
    /// A frame from the WS task (server → client).
    Wire(WsServerMsg),
    /// Connection-state event from the WS task.
    WireConn(WsConnEvent),
    /// Result of an HTTP request kicked off via `Action::Http`.
    Http(HttpResult),
}
