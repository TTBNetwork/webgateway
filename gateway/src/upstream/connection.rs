use std::collections::VecDeque;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::{
    ClientConfig,
    pki_types::{DnsName, ServerName},
};
use shared::streams::WrapperBufferStream;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
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

    /// 探活：确认对端没有关闭连接。
    ///
    /// 注意：**不能**用 `write(&[])` 做探活 —— tokio 对空缓冲区的 `poll_write`
    /// 不产生任何系统调用、恒返回 `Ok(0)`，因此原来的实现在任何情况下都返回
    /// `true`，等于完全没有检查（ISSUES.md P0-5）。
    ///
    /// 这里改用零长度 `read`：`poll_read` 对空 `ReadBuf` 会真正向 socket 发起
    /// 一次 `recv`，因此立刻返回 `Ok(())` 表示对端已关闭（EOF），返回 `Pending`
    /// 表示连接仍然存活。空缓冲区不会消费任何数据，所以这个探测不破坏
    /// keep-alive 复用所需的字节流。
    pub async fn is_healthy(&mut self) -> bool {
        let probe = async {
            std::future::poll_fn(|cx| {
                let mut buf = ReadBuf::uninit(&mut []);
                match Pin::new(&mut self.inner).poll_read(cx, &mut buf) {
                    // 读返回 0 字节且没有填充任何数据 = 对端已关闭。
                    Poll::Ready(Ok(())) => {
                        if buf.filled().is_empty() {
                            Poll::Ready(false)
                        } else {
                            Poll::Ready(true)
                        }
                    }
                    // 读错误（RST 等）同样视为不可用。
                    Poll::Ready(Err(_)) => Poll::Ready(false),
                    Poll::Pending => Poll::Ready(true),
                }
            })
            .await
        };
        matches!(
            tokio::time::timeout(std::time::Duration::from_millis(50), probe).await,
            Ok(true)
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
/// 未显式配置 `max_connections` 时每个站点可用的上游连接上限。
///
/// 原实现把 `0` 解释为 `Semaphore::MAX_PERMITS`（等于无上限），
/// 配合"连接要等 120s 超时才归还"的行为，fd 与上游连接数会随并发线性增长（P0-6）。
/// 256 足以支撑单站点的常规并发，同时给上游服务留出明确的连接预算。
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

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
/// 空闲连接队列的上限：超过后归还的连接直接关闭，避免"死连接堆积"。
const MAX_IDLE_CONNECTIONS: usize = 64;
/// 从池中取连接时等待许可的最长时间。超过说明上游吞吐已经跟不上了，
/// 此时快速失败（返回 502）比无限排队更好，否则请求会一直挂着直到连接级超时。
const ACQUIRE_PERMIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct UpstreamConnectionPool {
    config: UpstreamConnectionPoolConfig,
    idle: Mutex<VecDeque<UpstreamConnection>>,
    semaphore: Arc<Semaphore>,
    next_index: AtomicUsize,
}

impl UpstreamConnectionPool {
    pub fn new(config: UpstreamConnectionPoolConfig) -> Arc<Self> {
        // `max_connections == 0` 原本被映射为 `Semaphore::MAX_PERMITS`（等于无上限），
        // 于是每个并发请求都各占一条上游连接，fd 与上游连接数随并发线性增长（ISSUES.md P0-6）。
        // 这里给一个有限且有实际意义的默认值，并让配置项真正生效。
        let max = if config.max_connections == 0 {
            DEFAULT_MAX_CONNECTIONS
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

    /// 当前还剩多少个可用的连接许可。
    ///
    /// 原名 `max_connections` 有误导性（它返回"剩余"而不是"上限"），故改名。
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    pub async fn get(self: &Arc<Self>) -> anyhow::Result<PooledUpstreamConnection> {
        let permit = tokio::time::timeout(
            ACQUIRE_PERMIT_TIMEOUT,
            self.semaphore.clone().acquire_owned(),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Timed out waiting for an upstream connection permit after {:?}; \
                     the upstream pool is saturated",
                ACQUIRE_PERMIT_TIMEOUT
            )
        })??;

        // 关键修复（P1-7）：先把连接从队列中取出并**释放锁**，再在锁外做探活。
        // 原实现在持有 `tokio::sync::Mutex` 期间 await 最长 100ms 的健康检查，
        // 所有并发取连接都要在同一把锁上排队，成为全站吞吐瓶颈。
        loop {
            let candidate = {
                let mut idle = self.idle.lock().await;
                idle.pop_front()
            };
            match candidate {
                Some(mut conn) => {
                    if conn.is_healthy().await {
                        return Ok(PooledUpstreamConnection {
                            conn: Some(conn),
                            pool: self.clone(),
                            _permit: Some(permit),
                            reusable: Arc::new(AtomicBool::new(false)),
                            task_exited: Arc::new(AtomicBool::new(false)),
                            reused: true,
                        });
                    }
                    // 探活失败：丢弃并继续尝试队列中的下一条。
                    drop(conn.close().await);
                }
                None => break,
            }
        }

        let conn = self.try_create_connection().await?;
        Ok(PooledUpstreamConnection {
            conn: Some(conn),
            pool: self.clone(),
            _permit: Some(permit),
            reusable: Arc::new(AtomicBool::new(false)),
            task_exited: Arc::new(AtomicBool::new(false)),
            reused: false,
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

    /// 归还连接。
    ///
    /// **调用方必须在确认响应体已被完整读完、且连接可以安全复用之后才调用**，
    /// 否则残留的响应字节会被下一个请求读到（响应串包 / 跨站点数据泄露，ISSUES.md P0-5）。
    /// 归还一条已经确认可复用的连接。调用方必须已经确认响应体被完整消费。
    pub async fn return_connection(&self, mut conn: UpstreamConnection) {
        if !conn.is_healthy().await {
            let _ = conn.close().await;
            return;
        }
        let mut idle = self.idle.lock().await;
        if idle.len() >= MAX_IDLE_CONNECTIONS {
            drop(idle);
            let _ = conn.close().await;
            return;
        }
        idle.push_back(conn);
    }

    pub fn get_path(&self) -> Option<&Url> {
        self.config.url.as_ref()
    }
}

// ---------- 借出连接 ----------
/// 借出的上游连接。
///
/// ## 为什么要有 `reusable` 标志（ISSUES.md P0-5）
///
/// 原先无论响应体是否读完，连接都会在 `Drop` 时被放回 `idle` 队列。客户端在响应
/// 传输中途断开时，上游 socket 里仍残留未读的响应字节，下一个请求复用这条连接就会
/// 读到上一个请求的响应 —— 不同站点之间即为跨租户数据泄露。
///
/// 现在的规则是：**只有**观察到响应体被完整读到 EOF（由 `StatisticsIncoming`
/// 置位共享标志），连接才允许回到池中；否则一律关闭。
///
/// 标志用 `Arc<AtomicBool>` 是因为它同时被响应体（在请求处理路径上）和
/// 连接任务（在后台）读写，两者生命周期不同。
#[derive(Debug)]
pub struct PooledUpstreamConnection {
    conn: Option<UpstreamConnection>,
    pool: Arc<UpstreamConnectionPool>,
    /// 连接许可。用 `Option` 包裹是为了在没有 `Drop` 的情况下也能整体移出
    /// （见 [`PooledUpstreamConnection::take_parts`]）。
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
    /// 见上文说明；`false` 表示连接不可复用。
    reusable: Arc<AtomicBool>,
    /// 「驱动这条连接的 future 是否已经结束」的共享标志。
    ///
    /// 这条标志解决的是一个**致命的挂起**：`try_send_request` 把请求投递给由
    /// 连接任务持有的 dispatch 队列。任务一旦退出（上游 EOF/出错/超时），队列
    /// 就没有接收者了，此后复用该连接发送请求会**永久挂起**（实测：每隔一个请求
    /// 超时，且请求根本到不了上游）。
    ///
    /// 因此判定"能否放回空闲池"必须同时满足：
    ///   1. 响应体已读到 EOF（`reusable`，防跨请求串包，P0-5）；
    ///   2. 连接任务**仍在运行**（本标志为 false）—— 只要任务已结束，这条连接就没人驱动了。
    /// 把连接任务结束时间点记录下来，而不是在结束时清 `reusable`，是为了不干扰
    /// "响应体先于任务结束"这一正常复用路径。
    task_exited: Arc<AtomicBool>,
    /// 这条连接是否取自**空闲队列**（true）还是本轮新建的（false）。
    ///
    /// 用途：连接可能已被上游单方面关闭，而探活与真正发送之间存在时间窗口。
    /// 只有「复用的连接 + 请求体被交还（即尚未发出任何字节）」这两个条件同时成立时，
    /// 重试才是安全的 —— 这与 hyper-util `legacy::Client` 的判据一致（`connection_reused`）。
    reused: bool,
}

impl PooledUpstreamConnection {
    /// 取得"响应体已完整读完"的共享标志。
    ///
    /// 传给 [`crate::transport::StatisticsIncoming::with_body_drained_flag`]，
    /// 由响应体在读到 EOF 时置位。
    pub fn drained_flag(&self) -> Arc<AtomicBool> {
        self.reusable.clone()
    }

    /// 取得"连接任务已结束"的共享标志，由连接任务在退出前置位。
    pub fn task_exited_flag(&self) -> Arc<AtomicBool> {
        self.task_exited.clone()
    }

    /// 连接当前是否**真的**可以复用：响应体读完 **且** 仍有任务在驱动它。
    pub fn is_reusable(&self) -> bool {
        self.reusable.load(Ordering::Acquire) && !self.task_exited.load(Ordering::Acquire)
    }

    /// 这条连接是否来自空闲队列（即"复用"的连接）。
    ///
    /// 只有复用的连接才可能"取出来的瞬间已被上游关闭"，因此也只有它值得重试一次；
    /// 全新建立的连接失败说明上游本身有问题，重试无意义。
    pub fn is_reused(&self) -> bool {
        self.reused
    }

    /// 取出底层连接、池引用与配套的连接许可，用于在别处（例如 hyper 连接任务
    /// 结束之后）再决定是归还还是关闭。
    ///
    /// 许可与连接必须一起移动 —— 如果只移动连接而让许可在此处释放，池的并发上限
    /// 就会被绕过，`max_connections` 会再次形同虚设（P0-6）。
    pub fn take_parts(
        mut self,
    ) -> (
        UpstreamConnection,
        Arc<UpstreamConnectionPool>,
        Option<tokio::sync::OwnedSemaphorePermit>,
    ) {
        let conn = self
            .conn
            .take()
            .expect("PooledUpstreamConnection::take_parts called after the connection was taken");
        let pool = self.pool.clone();
        let permit = self._permit.take();
        (conn, pool, permit)
    }

    /// 取出底层连接（不关闭），保留许可由 `self` 的 `Drop` 释放。
    pub fn take_connection(&mut self) -> Option<UpstreamConnection> {
        self.conn.take()
    }

    pub async fn return_to_pool(mut self) -> anyhow::Result<()> {
        if let Some(conn) = self.conn.take() {
            if self.is_reusable() {
                self.pool.return_connection(conn).await;
            } else {
                // 无法确认响应体已读完 —— 直接关闭，绝不冒险复用。
                drop(conn.close().await);
            }
        }
        Ok(())
    }
}

impl Drop for PooledUpstreamConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let pool = self.pool.clone();
            // 只有响应体被完整读完后才允许回到池中（P0-5）。
            if self.is_reusable() {
                tokio::spawn(async move {
                    pool.return_connection(conn).await;
                });
            } else {
                tokio::spawn(async move {
                    drop(conn.close().await);
                });
            }
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
