use std::{
    error::Error,
    net::SocketAddr,
    sync::{Arc, LazyLock},
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

pub async fn listen(port: u16) -> anyhow::Result<()> {
    if LISTENERS.contains_key(&port) {
        return Ok(());
    }
    let thread = tokio::spawn(async move {
        let listener = CustomDualStackTcpListener::new_by_port(port).await.unwrap();
        event!(
            Level::INFO,
            "Listening on {:?}",
            listener.local_addrs().unwrap()
        );
        accept(listener).await;
    });

    LISTENERS.insert(port, thread);
    Ok(())
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
        let site = get_website(
            connection_req.host.as_str(),
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

        // ---- 普通 HTTP 转发（原有逻辑） ----
        // 获取连接（从池中取出）
        let pooled = pool
            .get()
            .await
            .with_context(|| "Unavailable connection from pool")?;
        let conn = pooled; //.conn.ok_or_else(|| anyhow::anyhow!("No connection"))?;

        let io = TokioIo::new(conn);
        let (mut c_req, connection) = client::conn::http1::Builder::new()
            .handshake(io)
            .await
            .with_context(|| "Failed to handshake upstream")?;

        tokio::task::spawn(async move {
            match timeout(Duration::from_secs(120), connection.with_upgrades()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => event!(Level::ERROR, "Upstream connection error: {e}"),
                Err(_) => event!(Level::WARN, "Upstream connection timeout, force closing"),
            }
            // println!("done");
        });

        let origin_version = origin_req.version();
        let mut req = Request::builder()
            .method(origin_req.method())
            .version(Version::HTTP_11);
        if let Some(v) = req.headers_mut() {
            v.extend(origin_req.headers().clone());
        }
        if let Some(v) = req.extensions_mut() {
            v.extend(origin_req.extensions().clone());
        }
        req = req.uri({
            let current_uri = pool.get_path().map_or_else(
                || origin_req.uri().path().to_string(),
                |v| {
                    let a = v.join(&origin_req.uri().path()[1..]).unwrap();
                    a.path().to_string()
                },
            );
            if let Some(query) = origin_req.uri().query() {
                format!("{}?{}", current_uri, query)
            } else {
                current_uri
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
        let final_req = req.body(origin_req.into_body()).unwrap();

        let resp = match c_req.send_request(final_req).await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::error!("Send request error: {:?}, source: {:?}", e, e.source());
                return Err(anyhow::anyhow!("Failed to send request: {}", e));
            }
        };

        let (mut parts, b) = resp.into_parts();
        parts.version = origin_version;
        let final_resp = Response::from_parts(
            parts,
            CResponse::Incoming(StatisticsIncoming::new(
                state.id,
                b,
                crate::transport::StatisticsIncomingType::Response,
            )),
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

        // 4. 构造转发请求
        let path = pool.get_path().map_or_else(
            || original_uri.path().to_string(),
            |v| {
                v.join(&original_uri.path()[1..])
                    .unwrap()
                    .path()
                    .to_string()
            },
        );
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
