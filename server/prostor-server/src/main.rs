//! `prostor` binary: reads `PROSTOR_*` configuration and serves until SIGINT/SIGTERM.

use prostor_server::{app, config::Config};
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
        // Nothing is enforced yet (M2 adds producer auth); this only says whether a
        // secret is configured.
        auth = if config.secret.is_some() { "configured" } else { "none" },
        "starting prostor"
    );

    let listener = TcpListener::bind(config.listen).await?;
    axum::serve(listener, app::router(&config))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("stopped");
    Ok(())
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
    tracing::info!("shutdown signal received");
}
