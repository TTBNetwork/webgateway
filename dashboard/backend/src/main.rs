//! WebGateway dashboard (master).
//!
//! The dashboard is the only process allowed to initialize or migrate the
//! PostgreSQL schema. Gateways wait until this process has published the
//! schema version in schema_state.

mod config;
mod router;
mod stats;

use shared::database::{Database, schema_version};
use tracing::{Level, event};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    shared::logger::init("dashboard=info,shared=info");

    let config = config::load()?;

    let database = Database::connect(&config.database).await?;
    database.initialize_master(&config.database).await?;
    database.verify_schema().await?;

    let stats_task = stats::spawn(database.clone());

    let address = std::net::SocketAddr::from(([0, 0, 0, 0], config.server.port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    event!(
        Level::INFO,
        %address,
        schema_version = schema_version(),
        "dashboard listening"
    );

    axum::serve(
        listener,
        router::router(database, config.server.api_token.clone()),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    stats_task.abort();
    event!(Level::INFO, "dashboard stopped");
    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        event!(Level::ERROR, %error, "failed to install the Ctrl-C handler");
    }
}
