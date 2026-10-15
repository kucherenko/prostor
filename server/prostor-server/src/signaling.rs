//! `GET /ws/room/{id}`: the producer's control channel, protocol v1
//! (`prostor_model::signaling`, `docs/protocol-ingest.md`).
//!
//! Phase 0 scope: `hello` (producer auth against `PROSTOR_SECRET`), `start_stream` with
//! destinations and keys (held in [`Rooms`] for the next ingest session, cleared by
//! `stop_stream` or when this socket closes) and `stop_stream`. Guests are welcomed and
//! then refused everything else until M5 adds WebRTC signaling. Keys never appear in
//! logs or in any message the server sends.

use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::HeaderMap,
    response::Response,
};
use prostor_model::{
    DestinationSummary, PeerRole, RoomClientMessage, RoomErrorCode, RoomServerMessage,
    signaling::MAX_DESTINATIONS,
};
use prostor_protocol::rtmp::Target;
use tokio::sync::OwnedSemaphorePermit;

use crate::{
    app::AppState,
    ingest::{CLOSE_BAD_MESSAGE, CLOSE_UNAUTHORIZED},
    room::RoomId,
    rooms::Destination,
    ws::{self, CLOSE_GOING_AWAY, Peer, SIGNALING_MAX_MESSAGE},
};

pub async fn room(
    // Extractors run in order: the room id is validated before the upgrade is attempted,
    // so a bad id is a 400 whether or not the request is a WebSocket handshake.
    Path(id): Path<RoomId>,
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
    ws.max_message_size(SIGNALING_MAX_MESSAGE)
        .max_frame_size(SIGNALING_MAX_MESSAGE)
        .on_upgrade(move |socket| tracker.track(handle(socket, id, state, permit)))
}

enum End {
    PeerGone,
    Shutdown,
    Error {
        code: RoomErrorCode,
        message: &'static str,
        close: u16,
    },
}

impl End {
    fn protocol(message: &'static str) -> Self {
        End::Error {
            code: RoomErrorCode::Protocol,
            message,
            close: CLOSE_BAD_MESSAGE,
        }
    }
}

async fn handle(socket: WebSocket, room: RoomId, state: AppState, _permit: OwnedSemaphorePermit) {
    let shutdown = state.shutdown().clone();
    let mut peer = Peer::new(socket, state.timeouts());
    tracing::info!(%room, "signaling peer connected");
    let mut role = None;
    let end = session(&mut peer, &room, &state, &mut role).await;
    if role == Some(PeerRole::Producer) {
        // The keys go with the producer.
        state.rooms().clear(&room);
    }
    match end {
        End::PeerGone => {}
        End::Shutdown => peer.close(CLOSE_GOING_AWAY, "server shutting down").await,
        End::Error {
            code,
            message,
            close,
        } => {
            let _ = send(
                &mut peer,
                &RoomServerMessage::Error {
                    code,
                    message: message.to_owned(),
                },
            )
            .await;
            peer.close(close, message).await;
        }
    }
    let _ = shutdown;
    tracing::info!(%room, role = ?role, "signaling peer left");
}

