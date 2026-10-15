//! WebSocket behaviour over a real loopback listener: upgrades, size limits, bad room
//! ids, idle timeout, the socket cap, shutdown, and that the server survives a rejected
//! message. The ingest protocol itself is covered in `tests/ingest.rs`.

use std::{net::SocketAddr, time::Duration};

use futures_util::{SinkExt, StreamExt};
use prostor_server::{
    app::{self, MAX_SOCKETS},
    config::Config,
    shutdown::Shutdown,
    ws::{INGEST_MAX_MESSAGE, Timeouts},
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{
    self, Message,
    protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn serve_with(timeouts: Timeouts) -> (SocketAddr, tempfile::TempDir, Shutdown) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        web_dir: dir.path().to_path_buf(),
        recordings_dir: dir.path().join("recordings"),
        secret: None,
        allow_private_destinations: false,
    };
    std::fs::create_dir_all(&config.recordings_dir).unwrap();
    let shutdown = Shutdown::new();
    let listener = TcpListener::bind(config.listen).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app::router_with_timeouts(&config, shutdown.clone(), timeouts);
    let serve_shutdown = shutdown.clone();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(serve_shutdown.cancelled())
            .await
            .unwrap();
    });
    (addr, dir, shutdown)
}

async fn serve() -> (SocketAddr, tempfile::TempDir, Shutdown) {
    serve_with(Timeouts::default()).await
}

fn client_config() -> WebSocketConfig {
    // The client must be allowed to send more than the server accepts.
    WebSocketConfig::default()
        .max_message_size(Some(64 * 1024 * 1024))
        .max_frame_size(Some(64 * 1024 * 1024))
}

async fn connect(addr: SocketAddr, path: &str) -> Result<Socket, tungstenite::Error> {
    let url = format!("ws://{addr}{path}");
    tokio_tungstenite::connect_async_with_config(url, Some(client_config()), false)
        .await
        .map(|(socket, _)| socket)
}

/// Reads until a close frame (its code) or the end of the stream (`None`).
async fn close_code(socket: &mut Socket) -> Option<u16> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Close(frame))) => return frame.map(|f| u16::from(f.code)),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

/// Starts a client-side close with code 1000; the server echoes the code back.
async fn close_normally(socket: &mut Socket) {
    socket
        .close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "bye".into(),
        }))
        .await
        .unwrap();
}

#[tokio::test]
async fn room_echoes_text_and_completes_the_close_handshake() {
    let (addr, _dir, _shutdown) = serve().await;
    let mut room = connect(addr, "/ws/room/room-1").await.unwrap();
    room.send(Message::Text("hello".into())).await.unwrap();
    let echoed = room.next().await.unwrap().unwrap();
    assert_eq!(echoed, Message::Text("hello".into()));
    close_normally(&mut room).await;
    assert_eq!(
        close_code(&mut room).await,
        Some(1000),
        "the close reply is flushed before the server drops the socket"
    );
}

#[tokio::test]
async fn bad_room_ids_get_a_400_instead_of_an_upgrade() {
    let (addr, _dir, _shutdown) = serve().await;
    for bad in ["/ws/ingest/..%2F..%2Fetc", "/ws/room/a%20b", "/ws/room/"] {
        match connect(addr, bad).await {
            Err(tungstenite::Error::Http(response)) => {
                assert!(
                    response.status() == 400 || response.status() == 404,
                    "{bad}: {}",
                    response.status()
                );
            }
            Err(other) => panic!("{bad}: unexpected error {other}"),
            Ok(_) => panic!("{bad}: upgraded"),
        }
    }
}

#[tokio::test]
async fn oversized_messages_close_the_socket_and_the_server_survives() {
    let (addr, _dir, _shutdown) = serve().await;

    let mut socket = connect(addr, "/ws/ingest/room-2").await.unwrap();
    // The server rejects the frame on its header, so the send itself may already fail
    // with a reset; what matters is that the connection ends and the server survives.
    let sent = socket
        .send(Message::Binary(vec![0u8; INGEST_MAX_MESSAGE + 1].into()))
        .await;
    // The server either answers with a close frame (1009) or drops the connection; this
    // loop must terminate either way (a hanging test means the server kept reading).
    let code = if sent.is_err() {
        None
    } else {
        close_code(&mut socket).await
    };
    if let Some(code) = code {
        assert_eq!(code, 1009);
    }
    drop(socket);

    // A well-behaved client still gets in afterwards.
    let mut again = connect(addr, "/ws/room/room-2").await.unwrap();
    again.send(Message::Text("x".into())).await.unwrap();
    assert_eq!(
        again.next().await.unwrap().unwrap(),
        Message::Text("x".into())
    );
    again.close(None).await.unwrap();
}

