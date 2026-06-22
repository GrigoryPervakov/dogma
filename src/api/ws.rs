//! WebSocket task — owns the connection, reads frames into `event_tx`,
//! writes `WsClientMsg` from `action_rx`. Reconnects with exponential backoff.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::{debug, error, info, warn};
use url::Url;

use crate::api::AuthHandle;
use crate::api::types::{Token, WsClientMsg, WsConnEvent, WsServerMsg};

pub enum WireOut {
    Server(WsServerMsg),
    Conn(WsConnEvent),
}

/// Spawn the WS task. Returns a sender for client messages.
///
/// `events_tx` receives both server frames and connection-state events.
pub fn spawn(
    base_http_url: String,
    auth: Arc<AuthHandle>,
    events_tx: mpsc::Sender<WireOut>,
) -> mpsc::Sender<WsClientMsg> {
    let (tx, rx) = mpsc::channel::<WsClientMsg>(128);
    tokio::spawn(run(base_http_url, auth, events_tx, rx));
    tx
}

async fn run(
    base_http_url: String,
    auth: Arc<AuthHandle>,
    events_tx: mpsc::Sender<WireOut>,
    mut client_rx: mpsc::Receiver<WsClientMsg>,
) {
    let mut backoff_ms: u64 = 1_000;
    let max_backoff_ms: u64 = 30_000;

    loop {
        // Rebuild the URL each attempt so a token reissued elsewhere (the HTTP
        // worker, or our own re-auth below) is picked up on reconnect.
        let token = auth.token().await;
        let ws_url = match build_ws_url(&base_http_url, &token) {
            Ok(u) => u,
            Err(e) => {
                error!(?e, "ws: bad url");
                let _ = events_tx
                    .send(WireOut::Conn(WsConnEvent::Disconnected {
                        reason: e.to_string(),
                        retry_in_ms: None,
                    }))
                    .await;
                return;
            }
        };

        let _ = events_tx.send(WireOut::Conn(WsConnEvent::Connecting)).await;

        match connect(&ws_url).await {
            Ok((mut sink, mut stream)) => {
                info!("ws: connected");
                backoff_ms = 1_000;
                let _ = events_tx.send(WireOut::Conn(WsConnEvent::Connected)).await;

                loop {
                    tokio::select! {
                        biased;

                        msg = client_rx.recv() => {
                            let Some(msg) = msg else {
                                debug!("ws: client_rx closed; shutting down task");
                                return;
                            };
                            let json = match serde_json::to_string(&msg) {
                                Ok(s) => s,
                                Err(e) => { error!(?e, "ws: encode client msg failed"); continue; }
                            };
                            if let Err(e) = sink.send(WsMsg::Text(json.into())).await {
                                warn!(?e, "ws: write failed; reconnecting");
                                break;
                            }
                        }

                        frame = stream.next() => {
                            match frame {
                                Some(Ok(WsMsg::Text(t))) => {
                                    match serde_json::from_str::<WsServerMsg>(&t) {
                                        Ok(parsed) => {
                                            if events_tx.send(WireOut::Server(parsed)).await.is_err() {
                                                return;
                                            }
                                        }
                                        Err(e) => {
                                            warn!(error=?e, raw=%t, "ws: parse server msg failed");
                                        }
                                    }
                                }
                                Some(Ok(WsMsg::Binary(_))) => { /* ignore */ }
                                Some(Ok(WsMsg::Ping(p))) => {
                                    let _ = sink.send(WsMsg::Pong(p)).await;
                                }
                                Some(Ok(WsMsg::Pong(_))) => {}
                                Some(Ok(WsMsg::Close(_))) => {
                                    info!("ws: server closed");
                                    break;
                                }
                                Some(Ok(WsMsg::Frame(_))) => {}
                                Some(Err(e)) => {
                                    warn!(?e, "ws: read error");
                                    break;
                                }
                                None => {
                                    info!("ws: stream ended");
                                    break;
                                }
                            }
                        }
                    }
                }

                let _ = events_tx
                    .send(WireOut::Conn(WsConnEvent::Disconnected {
                        reason: "connection closed".into(),
                        retry_in_ms: Some(backoff_ms),
                    }))
                    .await;
            }
            Err(e) => {
                let reason = format!("{e:#}");
                let auth_rejected = reason.contains("401")
                    || reason.contains("403")
                    || reason.contains("4001")
                    || reason.to_lowercase().contains("unauthorized");
                if auth_rejected {
                    // Token likely expired — try to reissue it from the stored
                    // password and reconnect, rather than giving up.
                    if auth.can_reauth() {
                        match auth.reauth().await {
                            Ok(_) => {
                                info!("ws: token reissued after auth rejection; reconnecting");
                                backoff_ms = 1_000;
                                continue;
                            }
                            Err(re) => error!(%re, "ws: re-auth failed"),
                        }
                    }
                    error!(%reason, "ws: auth rejected");
                    let _ = events_tx
                        .send(WireOut::Conn(WsConnEvent::AuthRejected))
                        .await;
                    return;
                }
                warn!(%reason, "ws: connect failed");
                let _ = events_tx
                    .send(WireOut::Conn(WsConnEvent::Disconnected {
                        reason,
                        retry_in_ms: Some(backoff_ms),
                    }))
                    .await;
            }
        }

        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms.saturating_mul(2)).min(max_backoff_ms);
    }
}

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMsg,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn connect(ws_url: &Url) -> Result<(WsSink, WsStream)> {
    let req = ws_url
        .as_str()
        .into_client_request()
        .context("ws: build request")?;
    let (stream, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .with_context(|| format!("connect ws {ws_url}"))?;
    let (sink, stream) = stream.split();
    Ok((sink, stream))
}

fn build_ws_url(base_http_url: &str, token: &Token) -> Result<Url> {
    let mut u = Url::parse(base_http_url).context("invalid server url")?;
    let scheme = match u.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => anyhow::bail!("unsupported scheme: {other}"),
    };
    u.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("set scheme"))?;
    u.set_path("/ws");
    u.query_pairs_mut().append_pair("token", token.as_str());
    Ok(u)
}