async fn session(
    peer: &mut Peer,
    room: &RoomId,
    state: &AppState,
    role: &mut Option<PeerRole>,
) -> End {
    let shutdown = state.shutdown().clone();
    let deadline = tokio::time::sleep(state.timeouts().hello);
    tokio::pin!(deadline);
    loop {
        let message = tokio::select! {
            message = peer.next() => message,
            _ = &mut deadline, if role.is_none() => {
                return End::protocol("expected hello within the timeout");
            }
            _ = shutdown.cancelled() => return End::Shutdown,
        };
        let text = match message {
            None => return End::PeerGone,
            Some(Message::Text(text)) => text,
            Some(Message::Binary(_)) => return End::protocol("binary frames are not used here"),
            Some(_) => continue,
        };
        let parsed = match serde_json::from_str::<RoomClientMessage>(&text) {
            Ok(parsed) => parsed,
            Err(_) => {
                return End::Error {
                    code: RoomErrorCode::BadMessage,
                    message: "invalid message",
                    close: CLOSE_BAD_MESSAGE,
                };
            }
        };
        match (parsed, *role) {
            (RoomClientMessage::Hello { auth, role: wanted }, None) => {
                if wanted == PeerRole::Producer
                    && let Some(secret) = state.secret()
                    && !auth.as_deref().is_some_and(|auth| secret.verify(auth))
                {
                    return End::Error {
                        code: RoomErrorCode::Unauthorized,
                        message: "missing or wrong auth",
                        close: CLOSE_UNAUTHORIZED,
                    };
                }
                *role = Some(wanted);
                let welcome = RoomServerMessage::Welcome {
                    room: room.to_string(),
                    role: wanted,
                };
                if send(peer, &welcome).await.is_err() {
                    return End::PeerGone;
                }
            }
            (RoomClientMessage::Hello { .. }, Some(_)) => {
                return End::protocol("hello was already received");
            }
            (_, None) => return End::protocol("expected hello first"),
            (RoomClientMessage::StartStream { destinations }, Some(PeerRole::Producer)) => {
                let reply = match parse_destinations(destinations) {
                    Ok(parsed) => {
                        let summary = parsed
                            .iter()
                            .map(|d| DestinationSummary {
                                id: d.id.clone().into(),
                                name: d.name.clone(),
                            })
                            .collect();
                        tracing::info!(%room, destinations = parsed.len(), "stream armed");
                        state.rooms().set_destinations(room, parsed);
                        RoomServerMessage::StreamReady {
                            destinations: summary,
                        }
                    }
                    Err(message) => RoomServerMessage::Error {
                        code: RoomErrorCode::InvalidDestination,
                        message,
                    },
                };
                if send(peer, &reply).await.is_err() {
                    return End::PeerGone;
                }
            }
            (RoomClientMessage::StopStream, Some(PeerRole::Producer)) => {
                state.rooms().clear(room);
                if send(peer, &RoomServerMessage::StreamStopped).await.is_err() {
                    return End::PeerGone;
                }
            }
            (_, Some(PeerRole::Guest)) => return End::protocol("producer-only message"),
        }
    }
}

/// Validates the declaration. The error names a destination id only: the URL could
/// contain a pasted key and the key itself must never travel back.
fn parse_destinations(
    specs: Vec<prostor_model::DestinationSpec>,
) -> Result<Vec<Destination>, String> {
    if specs.len() > MAX_DESTINATIONS {
        return Err(format!("at most {MAX_DESTINATIONS} destinations"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut destinations = Vec::with_capacity(specs.len());
    for spec in specs {
        let id = spec.id.as_str().to_owned();
        if id.is_empty() || id.len() > 64 || !seen.insert(id.clone()) {
            return Err("destination ids must be unique and 1-64 characters".to_owned());
        }
        let target =
            Target::new(&spec.url, &spec.key).map_err(|e| format!("destination {id}: {e}"))?;
        destinations.push(Destination {
            id,
            name: spec.name.chars().take(64).collect(),
            target,
        });
    }
    Ok(destinations)
}

async fn send(peer: &mut Peer, message: &RoomServerMessage) -> Result<(), axum::Error> {
    let text = serde_json::to_string(message).map_err(axum::Error::new)?;
    peer.send(Message::Text(text.into())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostor_model::DestinationSpec;

    fn spec(id: &str, url: &str, key: &str) -> DestinationSpec {
        DestinationSpec {
            id: id.into(),
            name: id.to_uppercase(),
            url: url.into(),
            key: key.into(),
        }
    }

    #[test]
    fn parses_destinations_and_refuses_bad_ones_without_echoing_secrets() {
        let ok = parse_destinations(vec![
            spec("a", "rtmp://live.twitch.tv/app", "KEY-A"),
            spec("b", "rtmps://a.rtmps.youtube.com/live2?backup=1", "KEY-B"),
        ])
        .unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[1].target.stream_key.expose_secret(), "KEY-B?backup=1");

        let bad =
            parse_destinations(vec![spec("x", "http://KEY-IN-URL/app", "KEY-X")]).unwrap_err();
        assert!(bad.contains("destination x"), "{bad}");
        assert!(!bad.contains("KEY"), "{bad}");

        let empty_key = parse_destinations(vec![spec("y", "rtmp://h/app", "")]).unwrap_err();
        assert!(empty_key.contains("destination y"), "{empty_key}");

        let dup = parse_destinations(vec![
            spec("a", "rtmp://h/app", "k"),
            spec("a", "rtmp://h/app", "k"),
        ])
        .unwrap_err();
        assert!(dup.contains("unique"), "{dup}");

        let too_many: Vec<_> = (0..=MAX_DESTINATIONS)
            .map(|i| spec(&format!("d{i}"), "rtmp://h/app", "k"))
            .collect();
        assert!(
            parse_destinations(too_many)
                .unwrap_err()
                .contains("at most")
        );
    }
}
