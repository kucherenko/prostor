//! WebSocket plumbing shared by the signaling (`signaling.rs`) and ingest (`ingest.rs`)
//! endpoints.
//!
//! What holds for every socket: room ids are validated, the `Origin` is checked, messages
//! are size-limited, silent peers are closed after [`Timeouts::idle`], the server pings
//! every [`Timeouts::ping`], the number of concurrent sockets is bounded, close handshakes
//! are completed before the socket is dropped (so peers see the code we chose, not 1006),
//! and a shutdown closes every socket with 1001.

use std::time::Duration;

use axum::{
    extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::time::Instant;

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

pub(crate) fn too_many_sockets() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "too many open sockets").into_response()
}

pub(crate) fn forbidden_origin() -> Response {
    (StatusCode::FORBIDDEN, "origin not allowed").into_response()
}

/// Whether a WebSocket upgrade may proceed given its `Origin`. Browsers always send one;
/// a request without it (tests, tools) is allowed. With an allow-list configured, the
/// origin must be listed; otherwise it must be the same origin the page was served from,
/// i.e. its host and port must equal the `Host` header. Comparison is case-insensitive
/// and a stray trailing slash is ignored.
pub fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin = origin.trim().trim_end_matches('/').to_ascii_lowercase();
    if !allowed.is_empty() {
        return allowed.contains(&origin);
    }
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let origin_host = origin
        .split_once("://")
        .map_or(origin.as_str(), |(_, rest)| rest)
        .trim_end_matches('/');
    origin_host == host.trim().to_ascii_lowercase()
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

#[cfg(test)]
mod origin_tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(origin) = origin {
            map.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        }
        if let Some(host) = host {
            map.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        }
        map
    }

    #[test]
    fn same_origin_by_default() {
        assert!(origin_allowed(&headers(None, Some("x:1")), &[]));
        assert!(origin_allowed(
            &headers(Some("http://Studio.local:8080"), Some("studio.local:8080")),
            &[]
        ));
        assert!(origin_allowed(
            &headers(Some("https://studio.local/"), Some("studio.local")),
            &[]
        ));
        assert!(!origin_allowed(
            &headers(Some("http://evil.example"), Some("studio.local")),
            &[]
        ));
        assert!(!origin_allowed(
            &headers(Some("http://studio.local:9000"), Some("studio.local:8080")),
            &[]
        ));
        assert!(!origin_allowed(&headers(Some("http://a"), None), &[]));
        assert!(!origin_allowed(&headers(Some("null"), Some("a")), &[]));
    }

    #[test]
    fn allow_list_when_configured() {
        let allowed = vec!["https://studio.example".to_owned()];
        assert!(origin_allowed(
            &headers(Some("https://Studio.example/"), Some("other")),
            &allowed
        ));
        assert!(!origin_allowed(
            &headers(
                Some("https://studio.example:8443"),
                Some("studio.example:8443")
            ),
            &allowed
        ));
        assert!(origin_allowed(&headers(None, None), &allowed));
    }
}
