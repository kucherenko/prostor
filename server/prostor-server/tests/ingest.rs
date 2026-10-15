//! The ingest protocol end to end over a real loopback listener: a full session whose
//! recording parses back to the media tags sent, authentication, protocol errors with
//! their close codes, and a shutdown that finalizes the file.

use std::{net::SocketAddr, path::Path, time::Duration};

use futures_util::{SinkExt, StreamExt};
use prostor_model::{
    EgressPhase, IngestClientMessage, IngestErrorCode, IngestServerMessage, StreamInfo,
};
use prostor_protocol::{
    flv::{Demuxer, Header, TagBuf, TagKind, parse_script_tag, parse_tag},
    ingest::encode_frame,
    secret::Secret,
};
use prostor_server::{app, config::Config, shutdown::Shutdown, ws::Timeouts};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

const FIXTURE: &[u8] =
    include_bytes!("../../../core/prostor-protocol/tests/fixtures/testsrc-720p-1s.flv");

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Server {
    addr: SocketAddr,
    recordings: std::path::PathBuf,
    shutdown: Shutdown,
    _dir: tempfile::TempDir,
}

async fn serve(secret: Option<&str>) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        web_dir: dir.path().to_path_buf(),
        recordings_dir: dir.path().join("recordings"),
        secret: secret.map(Secret::new),
        allowed_origins: vec![],
        allow_private_destinations: false,
    };
    std::fs::create_dir_all(&config.recordings_dir).unwrap();
    let shutdown = Shutdown::new();
    let listener = TcpListener::bind(config.listen).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let timeouts = Timeouts {
        hello: Duration::from_millis(500),
        ..Timeouts::default()
    };
    let router = app::router_with_timeouts(&config, shutdown.clone(), timeouts);
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(serve_shutdown.cancelled())
            .await
            .unwrap();
    });
    Server {
        addr,
        recordings: config.recordings_dir,
        shutdown,
        _dir: dir,
    }
}

async fn connect(addr: SocketAddr, room: &str) -> Socket {
    let config = WebSocketConfig::default()
        .max_message_size(Some(64 * 1024 * 1024))
        .max_frame_size(Some(64 * 1024 * 1024));
    let url = format!("ws://{addr}/ws/ingest/{room}");
    tokio_tungstenite::connect_async_with_config(url, Some(config), false)
        .await
        .map(|(socket, _)| socket)
        .unwrap()
}

/// The fixture's media tags (everything but the script tag), as raw wire bytes and parsed.
fn fixture_media() -> Vec<(Vec<u8>, TagBuf)> {
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

fn stream_info() -> StreamInfo {
    StreamInfo {
        width: 1280,
        height: 720,
        fps: 30.0,
        video_codec: "avc1.4d001f".into(),
        video_bitrate: 2_000_000,
        audio_codec: Some("mp4a.40.2".into()),
        audio_bitrate: Some(128_000),
        sample_rate: Some(48_000),
        channels: Some(2),
    }
}

fn hello(auth: Option<&str>) -> Message {
    let hello = IngestClientMessage::Hello {
        auth: auth.map(str::to_owned),
        stream: stream_info(),
    };
    Message::Text(serde_json::to_string(&hello).unwrap().into())
}

fn frame(seq: u32, tag: &[u8]) -> Message {
    let mut message = Vec::new();
    encode_frame(seq, tag, &mut message);
    Message::Binary(message.into())
}

/// The next server message, skipping pings and pongs. `Close` frames come back as
/// `Err(code)`.
async fn next_message(socket: &mut Socket) -> Result<IngestServerMessage, Option<u16>> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("server message")
        {
            Some(Ok(Message::Text(text))) => {
                return Ok(serde_json::from_str(&text).expect("valid server message"));
            }
            Some(Ok(Message::Close(frame))) => return Err(frame.map(|f| u16::from(f.code))),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(other)) => panic!("unexpected {other:?}"),
            Some(Err(e)) => panic!("socket error {e}"),
            None => return Err(None),
        }
    }
}

