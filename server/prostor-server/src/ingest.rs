//! `GET /ws/ingest/{room}`: the producer's FLV tags, protocol v1 (`prostor_model::ingest`,
//! `docs/protocol-ingest.md`).
//!
//! A session: `hello` within [`ws::Timeouts::hello`] (auth checked when a secret is
//! configured), `started` back, binary frames parsed with `prostor_protocol::ingest` and
//! handed to the [`Supervisor`], `ack` every [`ACK_INTERVAL`] when the acknowledged
//! sequence number changed, `egress_status` every [`STATUS_INTERVAL`], and
//! `end_of_stream` → drain → `ended` → close 1000. Every failure is answered with an
//! `error` message and a close code before the socket is dropped; error texts never
//! contain client bytes, and `hello.auth` is never logged.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::HeaderMap,
    response::Response,
};
use prostor_model::{
    EgressState, IngestClientMessage, IngestErrorCode, IngestServerMessage, StreamInfo,
};
use prostor_protocol::ingest::parse_frame;
use tokio::sync::OwnedSemaphorePermit;

use crate::{
    app::AppState,
    egress::{FileEgress, RtmpEgress, Supervisor, SupervisorError},
    room::RoomId,
    shutdown::Shutdown,
    ws::{self, CLOSE_GOING_AWAY, CLOSE_INTERNAL, CLOSE_NORMAL, INGEST_MAX_MESSAGE, Peer},
};

/// How often the acknowledged sequence number is sent, when it changed (ADR 0003 §3).
pub const ACK_INTERVAL: Duration = Duration::from_millis(100);
/// How often the egress states are sent.
pub const STATUS_INTERVAL: Duration = Duration::from_secs(1);
/// How long the egresses get to drain and finalize at the end of a session.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Close code: a message or frame could not be parsed, or the protocol was violated.
pub const CLOSE_BAD_MESSAGE: u16 = 4400;
/// Close code: `hello.auth` is missing or wrong.
pub const CLOSE_UNAUTHORIZED: u16 = 4401;

/// Largest frame dimension accepted in `hello`.
const MAX_DIMENSION: u32 = 8192;

pub async fn ingest(
    Path(room): Path<RoomId>,
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if state.shutdown().is_cancelled() {
        return ws::shutting_down();
    }
    if !ws::origin_allowed(&headers, state.allowed_origins()) {
        return ws::forbidden_origin();
    }
    let Some(permit) = state.socket_permit() else {
        return ws::too_many_sockets();
    };
    let tracker = state.shutdown().clone();
    ws.max_message_size(INGEST_MAX_MESSAGE)
        .max_frame_size(INGEST_MAX_MESSAGE)
        .on_upgrade(move |socket| tracker.track(handle(socket, room, state, permit)))
}

/// How a session ended.
enum End {
    /// `end_of_stream` received; the egresses have been drained.
    Ended,
    /// The peer closed, went away, timed out or sent something the socket rejected.
    PeerGone,
    /// The server is shutting down.
    Shutdown,
    /// Tell the peer why and close with `close`.
    Error {
        code: IngestErrorCode,
        message: String,
        close: u16,
    },
}

impl End {
    fn protocol(message: &str) -> Self {
        End::Error {
            code: IngestErrorCode::Protocol,
            message: message.to_owned(),
            close: CLOSE_BAD_MESSAGE,
        }
    }

    fn bad_message(message: &str) -> Self {
        End::Error {
            code: IngestErrorCode::BadMessage,
            message: message.to_owned(),
            close: CLOSE_BAD_MESSAGE,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            End::Ended => "ended",
            End::PeerGone => "peer gone",
            End::Shutdown => "shutdown",
            End::Error { .. } => "error",
        }
    }
}

async fn handle(socket: WebSocket, room: RoomId, state: AppState, _permit: OwnedSemaphorePermit) {
    let _session = state.ingest_session();
    let shutdown = state.shutdown().clone();
    let mut peer = Peer::new(socket, state.timeouts());
    tracing::info!(%room, "ingest connected");

    let info = match handshake(&mut peer, &state, &shutdown).await {
        Ok(info) => info,
        Err(end) => {
            tracing::info!(%room, outcome = end.name(), "ingest closed before start");
            finish(&mut peer, end, None).await;
            return;
        }
    };
    let (mut supervisor, recording) = match open(&room, &state, info).await {
        Ok(opened) => opened,
        Err(end) => {
            tracing::info!(%room, outcome = end.name(), "ingest closed before start");
            finish(&mut peer, end, None).await;
            return;
        }
    };
    tracing::info!(%room, recording, "ingest started");
    let started = IngestServerMessage::Started {
        room: room.to_string(),
        recording: Some(recording.clone()),
    };
    let end = if send(&mut peer, &started).await.is_err() {
        End::PeerGone
    } else {
        stream(&mut peer, &mut supervisor, &shutdown).await
    };

    let states = supervisor.stop(STOP_TIMEOUT).await;
    finish(&mut peer, end, Some((&supervisor, &states))).await;
    let (tags, bytes) = supervisor.received();
    let egress: Vec<String> = states
        .iter()
        .map(|s| format!("{}:{:?}/{}/{}dropped", s.id, s.state, s.tags, s.dropped))
        .collect();
    tracing::info!(
        %room, recording, tags, bytes, last_seq = supervisor.last_seq(),
        egress = ?egress, "ingest closed"
    );
}

