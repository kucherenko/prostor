//! The signaling protocol over a real listener: hello and auth, destinations declared
//! with `start_stream`, the `Origin` policy, and the whole path from a declaration
//! through an ingest session to an in-process RTMP server.

mod support;

use std::{net::SocketAddr, time::Duration};

use futures_util::{SinkExt, StreamExt};
use prostor_model::{
    DestinationSpec, EgressPhase, IngestClientMessage, IngestServerMessage, PeerRole,
    RoomClientMessage, RoomErrorCode, RoomServerMessage, StreamInfo,
};
use prostor_protocol::{flv::TagKind, ingest::encode_frame, secret::Secret};
use prostor_server::{
    app::{self, Options},
    config::Config,
    egress::ConnectPolicy,
    shutdown::Shutdown,
    ws::Timeouts,
};
use support::{Options as ServerOptions, RtmpTestServer, fixture_media};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Server {
    addr: SocketAddr,
    _dir: tempfile::TempDir,
}

async fn serve(secret: Option<&str>, allowed_origins: Vec<String>) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        web_dir: dir.path().to_path_buf(),
        recordings_dir: dir.path().join("recordings"),
        secret: secret.map(Secret::new),
        allowed_origins,
        allow_private_destinations: true,
    };
    std::fs::create_dir_all(&config.recordings_dir).unwrap();
    let listener = TcpListener::bind(config.listen).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let options = Options {
        timeouts: Timeouts {
            hello: Duration::from_millis(500),
            ..Timeouts::default()
        },
        connect: ConnectPolicy {
            allow_private: true,
            connect_timeout: Duration::from_secs(2),
            publish_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(2),
            reconnect_min: Duration::from_millis(50),
            reconnect_max: Duration::from_millis(200),
        },
    };
    let router = app::router_with_options(&config, Shutdown::new(), options);
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server { addr, _dir: dir }
}

async fn connect(addr: SocketAddr, path: &str) -> Socket {
    connect_with_origin(addr, path, None).await.unwrap()
}

async fn connect_with_origin(
    addr: SocketAddr,
    path: &str,
    origin: Option<&str>,
) -> Result<Socket, tungstenite::Error> {
    let mut request = format!("ws://{addr}{path}").into_client_request().unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", origin.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}

async fn send_room(socket: &mut Socket, message: &RoomClientMessage) {
    socket
        .send(Message::Text(
            serde_json::to_string(message).unwrap().into(),
        ))
        .await
        .unwrap();
}

async fn next_room(socket: &mut Socket) -> Result<RoomServerMessage, Option<u16>> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("server message")
        {
            Some(Ok(Message::Text(text))) => return Ok(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Close(frame))) => return Err(frame.map(|f| u16::from(f.code))),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return Err(None),
        }
    }
}

fn hello(auth: Option<&str>, role: PeerRole) -> RoomClientMessage {
    RoomClientMessage::Hello {
        auth: auth.map(str::to_owned),
        role,
    }
}

fn spec(id: &str, url: &str, key: &str) -> DestinationSpec {
    DestinationSpec {
        id: id.into(),
        name: format!("Destination {id}"),
        url: url.into(),
        key: key.into(),
    }
}

async fn expect_room_error(socket: &mut Socket, code: RoomErrorCode, close: u16) {
    match next_room(socket).await {
        Ok(RoomServerMessage::Error { code: got, message }) => {
            assert_eq!(got, code, "{message}");
        }
        other => panic!("expected {code:?}, got {other:?}"),
    }
    assert_eq!(next_room(socket).await, Err(Some(close)));
}

