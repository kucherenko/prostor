//! Coordinated shutdown: one cancellation token every long-lived handler watches, and a
//! tracker so `main` can wait for them to finish after the listener has stopped.
//!
//! axum's graceful shutdown waits for HTTP connections, but a WebSocket handed over by
//! `on_upgrade` runs in its own task that hyper no longer tracks. Without this module the
//! runtime would drop those tasks mid-write when `main` returns: no close frame, no flushed
//! recording.

use std::{future::Future, time::Duration};

use tokio_util::{
    sync::{CancellationToken, WaitForCancellationFutureOwned},
    task::{TaskTracker, task_tracker::TrackedFuture},
};

/// How long `main` waits for tracked sessions after the listener stopped.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared shutdown state. Cloning is cheap; every clone refers to the same token and
/// tracker.
#[derive(Debug, Clone, Default)]
pub struct Shutdown {
    token: CancellationToken,
    tracker: TaskTracker,
}

impl Shutdown {
    /// A fresh, not-yet-cancelled shutdown.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests shutdown: every [`Shutdown::cancelled`] future resolves.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether shutdown has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves once shutdown is requested. Owned, so it can be passed to
    /// `axum::serve(..).with_graceful_shutdown`.
    pub fn cancelled(&self) -> WaitForCancellationFutureOwned {
        self.token.clone().cancelled_owned()
    }

    /// Wraps a session future so [`Shutdown::wait`] waits for it.
    pub fn track<F: Future>(&self, future: F) -> TrackedFuture<F> {
        self.tracker.track_future(future)
    }

    /// Number of tracked sessions still running.
    pub fn tracked(&self) -> usize {
        self.tracker.len()
    }

    /// Stops accepting new sessions and waits up to `timeout` for the running ones.
    /// Returns `false` when some were still running at the deadline.
    pub async fn wait(&self, timeout: Duration) -> bool {
        self.tracker.close();
        tokio::time::timeout(timeout, self.tracker.wait())
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_for_tracked_sessions_that_react_to_cancellation() {
        let shutdown = Shutdown::new();
        let worker = shutdown.clone();
        let session = tokio::spawn(shutdown.track(async move {
            worker.cancelled().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }));
        assert_eq!(shutdown.tracked(), 1);
        assert!(!shutdown.is_cancelled());
        shutdown.cancel();
        assert!(shutdown.wait(Duration::from_secs(2)).await);
        assert_eq!(shutdown.tracked(), 0);
        session.await.unwrap();
    }

    #[tokio::test]
    async fn reports_sessions_that_miss_the_deadline() {
        let shutdown = Shutdown::new();
        let session = tokio::spawn(shutdown.track(std::future::pending::<()>()));
        shutdown.cancel();
        assert!(!shutdown.wait(Duration::from_millis(30)).await);
        session.abort();
    }
}
