//! WebSocket upgrade proxying and HTTP CONNECT tunnels.

use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use http::{
    HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, Uri, Version, header,
};
use http_body_util::{BodyExt, Empty, Full};
use hyper::{
    body::{Bytes, Incoming},
    upgrade::OnUpgrade,
};
use hyper_util::rt::TokioIo;
use tokio::{io::copy_bidirectional, net::TcpStream};
use tracing::{Level, event};

use crate::{
    access,
    proxy::{self, ProxyBody},
    state::{AppState, Backend, MatchContext},
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A plain TCP or TLS-wrapped upstream stream. WebSocket upgrades need the raw
/// stream so hyper can hand it to the 101 upgrade bridge after the handshake.
trait WebsocketUpstream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> WebsocketUpstream for T {}

type BoxedUpstreamStream = Box<dyn WebsocketUpstream>;

pub fn is_websocket(request: &Request<Incoming>) -> bool {
    is_websocket_headers(request.headers())
}

fn is_websocket_headers(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

pub async fn handle_websocket(
    state: Arc<AppState>,
    mut request: Request<Incoming>,
    peer: SocketAddr,
    scheme: &'static str,
    proxy_source: Option<SocketAddr>,
) -> Response<ProxyBody> {
    crate::metrics::record_websocket_request();
    let raw_host = proxy::request_host(&request);
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let user_agent = request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let version = request.version();
    let mut request_log = access::RequestLog {
        id: access::next_id(),
        host: raw_host.clone(),
        method: method.as_str().to_string(),
        path: path.clone(),
        headers: access::headers_json(request.headers()),
        http_version: access::version_label(version),
        remote_addr: peer.ip().to_string(),
        body_length: 0,
        website_id: None,
    };

    let Some(site) = state
        .routing
        .read()
        .expect("routing lock poisoned")
        .lookup(&raw_host)
    else {
        log_failure(&state, &request_log, StatusCode::NOT_FOUND, 9);
        return proxy::text_response(StatusCode::NOT_FOUND, "Not Found", &state);
    };

    let client_ip = site
        .client_ip
        .resolve(peer, proxy_source, request.headers());
    request_log.remote_addr = client_ip.to_string();
    request_log.website_id = Some(site.id.clone());

    if let Some(response) = proxy::rate_limit_response(&state, &request_log, client_ip, version) {
        return response;
    }

    let context = MatchContext {
        path: &path,
        query: query.as_deref(),
        method: method.as_str(),
        user_agent: user_agent.as_deref(),
    };
    let Some(backend) = site.next_backend(&context).cloned() else {
        log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
        return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
    };

    let Some((host, port)) = backend_addr(&backend) else {
        log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
        return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
    };

    let client_upgrade = hyper::upgrade::on(&mut request);
    let mut headers = request.headers().clone();
    if !raw_host.is_empty()
        && let Ok(value) = HeaderValue::from_str(&raw_host)
    {
        headers.insert(header::HOST, value);
    }
    site.headers
        .apply(&mut headers, &raw_host, scheme, client_ip);

    let tcp = match tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            event!(Level::WARN, %error, backend = %backend.url, "failed to connect to websocket upstream");
            log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
            return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
        Err(_) => {
            event!(Level::WARN, backend = %backend.url, "websocket upstream connect timed out");
            log_failure(&state, &request_log, StatusCode::GATEWAY_TIMEOUT, 15);
            return proxy::text_response(StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout", &state);
        }
    };

    // WebSocket upstreams may be https://; the raw TCP path above must be
    // wrapped in TLS before the HTTP/1.1 upgrade request is written.
    let stream: BoxedUpstreamStream = if backend.url.scheme() == "https" {
        let server_name = match rustls::pki_types::ServerName::try_from(host.clone()) {
            Ok(server_name) => server_name,
            Err(error) => {
                event!(Level::WARN, %error, backend = %backend.url, "invalid websocket upstream server name");
                log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
                return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
            }
        };
        let connector = tokio_rustls::TlsConnector::from(state.websocket_tls.clone());
        match tokio::time::timeout(Duration::from_secs(10), connector.connect(server_name, tcp))
            .await
        {
            Ok(Ok(stream)) => Box::new(stream),
            Ok(Err(error)) => {
                event!(Level::WARN, %error, backend = %backend.url, "websocket upstream TLS handshake failed");
                log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
                return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
            }
            Err(_) => {
                event!(Level::WARN, backend = %backend.url, "websocket upstream TLS handshake timed out");
                log_failure(&state, &request_log, StatusCode::GATEWAY_TIMEOUT, 15);
                return proxy::text_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Gateway Timeout",
                    &state,
                );
            }
        }
    } else {
        Box::new(tcp)
    };

    let (mut sender, connection) = match hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(
        TokioIo::new(stream),
    )
    .await
    {
        Ok(pair) => pair,
        Err(error) => {
            event!(Level::WARN, %error, backend = %backend.url, "websocket upstream handshake failed");
            log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
            return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            event!(Level::DEBUG, %error, "websocket upstream connection closed");
        }
    });

    let uri = match backend_request_target(&backend, request.uri()).parse::<Uri>() {
        Ok(uri) => uri,
        Err(error) => {
            event!(Level::WARN, %error, backend = %backend.url, "failed to build the websocket upstream target");
            log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
            return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };

    let mut upstream_request = match Request::builder()
        .method(method)
        .uri(uri)
        .version(Version::HTTP_11)
        .body(Empty::<Bytes>::new())
    {
        Ok(request) => request,
        Err(error) => {
            event!(Level::WARN, %error, "failed to build the websocket upstream request");
            log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
            return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };
    *upstream_request.headers_mut() = headers;

    let mut upstream_response = match sender.send_request(upstream_request).await {
        Ok(response) => response,
        Err(error) => {
            event!(Level::WARN, %error, backend = %backend.url, "websocket upstream request failed");
            log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
            return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };

    let status = upstream_response.status();
    let response_version = upstream_response.version();

    if status != StatusCode::SWITCHING_PROTOCOLS {
        let mut response_headers = upstream_response.headers().clone();
        proxy::log_event(
            &state,
            &request_log,
            status,
            &response_headers,
            Some(access::content_length(&response_headers)),
            response_version,
        );
        response_headers.insert(header::SERVER, HeaderValue::from_static("WebGateway"));
        if let Ok(value) = HeaderValue::from_str(&state.node.header_label()) {
            response_headers.insert(HeaderName::from_static("x-webgateway-node"), value);
        }
        let body = upstream_response
            .into_body()
            .map_err(|error| -> BoxError { Box::new(error) })
            .boxed();

        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.version_mut() = response_version;
        *response.headers_mut() = response_headers;
        return response;
    }

    let upstream_upgrade = hyper::upgrade::on(&mut upstream_response);
    let mut response_headers = upstream_response.headers().clone();
    response_headers.insert(header::SERVER, HeaderValue::from_static("WebGateway"));
    if let Ok(value) = HeaderValue::from_str(&state.node.header_label()) {
        response_headers.insert(HeaderName::from_static("x-webgateway-node"), value);
    }
    proxy::log_event(
        &state,
        &request_log,
        status,
        &response_headers,
        None,
        Version::HTTP_11,
    );

    spawn_upgrade_bridge(client_upgrade, upstream_upgrade, sender);

    let mut response = Response::new(empty_body());
    *response.status_mut() = status;
    *response.version_mut() = Version::HTTP_11;
    *response.headers_mut() = response_headers;
    response
}

pub async fn handle_connect(
    state: Arc<AppState>,
    mut request: Request<Incoming>,
    peer: SocketAddr,
    proxy_source: Option<SocketAddr>,
) -> Response<ProxyBody> {
    crate::metrics::record_connect_request();
    let authority = request
        .uri()
        .authority()
        .map(|authority| authority.to_string())
        .or_else(|| {
            request
                .headers()
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        });

    let log_target = authority
        .clone()
        .unwrap_or_else(|| proxy::request_host(&request));
    let mut request_log = access::RequestLog {
        id: access::next_id(),
        host: log_target.clone(),
        method: "CONNECT".to_string(),
        path: log_target,
        headers: access::headers_json(request.headers()),
        http_version: access::version_label(request.version()),
        remote_addr: peer.ip().to_string(),
        body_length: 0,
        website_id: None,
    };

    let Some(authority) = authority else {
        log_failure(&state, &request_log, StatusCode::BAD_REQUEST, 11);
        return proxy::text_response(StatusCode::BAD_REQUEST, "Bad Request", &state);
    };
    let Some((host, port)) = split_authority(&authority) else {
        log_failure(&state, &request_log, StatusCode::BAD_REQUEST, 11);
        return proxy::text_response(StatusCode::BAD_REQUEST, "Bad Request", &state);
    };

    if !state.connect_ports.contains(&port) {
        event!(
            Level::WARN,
            authority = %authority,
            port,
            "CONNECT target port is not allowed"
        );
        log_failure(&state, &request_log, StatusCode::FORBIDDEN, 9);
        return proxy::text_response(StatusCode::FORBIDDEN, "Forbidden", &state);
    }

    let Some(site) = state
        .routing
        .read()
        .expect("routing lock poisoned")
        .lookup(&host)
    else {
        log_failure(&state, &request_log, StatusCode::FORBIDDEN, 9);
        return proxy::text_response(StatusCode::FORBIDDEN, "Forbidden", &state);
    };

    let client_ip = site
        .client_ip
        .resolve(peer, proxy_source, request.headers());
    request_log.remote_addr = client_ip.to_string();
    request_log.website_id = Some(site.id.clone());

    if let Some(response) =
        proxy::rate_limit_response(&state, &request_log, client_ip, request.version())
    {
        return response;
    }

    let client_upgrade = hyper::upgrade::on(&mut request);

    let stream = match tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            event!(Level::WARN, %error, authority = %authority, "CONNECT upstream failed");
            log_failure(&state, &request_log, StatusCode::BAD_GATEWAY, 11);
            return proxy::text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
        Err(_) => {
            event!(Level::WARN, authority = %authority, "CONNECT upstream timed out");
            log_failure(&state, &request_log, StatusCode::GATEWAY_TIMEOUT, 15);
            return proxy::text_response(StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout", &state);
        }
    };

    proxy::log_event(
        &state,
        &request_log,
        StatusCode::OK,
        &HeaderMap::new(),
        None,
        Version::HTTP_11,
    );

    spawn_stream_bridge(client_upgrade, stream);

    Response::builder()
        .status(StatusCode::OK)
        .body(empty_body())
        .expect("empty CONNECT response")
}

/// Join the backend base path with the client request target. The client path
/// is preserved verbatim; only the base path is prepended.
fn backend_request_target(backend: &Backend, request_uri: &Uri) -> String {
    let base = backend.url.path().trim_end_matches('/');
    let path_and_query = request_uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    format!("{base}{path_and_query}")
}

fn backend_addr(backend: &Backend) -> Option<(String, u16)> {
    let host = backend.url.host_str()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some((
        host.to_string(),
        backend.url.port_or_known_default().unwrap_or(80),
    ))
}

fn split_authority(authority: &str) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        let port = rest
            .strip_prefix(':')
            .and_then(|port| port.parse().ok())
            .unwrap_or(443);
        return Some((host.to_string(), port));
    }

    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Some((host.to_string(), port.parse().ok()?)),
        _ => Some((authority.to_string(), 443)),
    }
}