#[tokio::test]
async fn producer_hello_needs_the_secret_and_guests_are_welcomed_only() {
    let server = serve(Some("s3cret"), vec![]).await;

    let mut socket = connect(server.addr, "/ws/room/r").await;
    send_room(&mut socket, &hello(Some("wrong"), PeerRole::Producer)).await;
    expect_room_error(&mut socket, RoomErrorCode::Unauthorized, 4401).await;

    let mut socket = connect(server.addr, "/ws/room/r").await;
    send_room(&mut socket, &RoomClientMessage::StopStream).await;
    expect_room_error(&mut socket, RoomErrorCode::Protocol, 4400).await;

    let mut socket = connect(server.addr, "/ws/room/r").await;
    socket
        .send(Message::Text(
            "{\"type\":\"hello\",\"auth\":\"SECRET-BAD\"".into(),
        ))
        .await
        .unwrap();
    match next_room(&mut socket).await {
        Ok(RoomServerMessage::Error { code, message }) => {
            assert_eq!(code, RoomErrorCode::BadMessage);
            assert!(!message.contains("SECRET"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }

    let mut socket = connect(server.addr, "/ws/room/r").await;
    send_room(&mut socket, &hello(Some("s3cret"), PeerRole::Producer)).await;
    assert_eq!(
        next_room(&mut socket).await,
        Ok(RoomServerMessage::Welcome {
            room: "r".into(),
            role: PeerRole::Producer
        })
    );
    send_room(&mut socket, &hello(Some("s3cret"), PeerRole::Producer)).await;
    expect_room_error(&mut socket, RoomErrorCode::Protocol, 4400).await;

    let mut guest = connect(server.addr, "/ws/room/r").await;
    send_room(&mut guest, &hello(None, PeerRole::Guest)).await;
    assert_eq!(
        next_room(&mut guest).await,
        Ok(RoomServerMessage::Welcome {
            room: "r".into(),
            role: PeerRole::Guest
        })
    );
    send_room(&mut guest, &RoomClientMessage::StopStream).await;
    expect_room_error(&mut guest, RoomErrorCode::Protocol, 4400).await;

    // No hello within the (shortened) timeout.
    let mut late = connect(server.addr, "/ws/room/r").await;
    expect_room_error(&mut late, RoomErrorCode::Protocol, 4400).await;
}

#[tokio::test]
async fn start_stream_validates_destinations_without_echoing_them() {
    let server = serve(None, vec![]).await;
    let mut socket = connect(server.addr, "/ws/room/r").await;
    send_room(&mut socket, &hello(None, PeerRole::Producer)).await;
    next_room(&mut socket).await.unwrap();

    send_room(
        &mut socket,
        &RoomClientMessage::StartStream {
            destinations: vec![
                spec("ok", "rtmp://127.0.0.1:1935/live", "KEY-OK"),
                spec("bad", "http://KEY-IN-URL/live", "KEY-BAD"),
            ],
        },
    )
    .await;
    match next_room(&mut socket).await {
        Ok(RoomServerMessage::Error { code, message }) => {
            assert_eq!(code, RoomErrorCode::InvalidDestination);
            assert!(message.contains("bad"), "{message}");
            assert!(!message.contains("KEY"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }

    // The socket stays open and a valid declaration is accepted.
    send_room(
        &mut socket,
        &RoomClientMessage::StartStream {
            destinations: vec![spec("ok", "rtmp://127.0.0.1:1935/live", "KEY-OK")],
        },
    )
    .await;
    match next_room(&mut socket).await {
        Ok(RoomServerMessage::StreamReady { destinations }) => {
            assert_eq!(destinations.len(), 1);
            assert_eq!(destinations[0].id.as_str(), "ok");
            assert_eq!(destinations[0].name, "Destination ok");
        }
        other => panic!("unexpected {other:?}"),
    }
    send_room(&mut socket, &RoomClientMessage::StopStream).await;
    assert_eq!(
        next_room(&mut socket).await,
        Ok(RoomServerMessage::StreamStopped)
    );
}

#[tokio::test]
async fn origins_are_checked_on_both_upgrades() {
    let same_origin = serve(None, vec![]).await;
    let host = same_origin.addr.to_string();
    for path in ["/ws/room/r", "/ws/ingest/r"] {
        assert!(
            connect_with_origin(same_origin.addr, path, Some(&format!("http://{host}")))
                .await
                .is_ok(),
            "{path}: same origin"
        );
        assert!(
            connect_with_origin(same_origin.addr, path, None)
                .await
                .is_ok(),
            "{path}: no origin (non-browser client)"
        );
        match connect_with_origin(same_origin.addr, path, Some("http://evil.example")).await {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 403),
            other => panic!("{path}: expected 403, got {:?}", other.map(|_| ())),
        }
    }

    let listed = serve(None, vec!["https://studio.example".to_owned()]).await;
    assert!(
        connect_with_origin(listed.addr, "/ws/room/r", Some("https://studio.example"))
            .await
            .is_ok()
    );
    let host = listed.addr.to_string();
    match connect_with_origin(listed.addr, "/ws/room/r", Some(&format!("http://{host}"))).await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 403),
        other => panic!(
            "expected 403 for an unlisted origin, got {:?}",
            other.map(|_| ())
        ),
    }
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

async fn next_ingest(socket: &mut Socket) -> Result<IngestServerMessage, Option<u16>> {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("server message")
        {
            Some(Ok(Message::Text(text))) => return Ok(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Close(frame))) => return Err(frame.map(|f| u16::from(f.code))),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return Err(None),
        }
    }
}

#[tokio::test]
async fn a_declared_destination_receives_the_ingest_session() {
    let rtmp = RtmpTestServer::start(ServerOptions::default()).await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = closed.local_addr().unwrap();
    drop(closed);
    let server = serve(Some("s3cret"), vec![]).await;

    // 1. The producer declares two destinations: one real, one unreachable.
    let mut control = connect(server.addr, "/ws/room/studio").await;
    send_room(&mut control, &hello(Some("s3cret"), PeerRole::Producer)).await;
    next_room(&mut control).await.unwrap();
    send_room(
        &mut control,
        &RoomClientMessage::StartStream {
            destinations: vec![
                spec("good", &format!("rtmp://{}/live", rtmp.addr), "key-good"),
                spec("dead", &format!("rtmp://{dead_addr}/live"), "key-dead"),
            ],
        },
    )
    .await;
    assert!(matches!(
        next_room(&mut control).await,
        Ok(RoomServerMessage::StreamReady { .. })
    ));

    // 2. The ingest session fans out to the recording and both destinations.
    let media = fixture_media();
    let mut ingest = connect(server.addr, "/ws/ingest/studio").await;
    let hello = IngestClientMessage::Hello {
        auth: Some("s3cret".into()),
        stream: stream_info(),
    };
    ingest
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    assert!(matches!(
        next_ingest(&mut ingest).await,
        Ok(IngestServerMessage::Started { .. })
    ));
    for (i, (raw, _)) in media.iter().enumerate() {
        let mut frame = Vec::new();
        encode_frame(i as u32 + 1, raw, &mut frame);
        ingest.send(Message::Binary(frame.into())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let end = IngestClientMessage::EndOfStream {
        last_seq: media.len() as u32,
    };
    ingest
        .send(Message::Text(serde_json::to_string(&end).unwrap().into()))
        .await
        .unwrap();
    let mut final_status = None;
    loop {
        match next_ingest(&mut ingest).await {
            Ok(IngestServerMessage::EgressStatus { egress }) => final_status = Some(egress),
            Ok(IngestServerMessage::Ended { .. }) => break,
            Ok(_) => {}
            Err(code) => panic!("closed early with {code:?}"),
        }
    }
    let status = final_status.expect("final egress_status");
    let by_id = |id: &str| {
        status
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    assert_eq!(by_id("file").state, EgressPhase::Stopped);
    let good = by_id("rtmp-good");
    assert_eq!(good.state, EgressPhase::Stopped, "{good:?}");
    assert_eq!(good.tags, 80, "{good:?}");
    assert_eq!(good.dropped, 0, "{good:?}");
    assert_eq!(good.reconnects, 0);
    let dead = by_id("rtmp-dead");
    assert_eq!(dead.tags, 0, "{dead:?}");
    assert_eq!(dead.dropped, 80, "{dead:?}");
    assert!(
        dead.detail.as_deref().unwrap_or("").contains("connect"),
        "{dead:?}"
    );
    for egress in &status {
        let text = format!("{egress:?}");
        assert!(
            !text.contains("key-"),
            "a key leaked into the status: {text}"
        );
    }

    // 3. The RTMP server got the preamble and every media tag, in order.
    assert!(
        rtmp.wait_for(Duration::from_secs(5), |r| r.finished == 1)
            .await
    );
    let recorded = rtmp.recorded();
    assert_eq!(
        recorded.publishes,
        vec![("live".to_owned(), "key-good".to_owned())]
    );
    assert_eq!(recorded.metadata_widths, vec![1280]);
    assert_eq!(recorded.tags.len(), media.len());
    assert_eq!(recorded.tags[0].kind, TagKind::Video);
    assert_eq!(recorded.tags[0].data, media[0].1.body);
    for (received, (_, sent)) in recorded.tags[2..].iter().zip(&media[2..]) {
        assert_eq!(received.data, sent.body);
        assert_eq!(received.timestamp, sent.timestamp_ms);
    }

    // 4. stop_stream forgets the destinations: the next session records only.
    send_room(&mut control, &RoomClientMessage::StopStream).await;
    assert_eq!(
        next_room(&mut control).await,
        Ok(RoomServerMessage::StreamStopped)
    );
    let mut again = connect(server.addr, "/ws/ingest/studio").await;
    let hello = IngestClientMessage::Hello {
        auth: Some("s3cret".into()),
        stream: stream_info(),
    };
    again
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    assert!(matches!(
        next_ingest(&mut again).await,
        Ok(IngestServerMessage::Started { .. })
    ));
    match next_ingest(&mut again).await {
        Ok(IngestServerMessage::EgressStatus { egress }) => {
            assert_eq!(egress.len(), 1, "{egress:?}");
            assert_eq!(egress[0].id, "file");
        }
        other => panic!("unexpected {other:?}"),
    }
}
