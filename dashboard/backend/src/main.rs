//! WebGateway dashboard (master).
//!
//! The dashboard is the only process allowed to initialize or migrate the
//! PostgreSQL schema. Gateways wait until this process has published the
//! schema version in schema_state.

mod auth;
mod certificate;
mod config;
mod router;
mod secrets;
mod websites;

use std::{future::IntoFuture, time::Duration};

use shared::database::{Database, schema_version};
use tracing::{Level, event};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    shared::logger::init("dashboard=info,shared=info");
    // Must run before any secret is read or written.
    shared::crypto::init_from_env()?;

    let config = config::load()?;

    let database = Database::connect(&config.database).await?;
    database.initialize_master(&config.database).await?;
    database.verify_schema().await?;
    secrets::encrypt_legacy_secrets(&database).await?;
    auth::ensure_default_admin(&database).await?;

    let stats_task = shared::stats::spawn(database.clone());
    let certificate_task = certificate::spawn(database.clone());

    let address = std::net::SocketAddr::from(([0, 0, 0, 0], config.server.port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    event!(
        Level::INFO,
        %address,
        schema_version = schema_version(),
        static_dir = ?config.server.static_dir,
        "dashboard listening"
    );

    let app = router::router(
        database,
        config.server.service_tokens(),
        config.server.session_ttl_secs,
        config.server.static_dir.clone(),
    );
    // The timeout only guards the drain after a signal: wrapping the whole
    // server future would make the process exit 30s after startup.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = cancel_tx.send(());
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        _ = cancel_rx => {
            match tokio::time::timeout(Duration::from_secs(30), &mut server).await {
                Ok(result) => result?,
                Err(_) => event!(Level::WARN, "dashboard shutdown timed out after 30s"),
            }
        }
    }

    stats_task.abort();
    certificate_task.abort();
    event!(Level::INFO, "dashboard stopped");
    Ok(())
}

/// Wait for SIGTERM (containers/systemd) or Ctrl-C, then drain gracefully.
async fn shutdown_signal() {
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {}
            Err(error) => {
                event!(Level::ERROR, %error, "failed to install the Ctrl-C handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                event!(Level::ERROR, %error, "failed to install the SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => event!(Level::INFO, "received Ctrl-C, shutting down"),
        () = terminate => event!(Level::INFO, "received SIGTERM, shutting down"),
    }
}
