use std::{
    net::SocketAddr,
    sync::{Arc, LazyLock, atomic::Ordering},
    time::Duration,
};

use ::protocols::tls::ProtocolTLS;
use anyhow::Context;
use dashmap::DashMap;
use http_body::Body;
use hyper::{
    Request, Response, StatusCode, Version, body::Incoming, client, service::service_fn, upgrade,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto::Builder,
};
use shared::{
    database::get_database,
    listener::CustomDualStackTcpListener,
    objectid::ObjectId,
    streams::{BufferStream, WrapperBufferStream},
};
use tokio::{
    io::copy_bidirectional,
    net::TcpStream,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_rustls::TlsAcceptor;
use tracing::{Level, event};

use crate::{
    access::{self, RequestContext, RequestLog, ResponseLog},
    state::{BaseClientState, ClientState},
    sync::{SERVER_CONFIG, websites::get_website},
    transport::{CResponse, CResponseResult, StatisticsIncoming},
    upstream::{connection::UpstreamConnectionPool, structs::ConnectionRequest},
};

pub mod connection;
mod protocols;
mod structs;

static HTTP_BUILDER: LazyLock<Builder<TokioExecutor>> = LazyLock::new(|| {
    let mut builder =
        hyper_util::server::conn::auto::Builder::<TokioExecutor>::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30))
        .keep_alive(true);
    builder
});

static LISTENERS: LazyLock<DashMap<u16, JoinHandle<()>>> = LazyLock::new(DashMap::default);

static TLS_ACCEPTOR: LazyLock<Arc<TlsAcceptor>> =
    LazyLock::new(|| Arc::new(TlsAcceptor::from(SERVER_CONFIG.clone())));

const EMFILE: i32 = 24;
const ENFILE: i32 = 23;
const ECONNABORTED: i32 = 103;

async fn accept(listener: CustomDualStackTcpListener) {
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                match e.raw_os_error() {
                    Some(EMFILE) | Some(ENFILE) => {
                        event!(Level::ERROR, "fd exhausted, backing off 100ms: {e}");
                        sleep(Duration::from_millis(100)).await;
                    }
                    Some(ECONNABORTED) => {
                        // 客户端在建连过程中断开，立即重试
                    }
                    _ => {
                        event!(Level::WARN, "accept error: {e}");
                        sleep(Duration::from_millis(10)).await;
                    }
                }
                continue;
            }
        };
        tokio::spawn(async move {
            let connection = match ConnectionCycle::new(stream, addr) {
                Ok(connection) => connection,
                Err(e) => {
                    event!(
                        Level::ERROR,
                        "Failed to create connection cycle, error: {e}"
                    );
                    return;
                }
            };
            match connection.handle_connection().await {
                Ok(()) => {}
                Err(e) => {
                    event!(
                        Level::ERROR,
                        "Failed to handle connection cycle, error: {e}"
                    );
                }
            }
        });
    }
}

/// 把实际监听的端口集合对齐到 `desired`：新增的开始监听，多余的中止。
///
/// 修复（ISSUES.md P0-8）：原先 `LISTENERS` 只增不减，站点移除端口后
/// **该端口会一直被监听**。现在每轮配置同步都做一次 diff。
pub async fn sync_listeners(desired: Vec<u16>) {
    let desired: std::collections::HashSet<u16> = desired.into_iter().collect();

    // 关闭不再需要的监听端口。
    let stale: Vec<u16> = LISTENERS
        .iter()
        .map(|entry| *entry.key())
        .filter(|port| !desired.contains(port))
        .collect();
    for port in stale {
        if let Some((_, handle)) = LISTENERS.remove(&port) {
            handle.abort();
            event!(Level::INFO, "Stopped listening on port {port}");
        }
    }

    // 启动新增端口。
    for port in desired {
        if let Err(e) = listen(port).await {
            event!(Level::ERROR, "Failed to listen port {port}: {e}");
        }
    }
}

