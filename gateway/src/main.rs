//! WebGateway data plane node.
//!
//! The gateway waits for the master to initialize the database schema, marks
//! itself online, then serves HTTP/HTTPS traffic from the websites it loads.

mod config;
mod listeners;
mod node;
mod proxy;
mod state;
mod sync;
mod tls;

use std::{
    net::IpAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::Context;
use shared::database::wait_for_master_schema;
use tokio::task::JoinHandle;
use tracing::{Level, event};

use crate::{
    config::Config,
    listeners::Listeners,
    node::NodeInfo,
    state::{AppState, CertStore},
    tls::default_certified_key,
};

#[tokio::main]
async fn main() {
    shared::logger::init("gateway=info,shared=info");

    if let Err(error) = run().await {
        event!(Level::ERROR, %error, "gateway stopped with an error");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let config = Config::load()?;

    // The gateway never migrates: it waits until the master publishes the schema version.
    let database = wait_for_master_schema(&config.database).await?;

    let node = NodeInfo::new(&config.node);
    node.register_online(&database).await?;

    event!(
        Level::INFO,
        machine_id = %node.machine_id,
        node = %node.label(),
        version = node.version,
        "gateway starting"
    );

    let default_cert =
        default_certified_key().context("failed to build the default TLS certificate")?;
    let certs = Arc::new(RwLock::new(Arc::new(CertStore::build(
        Vec::new(),
        default_cert,
    ))));
    let state = Arc::new(AppState::new(node.clone(), certs)?);

    let bind: IpAddr = config
        .server
        .bind
        .parse()
        .with_context(|| format!("invalid [server].bind address: {}", config.server.bind))?;
    let mut listeners = Listeners::new(bind);

    let mut ports = sync::sync_once(&database, &state, &config.server.ports)
        .await
        .context("initial configuration sync failed")?;
    listeners.reconcile(&ports, state.clone());

    let heartbeat = spawn_heartbeat(database.clone(), node.clone());

    loop {
        tokio::select! {
            _ = shutdown_signal() => {
                event!(Level::INFO, "shutdown signal received");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                match sync::sync_once(&database, &state, &config.server.ports).await {
                    Ok(next_ports) => {
                        ports = next_ports;
                        listeners.reconcile(&ports, state.clone());
                    }
                    Err(error) => {
                        event!(Level::WARN, %error, "configuration sync failed; keeping the previous configuration");
                    }
                }
            }
        }
    }

    heartbeat.abort();
    listeners.abort_all();
    node.mark_offline(&database).await?;
    database.pool().close().await;

    Ok(())
}

fn spawn_heartbeat(database: shared::database::Database, node: NodeInfo) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = node.heartbeat(&database).await {
                event!(Level::WARN, %error, "gateway heartbeat failed");
            }
        }
    })
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate =
            signal(SignalKind::terminate()).expect("failed to install the SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