/// Expects `error{code}` followed by a close frame with `close`.
async fn expect_error(socket: &mut Socket, code: IngestErrorCode, close: u16) {
    loop {
        match next_message(socket).await {
            Ok(IngestServerMessage::Error { code: got, message }) => {
                assert_eq!(got, code, "{message}");
                assert!(!message.is_empty());
                break;
            }
            Ok(IngestServerMessage::EgressStatus { .. } | IngestServerMessage::Ack { .. }) => {}
            other => panic!("expected error {code:?}, got {other:?}"),
        }
    }
    assert_eq!(next_message(socket).await, Err(Some(close)));
}

async fn expect_started(socket: &mut Socket, room: &str) -> String {
    match next_message(socket).await {
        Ok(IngestServerMessage::Started {
            room: got,
            recording,
        }) => {
            assert_eq!(got, room);
            recording.expect("a recording")
        }
        other => panic!("expected started, got {other:?}"),
    }
}

/// Parses a recording: the header, the metadata (returned as width) and the media tags.
fn parse_recording(path: &Path) -> (Header, f64, Vec<TagBuf>) {
    let bytes = std::fs::read(path).unwrap();
    let mut demuxer = Demuxer::new();
    demuxer.feed(&bytes).unwrap();
    let mut tags = demuxer.drain_tags().unwrap();
    assert_eq!(demuxer.pending(), 0, "trailing bytes in the recording");
    assert!(!tags.is_empty());
    assert_eq!(tags[0].kind, TagKind::Script);
    let (name, meta) = parse_script_tag(&tags[0].body).unwrap();
    assert_eq!(name, "onMetaData");
    let width = meta.get("width").and_then(|v| v.as_f64()).unwrap();
    tags.remove(0);
    (demuxer.header().unwrap(), width, tags)
}