/// 开始监听某个端口。
///
/// 修复（P1-8）：原先 `contains_key` → `spawn` → `insert` 不是原子操作，
/// 并发调用（启动同步与 NOTIFY 处理同时触发）会重复 bind；又因为
/// `listener.rs` 开启了 `SO_REUSEPORT`，两次 bind 都会"成功"，
/// 连接被内核分摊到两个 accept 循环。同时 bind 失败只会在任务内 panic，
/// 而 `LISTENERS` 已经标记该端口"已监听"，站点静默不服务且日志无线索。
/// 这里改为：先 bind 成功，再插入监听表。
pub async fn listen(port: u16) -> anyhow::Result<()> {
    if LISTENERS.contains_key(&port) {
        return Ok(());
    }
    let listener = CustomDualStackTcpListener::new_by_port(port)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind port {port}: {e}"))?;
    match listener.local_addrs() {
        Ok(addrs) => event!(Level::INFO, "Listening on {addrs:?}"),
        Err(e) => event!(
            Level::WARN,
            "Listening on port {port} but cannot read local addrs: {e}"
        ),
    }
    let thread = tokio::spawn(async move {
        accept(listener).await;
    });

    LISTENERS.insert(port, thread);
    Ok(())
}

/// 去掉 Host 头里的端口，只保留主机名。
///
/// 支持三种形式：`example.com`、`example.com:8080`、`[::1]:8080`。
/// 这样做是为了让站点匹配不受监听端口影响：站点配置里通常只写主机名，
/// 而浏览器访问非默认端口时 `Host` 会带端口。
fn strip_port(host: &str) -> &str {
    // IPv6 字面量：`[::1]:8080` → `[::1]`；`[::1]` → `[::1]`
    if let Some(rest) = host.strip_prefix('[') {
        return match rest.find(']') {
            Some(idx) => &host[..idx + 2],
            None => host,
        };
    }
    // 主机名 / IPv4：只按最后一个冒号切分，避免误伤 IPv6（上面已处理）。
    match host.rsplit_once(':') {
        Some((name, _port)) if !name.is_empty() => name,
        _ => host,
    }
}

// ==================== ConnectionCycle ====================
pub struct ConnectionCycle {
    stream: BufferStream,
    addr: SocketAddr,
    local_addr: SocketAddr,
    tls: Option<ProtocolTLS>,
}

impl ConnectionCycle {
    pub fn new(stream: TcpStream, addr: SocketAddr) -> anyhow::Result<Self> {
        let local_addr = stream.local_addr()?;
        Ok(Self {
            stream: BufferStream::new(WrapperBufferStream::Raw(stream)),
            addr,
            local_addr,
            tls: None,
        })
    }

    pub async fn handle_connection(mut self) -> anyhow::Result<()> {
        let (stream, _) = protocols::get_proxy_protocol(self.stream).await?;
        let (stream, inner_tls) = protocols::get_tls_sni(stream).await?;
        self.stream = match &inner_tls {
            Some(_) => {
                let s = TLS_ACCEPTOR.accept(stream).await?;
                BufferStream::new(WrapperBufferStream::TlsServerBufferStream(Box::new(s)))
            }
            None => stream,
        };
        self.tls = inner_tls;
        self.handle_hyper().await;
        Ok(())
    }

    async fn handle_hyper(self) {
        let state = Arc::new(BaseClientState {
            tls: self.tls,
            remote_addr: self.addr.ip(),
            local_addr: self.local_addr.ip(),
        });
        let io = TokioIo::new(self.stream);
        let conn = HTTP_BUILDER.serve_connection_with_upgrades(
            io,
            service_fn(move |req: Request<Incoming>| {
                let state = state.clone();
                let req_id = ObjectId::new();
                let uri = req.uri();
                let host = uri
                    .authority()
                    .map(|v| v.as_str().to_owned())
                    .unwrap_or_else(|| {
                        req.headers()
                            .get("host")
                            .and_then(|v| v.to_str().ok().map(|v| v.to_string()))
                            .unwrap_or_default()
                    });
                let path = req.uri();
                let conn_req = Arc::new(ConnectionRequest {
                    host: Arc::new(host),
                    path: Arc::new(path.path().to_string()),
                    // query: Arc::new(path.query().unwrap_or_default().to_string()),
                    req_id,
                });
                let (parts, body) = req.into_parts();
                let req = Request::from_parts(
                    parts,
                    StatisticsIncoming::new(
                        req_id,
                        body,
                        crate::transport::StatisticsIncomingType::Request,
                    ),
                );
                Self::handle_request(req, state, conn_req)
            }),
        );

        if let Err(_) = timeout(Duration::from_secs(300), conn).await {
            event!(Level::DEBUG, "connection lifetime exceeded, closing");
        }
    }

