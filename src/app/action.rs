//! Action — the things `update()` returns for the runtime to execute.

use crate::api::types::{HttpReq, WsClientMsg};
use crate::instance::InstanceId;

#[derive(Debug, Clone)]
pub enum Action {
    /// Send a frame over the given instance's WS connection.
    Ws {
        instance: InstanceId,
        msg: WsClientMsg,
    },
    /// Issue an HTTP request to the given instance; result lands as
    /// `AppEvent::Inst { ev: ConnEvent::Http, .. }`.
    Http { instance: InstanceId, req: HttpReq },
    /// Quit the application.
    Quit,
}
