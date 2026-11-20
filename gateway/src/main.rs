//! WebGateway data plane node.
//!
//! The gateway waits for the master to initialize the database schema, marks
//! itself online, then serves HTTP/HTTPS traffic from the websites it loads.

mod access;
mod cache;
mod config;
mod health;
mod http3;
mod listeners;
mod metrics;
mod node;
mod notify;
mod proxy;
mod proxy_protocol;
mod ratelimit;
mod state;
mod sync;
mod tls;
mod upgrade;

use std::{
    net::IpAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::Context;
use shared::database::wait_for_master_schema;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{Level, event};

use crate::{
    config::Config,
    listeners::Listeners,
    node::NodeInfo,
    notify::ConfigEvent,
    state::{AppState, CertStore, StateLimits},
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
    // Certificate private keys may be encrypted at rest by the dashboard.
    shared::crypto::init_from_env()?;
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
    let (log_sender, log_writer) = access::spawn(database.clone());
    let upstream_ca_file = config
        .upstream_tls
        .ca_file
        .as_deref()
        .map(std::path::Path::new);
    let state = Arc::new(AppState::new(
        node.clone(),
        certs,
        log_sender,
        upstream_ca_file,
        StateLimits {
            cache_max_size_bytes: config.server.cache_max_size_bytes,
            max_request_body_bytes: config.server.max_request_body_bytes,
            max_response_body_bytes: config.server.max_response_body_bytes,
            connect_ports: config.server.connect_ports.clone(),
            h3_port: config.server.http3_ports.first().copied(),
            rate_limit: config.server.rate_limit.clone(),
        },
    )?);

    let bind: IpAddr = config
        .server
        .bind
        .parse()
        .with_context(|| format!("invalid [server].bind address: {}", config.server.bind))?;
    let health_task = health::spawn(state.clone());
    let metrics_task = match config.server.metrics_port {
        Some(port) => {
            let metrics_bind: IpAddr = config.server.metrics_bind.parse().with_context(|| {
                format!(
                    "invalid [server].metrics_bind address: {}",
                    config.server.metrics_bind
                )
            })?;
            Some(metrics::spawn(metrics_bind, port, state.clone()).await?)
        }
        None => None,
    };
    let mut listeners = Listeners::new(
        bind,
        config.server.max_connections,
        config.server.max_header_bytes,
    );

    let initial_sync = sync::sync_once(&database, &state, &config.server.ports).await;
    metrics::record_sync_result(initial_sync.is_ok());
    let (mut ports, mut watermark) = initial_sync.context("initial configuration sync failed")?;
    listeners.reconcile(&ports, state.clone());

    let mut http3 = if config.server.http3_ports.is_empty() {
        None
    } else {
        Some(http3::spawn(
            bind,
            &config.server.http3_ports,
            state.clone(),
            listeners.inflight(),
        )?)
    };

    let heartbeat = spawn_heartbeat(database.clone(), node.clone());
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let notification_task = tokio::spawn(notify::listen(database.clone(), event_tx));

    loop {
        tokio::select! {
            _ = shutdown_signal() => {
                event!(Level::INFO, "shutdown signal received");
                break;
            }
            Some(event) = event_rx.recv() => {
                // Coalesce notifications that arrived while the last sync ran.
                let mut force = event == ConfigEvent::Changed;
                while let Ok(next) = event_rx.try_recv() {
                    force |= next == ConfigEvent::Changed;
                }

                let should_sync = if force {
                    true
                } else {
                    match sync::current_watermark(&database).await {
                        Ok(current) if current == watermark => {
                            event!(Level::DEBUG, "gateway configuration is up to date");
                            false
                        }
                        Ok(_) => true,
                        Err(error) => {
                            event!(Level::WARN, %error, "failed to read the configuration watermark");
                            false
                        }
                    }
                };

                if should_sync {
                    match sync::sync_once(&database, &state, &config.server.ports).await {
                        Ok((next_ports, next_watermark)) => {
                            metrics::record_sync_result(true);
                            ports = next_ports;
                            watermark = next_watermark;
                            listeners.reconcile(&ports, state.clone());
                        }
                        Err(error) => {
                            metrics::record_sync_result(false);
                            event!(Level::WARN, %error, "configuration sync failed; keeping the previous configuration");
                        }
                    }
                }
            }
        }
    }

    // Stop accepting new TCP and QUIC connections, drain in-flight HTTP/TCP and
    // HTTP/3 requests (they share the in-flight counter), then flush logs before
    // marking the node offline.
    notification_task.abort();
    // Stop accepting new QUIC connections but keep existing streams alive so
    // they can finish inside the drain window.
    if let Some(http3) = http3.as_mut() {
        http3.stop_accepting();
    }
    let drained = listeners.shutdown(Duration::from_secs(10)).await;
    if !drained {
        event!(
            Level::WARN,
            "timed out draining in-flight connections; continuing shutdown"
        );
    }
    if let Some(http3) = http3 {
        http3.close();
        http3.abort();
    }
    health_task.abort();
    if let Some(task) = metrics_task {
        task.abort();
    }
    log_writer.shutdown().await;
    heartbeat.abort();
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