    // ---------- 请求处理函数 ----------
    async fn handle_request(
        req: Request<StatisticsIncoming>,
        base_state: Arc<BaseClientState>,
        connection_req: Arc<ConnectionRequest>,
    ) -> anyhow::Result<hyper::Response<CResponse>> {
        // Host 头可能带端口（`example.com:8080`、`[::1]:8080`），而站点配置里
        // 通常只写主机名。不做归一化的话，监听非 80/443 端口时所有请求都会
        // 匹配不到站点而返回 404 —— 多端口监听等于完全失效。
        let site = get_website(
            strip_port(connection_req.host.as_str()),
            Some(connection_req.path.as_str()),
        )
        .await;
        let website_id = site.as_ref().map(|v| v.inner().id);
        let req_log = RequestLog::new(RequestContext {
            req_id: connection_req.req_id,
            host: connection_req.host.to_string(),
            uri: req.uri().clone(),
            headers: req.headers().clone(),
            method: req.method().clone(),
            version: req.version(),
            body_length: req.body().size_hint(),
            remote_addr: base_state.remote_addr.to_string(),
            website_id,
        });

        let resp = match req_log {
            Ok(req_log) => {
                access::add_request_log(&req_log);
                match site {
                    Some(site) => {
                        let state = ClientState {
                            base: base_state,
                            website: site.clone(),
                            host: connection_req.host.to_string(),
                            id: connection_req.req_id,
                        };
                        Self::wrapper_inner_core_handle(req, state).await
                    }
                    None => CResponseResult::NotFoundGateway,
                }
            }
            Err(_) => CResponseResult::BadRequest,
        };

        let mut responsed_at = None;
        let mut final_resp = match resp {
            CResponseResult::NotFoundGateway => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(CResponse::new_from_string("Not Found"))
                .unwrap(),
            CResponseResult::GatewayError(e) => {
                event!(Level::ERROR, "Gateway error: {e:?}");
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header("Detail-Error", e.to_string())
                    .body(CResponse::new_from_string("Gateway error"))
                    .unwrap()
            }
            CResponseResult::Timeout => Response::builder()
                .status(StatusCode::REQUEST_TIMEOUT)
                .body(CResponse::new_from_string("Request Timeout"))
                .unwrap(),
            CResponseResult::BadRequest => Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(CResponse::new_from_string("Bad Request"))
                .unwrap(),
            CResponseResult::Backend(resp) => {
                responsed_at = Some(get_database().get_database_time().unwrap());
                resp
            }
        };
        final_resp
            .headers_mut()
            .insert("Server", "WebGateway".parse()?);
        access::add_response_log(
            &ResponseLog::new(
                connection_req.req_id,
                final_resp.version(),
                final_resp.headers(),
                final_resp.status().as_u16(),
                final_resp.size_hint(),
                responsed_at,
                website_id,
            )
            .unwrap(),
        );
        Ok(final_resp)
    }

    async fn wrapper_inner_core_handle(
        req: Request<StatisticsIncoming>,
        state: ClientState,
    ) -> CResponseResult {
        let resp = timeout(Duration::from_secs(60), Self::inner_core_handle(req, state)).await;
        match resp {
            Ok(v) => match v {
                Ok(v) => CResponseResult::Backend(v),
                Err(e) => CResponseResult::GatewayError(e),
            },
            Err(_) => CResponseResult::Timeout,
        }
    }

    // -------- inner_core_handle (支持升级) --------
    async fn inner_core_handle(
        origin_req: Request<StatisticsIncoming>,
        state: ClientState,
    ) -> anyhow::Result<hyper::Response<CResponse>> {
        let site = &state.website.clone();
        let pool = site.pool();

        // 检测是否为 WebSocket 升级请求
        let is_upgrade = origin_req
            .headers()
            .get("upgrade")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);

        if is_upgrade {
            return Self::handle_upgrade(origin_req, state, pool.clone()).await;
        }

        let origin_version = origin_req.version();
        let (parts, body) = origin_req.into_parts();
        let origin_method = parts.method;
        let origin_uri = parts.uri;
        let origin_headers = parts.headers;
        let origin_extensions = parts.extensions;

