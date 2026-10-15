//! Shared helpers for the integration tests: the FLV fixture and an in-process RTMP
//! server built on `rml_rtmp`'s `ServerSession`, which records what it receives and can
//! misbehave on request (reject the publish, drop the socket mid-stream).

#![allow(dead_code)]

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use prostor_protocol::flv::{Header, TagBuf, TagKind, parse_tag};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub const FIXTURE: &[u8] =
    include_bytes!("../../../../core/prostor-protocol/tests/fixtures/testsrc-720p-1s.flv");

/// The fixture's media tags (everything but the script tag): raw wire bytes and parsed.
pub fn fixture_media() -> Vec<(Vec<u8>, TagBuf)> {
    let (_, mut pos) = Header::parse(FIXTURE).unwrap().unwrap();
    let mut tags = Vec::new();
    while pos < FIXTURE.len() {
        let (tag, n) = parse_tag(&FIXTURE[pos..]).unwrap().unwrap();
        if tag.kind != TagKind::Script {
            tags.push((FIXTURE[pos..pos + n].to_vec(), tag.to_owned()));
        }
        pos += n;
    }
    assert_eq!(tags.len(), 82);
    tags
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedTag {
    pub connection: u32,
    pub kind: TagKind,
    pub timestamp: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Recorded {
    pub connections: u32,
    /// `(app, key)` per accepted or rejected publish request.
    pub publishes: Vec<(String, String)>,
    pub metadata_widths: Vec<u32>,
    pub tags: Vec<RecordedTag>,
    pub finished: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Reject every publish request with this status code.
    pub reject_publish: Option<&'static str>,
    /// Close the first connection's socket after this many media tags.
    pub drop_after_tags: Option<usize>,
}

pub struct RtmpTestServer {
    pub addr: SocketAddr,
    recorded: Arc<Mutex<Recorded>>,
}

impl RtmpTestServer {
    pub async fn start(options: Options) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let shared = recorded.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let connection = {
                    let mut r = shared.lock().unwrap();
                    r.connections += 1;
                    r.connections
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(socket, connection, options, shared).await;
                });
            }
        });
        RtmpTestServer { addr, recorded }
    }

    pub fn url(&self, app: &str, key: &str) -> String {
        format!("rtmp://{}/{app}/{key}", self.addr)
    }

    pub fn recorded(&self) -> Recorded {
        self.recorded.lock().unwrap().clone()
    }

    /// Polls until `predicate` holds or `timeout` passes.
    pub async fn wait_for(&self, timeout: Duration, predicate: impl Fn(&Recorded) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if predicate(&self.recorded()) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn serve(
    mut socket: TcpStream,
    connection: u32,
    options: Options,
    recorded: Arc<Mutex<Recorded>>,
) -> std::io::Result<()> {
    let mut handshake = Handshake::new(PeerType::Server);
    let mut buf = vec![0u8; 16 * 1024];
    let mut session: Option<ServerSession> = None;
    let mut leftover = Vec::new();
    while session.is_none() {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        match handshake.process_bytes(&buf[..n]).unwrap() {
            HandshakeProcessResult::InProgress { response_bytes } => {
                socket.write_all(&response_bytes).await?;
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                socket.write_all(&response_bytes).await?;
                let (s, results) = ServerSession::new(ServerSessionConfig::new()).unwrap();
                session = Some(s);
                leftover = remaining_bytes;
                let mut out = Vec::new();
                handle_results(
                    session.as_mut().unwrap(),
                    results,
                    connection,
                    options,
                    &recorded,
                    &mut out,
                );
                socket.write_all(&out).await?;
            }
        }
    }
    let session = session.as_mut().unwrap();
    let mut media_tags = 0usize;
    let mut pending = leftover;
    loop {
        if !pending.is_empty() {
            let input = std::mem::take(&mut pending);
            let results = match session.handle_input(&input) {
                Ok(results) => results,
                Err(_) => return Ok(()),
            };
            let mut out = Vec::new();
            let media = handle_results(session, results, connection, options, &recorded, &mut out);
            socket.write_all(&out).await?;
            media_tags += media;
            if let Some(limit) = options.drop_after_tags
                && connection == 1
                && media_tags >= limit
            {
                // Simulate the platform going away: no deleteStream, just a dead socket.
                drop(socket);
                return Ok(());
            }
        }
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        pending.extend_from_slice(&buf[..n]);
    }
}

/// Applies the session results; returns how many media tags were received.
fn handle_results(
    session: &mut ServerSession,
    results: Vec<ServerSessionResult>,
    connection: u32,
    options: Options,
    recorded: &Arc<Mutex<Recorded>>,
    out: &mut Vec<u8>,
) -> usize {
    let mut media = 0;
    for result in results {
        match result {
            ServerSessionResult::OutboundResponse(packet) => out.extend_from_slice(&packet.bytes),
            ServerSessionResult::RaisedEvent(event) => {
                let more = match event {
                    ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                        session.accept_request(request_id).unwrap()
                    }
                    ServerSessionEvent::PublishStreamRequested {
                        request_id,
                        app_name,
                        stream_key,
                        ..
                    } => {
                        recorded
                            .lock()
                            .unwrap()
                            .publishes
                            .push((app_name, stream_key));
                        match options.reject_publish {
                            Some(code) => {
                                session.reject_request(request_id, code, "refused").unwrap()
                            }
                            None => session.accept_request(request_id).unwrap(),
                        }
                    }
                    ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                        recorded
                            .lock()
                            .unwrap()
                            .metadata_widths
                            .push(metadata.video_width.unwrap_or(0));
                        Vec::new()
                    }
                    ServerSessionEvent::VideoDataReceived {
                        data, timestamp, ..
                    } => {
                        media += 1;
                        recorded.lock().unwrap().tags.push(RecordedTag {
                            connection,
                            kind: TagKind::Video,
                            timestamp: timestamp.value,
                            data: data.to_vec(),
                        });
                        Vec::new()
                    }
                    ServerSessionEvent::AudioDataReceived {
                        data, timestamp, ..
                    } => {
                        media += 1;
                        recorded.lock().unwrap().tags.push(RecordedTag {
                            connection,
                            kind: TagKind::Audio,
                            timestamp: timestamp.value,
                            data: data.to_vec(),
                        });
                        Vec::new()
                    }
                    ServerSessionEvent::PublishStreamFinished { .. } => {
                        recorded.lock().unwrap().finished += 1;
                        Vec::new()
                    }
                    _ => Vec::new(),
                };
                media += handle_results(session, more, connection, options, recorded, out);
            }
            ServerSessionResult::UnhandleableMessageReceived(_) => {}
        }
    }
    media
}