#[tokio::test]
async fn a_large_message_under_the_limit_is_consumed() {
    let (addr, _dir, _shutdown) = serve().await;
    let mut socket = connect(addr, "/ws/ingest/room-3").await.unwrap();
    socket
        .send(Message::Binary(vec![7u8; INGEST_MAX_MESSAGE - 16].into()))
        .await
        .unwrap();
    // Consumed and then refused by the protocol (no hello first), with a close code
    // rather than a dropped connection.
    assert_eq!(close_code(&mut socket).await, Some(4400));
}

#[tokio::test]
async fn peers_that_answer_pings_stay_open_and_dead_ones_are_closed_at_the_idle_timeout() {
    let timeouts = Timeouts {
        idle: Duration::from_millis(300),
        ping: Duration::from_millis(100),
        hello: Duration::from_secs(5),
    };
    let (addr, _dir, _shutdown) = serve_with(timeouts).await;

    // A healthy idle peer reads, so tungstenite answers every ping; it is never closed,
    // even though it sends nothing of its own for longer than the idle timeout.
    let mut healthy = connect(addr, "/ws/room/healthy").await.unwrap();
    let mut pings = 0;
    let watched = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match healthy.next().await {
                Some(Ok(Message::Ping(_))) => pings += 1,
                Some(Ok(Message::Close(frame))) => panic!("closed a healthy peer: {frame:?}"),
                Some(Ok(other)) => panic!("unexpected {other:?}"),
                Some(Err(e)) => panic!("socket error {e}"),
                None => panic!("dropped a healthy peer"),
            }
        }
    })
    .await;
    assert!(watched.is_err(), "the watch period ends by timeout");
    assert!(pings >= 5, "{pings} pings in one second");

    // A dead peer never reads, so it never answers a ping: closed with 1008 at the idle
    // timeout, not kept alive by the server's own pings.
    let mut dead = connect(addr, "/ws/room/dead").await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut pings = 0;
    let code = loop {
        match tokio::time::timeout(Duration::from_secs(3), dead.next())
            .await
            .expect("the server must close an idle socket")
        {
            Some(Ok(Message::Ping(_))) => pings += 1,
            Some(Ok(Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
            Some(Ok(other)) => panic!("unexpected {other:?}"),
            Some(Err(_)) | None => break None,
        }
    };
    assert_eq!(code, Some(1008), "idle close code");
    assert!(
        (1..=4).contains(&pings),
        "{pings} pings: the server pinged until the idle timeout, then closed"
    );
}

#[tokio::test]
async fn the_socket_cap_answers_503_and_frees_up_again() {
    let (addr, _dir, _shutdown) = serve().await;
    let mut sockets = Vec::new();
    for i in 0..MAX_SOCKETS {
        sockets.push(connect(addr, &format!("/ws/room/cap-{i}")).await.unwrap());
    }
    match connect(addr, "/ws/room/one-too-many").await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 503),
        other => panic!("expected 503, got {:?}", other.map(|_| ())),
    }
    let mut first = sockets.remove(0);
    close_normally(&mut first).await;
    assert_eq!(close_code(&mut first).await, Some(1000));
    // The permit is released when the handler ends; give it a moment.
    let mut again = Err(tungstenite::Error::ConnectionClosed);
    for _ in 0..50 {
        again = connect(addr, "/ws/room/after-release").await;
        if again.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    again.expect("a slot frees up after a socket closes");
}

#[tokio::test]
async fn shutdown_closes_open_sockets_with_1001_and_refuses_new_ones() {
    let (addr, _dir, shutdown) = serve().await;
    let mut room = connect(addr, "/ws/room/bye").await.unwrap();
    room.send(Message::Text("ping".into())).await.unwrap();
    assert_eq!(
        room.next().await.unwrap().unwrap(),
        Message::Text("ping".into())
    );

    shutdown.cancel();
    assert_eq!(close_code(&mut room).await, Some(1001));
    assert!(
        shutdown.wait(Duration::from_secs(2)).await,
        "handlers finished"
    );
    assert!(connect(addr, "/ws/room/late").await.is_err());
}
