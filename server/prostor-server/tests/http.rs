//! Boots the router against a temporary web directory and exercises the HTTP
//! surface without opening a socket.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use prostor_server::{app, config::Config, shutdown::Shutdown};
use tower::ServiceExt;

fn config_with_web_dir(dir: &std::path::Path) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        web_dir: dir.to_path_buf(),
        recordings_dir: dir.join("recordings"),
        secret: None,
        allow_private_destinations: false,
    }
}

fn router(config: &Config) -> axum::Router {
    app::router(config, Shutdown::new())
}

/// A web directory with an app shell, an asset tree and a dotfile.
fn web_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<html>prostor shell</html>").unwrap();
    std::fs::write(dir.path().join("robots.txt"), "User-agent: *\n").unwrap();
    std::fs::create_dir_all(dir.path().join("_app/immutable")).unwrap();
    std::fs::write(dir.path().join("_app/immutable/start.js"), "console.log(1)").unwrap();
    std::fs::write(dir.path().join(".hidden-test"), "do not serve").unwrap();
    std::fs::write(dir.path().join("_app/.secret"), "do not serve").unwrap();
    dir
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String, String) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

async fn healthz(app: &axum::Router) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn healthz_reports_versions_and_whether_recordings_can_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_web_dir(dir.path());
    let app = router(&config);

    let json = healthz(&app).await;
    assert_eq!(
        json["status"], "degraded",
        "recordings dir does not exist yet"
    );
    assert_eq!(json["recordings_dir_writable"], false);
    assert_eq!(json["ingest_sessions"], 0);
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["protocol"], prostor_protocol::crate_version());

    std::fs::create_dir_all(&config.recordings_dir).unwrap();
    let json = healthz(&app).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["recordings_dir_writable"], true);
    assert_eq!(
        std::fs::read_dir(&config.recordings_dir).unwrap().count(),
        0,
        "the probe file is removed"
    );
}

#[tokio::test]
async fn serves_static_files_and_falls_back_to_index_for_app_routes() {
    let dir = web_dir();
    let app = router(&config_with_web_dir(dir.path()));

    let (status, _, body) = get(&app, "/robots.txt").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "User-agent: *\n"));

    let (status, content_type, body) = get(&app, "/_app/immutable/start.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.contains("javascript"), "{content_type}");
    assert_eq!(body, "console.log(1)");

    for route in ["/", "/studio", "/join/some-room", "/join/some-room/"] {
        let (status, content_type, body) = get(&app, route).await;
        assert_eq!(status, StatusCode::OK, "{route}");
        assert!(
            content_type.starts_with("text/html"),
            "{route}: {content_type}"
        );
        assert_eq!(body, "<html>prostor shell</html>", "{route}");
    }
}

#[tokio::test]
async fn missing_assets_are_404_not_the_app_shell() {
    let dir = web_dir();
    let app = router(&config_with_web_dir(dir.path()));
    for asset in ["/_app/missing.js", "/favicon.ico", "/studio/x.png"] {
        let (status, _, body) = get(&app, asset).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{asset}");
        assert!(!body.contains("prostor shell"), "{asset} served the shell");
    }
}

#[tokio::test]
async fn dotfiles_and_traversal_never_leave_the_web_dir() {
    let dir = web_dir();
    let app = router(&config_with_web_dir(dir.path()));
    let outside = dir.path().join("../outside-marker.txt");
    std::fs::write(&outside, "outside").unwrap();
    let payloads = [
        "/.hidden-test",
        "/_app/.secret",
        "/%2ehidden-test",
        "/_app/%2e%2e/.hidden-test",
        "/../outside-marker.txt",
        "/%2e%2e/outside-marker.txt",
        "/_app/../../outside-marker.txt",
        "/_app/%2e%2e%2f%2e%2e%2foutside-marker.txt",
        "/..%5c..%5coutside-marker.txt",
        "/%c0%ae%c0%ae/outside-marker.txt",
        "/%00",
        "//outside-marker.txt",
    ];
    for payload in payloads {
        let (status, _, body) = get(&app, payload).await;
        assert_ne!(body, "do not serve", "{payload} served a dotfile");
        assert_ne!(body, "outside", "{payload} escaped the web dir");
        assert!(
            status == StatusCode::NOT_FOUND || body == "<html>prostor shell</html>",
            "{payload}: {status} {body}"
        );
    }
    std::fs::remove_file(outside).ok();
}

#[tokio::test]
async fn bad_room_ids_are_rejected_before_the_upgrade() {
    let dir = web_dir();
    let app = router(&config_with_web_dir(dir.path()));
    for bad in [
        "/ws/ingest/..%2F..%2Fetc%2Fcron.d%2Fx",
        "/ws/room/a%20b",
        "/ws/ingest/nul%00",
        &format!("/ws/room/{}", "x".repeat(65)),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(bad)
                    .header(header::CONNECTION, "upgrade")
                    .header(header::UPGRADE, "websocket")
                    .header(header::SEC_WEBSOCKET_VERSION, "13")
                    .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
}