/// Waits for `hello`, checks the secret and validates the stream description.
async fn handshake(
    peer: &mut Peer,
    state: &AppState,
    shutdown: &Shutdown,
) -> Result<StreamInfo, End> {
    let deadline = tokio::time::sleep(state.timeouts().hello);
    tokio::pin!(deadline);
    loop {
        let message = tokio::select! {
            message = peer.next() => message,
            _ = &mut deadline => return Err(End::protocol("expected hello within the timeout")),
            _ = shutdown.cancelled() => return Err(End::Shutdown),
        };
        let text = match message {
            None => return Err(End::PeerGone),
            Some(Message::Ping(_) | Message::Pong(_) | Message::Close(_)) => continue,
            Some(Message::Binary(_)) => return Err(End::protocol("expected hello before media")),
            Some(Message::Text(text)) => text,
        };
        let parsed = serde_json::from_str::<IngestClientMessage>(&text)
            .map_err(|_| End::bad_message("invalid message"))?;
        return match parsed {
            IngestClientMessage::Hello { auth, stream } => {
                if let Some(secret) = state.secret()
                    && !auth.as_deref().is_some_and(|auth| secret.verify(auth))
                {
                    return Err(End::Error {
                        code: IngestErrorCode::Unauthorized,
                        message: "missing or wrong auth".to_owned(),
                        close: CLOSE_UNAUTHORIZED,
                    });
                }
                validate(&stream)?;
                Ok(stream)
            }
            IngestClientMessage::EndOfStream { .. } => Err(End::protocol("expected hello first")),
        };
    }
}

fn validate(stream: &StreamInfo) -> Result<(), End> {
    if !(1..=MAX_DIMENSION).contains(&stream.width) || !(1..=MAX_DIMENSION).contains(&stream.height)
    {
        return Err(End::protocol("unsupported frame size"));
    }
    if !(stream.fps.is_finite() && stream.fps > 0.0 && stream.fps <= 240.0) {
        return Err(End::protocol("unsupported frame rate"));
    }
    if !stream.video_codec.starts_with("avc1.") {
        return Err(End::protocol("unsupported video codec (H.264 only)"));
    }
    if stream
        .audio_codec
        .as_deref()
        .is_some_and(|codec| codec != "mp4a.40.2")
    {
        return Err(End::protocol("unsupported audio codec (AAC-LC only)"));
    }
    Ok(())
}

/// Builds the supervisor and the recording. The file is created here so a failure is
/// reported before `started`.
async fn open(
    room: &RoomId,
    state: &AppState,
    info: StreamInfo,
) -> Result<(Supervisor, String), End> {
    let mut supervisor = Supervisor::new(room.clone(), info).map_err(|error| {
        tracing::error!(%room, %error, "cannot build the stream metadata");
        End::Error {
            code: IngestErrorCode::Internal,
            message: "cannot build the stream metadata".to_owned(),
            close: CLOSE_INTERNAL,
        }
    })?;
    let file = FileEgress::create(state.recordings_dir(), room, SystemTime::now())
        .await
        .map_err(|error| {
            tracing::error!(%room, %error, dir = %state.recordings_dir().display(),
                "cannot create the recording");
            End::Error {
                code: IngestErrorCode::Egress,
                message: "cannot create the recording".to_owned(),
                close: CLOSE_INTERNAL,
            }
        })?;
    let name = file.file_name().to_owned();
    supervisor.add(Box::new(file));
    for destination in state.rooms().destinations(room) {
        tracing::info!(%room, id = %destination.id, target = %destination.target.tc_url(),
            "adding rtmp egress");
        supervisor.add(Box::new(RtmpEgress::new(
            format!("rtmp-{}", destination.id),
            destination.target,
            state.connect_policy(),
        )));
    }
    Ok((supervisor, name))
}

