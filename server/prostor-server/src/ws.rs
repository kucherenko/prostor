//! WebSocket plumbing shared by the signaling and ingest endpoints, and the signaling
//! endpoint itself (still a stub: the protocol is defined in the Phase 0 plan, M5).
//!
//! What is real for every socket: room ids are validated, messages are size-limited,
//! silent peers are closed after [`Timeouts::idle`], the server pings every
//! [`Timeouts::ping`], the number of concurrent sockets is bounded, close handshakes are
//! completed before the socket is dropped (so peers see the code we chose, not 1006),
//! and a shutdown closes every socket with 1001.

use std::time::Duration;

use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::{sync::OwnedSemaphorePermit, time::Instant};

use crate::{app::AppState, room::RoomId, shutdown::Shutdown};

/// Largest ingest message: one FLV tag. A 1080p keyframe at 6 Mbit/s is ~150 KB.
pub const INGEST_MAX_MESSAGE: usize = 4 * 1024 * 1024;
/// Largest signaling message (SDP offers are a few KB).
pub const SIGNALING_MAX_MESSAGE: usize = 64 * 1024;
/// A socket that sends nothing for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// The server pings this often so a healthy idle peer keeps answering.
pub const PING_INTERVAL: Duration = Duration::from_secs(10);
/// An ingest peer must send `hello` within this long after connecting.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a close handshake may take before the socket is dropped anyway.
const CLOSE_DRAIN: Duration = Duration::from_secs(2);

/// Close code: normal end of the session.
pub const CLOSE_NORMAL: u16 = 1000;
/// Close code: the server is shutting down.
pub const CLOSE_GOING_AWAY: u16 = 1001;
/// Close code: policy violation (an idle peer).
pub const CLOSE_POLICY: u16 = 1008;
/// Close code: a message over the size limit.
pub const CLOSE_TOO_LARGE: u16 = 1009;
/// Close code: a server-side failure.
pub const CLOSE_INTERNAL: u16 = 1011;

/// Timing of a socket. Tests shorten these; production uses the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub idle: Duration,
    pub ping: Duration,
    pub hello: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            idle: IDLE_TIMEOUT,
            ping: PING_INTERVAL,
            hello: HELLO_TIMEOUT,
        }
    }
}

/// `GET /ws/room/{id}`: signaling between the producer and the guests of a room.
/// Stub: echoes text frames back to the sender.
pub async fn room(
    // Extractors run in order: the room id is validated before the upgrade is attempted,
    // so a bad id is a 400 whether or not the request is a WebSocket handshake.
    Path(id): Path<RoomId>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    if state.shutdown().is_cancelled() {
        return shutting_down();
    }
    let Some(permit) = state.socket_permit() else {
        return too_many_sockets();
    };
    let shutdown = state.shutdown().clone();
    let timeouts = state.timeouts();
    ws.max_message_size(SIGNALING_MAX_MESSAGE)
        .max_frame_size(SIGNALING_MAX_MESSAGE)
        .on_upgrade(move |socket| {
            shutdown
                .clone()
                .track(handle_room(socket, id, permit, timeouts, shutdown))
        })
}

async fn handle_room(
    socket: WebSocket,
    room: RoomId,
    _permit: OwnedSemaphorePermit,
    timeouts: Timeouts,
    shutdown: Shutdown,
) {
    tracing::info!(%room, "signaling peer joined");
    let mut peer = Peer::new(socket, timeouts);
    loop {
        tokio::select! {
            message = peer.next() => match message {
                Some(Message::Text(text)) => {
                    if peer.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Some(_) => {}
                None => break,
            },
            _ = shutdown.cancelled() => {
                peer.close(CLOSE_GOING_AWAY, "server shutting down").await;
                break;
            }
        }
    }
    tracing::info!(%room, "signaling peer left");
}

pub(crate) fn too_many_sockets() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "too many open sockets").into_response()
}

pub(crate) fn shutting_down() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response()
}

/// A socket with the idle timeout, the ping schedule and a complete close handshake.
pub(crate) struct Peer {
    socket: WebSocket,
    ping: tokio::time::Interval,
    timeouts: Timeouts,
    /// Pushed forward by every received frame only: pings the server sends and ticks of
    /// the select loop never extend it.
    idle_deadline: Instant,
    /// A close frame was sent or received; nothing more is read or written.
    closed: bool,
}

impl Peer {
    pub(crate) fn new(socket: WebSocket, timeouts: Timeouts) -> Self {
        let mut ping = tokio::time::interval(timeouts.ping);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping.reset();
        Peer {
            socket,
            ping,
            timeouts,
            idle_deadline: Instant::now() + timeouts.idle,
            closed: false,
        }
    }

    /// The next message, or `None` once the peer closed, went away, timed out, or sent
    /// something the socket rejected (logged, and answered with a close frame). Close
    /// frames are consumed here: the handshake is completed before `None` is returned.
    pub(crate) async fn next(&mut self) -> Option<Message> {
        if self.closed {
            return None;
        }
        loop {
            tokio::select! {
                received = self.socket.recv() => {
                    return match received {
                        Some(Ok(Message::Close(_))) => {
                            // axum queues the close reply; reading on flushes it.
                            self.closed = true;
                            self.drain().await;
                            None
                        }
                        Some(Ok(message)) => {
                            self.idle_deadline = Instant::now() + self.timeouts.idle;
                            Some(message)
                        }
                        Some(Err(error)) => {
                            tracing::warn!(%error, "websocket error; closing");
                            self.close(CLOSE_TOO_LARGE, "message rejected").await;
                            None
                        }
                        None => {
                            self.closed = true;
                            None
                        }
                    };
                }
                _ = tokio::time::sleep_until(self.idle_deadline) => {
                    tracing::info!("websocket idle for {:?}; closing", self.timeouts.idle);
                    self.close(CLOSE_POLICY, "idle").await;
                    return None;
                }
                _ = self.ping.tick() => {
                    if self.socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                        self.closed = true;
                        return None;
                    }
                }
            }
        }
    }

    /// Sends a message; an error means the peer is gone.
    pub(crate) async fn send(&mut self, message: Message) -> Result<(), axum::Error> {
        if self.closed {
            return Err(axum::Error::new("socket is closed"));
        }
        self.socket.send(message).await
    }

    /// Sends a close frame and waits (bounded) for the peer's reply, so the frame is
    /// flushed and the peer sees `code` rather than an abnormal closure.
    pub(crate) async fn close(&mut self, code: u16, reason: impl Into<Utf8Bytes>) {
        if self.closed {
            return;
        }
        self.closed = true;
        let frame = CloseFrame {
            code,
            reason: reason.into(),
        };
        if self.socket.send(Message::Close(Some(frame))).await.is_ok() {
            self.drain().await;
        }
    }

    async fn drain(&mut self) {
        let _ = tokio::time::timeout(CLOSE_DRAIN, async {
            while let Some(Ok(_)) = self.socket.recv().await {}
        })
        .await;
    }
}
