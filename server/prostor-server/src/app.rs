//! The axum router and the application state shared by handlers.

use std::{
    path::{Path as FsPath, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::ServiceExt;
use tower_http::{services::ServeDir, trace::TraceLayer};

use crate::{
    config::{Config, Secret},
    egress::ConnectPolicy,
    ingest,
    rooms::Rooms,
    shutdown::Shutdown,
    signaling,
    ws::Timeouts,
};

/// Most WebSockets open at once (producer, guests, and a margin).
pub const MAX_SOCKETS: usize = 64;

/// State shared by all handlers.
#[derive(Clone)]
pub struct AppState {
    sockets: Arc<Semaphore>,
    web: ServeDir,
    secret: Option<Secret>,
    recordings_dir: PathBuf,
    shutdown: Shutdown,
    timeouts: Timeouts,
    connect: ConnectPolicy,
    allowed_origins: Arc<Vec<String>>,
    rooms: Rooms,
    ingest_sessions: Arc<AtomicUsize>,
    probe_counter: Arc<AtomicU64>,
}

/// Knobs the tests turn; production uses the defaults derived from the config.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub timeouts: Timeouts,
    pub connect: ConnectPolicy,
}

impl Options {
    /// Production options for `config`.
    pub fn from_config(config: &Config) -> Self {
        Options {
            timeouts: Timeouts::default(),
            connect: ConnectPolicy {
                allow_private: config.allow_private_destinations,
                ..ConnectPolicy::default()
            },
        }
    }
}

impl AppState {
    fn new(config: &Config, shutdown: Shutdown, options: Options) -> Self {
        AppState {
            sockets: Arc::new(Semaphore::new(MAX_SOCKETS)),
            web: ServeDir::new(&config.web_dir),
            secret: config.secret.clone(),
            recordings_dir: config.recordings_dir.clone(),
            shutdown,
            timeouts: options.timeouts,
            connect: options.connect,
            allowed_origins: Arc::new(config.allowed_origins.clone()),
            rooms: Rooms::default(),
            ingest_sessions: Arc::new(AtomicUsize::new(0)),
            probe_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A permit for one more WebSocket, or `None` when the cap is reached.
    pub fn socket_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.sockets.clone().try_acquire_owned().ok()
    }

    /// The producer secret, when one is configured.
    pub fn secret(&self) -> Option<&Secret> {
        self.secret.as_ref()
    }

    /// Where recordings are written.
    pub fn recordings_dir(&self) -> &FsPath {
        &self.recordings_dir
    }

    /// The shutdown every long-lived handler watches.
    pub fn shutdown(&self) -> &Shutdown {
        &self.shutdown
    }

    /// Socket timing.
    pub fn timeouts(&self) -> Timeouts {
        self.timeouts
    }

    /// How RTMP egresses connect and retry.
    pub fn connect_policy(&self) -> ConnectPolicy {
        self.connect
    }

    /// Origins allowed on WebSocket upgrades (empty: same-origin only).
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }

    /// Per-room state shared by the signaling and ingest handlers.
    pub fn rooms(&self) -> &Rooms {
        &self.rooms
    }

    /// Ingest sessions open right now.
    pub fn ingest_sessions(&self) -> usize {
        self.ingest_sessions.load(Ordering::Relaxed)
    }

    /// Counts an ingest session for as long as the guard lives.
    pub(crate) fn ingest_session(&self) -> SessionGuard {
        self.ingest_sessions.fetch_add(1, Ordering::Relaxed);
        SessionGuard(self.ingest_sessions.clone())
    }

    /// Whether a file can be created in the recordings directory right now.
    async fn recordings_dir_writable(&self) -> bool {
        let n = self.probe_counter.fetch_add(1, Ordering::Relaxed);
        let probe = self
            .recordings_dir
            .join(format!(".healthz-{}-{n}", std::process::id()));
        let created = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .await
            .is_ok();
        if created {
            let _ = tokio::fs::remove_file(&probe).await;
        }
        created
    }
}

/// Decrements the ingest session count on drop.
pub(crate) struct SessionGuard(Arc<AtomicUsize>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Builds the router: health check, signaling and ingest WebSockets, and the
/// web app as static files with an SPA fallback to `index.html` for app routes.
/// Handlers watch `shutdown` and are tracked by it.
pub fn router(config: &Config, shutdown: Shutdown) -> Router {
    router_with_options(config, shutdown, Options::from_config(config))
}

/// [`router`] with custom socket timing (tests).
pub fn router_with_timeouts(config: &Config, shutdown: Shutdown, timeouts: Timeouts) -> Router {
    let options = Options {
        timeouts,
        ..Options::from_config(config)
    };
    router_with_options(config, shutdown, options)
}

/// [`router`] with every knob set (tests).
pub fn router_with_options(config: &Config, shutdown: Shutdown, options: Options) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws/room/{id}", get(signaling::room))
        .route("/ws/ingest/{room}", get(ingest::ingest))
        .fallback(static_files)
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
        .with_state(AppState::new(config, shutdown, options))
}

/// The request span: method and path only. The query string may carry tokens, so it is
/// never recorded.
fn request_span(request: &Request) -> tracing::Span {
    tracing::info_span!(
        "request",
        method = %request.method(),
        path = %request.uri().path(),
    )
}

async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    let writable = state.recordings_dir_writable().await;
    Json(serde_json::json!({
        "status": if writable { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": prostor_protocol::crate_version(),
        "recordings_dir_writable": writable,
        "ingest_sessions": state.ingest_sessions(),
        "armed_rooms": state.rooms().armed_rooms(),
    }))
}

/// Static files with an SPA fallback. Dotfiles and traversal are refused before the
/// filesystem is consulted; a missing path falls back to `index.html` only when it
/// looks like an app route (no extension in its last segment), so a stale asset URL is
/// a 404 rather than an HTML page served as JavaScript.
async fn static_files(State(state): State<AppState>, request: Request) -> Response {
    let Some(segments) = safe_segments(request.uri()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let is_asset = segments.last().is_some_and(|last| last.contains('.'));
    let method = request.method().clone();
    match state.web.clone().oneshot(request).await {
        Ok(response) if response.status() != StatusCode::NOT_FOUND => response.into_response(),
        _ if is_asset => StatusCode::NOT_FOUND.into_response(),
        _ => index(&state, method).await,
    }
}

async fn index(state: &AppState, method: Method) -> Response {
    let method = if method == Method::HEAD {
        Method::HEAD
    } else {
        Method::GET
    };
    let request = Request::builder()
        .method(method)
        .uri("/index.html")
        .body(Body::empty())
        .expect("static request");
    match state.web.clone().oneshot(request).await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
    }
}

/// Percent-decodes the path and splits it into segments, refusing anything that is not
/// UTF-8, contains NUL, or has a segment starting with a dot (dotfiles, `.` and `..`).
fn safe_segments(uri: &Uri) -> Option<Vec<String>> {
    let decoded = percent_encoding::percent_decode_str(uri.path())
        .decode_utf8()
        .ok()?;
    if decoded.contains('\0') {
        return None;
    }
    let segments: Vec<String> = decoded
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if segments.iter().any(|s| s.starts_with('.')) {
        return None;
    }
    Some(segments)
}

/// Whether `path` is a file inside `web_dir` (used by tests and future handlers).
pub fn is_within(web_dir: &FsPath, path: &FsPath) -> bool {
    path.starts_with(web_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(path: &str) -> Option<Vec<String>> {
        safe_segments(&path.parse::<Uri>().unwrap())
    }

    #[test]
    fn splits_plain_paths() {
        assert_eq!(segments("/"), Some(vec![]));
        assert_eq!(
            segments("/studio/room-1"),
            Some(vec!["studio".to_owned(), "room-1".to_owned()])
        );
        assert_eq!(
            segments("/_app/immutable/x.js"),
            Some(vec![
                "_app".to_owned(),
                "immutable".to_owned(),
                "x.js".to_owned()
            ])
        );
    }

    #[test]
    fn refuses_dotfiles_and_traversal_in_any_encoding() {
        for bad in [
            "/.env",
            "/.hidden-test",
            "/_app/.hidden",
            "/..",
            "/../etc/passwd",
            "/_app/../../etc/passwd",
            "/%2e%2e/etc/passwd",
            "/%2eenv",
            "/_app/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
            "/..%5c..%5cwindows",
            "/%00",
            "/x%00.js",
            "/%c0%ae%c0%ae/etc",
        ] {
            assert_eq!(segments(bad), None, "{bad}");
        }
    }

    #[test]
    fn within_check() {
        assert!(is_within(
            FsPath::new("/app/web"),
            FsPath::new("/app/web/index.html")
        ));
        assert!(!is_within(
            FsPath::new("/app/web"),
            FsPath::new("/app/index.html")
        ));
    }
}
