//! What the request span records. Lives in its own test binary on purpose: `tracing`
//! caches per-callsite interest process-wide, so a test that installs a subscriber must
//! not share a process with tests that hit the same callsites first.

use std::{
    io,
    sync::{Arc, Mutex},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use prostor_server::{app, config::Config};
use tower::ServiceExt;

/// Captures everything the tracing subscriber writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn request_logs_carry_the_path_but_never_the_query_string() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        web_dir: dir.path().to_path_buf(),
        recordings_dir: dir.path().join("recordings"),
        secret: None,
    };
    let app = app::router(&config);
    let response = app
        .oneshot(
            Request::get("/healthz?token=SENSITIVE-QUERY-TOKEN&x=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("path=/healthz"), "no request span in: {logs}");
    assert!(logs.contains("method=GET"), "no method in: {logs}");
    assert!(
        !logs.contains("SENSITIVE-QUERY-TOKEN"),
        "query logged: {logs}"
    );
    assert!(!logs.contains("token="), "query logged: {logs}");
}
