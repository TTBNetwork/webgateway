use std::{
    net::SocketAddr,
    sync::{Arc, LazyLock},
    time::Duration,
};

use dashmap::DashMap;
use http_body::Body;
use hyper::{Request, Response, StatusCode, Version, body::Incoming, client, service::service_fn};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
};
use shared::{
    database::get_database,
    listener::CustomDualStackTcpListener,
    objectid::ObjectId,
    streams::{BufferStream, WrapperBufferStream},
};
use tokio::{net::TcpStream, task::JoinHandle, time::timeout};
use tokio_rustls::TlsAcceptor;
use tracing::{Level, event};

use crate::{
    access::{self, RequestContext, RequestLog, ResponseLog},
    state::{BaseClientState, ClientState},
    sync::{SERVER_CONFIG, websites::get_website},
    transport::{CResponse, CResponseResult, StatisticsIncoming},
};
pub mod backends;
pub mod protocols;

static HTTP_BUILDER: LazyLock<Builder<TokioExecutor>> = LazyLock::new(|| {
    hyper_util::server::conn::auto::Builder::<TokioExecutor>::new(TokioExecutor::new())
});

static LISTENERS: LazyLock<DashMap<u16, JoinHandle<()>>> = LazyLock::new(DashMap::default);

static TLS_ACCEPTOR: LazyLock<Arc<TlsAcceptor>> =
    LazyLock::new(|| Arc::new(TlsAcceptor::from(SERVER_CONFIG.clone())));

async fn accept(listener: CustomDualStackTcpListener) {
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => {
                continue;
            }
        };
        tokio::spawn(async move {
            let _ = handle_connection(stream, addr).await;
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

async fn handle_connection(stream: TcpStream, addr: SocketAddr) -> anyhow::Result<()> {
    let local_addr = stream.local_addr()?;
    event!(Level::INFO, "Connection from {}", addr);
    let stream = BufferStream::new(WrapperBufferStream::Raw(stream));
    let (stream, _) = protocols::get_proxy_protocol(stream).await?;
    // event!(Level::INFO, "Proxy protocol: {:?}", proxy_protocol);
    let (stream, tls) = protocols::get_tls_sni(stream).await?;
    let final_stream = match &tls {
        Some(_) => {
            let s = TLS_ACCEPTOR.accept(stream).await?;
            BufferStream::new(WrapperBufferStream::TlsServerBufferStream(Box::new(s)))
        }
        None => stream,
    };
    // if is proxyprotocol
    let state = Arc::new(BaseClientState {
        tls,
        remote_addr: addr.ip(),
        local_addr: local_addr.ip(),
    });
    let io = TokioIo::new(final_stream);
    let _ = HTTP_BUILDER
        .serve_connection(
            io,
            service_fn(move |req: Request<Incoming>| {
                let state = state.clone();
                let req_id = ObjectId::new();
                let host = req
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok().map(|v| v.to_string()))
                    .unwrap_or_else(|| req.uri().host().map(|v| v.to_string()).unwrap_or_default());
                let (parts, body) = req.into_parts();
                let req = Request::from_parts(
                    parts,
                    StatisticsIncoming::new(
                        req_id,
                        body,
                        crate::transport::StatisticsIncomingType::Request,
                    ),
                );
                handle(req, state, host, req_id)
            }),
        )
        .await;
    Ok(())
}

pub async fn handle(
    req: Request<StatisticsIncoming>,
    base_state: Arc<BaseClientState>,
    host: String,
    req_id: ObjectId,
) -> anyhow::Result<hyper::Response<CResponse>> {
    let site = get_website(&host).await;
    let website_id = site.as_ref().map(|v| v.inner().id);
    let req_log = RequestLog::new(RequestContext {
        req_id,
        host: host.clone(),
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
            let site = get_website(&host).await;
            match site {
                Some(site) => {
                    let state = ClientState {
                        base: base_state,
                        website: site.clone(),
                        host,
                        id: req_id,
                    };
                    let resp = wrapper_inner_handle(req, state).await;
                    match resp {
                        CResponseResult::Backend(resp) => {
                            access::add_response_log(
                                &ResponseLog::new(
                                    req_id,
                                    resp.version(),
                                    resp.headers(),
                                    resp.status().as_u16(),
                                    resp.body().size_hint(),
                                    Some(get_database().get_database_time().unwrap()),
                                    website_id,
                                )
                                .unwrap(),
                            );
                            return Ok(resp);
                        }
                        resp => resp,
                    }
                }
                None => CResponseResult::NotFoundGateway,
            }
        }
        Err(_) => CResponseResult::BadRequest,
    };

    // let resp = wrapper_inner_handle(req, base_state, host, &req_id).await;

    let final_resp = match resp {
        CResponseResult::NotFoundGateway => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(CResponse::new_from_string("Not Found"))
            .unwrap(),
        CResponseResult::GatewayError(e) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(CResponse::new_from_string(e.to_string()))
            .unwrap(),
        CResponseResult::Timeout => Response::builder()
            .status(StatusCode::REQUEST_TIMEOUT)
            .body(CResponse::new_from_string("Request Timeout"))
            .unwrap(),
        CResponseResult::BadRequest => Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(CResponse::new_from_string("Bad Request"))
            .unwrap(),
        CResponseResult::Backend(_) => unreachable!(),
    };
    access::add_response_log(
        &ResponseLog::new(
            req_id,
            final_resp.version(),
            final_resp.headers(),
            final_resp.status().as_u16(),
            final_resp.size_hint(),
            None,
            website_id,
        )
        .unwrap(),
    );
    Ok(final_resp)
}

async fn wrapper_inner_handle(
    req: Request<StatisticsIncoming>,
    state: ClientState,
) -> CResponseResult {
    let resp = timeout(Duration::from_secs(60), inner_handle(req, state)).await;
    match resp {
        Ok(v) => match v {
            Ok(v) => CResponseResult::Backend(v),
            Err(e) => CResponseResult::GatewayError(e),
        },
        Err(_) => CResponseResult::Timeout,
    }
}

async fn inner_handle(
    origin_req: Request<StatisticsIncoming>,
    state: ClientState,
) -> anyhow::Result<hyper::Response<CResponse>> {
    let site = &state.website;

    let pool = site.pool();
    let conn = pool.get().await?;
    let io = TokioIo::new(conn);
    let (mut c_req, connection) = client::conn::http1::handshake(io).await?;
    tokio::task::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("Connection error: {}", err);
        }
    });
    let origin_version = origin_req.version();
    let mut req = Request::builder()
        .method(origin_req.method())
        .version(Version::HTTP_11);
    if let Some(v) = req.headers_mut() {
        v.extend(origin_req.headers().clone())
    }
    if let Some(v) = req.extensions_mut() {
        v.extend(origin_req.extensions().clone())
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

    // insert custom headers
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

    let mut resp = c_req.send_request(final_req).await?;
    resp.headers_mut().insert("Server", "WebGateway".parse()?);
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
