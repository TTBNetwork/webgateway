use std::collections::VecDeque;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use rustls::{
    ClientConfig,
    pki_types::{DnsName, ServerName},
};
use shared::streams::WrapperBufferStream;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Semaphore};
use url::Url;

// ---------- UpstreamConnection ----------
#[derive(Debug)]
pub struct UpstreamConnection {
    inner: WrapperBufferStream,
}

impl UpstreamConnection {
    pub async fn new_tcp(addr: SocketAddr) -> anyhow::Result<Self> {
        Ok(Self {
            inner: WrapperBufferStream::Raw(TcpStream::connect(addr).await?),
        })
    }

    pub async fn new_tls(
        addr: SocketAddr,
        config: Arc<ClientConfig>,
        hostname: Option<impl Into<String>>,
    ) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Self::new_tls_from_raw(stream, config, hostname).await
    }

    pub async fn new_tls_from_raw(
        stream: TcpStream,
        config: Arc<ClientConfig>,
        hostname: Option<impl Into<String>>,
    ) -> anyhow::Result<Self> {
        let connector = tokio_rustls::TlsConnector::from(config);
        let server_name = match hostname {
            Some(h) => {
                let host = h.into();
                if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                    ServerName::IpAddress(ip.into())
                } else {
                    DnsName::try_from(host)
                        .map_err(|_| anyhow::anyhow!("invalid DNS name"))?
                        .into()
                }
            }
            None => ServerName::IpAddress(stream.peer_addr()?.ip().into()),
        };
        Ok(Self {
            inner: WrapperBufferStream::TlsClient(Box::new(
                connector.connect(server_name, stream).await?,
            )),
        })
    }

    pub async fn close(self) -> anyhow::Result<()> {
        Ok(self.inner.close().await?)
    }

    pub async fn is_healthy(&mut self) -> bool {
        matches!(
            tokio::time::timeout(std::time::Duration::from_millis(100), self.inner.write(&[]))
                .await,
            Ok(Ok(_))
        )
    }
}

// 完整的 AsyncRead 实现（委托给 inner）
impl AsyncRead for UpstreamConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

// 完整的 AsyncWrite 实现（委托给 inner）
impl AsyncWrite for UpstreamConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ---------- 配置 ----------
#[derive(Debug, Clone)]
pub struct UpstreamConnectionPoolConfig {
    pub targets: Vec<SocketAddr>,
    pub max_connections: usize,
    pub tls: bool,
    pub tls_config: Option<Arc<ClientConfig>>,
    pub hostname: Option<String>,
    pub url: Option<Url>,
}

impl UpstreamConnectionPoolConfig {
    pub fn new(target: SocketAddr) -> Self {
        Self {
            targets: vec![target],
            max_connections: 0,
            tls: false,
            tls_config: None,
            hostname: None,
            url: None,
        }
    }

    pub fn new_from_targets(targets: Vec<SocketAddr>) -> Self {
        Self {
            targets,
            max_connections: 0,
            tls: false,
            tls_config: None,
            hostname: None,
            url: None,
        }
    }

    pub fn max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    pub fn tls(mut self, config: Arc<ClientConfig>, hostname: Option<String>) -> Self {
        self.tls = true;
        self.tls_config = Some(config);
        self.hostname = hostname;
        self
    }

    pub fn url(mut self, url: Url) -> Self {
        self.url = Some(url);
        self
    }
}

// ---------- 连接池 ----------
#[derive(Debug)]
pub struct UpstreamConnectionPool {
    config: UpstreamConnectionPoolConfig,
    idle: Mutex<VecDeque<UpstreamConnection>>,
    semaphore: Arc<Semaphore>,
    next_index: AtomicUsize,
}

impl UpstreamConnectionPool {
    pub fn new(config: UpstreamConnectionPoolConfig) -> Arc<Self> {
        let max = if config.max_connections == 0 {
            Semaphore::MAX_PERMITS
        } else {
            config.max_connections
        };
        Arc::new(Self {
            config,
            idle: Mutex::new(VecDeque::new()),
            semaphore: Arc::new(Semaphore::new(max)),
            next_index: AtomicUsize::new(0),
        })
    }

