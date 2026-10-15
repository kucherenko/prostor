//! Signaling protocol v1: the producer's control channel at `/ws/room/<id>` (text frames,
//! JSON with a `type` field). In Phase 0 it carries the stream's destinations and keys
//! (`start_stream`), which never travel over the ingest socket and are never written
//! anywhere; M5 adds the guest handshake (offer/answer/ICE relay) to the same socket.
//!
//! A session: `hello` within 5 seconds (the producer's `auth` is the shared secret when
//! the server has one), `welcome` back; `start_stream` declares destinations and is
//! answered with `stream_ready` once every destination parsed; the browser then opens the
//! ingest socket, whose session fans out to those destinations; `stop_stream` (or the
//! socket closing) forgets them. See `docs/protocol-ingest.md`.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::DestinationId;

/// Most destinations a session may declare.
pub const MAX_DESTINATIONS: usize = 8;

/// Who is connecting to the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PeerRole {
    /// The studio: may declare destinations and start the stream.
    Producer,
    /// A guest (M5): may only exchange WebRTC signaling.
    Guest,
}

/// A destination as declared at stream start: the board's public URL plus the key the
/// producer typed in. `Debug` redacts the key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DestinationSpec {
    pub id: DestinationId,
    /// Shown in status reports.
    pub name: String,
    /// `rtmp://` or `rtmps://` URL without the key, e.g. `rtmp://live.twitch.tv/app`.
    pub url: String,
    /// The stream key. Memory only, never logged, never persisted by the server.
    pub key: String,
}

impl fmt::Debug for DestinationSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DestinationSpec")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("key", &"<redacted>")
            .finish()
    }
}

/// Messages from the browser to the server.
#[derive(Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum RoomClientMessage {
    /// First message of a session. `auth` is the producer secret when the server has one
    /// configured; compared in constant time and never logged.
    Hello {
        auth: Option<String>,
        role: PeerRole,
    },
    /// Declares where the next ingest session goes. Replaces any earlier declaration.
    StartStream { destinations: Vec<DestinationSpec> },
    /// Forgets the destinations (and their keys).
    StopStream,
}

impl fmt::Debug for RoomClientMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoomClientMessage::Hello { auth, role } => f
                .debug_struct("Hello")
                .field("auth", &auth.as_ref().map(|_| "<redacted>"))
                .field("role", role)
                .finish(),
            RoomClientMessage::StartStream { destinations } => f
                .debug_struct("StartStream")
                .field("destinations", destinations)
                .finish(),
            RoomClientMessage::StopStream => f.write_str("StopStream"),
        }
    }
}

/// A destination as the server reports it back: no URL, no key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DestinationSummary {
    pub id: DestinationId,
    pub name: String,
}

/// Messages from the server to the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum RoomServerMessage {
    /// Answer to `hello`.
    Welcome { room: String, role: PeerRole },
    /// Answer to `start_stream`: every destination parsed and is armed for the next
    /// ingest session.
    StreamReady {
        destinations: Vec<DestinationSummary>,
    },
    /// Answer to `stop_stream`.
    StreamStopped,
    /// The server is about to close the socket with a 4xxx code, or refused one message.
    Error {
        code: RoomErrorCode,
        message: String,
    },
}

/// Why the server refused a message or closes the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RoomErrorCode {
    /// `hello.auth` is missing or wrong (close code 4401).
    Unauthorized,
    /// A text message was not valid JSON for this protocol (close code 4400).
    BadMessage,
    /// `hello` missing, late or repeated; a producer-only message from a guest (4400).
    Protocol,
    /// One destination in `start_stream` did not parse; the socket stays open and the
    /// earlier declaration stands. `message` names the destination id, never the URL
    /// or the key.
    InvalidDestination,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> DestinationSpec {
        DestinationSpec {
            id: DestinationId::new("d1"),
            name: "Twitch".into(),
            url: "rtmp://live.twitch.tv/app".into(),
            key: "live_123_SECRET".into(),
        }
    }

    #[test]
    fn client_messages_have_the_documented_shape() {
        assert_eq!(
            serde_json::to_value(RoomClientMessage::Hello {
                auth: Some("s".into()),
                role: PeerRole::Producer,
            })
            .unwrap(),
            json!({ "type": "hello", "auth": "s", "role": "producer" })
        );
        assert_eq!(
            serde_json::to_value(RoomClientMessage::StartStream {
                destinations: vec![spec()],
            })
            .unwrap(),
            json!({
                "type": "start_stream",
                "destinations": [{
                    "id": "d1", "name": "Twitch", "url": "rtmp://live.twitch.tv/app",
                    "key": "live_123_SECRET"
                }]
            })
        );
        assert_eq!(
            serde_json::to_value(RoomClientMessage::StopStream).unwrap(),
            json!({ "type": "stop_stream" })
        );
    }

    #[test]
    fn server_messages_have_the_documented_shape() {
        assert_eq!(
            serde_json::to_value(RoomServerMessage::Welcome {
                room: "r".into(),
                role: PeerRole::Guest,
            })
            .unwrap(),
            json!({ "type": "welcome", "room": "r", "role": "guest" })
        );
        assert_eq!(
            serde_json::to_value(RoomServerMessage::StreamReady {
                destinations: vec![DestinationSummary {
                    id: DestinationId::new("d1"),
                    name: "Twitch".into(),
                }],
            })
            .unwrap(),
            json!({ "type": "stream_ready", "destinations": [{ "id": "d1", "name": "Twitch" }] })
        );
        assert_eq!(
            serde_json::to_value(RoomServerMessage::Error {
                code: RoomErrorCode::InvalidDestination,
                message: "d1".into(),
            })
            .unwrap(),
            json!({ "type": "error", "code": "invalid_destination", "message": "d1" })
        );
        let json = serde_json::to_string(&RoomServerMessage::StreamStopped).unwrap();
        assert_eq!(
            serde_json::from_str::<RoomServerMessage>(&json).unwrap(),
            RoomServerMessage::StreamStopped
        );
    }

    #[test]
    fn debug_output_never_contains_secrets() {
        let hello = RoomClientMessage::Hello {
            auth: Some("hunter2".into()),
            role: PeerRole::Producer,
        };
        let start = RoomClientMessage::StartStream {
            destinations: vec![spec()],
        };
        for text in [
            format!("{hello:?}"),
            format!("{hello:#?}"),
            format!("{start:?}"),
            format!("{start:#?}"),
            format!("{:?}", spec()),
        ] {
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains("SECRET"), "{text}");
        }
        assert!(format!("{start:?}").contains("live.twitch.tv"));
        assert!(format!("{start:?}").contains("<redacted>"));
    }
}