#[tokio::test]
async fn a_full_session_is_recorded_byte_for_byte_and_acknowledged() {
    let server = serve(Some("s3cret")).await;
    let media = fixture_media();
    let mut socket = connect(server.addr, "studio").await;

    socket.send(hello(Some("s3cret"))).await.unwrap();
    let recording = expect_started(&mut socket, "studio").await;
    assert!(
        recording.starts_with("studio-") && recording.ends_with(".flv"),
        "{recording}"
    );
    assert!(
        !recording.contains('/'),
        "a file name, not a path: {recording}"
    );

    for (i, (raw, _)) in media.iter().enumerate() {
        socket.send(frame(i as u32 + 1, raw)).await.unwrap();
    }
    let last_seq = media.len() as u32;
    // Wait for the ack of the last frame before ending, as the browser will.
    let mut acked = 0;
    let mut acks = 0;
    while acked < last_seq {
        match next_message(&mut socket).await {
            Ok(IngestServerMessage::Ack { seq, server_ms }) => {
                assert!(seq > acked, "acks are monotonic");
                assert!(server_ms > 1.7e12);
                acked = seq;
                acks += 1;
            }
            Ok(IngestServerMessage::EgressStatus { egress }) => {
                assert_eq!(egress.len(), 1);
                assert_eq!(egress[0].id, "file");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(acks >= 1);

    let end = IngestClientMessage::EndOfStream { last_seq };
    socket
        .send(Message::Text(serde_json::to_string(&end).unwrap().into()))
        .await
        .unwrap();
    let mut final_status = None;
    loop {
        match next_message(&mut socket).await {
            Ok(IngestServerMessage::EgressStatus { egress }) => final_status = Some(egress),
            Ok(IngestServerMessage::Ack { .. }) => {}
            Ok(IngestServerMessage::Ended { last_seq: got }) => {
                assert_eq!(got, last_seq);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let status = final_status.expect("a final egress_status before ended");
    assert_eq!(status[0].state, EgressPhase::Stopped);
    assert_eq!(
        status[0].tags, 80,
        "82 media tags minus the 2 cached sequence headers"
    );
    assert_eq!(status[0].dropped, 0);
    assert_eq!(status[0].last_seq, last_seq);
    assert_eq!(next_message(&mut socket).await, Err(Some(1000)));

    let path = server.recordings.join(&recording);
    let (header, width, tags) = parse_recording(&path);
    assert_eq!(
        header,
        Header {
            has_audio: true,
            has_video: true
        }
    );
    assert_eq!(width, 1280.0);
    assert_eq!(tags.len(), media.len());
    for (i, (recorded, (_, sent))) in tags.iter().zip(&media).enumerate() {
        assert_eq!(recorded, sent, "tag {i}");
    }
    ffprobe_codecs(&path);
}

/// When `ffprobe` is on PATH, the recording must decode as H.264 + AAC.
fn ffprobe_codecs(path: &Path) {
    let output = match std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
    {
        Ok(output) => output,
        Err(_) => {
            eprintln!("ffprobe not found; skipping the codec check");
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let codecs: Vec<&str> = stdout.lines().map(str::trim).collect();
    assert!(codecs.contains(&"h264"), "{codecs:?}");
    assert!(codecs.contains(&"aac"), "{codecs:?}");
}

#[tokio::test]
async fn a_wrong_or_missing_secret_is_refused_with_4401() {
    let server = serve(Some("s3cret")).await;
    for auth in [Some("wrong"), None, Some("")] {
        let mut socket = connect(server.addr, "studio").await;
        socket.send(hello(auth)).await.unwrap();
        expect_error(&mut socket, IngestErrorCode::Unauthorized, 4401).await;
    }
    assert_eq!(
        std::fs::read_dir(&server.recordings).unwrap().count(),
        0,
        "no recording was created"
    );
}

#[tokio::test]
async fn without_a_configured_secret_hello_needs_no_auth() {
    let server = serve(None).await;
    let mut socket = connect(server.addr, "open").await;
    socket.send(hello(None)).await.unwrap();
    expect_started(&mut socket, "open").await;
}

#[tokio::test]
async fn anything_but_hello_first_is_a_protocol_error_with_4400() {
    let server = serve(None).await;
    let media = fixture_media();

    let mut socket = connect(server.addr, "r").await;
    socket.send(frame(1, &media[0].0)).await.unwrap();
    expect_error(&mut socket, IngestErrorCode::Protocol, 4400).await;

    let mut socket = connect(server.addr, "r").await;
    let end = IngestClientMessage::EndOfStream { last_seq: 0 };
    socket
        .send(Message::Text(serde_json::to_string(&end).unwrap().into()))
        .await
        .unwrap();
    expect_error(&mut socket, IngestErrorCode::Protocol, 4400).await;

    let mut socket = connect(server.addr, "r").await;
    socket
        .send(Message::Text(
            "{\"type\":\"hello\",\"auth\":\"SECRET-IN-BAD-JSON\"".into(),
        ))
        .await
        .unwrap();
    match next_message(&mut socket).await {
        Ok(IngestServerMessage::Error { code, message }) => {
            assert_eq!(code, IngestErrorCode::BadMessage);
            assert!(
                !message.contains("SECRET"),
                "client bytes echoed: {message}"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(next_message(&mut socket).await, Err(Some(4400)));

    // No hello at all within the (shortened) timeout.
    let mut socket = connect(server.addr, "r").await;
    expect_error(&mut socket, IngestErrorCode::Protocol, 4400).await;
}

#[tokio::test]
async fn media_before_the_video_sequence_header_is_a_protocol_error() {
    let server = serve(None).await;
    let media = fixture_media();
    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    expect_started(&mut socket, "r").await;
    // The fixture's third media tag is the first video frame.
    assert_eq!(media[2].1.kind, TagKind::Video);
    socket.send(frame(1, &media[2].0)).await.unwrap();
    expect_error(&mut socket, IngestErrorCode::Protocol, 4400).await;
}

#[tokio::test]
async fn a_malformed_frame_is_a_bad_message_error() {
    let server = serve(None).await;
    let media = fixture_media();
    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    expect_started(&mut socket, "r").await;
    socket.send(frame(1, &media[0].0)).await.unwrap();
    let mut truncated = media[1].0.clone();
    truncated.truncate(truncated.len() - 3);
    socket.send(frame(2, &truncated)).await.unwrap();
    expect_error(&mut socket, IngestErrorCode::BadMessage, 4400).await;

    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    expect_started(&mut socket, "r").await;
    socket
        .send(Message::Binary(vec![0, 0, 0, 1].into()))
        .await
        .unwrap();
    expect_error(&mut socket, IngestErrorCode::BadMessage, 4400).await;
}

#[tokio::test]
async fn a_second_hello_or_a_backwards_sequence_number_ends_the_session() {
    let server = serve(None).await;
    let media = fixture_media();
    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    expect_started(&mut socket, "r").await;
    socket.send(hello(None)).await.unwrap();
    expect_error(&mut socket, IngestErrorCode::Protocol, 4400).await;

    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    expect_started(&mut socket, "r").await;
    socket.send(frame(5, &media[0].0)).await.unwrap();
    socket.send(frame(5, &media[1].0)).await.unwrap();
    expect_error(&mut socket, IngestErrorCode::Protocol, 4400).await;
}

#[tokio::test]
async fn shutdown_closes_with_1001_and_finalizes_the_recording() {
    let server = serve(None).await;
    let media = fixture_media();
    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    let recording = expect_started(&mut socket, "r").await;
    let sent = 40;
    for (i, (raw, _)) in media.iter().take(sent).enumerate() {
        socket.send(frame(i as u32 + 1, raw)).await.unwrap();
    }
    // Wait until the server has consumed everything, then pull the plug.
    loop {
        match next_message(&mut socket).await {
            Ok(IngestServerMessage::Ack { seq, .. }) if seq == sent as u32 => break,
            Ok(_) => {}
            Err(code) => panic!("closed early with {code:?}"),
        }
    }
    server.shutdown.cancel();
    let code = loop {
        match next_message(&mut socket).await {
            Ok(_) => {}
            Err(code) => break code,
        }
    };
    assert_eq!(code, Some(1001));
    assert!(
        server.shutdown.wait(Duration::from_secs(5)).await,
        "handlers drained"
    );

    let (_, _, tags) = parse_recording(&server.recordings.join(&recording));
    assert_eq!(tags.len(), sent);
    for (recorded, (_, sent)) in tags.iter().zip(&media) {
        assert_eq!(recorded, sent);
    }
}

#[tokio::test]
async fn a_peer_that_vanishes_still_gets_its_recording_finalized() {
    let server = serve(None).await;
    let media = fixture_media();
    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    let recording = expect_started(&mut socket, "r").await;
    for (i, (raw, _)) in media.iter().take(10).enumerate() {
        socket.send(frame(i as u32 + 1, raw)).await.unwrap();
    }
    loop {
        match next_message(&mut socket).await {
            Ok(IngestServerMessage::Ack { seq: 10, .. }) => break,
            Ok(_) => {}
            Err(code) => panic!("closed early with {code:?}"),
        }
    }
    socket.close(None).await.unwrap();
    drop(socket);
    // The handler finalizes on its own; poll the file until it holds everything.
    let path = server.recordings.join(&recording);
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(&path) {
            let mut demuxer = Demuxer::new();
            demuxer.feed(&bytes).unwrap();
            if let Ok(tags) = demuxer.drain_tags()
                && tags.len() == 11
                && demuxer.pending() == 0
            {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("recording was not finalized");
}

#[tokio::test]
async fn healthz_counts_open_ingest_sessions() {
    let server = serve(None).await;
    let count = || async {
        let body = http_get(server.addr, "/healthz").await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        json["ingest_sessions"].as_u64().unwrap()
    };
    assert_eq!(count().await, 0);
    let mut socket = connect(server.addr, "r").await;
    socket.send(hello(None)).await.unwrap();
    expect_started(&mut socket, "r").await;
    assert_eq!(count().await, 1);
    socket.close(None).await.unwrap();
    let _ = next_message(&mut socket).await;
    for _ in 0..50 {
        if count().await == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("session count did not drop");
}

/// A minimal HTTP GET over a raw socket (no HTTP client dependency in the tests).
async fn http_get(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8(response).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(
        head.starts_with("HTTP/1.0 200") || head.starts_with("HTTP/1.1 200"),
        "{head}"
    );
    body.to_owned()
}
