//! WebSocket behaviour over a real loopback listener: upgrades, size limits, bad room
//! ids, and that the server survives a rejected message.

use futures_util::{SinkExt, StreamExt};
use prostor_server::{app, config::Config, ws::INGEST_MAX_MESSAGE};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{self, Message, protocol::WebSocketConfig};

async fn serve() -> (SocketAddr, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        web_dir: dir.path().to_path_buf(),
        recordings_dir: dir.path().join("recordings"),
        secret: None,
    };
    let listener = TcpListener::bind(config.listen).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app::router(&config)).await.unwrap();
    });
    (addr, dir)
}

fn client_config() -> WebSocketConfig {
    // The client must be allowed to send more than the server accepts.
    WebSocketConfig::default()
        .max_message_size(Some(64 * 1024 * 1024))
        .max_frame_size(Some(64 * 1024 * 1024))
}

async fn connect(
    addr: SocketAddr,
    path: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Error,
> {
    let url = format!("ws://{addr}{path}");
    tokio_tungstenite::connect_async_with_config(url, Some(client_config()), false)
        .await
        .map(|(socket, _)| socket)
}

#[tokio::test]
async fn ingest_accepts_binary_frames_and_room_echoes_text() {
    let (addr, _dir) = serve().await;

    let mut ingest = connect(addr, "/ws/ingest/room-1").await.unwrap();
    ingest
        .send(Message::Binary(vec![0u8; 1024].into()))
        .await
        .unwrap();
    ingest.close(None).await.unwrap();

    let mut room = connect(addr, "/ws/room/room-1").await.unwrap();
    room.send(Message::Text("hello".into())).await.unwrap();
    let echoed = room.next().await.unwrap().unwrap();
    assert_eq!(echoed, Message::Text("hello".into()));
    room.close(None).await.unwrap();
}

#[tokio::test]
async fn bad_room_ids_get_a_400_instead_of_an_upgrade() {
    let (addr, _dir) = serve().await;
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
    let (addr, _dir) = serve().await;

    let mut socket = connect(addr, "/ws/ingest/room-2").await.unwrap();
    // The server rejects the frame on its header, so the send itself may already fail
    // with a reset; what matters is that the connection ends and the server survives.
    let sent = socket
        .send(Message::Binary(vec![0u8; INGEST_MAX_MESSAGE + 1].into()))
        .await;
    // The server either answers with a close frame (1009) or drops the connection; this
    // loop must terminate either way (a hanging test means the server kept reading).
    let close_code = loop {
        if sent.is_err() {
            break None;
        }
        match socket.next().await {
            Some(Ok(Message::Close(frame))) => break frame.map(|f| u16::from(f.code)),
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
            Some(Ok(other)) => panic!("unexpected {other:?}"),
            Some(Err(_)) | None => break None,
        }
    };
    if let Some(code) = close_code {
        assert_eq!(code, 1009);
    }
    drop(socket);

    // A well-behaved client still gets in afterwards.
    let mut again = connect(addr, "/ws/ingest/room-2").await.unwrap();
    again
        .send(Message::Binary(vec![1u8; 16].into()))
        .await
        .unwrap();
    again.close(None).await.unwrap();
}

#[tokio::test]
async fn an_oversized_message_under_the_limit_is_fine() {
    let (addr, _dir) = serve().await;
    let mut socket = connect(addr, "/ws/ingest/room-3").await.unwrap();
    socket
        .send(Message::Binary(vec![7u8; INGEST_MAX_MESSAGE - 16].into()))
        .await
        .unwrap();
    socket.close(None).await.unwrap();
    // The close handshake completes, which means the server consumed the message.
    while let Some(next) = socket.next().await {
        if next.is_err() {
            break;
        }
    }
}