    pub async fn get(self: &Arc<Self>) -> anyhow::Result<PooledUpstreamConnection> {
        let permit = self.semaphore.clone().acquire_owned().await?;
        loop {
            let mut idle = self.idle.lock().await;
            if let Some(mut conn) = idle.pop_front() {
                if conn.is_healthy().await {
                    return Ok(PooledUpstreamConnection {
                        conn: Some(conn),
                        pool: self.clone(),
                        _permit: permit,
                    });
                } else {
                    drop(conn.close().await);
                    continue;
                }
            }
            break;
        }
        let conn = self.try_create_connection().await?;
        Ok(PooledUpstreamConnection {
            conn: Some(conn),
            pool: self.clone(),
            _permit: permit,
        })
    }

    pub async fn create_connection(&self) -> anyhow::Result<UpstreamConnection> {
        self.try_create_connection().await
    }

    async fn try_create_connection(&self) -> anyhow::Result<UpstreamConnection> {
        let targets = &self.config.targets;
        if targets.is_empty() {
            return Err(anyhow::anyhow!("No upstream targets configured"));
        }
        let start = self.next_index.fetch_add(1, Ordering::Relaxed) % targets.len();
        for i in 0..targets.len() {
            let idx = (start + i) % targets.len();
            let addr = targets[idx];
            match self.connect_to_addr(addr).await {
                Ok(conn) => return Ok(conn),
                Err(e) => tracing::warn!("Failed to connect to {}: {}", addr, e),
            }
        }
        Err(anyhow::anyhow!("All upstreams are unreachable"))
    }

    async fn connect_to_addr(&self, addr: SocketAddr) -> anyhow::Result<UpstreamConnection> {
        if self.config.tls {
            let config = self.config.tls_config.clone().expect("TLS config missing");
            UpstreamConnection::new_tls(addr, config, self.config.hostname.clone()).await
        } else {
            UpstreamConnection::new_tcp(addr).await
        }
    }

    async fn return_connection(&self, mut conn: UpstreamConnection) {
        if !conn.is_healthy().await {
            let _ = conn.close().await;
            return;
        }
        let mut idle = self.idle.lock().await;
        idle.push_back(conn);
    }

    pub fn get_path(&self) -> Option<&Url> {
        self.config.url.as_ref()
    }
}

// ---------- 借出连接 ----------
#[derive(Debug)]
pub struct PooledUpstreamConnection {
    conn: Option<UpstreamConnection>,
    pool: Arc<UpstreamConnectionPool>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl PooledUpstreamConnection {
    pub async fn return_to_pool(mut self) -> anyhow::Result<()> {
        if let Some(conn) = self.conn.take() {
            self.pool.return_connection(conn).await;
        }
        Ok(())
    }
}

impl Drop for PooledUpstreamConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                pool.return_connection(conn).await;
            });
        }
    }
}

// ----- 为 PooledUpstreamConnection 实现 AsyncRead / AsyncWrite -----
impl AsyncRead for PooledUpstreamConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(self.conn.as_mut().unwrap()).poll_read(cx, buf)
    }
}

impl AsyncWrite for PooledUpstreamConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(self.conn.as_mut().unwrap()).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(self.conn.as_mut().unwrap()).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.conn.as_ref().unwrap().is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(self.conn.as_mut().unwrap()).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(self.conn.as_mut().unwrap()).poll_shutdown(cx)
    }
}

// == MixedUpstreamConnection
#[derive(Debug)]
pub enum MixedUpstreamConnection {
    Pool(PooledUpstreamConnection),
    Raw(UpstreamConnection),
}

impl AsyncRead for MixedUpstreamConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MixedUpstreamConnection::Pool(p) => Pin::new(p).poll_read(cx, buf),
            MixedUpstreamConnection::Raw(r) => Pin::new(r).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MixedUpstreamConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MixedUpstreamConnection::Pool(p) => Pin::new(p).poll_write(cx, buf),
            MixedUpstreamConnection::Raw(r) => Pin::new(r).poll_write(cx, buf),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MixedUpstreamConnection::Pool(p) => Pin::new(p).poll_write_vectored(cx, bufs),
            MixedUpstreamConnection::Raw(r) => Pin::new(r).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            MixedUpstreamConnection::Pool(p) => p.is_write_vectored(),
            MixedUpstreamConnection::Raw(r) => r.is_write_vectored(),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MixedUpstreamConnection::Pool(p) => Pin::new(p).poll_flush(cx),
            MixedUpstreamConnection::Raw(r) => Pin::new(r).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MixedUpstreamConnection::Pool(p) => Pin::new(p).poll_shutdown(cx),
            MixedUpstreamConnection::Raw(r) => Pin::new(r).poll_shutdown(cx),
        }
    }
}