fn log_failure(
    state: &AppState,
    request: &access::RequestLog,
    status: StatusCode,
    body_length: u64,
) {
    proxy::log_event(
        state,
        request,
        status,
        &HeaderMap::new(),
        Some(body_length),
        Version::HTTP_11,
    );
}

fn empty_body() -> ProxyBody {
    Full::new(Bytes::new())
        .map_err(|never: Infallible| -> BoxError { match never {} })
        .boxed()
}

fn spawn_upgrade_bridge(
    client: OnUpgrade,
    upstream: OnUpgrade,
    sender: hyper::client::conn::http1::SendRequest<Empty<Bytes>>,
) {
    tokio::spawn(async move {
        // Keep the request channel alive until the upgraded tunnel ends.
        let _sender = sender;
        match tokio::try_join!(client, upstream) {
            Ok((client, upstream)) => {
                relay(TokioIo::new(client), TokioIo::new(upstream)).await;
            }
            Err(error) => event!(Level::DEBUG, %error, "websocket upgrade failed"),
        }
    });
}

fn spawn_stream_bridge(client: OnUpgrade, upstream: TcpStream) {
    tokio::spawn(async move {
        match client.await {
            Ok(client) => relay(TokioIo::new(client), upstream).await,
            Err(error) => event!(Level::DEBUG, %error, "CONNECT upgrade failed"),
        }
    });
}

