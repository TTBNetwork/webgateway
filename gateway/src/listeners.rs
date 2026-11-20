//! Dynamic port listeners (HTTP and TLS).

use std::{
    collections::{BTreeSet, HashMap},
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::JoinHandle,
};
use tracing::{Level, event};

use crate::proxy;
use crate::proxy_protocol;
use crate::state::AppState;

/// Tracks connections currently being served so shutdown can drain them.
#[derive(Clone, Default)]
struct Inflight {
    count: Arc<AtomicUsize>,
}

struct InflightGuard {
    inflight: Inflight,
}

impl Inflight {
    fn enter(&self) -> InflightGuard {
        self.count.fetch_add(1, Ordering::SeqCst);
        InflightGuard {
            inflight: self.clone(),
        }
    }

    async fn wait_idle(&self, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if self.count.load(Ordering::SeqCst) == 0 {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.inflight.count.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Listeners {
    bind: IpAddr,
    tasks: HashMap<u16, JoinHandle<()>>,
    shutdown: watch::Sender<bool>,
    inflight: Inflight,
    connections: Arc<Semaphore>,
    max_header_bytes: usize,
}

impl Listeners {
    pub fn new(bind: IpAddr, max_connections: usize, max_header_bytes: usize) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            bind,
            tasks: HashMap::new(),
            shutdown,
            inflight: Inflight::default(),
            connections: Arc::new(Semaphore::new(max_connections.max(1))),
            max_header_bytes,
        }
    }

    pub fn reconcile(&mut self, ports: &BTreeSet<u16>, state: Arc<AppState>) {
        let finished: Vec<u16> = self
            .tasks
            .iter()
            .filter(|(_, handle)| handle.is_finished())
            .map(|(port, _)| *port)
            .collect();
        for port in finished {
            self.tasks.remove(&port);
        }

        for port in ports {
            if self.tasks.contains_key(port) {
                continue;
            }

            let state = state.clone();
            let bind = self.bind;
            let port = *port;
            let shutdown = self.shutdown.subscribe();
            let inflight = self.inflight.clone();
            let connections = self.connections.clone();
            let max_header_bytes = self.max_header_bytes;
            let handle = tokio::spawn(async move {
                if let Err(error) = serve_port(
                    bind,
                    port,
                    state,
                    shutdown,
                    inflight,
                    connections,
                    max_header_bytes,
                )
                .await
                {
                    event!(Level::ERROR, port, %error, "listener stopped");
                }
            });
            self.tasks.insert(port, handle);
        }

        let stale: Vec<u16> = self
            .tasks
            .keys()
            .filter(|port| !ports.contains(port))
            .copied()
            .collect();
        for port in stale {
            if let Some(handle) = self.tasks.remove(&port) {
                handle.abort();
                event!(Level::INFO, port, "listener removed");
            }
        }
    }

    /// Stop accepting new connections, wait for in-flight requests, and report
    /// whether the drain finished within the limit.
    pub async fn shutdown(&mut self, drain_limit: Duration) -> bool {
        let _ = self.shutdown.send(true);
        for (_, handle) in self.tasks.drain() {
            let _ = handle.await;
        }
        self.inflight.wait_idle(drain_limit).await
    }
}

async fn serve_port(
    bind: IpAddr,
    port: u16,
    state: Arc<AppState>,
    mut shutdown: watch::Receiver<bool>,
    inflight: Inflight,
    connections: Arc<Semaphore>,
    max_header_bytes: usize,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(bind, port))
        .await
        .with_context(|| format!("failed to bind port {port}"))?;
    event!(Level::INFO, port, "gateway listener started");

    loop {
        let (stream, peer) = tokio::select! {
            _ = shutdown.changed() => {
                event!(Level::INFO, port, "listener stopped accepting connections");
                return Ok(());
            }
            accepted = listener.accept() => match accepted {
                Ok(connection) => connection,
                Err(error) => {
                    event!(Level::WARN, port, %error, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            }
        };

        let permit = match connections.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                event!(
                    Level::WARN,
                    port,
                    "connection limit reached; dropping new connection"
                );
                drop(stream);
                continue;
            }
        };

        let state = state.clone();
        let guard = inflight.enter();
        tokio::spawn(async move {
            let _permit = permit;
            let _guard = guard;
            if let Err(error) = handle_connection(state, stream, peer, max_header_bytes).await {
                event!(Level::DEBUG, %peer, %error, "connection closed with an error");
            }
        });
    }
}

async fn handle_connection(
    state: Arc<AppState>,
    mut stream: TcpStream,
    peer: SocketAddr,
    max_header_bytes: usize,
) -> anyhow::Result<()> {
    let proxy = proxy_protocol::read_header(&mut stream)
        .await
        .context("failed to read the PROXY protocol header")?;
    if proxy.present {
        event!(Level::DEBUG, source = ?proxy.source, "PROXY protocol header received");
    }

    let mut first_byte = [0u8; 1];
    let read = stream.peek(&mut first_byte).await?;
    if read == 0 {
        return Ok(());
    }

    if first_byte[0] == 0x16 {
        let stream = state
            .tls_acceptor
            .accept(stream)
            .await
            .context("TLS handshake failed")?;
        serve(state, stream, peer, "https", proxy.source, max_header_bytes).await
    } else {
        serve(state, stream, peer, "http", proxy.source, max_header_bytes).await
    }
}

async fn serve<S>(
    state: Arc<AppState>,
    io: S,
    peer: SocketAddr,
    scheme: &'static str,
    proxy_source: Option<SocketAddr>,
    max_header_bytes: usize,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |request| {
        let state = state.clone();
        async move {
            let response = proxy::handle(state, request, peer, scheme, proxy_source).await;
            Ok::<_, Infallible>(response)
        }
    });

    let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    builder.http1().max_buf_size(max_header_bytes);
    builder
        .http2()
        .max_header_list_size(u32::try_from(max_header_bytes).unwrap_or(u32::MAX));
    builder
        .serve_connection_with_upgrades(TokioIo::new(io), service)
        .await
        .map_err(|error| anyhow::anyhow!("HTTP connection failed: {error}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inflight_wait_tracks_open_connections() {
        let inflight = Inflight::default();
        let guard = inflight.enter();
        assert!(
            !inflight.wait_idle(Duration::from_millis(30)).await,
            "a live connection must prevent the drain from completing"
        );
        drop(guard);
        assert!(
            inflight.wait_idle(Duration::from_secs(1)).await,
            "the drain must complete after the last connection closes"
        );
    }
}
