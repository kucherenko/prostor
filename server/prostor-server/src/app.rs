//! The axum router and the application state shared by handlers.

use std::{path::Path as FsPath, sync::Arc};

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

use crate::{config::Config, ws};

/// Most WebSockets open at once (producer, guests, and a margin).
pub const MAX_SOCKETS: usize = 64;

/// State shared by all handlers.
#[derive(Clone)]
pub struct AppState {
    sockets: Arc<Semaphore>,
    web: ServeDir,
}

impl AppState {
    fn new(config: &Config) -> Self {
        AppState {
            sockets: Arc::new(Semaphore::new(MAX_SOCKETS)),
            web: ServeDir::new(&config.web_dir),
        }
    }

    /// A permit for one more WebSocket, or `None` when the cap is reached.
    pub fn socket_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.sockets.clone().try_acquire_owned().ok()
    }
}

/// Builds the router: health check, signaling and ingest WebSockets, and the
/// web app as static files with an SPA fallback to `index.html` for app routes.
pub fn router(config: &Config) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws/room/{id}", get(ws::room))
        .route("/ws/ingest/{room}", get(ws::ingest))
        .fallback(static_files)
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
        .with_state(AppState::new(config))
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

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": prostor_protocol::crate_version(),
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