async fn relay<A, B>(mut client: A, mut upstream: B)
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if let Err(error) = copy_bidirectional(&mut client, &mut upstream).await {
        event!(Level::DEBUG, %error, "upgraded connection closed with an error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_websocket_upgrade_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        assert!(is_websocket_headers(&headers));

        headers.insert(header::UPGRADE, HeaderValue::from_static("h2c"));
        assert!(!is_websocket_headers(&headers));
    }

    fn backend(url: &str) -> Backend {
        Backend {
            url: url::Url::parse(url).unwrap(),
            match_path: None,
            rules: crate::state::MatchRules::default(),
            weight: 1,
            priority: 0,
        }
    }

    #[test]
    fn joins_the_backend_base_path_for_websockets() {
        let uri: Uri = "/socket.io/?EIO=4&transport=websocket".parse().unwrap();
        assert_eq!(
            backend_request_target(&backend("https://127.0.0.1:24444/mc"), &uri),
            "/mc/socket.io/?EIO=4&transport=websocket"
        );
        assert_eq!(
            backend_request_target(&backend("https://127.0.0.1:24444"), &uri),
            "/socket.io/?EIO=4&transport=websocket"
        );
        assert_eq!(
            backend_request_target(&backend("https://127.0.0.1:24444/"), &uri),
            "/socket.io/?EIO=4&transport=websocket"
        );
    }

    #[test]
    fn splits_connect_authorities() {
        assert_eq!(
            split_authority("example.com:8443"),
            Some(("example.com".to_string(), 8443))
        );
        assert_eq!(
            split_authority("example.com"),
            Some(("example.com".to_string(), 443))
        );
        assert_eq!(
            split_authority("[::1]:9500"),
            Some(("::1".to_string(), 9500))
        );
    }
}
