//! Dynamic port listeners (HTTP and TLS).

use std::{
    collections::{BTreeSet, HashMap},
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tracing::{Level, event};

use crate::proxy;
use crate::proxy_protocol;
use crate::state::AppState;

pub struct Listeners {
    bind: IpAddr,
    tasks: HashMap<u16, JoinHandle<()>>,
}

impl Listeners {
    pub fn new(bind: IpAddr) -> Self {
        Self {
            bind,
            tasks: HashMap::new(),
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
            let handle = tokio::spawn(async move {
                if let Err(error) = serve_port(bind, port, state).await {
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

    pub fn abort_all(&mut self) {
        for (_, handle) in self.tasks.drain() {
            handle.abort();
        }
    }
}

async fn serve_port(bind: IpAddr, port: u16, state: Arc<AppState>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(bind, port))
        .await
        .with_context(|| format!("failed to bind port {port}"))?;
    event!(Level::INFO, port, "gateway listener started");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                event!(Level::WARN, port, %error, "accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(state, stream, peer).await {
                event!(Level::DEBUG, %peer, %error, "connection closed with an error");
            }
        });
    }
}

async fn handle_connection(
    state: Arc<AppState>,
    mut stream: TcpStream,
    peer: SocketAddr,
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
        serve(state, stream, peer, "https", proxy.source).await
    } else {
        serve(state, stream, peer, "http", proxy.source).await
    }
}

async fn serve<S>(
    state: Arc<AppState>,
    io: S,
    peer: SocketAddr,
    scheme: &'static str,
    proxy_source: Option<SocketAddr>,
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

    hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(io), service)
        .await
        .map_err(|error| anyhow::anyhow!("HTTP connection failed: {error}"))?;

    Ok(())
}