        // 请求在**循环外**构造一次；重试时复用同一份 metadata，
        // body 由 `try_send_request` 在未发出任何字节的情况下原样交还。
        let mut req = Request::builder()
            .method(&origin_method)
            .version(Version::HTTP_11);
        if let Some(v) = req.headers_mut() {
            v.extend(origin_headers.clone());
        }
        if let Some(v) = req.extensions_mut() {
            v.extend(origin_extensions.clone());
        }
        req = req.uri({
            // 修复（P1-5）：
            // 1) 原代码对 `uri.path()` 无条件做 `[1..]` 切片，authority-form 请求
            //    （如 `CONNECT host:port`）的 path 是空串，`&""[1..]` 会越界 panic，
            //    而这个分支**总是**会被走到（`get_path()` 必然返回 Some）。
            // 2) 原代码用 `Url::join` 拼接，遵循 RFC 3986 —— 对没有尾斜杠的 base
            //    会替换掉最后一段：后端配置 `http://h/api` 时 `join("foo/bar")`
            //    得到 `http://h/foo/bar`，`/api` 前缀被静默丢弃。
            // 这里改为显式保留 base path 前缀，并对空 path / join 失败做兜底。
            let origin_path = origin_uri.path();
            let base = pool
                .get_path()
                .map(|v| v.path().trim_end_matches('/'))
                .filter(|v| !v.is_empty() && *v != "/");
            let path = match base {
                Some(base) => {
                    // 显式拼接，保留 `/api` 这类 base path 前缀（不再用 `Url::join`）。
                    if origin_path.is_empty() {
                        format!("{base}/")
                    } else {
                        format!("{base}{origin_path}")
                    }
                }
                None => origin_path.to_owned(),
            };
            if let Some(query) = origin_uri.query() {
                format!("{}?{}", path, query)
            } else {
                path
            }
        });

        let headers = req.headers_mut().unwrap();
        headers.insert("Host", state.host.parse()?);
        headers.insert("X-Real-Ip", format!("{}", &state.remote_addr()).parse()?);
        headers.insert(
            "X-Forwarded-For",
            format!("{}", state.remote_addr()).parse()?,
        );
        headers.insert("X-Forwarded-Proto", state.scheme().to_string().parse()?);
        headers.insert("X-Forwarded-Host", state.host.parse()?);
        let mut request = req.body(body).unwrap();

        // 复用的 keep-alive 连接可能已被上游单方面关闭（上游空闲超时最常见），
        // 而 `is_healthy()` 的探活与真正发送之间存在时间窗口，hyper 会在发送阶段
        // 返回 `Kind::Canceled`（其 Display 即用户看到的 “operation was canceled”）。
        //
        // 判据与 hyper-util `legacy::Client` 一致：**只有**「连接取自空闲队列（复用）
        // + 请求被原样交还（说明连一个字节都没发出去）」时重试才安全 —— 这正是
        // `try_send_request` 相对 `send_request` 的价值：它会把请求体还回来。
        // 不重试的话，每个被上游回收的空闲连接都会把一次正常请求变成 502。
        let (resp, body_drained) = loop {
            let pooled = pool
                .get()
                .await
                .with_context(|| "Unavailable connection from pool")?;
            // 响应体读到 EOF 时会置位这个共享标志；只有它被置位，连接才允许回到池中。
            // 客户端中途断开导致响应体没读完时，连接会被关闭（P0-5）。
            let body_drained = pooled.drained_flag();
            // 连接任务与响应体各持一份引用。
            let body_drained_for_task = body_drained.clone();
            // 任务退出前置位：此后这条连接不再有任何 future 在驱动，绝不能再复用。
            let task_exited_for_task = pooled.task_exited_flag();
            let reused = pooled.is_reused();

            // 把整个 `PooledUpstreamConnection`（含连接许可）交进 hyper 的 IO：
            // 它会被持有到连接任务结束，`Drop` 时按 `reusable` 决定归还还是关闭。
            // 许可若在这里提前释放，池的并发上限就会被绕过（P0-6）。
            let io = TokioIo::new(pooled);
            let (mut c_req, mut connection) = client::conn::http1::Builder::new()
                .handshake(io)
                .await
                .with_context(|| "Failed to handshake upstream")?;

            tokio::task::spawn(async move {
                // 用 `poll_without_shutdown` 而不是 `with_upgrades()`：这样连接结束时可以
                // `into_parts()` 取回 IO 对象（内含 `PooledUpstreamConnection`），
                // 由我们带外决定归还还是关闭。若直接 `with_upgrades()` 消费掉
                // `Connection`，IO 会随之 drop，只能依赖 IO 自身的 `Drop`，
                // 连接许可与复用判定都会失控。
                let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
                let finished = std::future::poll_fn(|cx| {
                    if tokio::time::Instant::now() >= deadline {
                        return std::task::Poll::Ready(Err(()));
                    }
                    // 注意：这里**不能**把 `Ready(Err(e))` 映射成 `Ready(Ok(()))`。
                    // 一旦映射，上层只看得到"正常结束"，于是"响应体已读完"的标志
                    // 会原样保留，这条**已经死掉的**连接会被 `Drop` 放回空闲池；
                    // 下一个请求复用它时，驱动该连接的 future 早已不存在，
                    // `try_send_request` 会把请求投进没有接收者的队列并**永久挂起**
                    // （实测表现：每隔一个请求超时，且请求根本到不了上游）。
                    match connection.poll_without_shutdown(cx) {
                        std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(())),
                        std::task::Poll::Ready(Err(e)) => {
                            event!(Level::DEBUG, "Upstream connection ended with error: {e}");
                            std::task::Poll::Ready(Err(()))
                        }
                        std::task::Poll::Pending => std::task::Poll::Pending,
                    }
                })
                .await;

                // 任务即将退出：**无论退出原因**，此后都没有 future 在驱动这条连接，
                // 复用它会命中"投递到没有接收者的队列 → 永久挂起"。
                // 因此这里统一置位 `task_exited`，让 `Drop` 走关闭分支。
                task_exited_for_task.store(true, Ordering::Release);
                if finished.is_err() {
                    event!(
                        Level::DEBUG,
                        "Upstream connection ended with error or lifetime timeout; closing it \
                         instead of returning it to the idle pool"
                    );
                }
                let _ = &body_drained_for_task;
                let parts = connection.into_parts();
                drop(parts.read_buf);
                drop(parts.io);
            });

