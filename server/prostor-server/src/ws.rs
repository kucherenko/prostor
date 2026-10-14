//! WebSocket endpoints. Both are stubs: the signaling protocol and the FLV
//! ingest protocol are defined in the Phase 0 plan. What is already real: room ids are
//! validated, messages are size-limited, idle sockets are closed, and the number of
//! concurrent sockets is bounded.

use std::time::Duration;

use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::sync::OwnedSemaphorePermit;

use crate::{app::AppState, room::RoomId};

/// Largest ingest message: one FLV tag. A 1080p keyframe at 6 Mbit/s is ~150 KB.
pub const INGEST_MAX_MESSAGE: usize = 4 * 1024 * 1024;
/// Largest signaling message (SDP offers are a few KB).
pub const SIGNALING_MAX_MESSAGE: usize = 64 * 1024;
/// A socket that sends nothing for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// The server pings this often so a healthy idle peer keeps answering.
pub const PING_INTERVAL: Duration = Duration::from_secs(10);
/// Close code for a message over the size limit.
const CLOSE_TOO_LARGE: u16 = 1009;
/// Close code for an idle peer (policy violation).
const CLOSE_IDLE: u16 = 1008;

/// `GET /ws/room/{id}`: signaling between the producer and the guests of a room.
/// Stub: echoes text frames back to the sender.
pub async fn room(
    // Extractors run in order: the room id is validated before the upgrade is attempted,
    // so a bad id is a 400 whether or not the request is a WebSocket handshake.
    Path(id): Path<RoomId>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(permit) = state.socket_permit() else {
        return too_many_sockets();
    };
    ws.max_message_size(SIGNALING_MAX_MESSAGE)
        .max_frame_size(SIGNALING_MAX_MESSAGE)
        .on_upgrade(move |socket| handle_room(socket, id, permit))
}

async fn handle_room(socket: WebSocket, room: RoomId, _permit: OwnedSemaphorePermit) {
    tracing::info!(%room, "signaling peer joined");
    let mut peer = Peer::new(socket);
    while let Some(message) = peer.next().await {
        match message {
            Message::Text(text) => {
                if peer.socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => {}
        }
    }
    tracing::info!(%room, "signaling peer left");
}

/// `GET /ws/ingest/{room}`: FLV tags from the producer as binary frames.
/// Stub: counts frames and bytes and logs a summary when the socket closes.
pub async fn ingest(
    Path(room): Path<RoomId>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(permit) = state.socket_permit() else {
        return too_many_sockets();
    };
    ws.max_message_size(INGEST_MAX_MESSAGE)
        .max_frame_size(INGEST_MAX_MESSAGE)
        .on_upgrade(move |socket| handle_ingest(socket, room, permit))
}

async fn handle_ingest(socket: WebSocket, room: RoomId, _permit: OwnedSemaphorePermit) {
    tracing::info!(%room, "ingest started");
    let (mut frames, mut bytes) = (0u64, 0u64);
    let mut peer = Peer::new(socket);
    while let Some(message) = peer.next().await {
        match message {
            Message::Binary(data) => {
                frames += 1;
                bytes += data.len() as u64;
            }
            Message::Close(_) => break,
            Message::Text(_) | Message::Ping(_) | Message::Pong(_) => {}
        }
    }
    tracing::info!(%room, frames, bytes, "ingest closed");
}

fn too_many_sockets() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "too many open sockets").into_response()
}

/// A socket with the idle timeout and the ping schedule applied.
struct Peer {
    socket: WebSocket,
    ping: tokio::time::Interval,
}

impl Peer {
    fn new(socket: WebSocket) -> Self {
        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping.reset();
        Peer { socket, ping }
    }

    /// The next message, or `None` when the peer went away, timed out, or sent
    /// something the socket rejected (logged, and answered with a close frame).
    async fn next(&mut self) -> Option<Message> {
        loop {
            tokio::select! {
                received = tokio::time::timeout(IDLE_TIMEOUT, self.socket.recv()) => {
                    return match received {
                        Ok(Some(Ok(message))) => Some(message),
                        Ok(Some(Err(error))) => {
                            tracing::warn!(%error, "websocket error; closing");
                            self.close(CLOSE_TOO_LARGE, "message rejected").await;
                            None
                        }
                        Ok(None) => None,
                        Err(_elapsed) => {
                            tracing::info!("websocket idle for {IDLE_TIMEOUT:?}; closing");
                            self.close(CLOSE_IDLE, "idle").await;
                            None
                        }
                    };
                }
                _ = self.ping.tick() => {
                    if self.socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                        return None;
                    }
                }
            }
        }
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        let frame = CloseFrame {
            code,
            reason: reason.into(),
        };
        let _ = self.socket.send(Message::Close(Some(frame))).await;
    }
}
