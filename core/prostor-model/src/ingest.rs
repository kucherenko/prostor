//! Ingest wire protocol v1: the producer's browser pushes FLV tags to the server over a
//! binary WebSocket at `/ws/ingest/<room>`.
//!
//! The protocol has two kinds of frames:
//!
//! - **Text frames** carry JSON messages: [`IngestClientMessage`] from the browser and
//!   [`IngestServerMessage`] from the server. Every message has a `type` field.
//! - **Binary frames** carry media. Each binary frame is exactly
//!   `u32 big-endian seq` followed by **one complete FLV tag** (11-byte tag header, body,
//!   4-byte `PreviousTagSize` trailer) as produced by the browser's `FlvMuxer`. One tag
//!   per message, no batching. Sequence numbers start at 1 and increase by 1 per frame.
//!
//! The browser never sends the FLV file header or the `onMetaData` script tag: the
//! server builds both from [`StreamInfo`] for recordings and for RTMP destinations. The
//! video sequence header must arrive before the first media frame of either kind, the
//! audio sequence header before the first audio frame (the headers may come in either
//! order).
//!
//! A session: the browser sends `hello` within 5 seconds of connecting, the server
//! answers `started`, media frames flow, the server answers with `ack` every 100 ms and
//! `egress_status` every second, the browser sends `end_of_stream`, the server drains,
//! answers `ended` and closes with code 1000. See `docs/protocol-ingest.md`.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Messages from the browser to the server (text frames).
///
/// `Debug` is hand-written: `hello.auth` is the producer secret and prints as
/// `<redacted>`, so a logged message can never leak it.
#[derive(Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum IngestClientMessage {
    /// First message of a session. `auth` is the producer secret when the server has one
    /// configured; it is compared in constant time and never logged.
    Hello {
        auth: Option<String>,
        stream: StreamInfo,
    },
    /// Last message of a session: the sequence number of the last media frame sent.
    /// The server drains its egresses, answers [`IngestServerMessage::Ended`] and closes.
    EndOfStream { last_seq: u32 },
}

impl fmt::Debug for IngestClientMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IngestClientMessage::Hello { auth, stream } => f
                .debug_struct("Hello")
                .field("auth", &auth.as_ref().map(|_| "<redacted>"))
                .field("stream", stream)
                .finish(),
            IngestClientMessage::EndOfStream { last_seq } => f
                .debug_struct("EndOfStream")
                .field("last_seq", last_seq)
                .finish(),
        }
    }
}

/// Parameters of the encoded stream, as configured in the browser's encoders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// WebCodecs codec string of the video encoder, e.g. `avc1.4d0028`.
    pub video_codec: String,
    /// Target video bitrate in bits per second.
    pub video_bitrate: u32,
    /// WebCodecs codec string of the audio encoder (`mp4a.40.2`); `None` for video only.
    pub audio_codec: Option<String>,
    /// Target audio bitrate in bits per second.
    pub audio_bitrate: Option<u32>,
    /// Audio sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Audio channel count.
    pub channels: Option<u8>,
}

/// Messages from the server to the browser (text frames).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum IngestServerMessage {
    /// Answer to `hello`: the session is open. `recording` is the file name (not the
    /// path) of the recording, if one is being written.
    Started {
        room: String,
        recording: Option<String>,
    },
    /// Every live egress has consumed every frame up to and including `seq`.
    /// `server_ms` is the server's wall clock in milliseconds since the Unix epoch.
    Ack { seq: u32, server_ms: f64 },
    /// Per-egress state, sent about once per second and on `end_of_stream`.
    EgressStatus { egress: Vec<EgressState> },
    /// Answer to `end_of_stream`, sent once the egresses have drained.
    Ended { last_seq: u32 },
    /// The server is about to close the socket with a 4xxx code.
    Error {
        code: IngestErrorCode,
        message: String,
    },
}

/// Why the server closes an ingest session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum IngestErrorCode {
    /// `hello.auth` is missing or wrong (close code 4401).
    Unauthorized,
    /// A frame could not be parsed, or a text message was not valid (close code 4400).
    BadMessage,
    /// The session violated the protocol: no `hello` first, media before the video
    /// sequence header, an unsupported codec (close code 4400).
    Protocol,
    /// An egress could not be set up, for example the recordings directory is not
    /// writable (close code 1011).
    Egress,
    /// Anything else (close code 1011).
    Internal,
}

/// Where a session's media goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EgressKind {
    /// An FLV recording on the server's disk.
    File,
    /// An RTMP(S) destination.
    Rtmp,
}

/// Lifecycle of one egress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EgressPhase {
    /// Created, waiting for the first media tag.
    Starting,
    /// Consuming tags.
    Live,
    /// Consuming tags but dropping some (its queue overflowed).
    Degraded,
    /// The connection was lost; tags are dropped until it is back.
    Reconnecting,
    /// Stopped after an error; no longer consuming.
    Failed,
    /// Stopped cleanly.
    Stopped,
}