            let mut send_err = match c_req.try_send_request(request).await {
                Ok(resp) => break (resp, body_drained),
                Err(e) => e,
            };

            // `TrySendError` 只实现 `Debug`（它可能携带请求体，不便实现 `Display`）。
            let send_err_text = format!("{send_err:?}");
            match send_err.take_message() {
                // 请求体被交还 = 一个字节都没发出去，重发是安全的。
                Some(req) if reused => {
                    event!(
                        Level::DEBUG,
                        "Reused upstream connection was already closed by the peer; \
                         retrying once on a fresh connection: {send_err_text}"
                    );
                    request = req;
                }
                Some(_) => {
                    tracing::error!("Send request error: {send_err_text}");
                    return Err(anyhow::anyhow!("Failed to send request: {send_err_text}"));
                }
                None => {
                    tracing::error!("Send request error: {send_err_text}");
                    return Err(anyhow::anyhow!("Failed to send request: {send_err_text}"));
                }
            }
        };

        let (mut parts, b) = resp.into_parts();
        parts.version = origin_version;
        let final_resp = Response::from_parts(
            parts,
            CResponse::Incoming(
                StatisticsIncoming::new(
                    state.id,
                    b,
                    crate::transport::StatisticsIncomingType::Response,
                )
                .with_body_drained_flag(body_drained),
            ),
        );
        Ok(final_resp)
    }

    // -------- WebSocket 升级处理 --------
    async fn handle_upgrade(
        req: Request<StatisticsIncoming>,
        state: ClientState,
        pool: Arc<UpstreamConnectionPool>, // 不再使用池
    ) -> anyhow::Result<hyper::Response<CResponse>> {
        // 1. 从客户端请求中取出 OnUpgrade
        let (mut parts, body) = req.into_parts();
        let client_on_upgrade = parts
            .extensions
            .remove::<upgrade::OnUpgrade>()
            .context("Missing OnUpgrade extension")?;
        // 保留原始头部和版本
        let original_headers = parts.headers.clone();
        let original_version = parts.version;
        let original_method = parts.method.clone();
        let original_uri = parts.uri.clone();

        // 2. 获取后端地址，新建连接（不从池中取）
        // 假设 state.website 有 get_addr() 返回 SocketAddr
        let stream = pool.create_connection().await?;
        let io = TokioIo::new(stream);

        // 3. 与后端握手
        let (mut c_req, connection) = client::conn::http1::Builder::new()
            .handshake(io)
            .await
            .context("Failed to handshake upstream")?;

        tokio::task::spawn(async move {
            match timeout(Duration::from_secs(120), connection.with_upgrades()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => event!(Level::ERROR, "Upstream connection error: {e}"),
                Err(_) => event!(Level::WARN, "Upstream connection timeout, force closing"),
            }
        });

        // 4. 构造转发请求（路径拼接与普通转发保持一致，见 P1-5）
        let origin_path = original_uri.path();
        let base = pool
            .get_path()
            .map(|v| v.path().trim_end_matches('/'))
            .filter(|v| !v.is_empty() && *v != "/");
        let path = match base {
            Some(base) => {
                if origin_path.is_empty() {
                    format!("{base}/")
                } else {
                    format!("{base}{origin_path}")
                }
            }
            None => origin_path.to_owned(),
        };
        let query = original_uri.query().unwrap_or_default();
        let new_uri = if query.is_empty() {
            path
        } else {
            format!("{}?{}", path, query)
        };

        // 注意：body 是 StatisticsIncoming，需要转换为 Incoming
        let incoming_body = body; // 假设有 into_inner
        let mut forward_req = Request::builder()
            .method(original_method)
            .version(original_version) // 保留原始版本
            .uri(new_uri)
            .body(incoming_body)
            .unwrap();

        // 复制原始头部
        *forward_req.headers_mut() = original_headers;
        // 添加/覆盖代理头
        forward_req
            .headers_mut()
            .insert("Host", state.host.parse()?);
        forward_req
            .headers_mut()
            .insert("X-Real-Ip", state.remote_addr().to_string().parse()?);
        forward_req
            .headers_mut()
            .insert("X-Forwarded-For", state.remote_addr().to_string().parse()?);
        forward_req
            .headers_mut()
            .insert("X-Forwarded-Proto", state.scheme().to_string().parse()?);
        forward_req
            .headers_mut()
            .insert("X-Forwarded-Host", state.host.parse()?);

        // 5. 发送请求
        let backend_resp = c_req
            .send_request(forward_req)
            .await
            .context("Failed to send request to backend")?;

        let (final_backend_resp_parts, final_backend_resp_body) = backend_resp.into_parts();
        let final_backend_resp = Response::from_parts(
            final_backend_resp_parts,
            CResponse::Incoming(StatisticsIncoming::new(
                state.id,
                final_backend_resp_body,
                crate::transport::StatisticsIncomingType::Response,
            )),
        );

        // 6. 判断状态
        if final_backend_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
            let (mut backend_parts, backend_body) = final_backend_resp.into_parts();
            let backend_on_upgrade = backend_parts
                .extensions
                .remove::<upgrade::OnUpgrade>()
                .context("Backend did not provide OnUpgrade")?;

            // 构建客户端 101 响应
            let mut client_resp = Response::builder()
                .status(StatusCode::SWITCHING_PROTOCOLS)
                .version(original_version)
                .body(backend_body)?;
            // 复制升级相关头
            for (k, v) in backend_parts.headers.iter() {
                if k.as_str().eq_ignore_ascii_case("upgrade")
                    || k.as_str().eq_ignore_ascii_case("connection")
                    || k.as_str().starts_with("sec-websocket-")
                {
                    client_resp.headers_mut().insert(k.clone(), v.clone());
                }
            }
            client_resp
                .extensions_mut()
                .insert(client_on_upgrade.clone());

            // 桥接
            tokio::spawn(async move {
                match tokio::try_join!(client_on_upgrade, backend_on_upgrade) {
                    Ok((client_upgraded, backend_upgraded)) => {
                        let mut client_io = TokioIo::new(client_upgraded);
                        let mut backend_io = TokioIo::new(backend_upgraded);
                        let _ = copy_bidirectional(&mut client_io, &mut backend_io).await;
                    }
                    Err(e) => tracing::warn!("WebSocket upgrade failed: {}", e),
                }
            });

            Ok(client_resp)
        } else {
            Ok(final_backend_resp)
            // anyhow::bail!("Backend responded with {}", final_backend_resp.status());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::strip_port;

    #[test]
    fn strips_port_from_host_header() {
        assert_eq!(strip_port("example.com"), "example.com");
        assert_eq!(strip_port("example.com:8080"), "example.com");
        assert_eq!(strip_port("example.com:80"), "example.com");
        assert_eq!(strip_port("[::1]:8080"), "[::1]");
        assert_eq!(strip_port("[::1]"), "[::1]");
        assert_eq!(strip_port("127.0.0.1:18081"), "127.0.0.1");
        // 没有端口、结尾是冒号等异常输入不应 panic。
        assert_eq!(strip_port(""), "");
        assert_eq!(strip_port(":8080"), ":8080");
    }
}
