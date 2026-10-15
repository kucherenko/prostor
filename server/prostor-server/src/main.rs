//! `prostor` binary: reads `PROSTOR_*` configuration and serves until SIGINT/SIGTERM.
//!
//! The first signal stops the listener and asks every WebSocket session to close
//! (1001) and finalize its recording; `main` then waits up to
//! [`shutdown::DRAIN_TIMEOUT`] for them. A second signal exits immediately.

use prostor_server::{
    app,
    config::Config,
    shutdown::{DRAIN_TIMEOUT, Shutdown},
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_env("PROSTOR_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::from_env()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        listen = %config.listen,
        web_dir = %config.web_dir.display(),
        recordings_dir = %config.recordings_dir.display(),
        auth = if config.secret.is_some() { "required" } else { "none" },
        origins = if config.allowed_origins.is_empty() { "same-origin".to_owned() } else { config.allowed_origins.join(",") },
        private_destinations = config.allow_private_destinations,
        "starting prostor"
    );
    if config.secret.is_none() && !config.listen.ip().is_loopback() {
        tracing::warn!(
            "no PROSTOR_SECRET and listening on {}: anyone who can reach the port can \
             start an ingest session and write recordings",
            config.listen
        );
    }
    tokio::fs::create_dir_all(&config.recordings_dir)
        .await
        .map_err(|e| {
            format!(
                "cannot create the recordings directory {}: {e}",
                config.recordings_dir.display()
            )
        })?;

    let shutdown = Shutdown::new();
    tokio::spawn(watch_signals(shutdown.clone()));

    let listener = TcpListener::bind(config.listen).await?;
    axum::serve(listener, app::router(&config, shutdown.clone()))
        .with_graceful_shutdown(shutdown.cancelled())
        .await?;
    tracing::info!(
        sessions = shutdown.tracked(),
        "listener stopped; draining sessions"
    );
    if !shutdown.wait(DRAIN_TIMEOUT).await {
        tracing::warn!(
            sessions = shutdown.tracked(),
            "sessions still running after {DRAIN_TIMEOUT:?}; exiting anyway"
        );
    }
    tracing::info!("stopped");
    Ok(())
}

/// First signal: graceful shutdown. Second signal: exit now.
async fn watch_signals(shutdown: Shutdown) {
    shutdown_signal().await;
    tracing::info!("shutdown signal received; closing sessions (send again to exit now)");
    shutdown.cancel();
    shutdown_signal().await;
    tracing::warn!("second signal; exiting without draining");
    std::process::exit(1);
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install the ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install the SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