/// State of one egress, as reported in [`IngestServerMessage::EgressStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EgressState {
    /// Stable within the session, e.g. `file` or `rtmp-<destination id>`.
    pub id: String,
    pub kind: EgressKind,
    pub state: EgressPhase,
    /// Bytes handed to the egress so far.
    #[ts(type = "number")]
    pub bytes: u64,
    /// Tags handed to the egress so far.
    #[ts(type = "number")]
    pub tags: u64,
    /// Tags dropped for this egress: its queue was full, or it was reconnecting.
    #[ts(type = "number")]
    pub dropped: u64,
    /// The newest sequence number the egress has consumed.
    pub last_seq: u32,
    /// Connections re-established since the start (RTMP).
    #[ts(type = "number")]
    pub reconnects: u64,
    /// The last problem, in a few words and never containing a key; `None` when healthy.
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream() -> StreamInfo {
        StreamInfo {
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "avc1.4d0028".into(),
            video_bitrate: 6_000_000,
            audio_codec: Some("mp4a.40.2".into()),
            audio_bitrate: Some(128_000),
            sample_rate: Some(48_000),
            channels: Some(2),
        }
    }

    #[test]
    fn client_messages_have_the_documented_shape() {
        let hello = IngestClientMessage::Hello {
            auth: Some("s3cret".into()),
            stream: stream(),
        };
        assert_eq!(
            serde_json::to_value(&hello).unwrap(),
            json!({
                "type": "hello",
                "auth": "s3cret",
                "stream": {
                    "width": 1920,
                    "height": 1080,
                    "fps": 30.0,
                    "video_codec": "avc1.4d0028",
                    "video_bitrate": 6_000_000,
                    "audio_codec": "mp4a.40.2",
                    "audio_bitrate": 128_000,
                    "sample_rate": 48_000,
                    "channels": 2
                }
            })
        );
        assert_eq!(
            serde_json::to_value(IngestClientMessage::EndOfStream { last_seq: 42 }).unwrap(),
            json!({ "type": "end_of_stream", "last_seq": 42 })
        );
    }

    #[test]
    fn hello_without_auth_or_audio_parses() {
        let parsed: IngestClientMessage = serde_json::from_value(json!({
            "type": "hello",
            "auth": null,
            "stream": {
                "width": 1280, "height": 720, "fps": 30, "video_codec": "avc1.4d001f",
                "video_bitrate": 2_500_000, "audio_codec": null, "audio_bitrate": null,
                "sample_rate": null, "channels": null
            }
        }))
        .unwrap();
        match parsed {
            IngestClientMessage::Hello { auth, stream } => {
                assert_eq!(auth, None);
                assert_eq!(stream.audio_codec, None);
                assert_eq!(stream.width, 1280);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn server_messages_have_the_documented_shape() {
        assert_eq!(
            serde_json::to_value(IngestServerMessage::Started {
                room: "studio".into(),
                recording: Some("studio-20261014-181500.flv".into()),
            })
            .unwrap(),
            json!({ "type": "started", "room": "studio", "recording": "studio-20261014-181500.flv" })
        );
        assert_eq!(
            serde_json::to_value(IngestServerMessage::Ack {
                seq: 7,
                server_ms: 1_760_000_000_000.5,
            })
            .unwrap(),
            json!({ "type": "ack", "seq": 7, "server_ms": 1_760_000_000_000.5 })
        );
        assert_eq!(
            serde_json::to_value(IngestServerMessage::Ended { last_seq: 7 }).unwrap(),
            json!({ "type": "ended", "last_seq": 7 })
        );
        assert_eq!(
            serde_json::to_value(IngestServerMessage::Error {
                code: IngestErrorCode::Unauthorized,
                message: "bad auth".into(),
            })
            .unwrap(),
            json!({ "type": "error", "code": "unauthorized", "message": "bad auth" })
        );
        assert_eq!(
            serde_json::to_value(IngestServerMessage::EgressStatus {
                egress: vec![EgressState {
                    id: "file".into(),
                    kind: EgressKind::File,
                    state: EgressPhase::Live,
                    bytes: 1024,
                    tags: 3,
                    dropped: 0,
                    last_seq: 3,
                    reconnects: 0,
                    detail: None,
                }],
            })
            .unwrap(),
            json!({
                "type": "egress_status",
                "egress": [{
                    "id": "file", "kind": "file", "state": "live",
                    "bytes": 1024, "tags": 3, "dropped": 0, "last_seq": 3,
                    "reconnects": 0, "detail": null
                }]
            })
        );
    }

    #[test]
    fn every_message_round_trips() {
        let messages = vec![
            IngestServerMessage::Started {
                room: "r".into(),
                recording: None,
            },
            IngestServerMessage::Ack {
                seq: 1,
                server_ms: 2.0,
            },
            IngestServerMessage::EgressStatus { egress: vec![] },
            IngestServerMessage::Ended { last_seq: 1 },
            IngestServerMessage::Error {
                code: IngestErrorCode::Egress,
                message: "disk full".into(),
            },
        ];
        for message in messages {
            let json = serde_json::to_string(&message).unwrap();
            let back: IngestServerMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(back, message);
        }
        let hello = IngestClientMessage::Hello {
            auth: None,
            stream: stream(),
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(
            serde_json::from_str::<IngestClientMessage>(&json).unwrap(),
            hello
        );
    }

    #[test]
    fn debug_output_never_contains_the_secret() {
        let hello = IngestClientMessage::Hello {
            auth: Some("hunter2".into()),
            stream: stream(),
        };
        for text in [format!("{hello:?}"), format!("{hello:#?}")] {
            assert!(!text.contains("hunter2"), "{text}");
            assert!(text.contains("<redacted>"), "{text}");
            assert!(text.contains("avc1.4d0028"), "{text}");
        }
        let without = IngestClientMessage::Hello {
            auth: None,
            stream: stream(),
        };
        assert!(format!("{without:?}").contains("auth: None"));
    }

    #[test]
    fn codes_and_phases_are_snake_case_strings() {
        assert_eq!(
            serde_json::to_value(IngestErrorCode::BadMessage).unwrap(),
            json!("bad_message")
        );
        assert_eq!(
            serde_json::to_value(EgressPhase::Degraded).unwrap(),
            json!("degraded")
        );
        assert_eq!(
            serde_json::to_value(EgressKind::Rtmp).unwrap(),
            json!("rtmp")
        );
    }
}