/// The media loop.
async fn stream(peer: &mut Peer, supervisor: &mut Supervisor, shutdown: &Shutdown) -> End {
    let mut ack = tokio::time::interval(ACK_INTERVAL);
    ack.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut status = tokio::time::interval(STATUS_INTERVAL);
    status.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_acked = 0u32;
    loop {
        tokio::select! {
            message = peer.next() => match message {
                None => return End::PeerGone,
                Some(Message::Binary(data)) => {
                    let (seq, tag) = match parse_frame(&data) {
                        Ok(frame) => frame,
                        Err(error) => return End::bad_message(&format!("invalid media frame: {error}")),
                    };
                    if let Err(error) = supervisor.push(seq, tag.to_owned()).await {
                        return match error {
                            SupervisorError::Flv(error) => {
                                End::bad_message(&format!("invalid media tag: {error}"))
                            }
                            SupervisorError::Protocol(why) => End::protocol(why),
                        };
                    }
                }
                Some(Message::Text(text)) => match serde_json::from_str::<IngestClientMessage>(&text) {
                    Ok(IngestClientMessage::EndOfStream { last_seq }) => {
                        if last_seq != supervisor.last_seq() {
                            tracing::warn!(
                                announced = last_seq, received = supervisor.last_seq(),
                                "end_of_stream disagrees with the frames received"
                            );
                        }
                        return End::Ended;
                    }
                    Ok(IngestClientMessage::Hello { .. }) => {
                        return End::protocol("hello was already received");
                    }
                    Err(_) => return End::bad_message("invalid message"),
                },
                Some(Message::Ping(_) | Message::Pong(_) | Message::Close(_)) => {}
            },
            _ = ack.tick() => {
                let acked = supervisor.acked_seq();
                if acked != last_acked {
                    last_acked = acked;
                    let message = IngestServerMessage::Ack { seq: acked, server_ms: server_ms() };
                    if send(peer, &message).await.is_err() {
                        return End::PeerGone;
                    }
                }
            }
            _ = status.tick() => {
                let message = IngestServerMessage::EgressStatus { egress: supervisor.states() };
                if send(peer, &message).await.is_err() {
                    return End::PeerGone;
                }
            }
            _ = shutdown.cancelled() => return End::Shutdown,
        }
    }
}

/// Tells the peer how the session ended and completes the close handshake.
async fn finish(peer: &mut Peer, end: End, session: Option<(&Supervisor, &Vec<EgressState>)>) {
    match end {
        End::Ended => {
            if let Some((supervisor, states)) = session {
                let status = IngestServerMessage::EgressStatus {
                    egress: states.clone(),
                };
                let ended = IngestServerMessage::Ended {
                    last_seq: supervisor.last_seq(),
                };
                if send(peer, &status).await.is_ok() {
                    let _ = send(peer, &ended).await;
                }
            }
            peer.close(CLOSE_NORMAL, "end of stream").await;
        }
        End::Shutdown => peer.close(CLOSE_GOING_AWAY, "server shutting down").await,
        End::PeerGone => {}
        End::Error {
            code,
            message,
            close,
        } => {
            let reason = serde_json::to_value(code)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let _ = send(peer, &IngestServerMessage::Error { code, message }).await;
            peer.close(close, reason).await;
        }
    }
}

async fn send(peer: &mut Peer, message: &IngestServerMessage) -> Result<(), axum::Error> {
    let text = serde_json::to_string(message).map_err(axum::Error::new)?;
    peer.send(Message::Text(text.into())).await
}

/// Wall clock in milliseconds since the Unix epoch, for `ack.server_ms`.
fn server_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn rejected(stream: &StreamInfo) -> String {
        match validate(stream) {
            Err(End::Error {
                message,
                code,
                close,
            }) => {
                assert_eq!(code, IngestErrorCode::Protocol);
                assert_eq!(close, CLOSE_BAD_MESSAGE);
                message
            }
            Err(_) => panic!("wrong end"),
            Ok(()) => panic!("accepted"),
        }
    }

    #[test]
    fn validates_the_stream_description() {
        assert!(validate(&stream()).is_ok());
        let mut video_only = stream();
        video_only.audio_codec = None;
        assert!(validate(&video_only).is_ok());

        let mut bad = stream();
        bad.width = 0;
        assert!(rejected(&bad).contains("frame size"));
        bad = stream();
        bad.height = MAX_DIMENSION + 1;
        assert!(rejected(&bad).contains("frame size"));
        bad = stream();
        bad.fps = f64::NAN;
        assert!(rejected(&bad).contains("frame rate"));
        bad = stream();
        bad.video_codec = "vp09.00.10.08".into();
        assert!(rejected(&bad).contains("video codec"));
        bad = stream();
        bad.audio_codec = Some("opus".into());
        assert!(rejected(&bad).contains("audio codec"));
    }

    #[test]
    fn server_ms_is_a_recent_wall_clock() {
        assert!(server_ms() > 1.7e12);
    }
}
