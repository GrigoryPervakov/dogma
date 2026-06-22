//! Action — the things `update()` returns for the runtime to execute.

use crate::api::types::{HttpReq, WsClientMsg};

#[derive(Debug, Clone)]
pub enum Action {
    /// Send a frame to the server over WS.
    Ws(WsClientMsg),
    /// Issue an HTTP request; result lands as AppEvent::Http.
    Http(HttpReq),
    /// Quit the application.
    Quit,
}
