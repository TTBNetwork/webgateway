//! HTTP reverse proxy.

use std::{convert::Infallible, error::Error, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, Uri, Version, header,
};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use tracing::{Level, event};

use crate::state::{AppState, Backend};

pub type ProxyBody = BoxBody<Bytes, Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub async fn handle(
    state: Arc<AppState>,
    request: Request<Incoming>,
    peer: SocketAddr,
    scheme: &'static str,
    proxy_source: Option<SocketAddr>,
) -> Response<ProxyBody> {
    let raw_host = request_host(&request);
    let path = request.uri().path().to_string();
    let method_label = request.method().as_str().to_string();
    let query = request.uri().query().map(str::to_string);
    let user_agent = request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let grpc = is_grpc(request.headers());
    let request_version = request.version();
    let mut request_log = crate::access::RequestLog {
        id: crate::access::next_id(),
        host: raw_host.clone(),
        method: method_label.clone(),
        path: path.clone(),
        headers: crate::access::headers_json(request.headers()),
        http_version: crate::access::version_label(request_version),
        remote_addr: peer.ip().to_string(),
        body_length: crate::access::content_length(request.headers()),
        website_id: None,
    };

    let Some(site) = state
        .routing
        .read()
        .expect("routing lock poisoned")
        .lookup(&raw_host)
    else {
        log_event(
            &state,
            &request_log,
            StatusCode::NOT_FOUND,
            &HeaderMap::new(),
            Some(9),
            request_version,
        );
        return text_response(StatusCode::NOT_FOUND, "Not Found", &state);
    };

    let client_ip = site
        .client_ip
        .resolve(peer, proxy_source, request.headers());
    request_log.remote_addr = client_ip.to_string();
    request_log.website_id = Some(site.id.clone());

    let match_context = crate::state::MatchContext {
        path: &path,
        query: query.as_deref(),
        method: &method_label,
        user_agent: user_agent.as_deref(),
    };
    let Some(backend) = site.next_backend(&match_context) else {
        log_event(
            &state,
            &request_log,
            StatusCode::BAD_GATEWAY,
            &HeaderMap::new(),
            Some(11),
            request_version,
        );
        return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
    };

    let upstream_uri = match build_upstream_uri(backend, request.uri()) {
        Ok(uri) => uri,
        Err(error) => {
            event!(Level::ERROR, %error, "failed to build the upstream uri");
            log_event(
                &state,
                &request_log,
                StatusCode::BAD_GATEWAY,
                &HeaderMap::new(),
                Some(11),
                request_version,
            );
            return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };

    let mut headers = request.headers().clone();
    remove_hop_by_hop(&mut headers);
    if grpc {
        // gRPC requires TE: trailers, but TE is on the hop-by-hop list.
        headers.insert(header::TE, HeaderValue::from_static("trailers"));
    }

    // Keep the original Host unless the site proxy headers override it.
    if !raw_host.is_empty()
        && let Ok(value) = HeaderValue::from_str(&raw_host)
    {
        headers.insert(header::HOST, value);
    }
    site.headers
        .apply(&mut headers, &raw_host, scheme, client_ip);

    let method = request.method().clone();
    let body = request.into_body();

    let mut upstream_request = match Request::builder()
        .method(method)
        .uri(upstream_uri)
        .version(if grpc {
            Version::HTTP_2
        } else {
            Version::HTTP_11
        })
        .body(body)
    {
        Ok(request) => request,
        Err(error) => {
            event!(Level::ERROR, %error, "failed to build the upstream request");
            log_event(
                &state,
                &request_log,
                StatusCode::BAD_GATEWAY,
                &HeaderMap::new(),
                Some(11),
                request_version,
            );
            return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };
    *upstream_request.headers_mut() = headers;

    let upstream_response = if grpc {
        // gRPC streams can be long-lived; do not apply the HTTP timeout here.
        match state.grpc_client.request(upstream_request).await {
            Ok(response) => response,
            Err(error) => {
                event!(Level::WARN, %error, "gRPC upstream request failed");
                log_event(
                    &state,
                    &request_log,
                    StatusCode::BAD_GATEWAY,
                    &HeaderMap::new(),
                    Some(11),
                    request_version,
                );
                return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
            }
        }
    } else {
        match tokio::time::timeout(
            Duration::from_secs(60),
            state.client.request(upstream_request),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                event!(Level::WARN, %error, "upstream request failed");
                log_event(
                    &state,
                    &request_log,
                    StatusCode::BAD_GATEWAY,
                    &HeaderMap::new(),
                    Some(11),
                    request_version,
                );
                return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
            }
            Err(_) => {
                event!(Level::WARN, "upstream request timed out");
                log_event(
                    &state,
                    &request_log,
                    StatusCode::GATEWAY_TIMEOUT,
                    &HeaderMap::new(),
                    Some(15),
                    request_version,
                );
                return text_response(StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout", &state);
            }
        }
    };

    let status = upstream_response.status();
    let version = upstream_response.version();
    let response_length = upstream_response
        .headers()
        .get(header::CONTENT_LENGTH)
        .map(|_| crate::access::content_length(upstream_response.headers()));
    log_event(
        &state,
        &request_log,
        status,
        upstream_response.headers(),
        response_length,
        version,
    );

    let mut headers = upstream_response.headers().clone();
    remove_hop_by_hop(&mut headers);
    headers.insert(header::SERVER, HeaderValue::from_static("WebGateway"));
    if let Ok(value) = HeaderValue::from_str(&state.node.header_label()) {
        headers.insert(HeaderName::from_static("x-webgateway-node"), value);
    }

    let body = upstream_response
        .into_body()
        .map_err(|error| -> BoxError { Box::new(error) })
        .boxed();

    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.version_mut() = version;
    *response.headers_mut() = headers;
    response
}

fn log_event(
    state: &AppState,
    request: &crate::access::RequestLog,
    status: StatusCode,
    headers: &HeaderMap,
    body_length: Option<u64>,
    version: http::Version,
) {
    let response = crate::access::ResponseLog {
        id: request.id.clone(),
        status: status.as_u16(),
        headers: crate::access::headers_json(headers),
        http_version: crate::access::version_label(version),
        body_length,
        website_id: request.website_id.clone(),
    };

    let event = crate::access::AccessEvent {
        request: request.clone(),
        response,
    };

    if let Err(error) = state.logs.try_send(event) {
        tracing::debug!(%error, "dropping access log event");
    }
}

fn is_grpc(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/grpc"))
}

pub(crate) fn request_host(request: &Request<Incoming>) -> String {
    request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            request
                .uri()
                .authority()
                .map(|authority| authority.as_str().to_string())
        })
        .unwrap_or_default()
}

pub(crate) fn build_upstream_uri(backend: &Backend, request_uri: &Uri) -> anyhow::Result<Uri> {
    let host = backend.url.host_str().context("backend url has no host")?;
    let host = if host.starts_with('[') {
        host.to_string()
    } else if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };

    let authority = match backend.url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };

    let base = backend.url.path().trim_end_matches('/');
    let path_and_query = request_uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");

    let uri = format!(
        "{}://{}{}{}",
        backend.url.scheme(),
        authority,
        base,
        path_and_query
    );
    uri.parse()
        .with_context(|| format!("invalid upstream uri: {uri}"))
}

fn remove_hop_by_hop(headers: &mut HeaderMap) {
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
}

fn text_response(
    status: StatusCode,
    message: &'static str,
    state: &AppState,
) -> Response<ProxyBody> {
    let body = Full::new(Bytes::from_static(message.as_bytes()))
        .map_err(|never: Infallible| -> BoxError { match never {} })
        .boxed();

    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::SERVER, HeaderValue::from_static("WebGateway"));
    if let Ok(value) = HeaderValue::from_str(&state.node.header_label()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-webgateway-node"), value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(url: &str) -> Backend {
        Backend {
            url: url::Url::parse(url).unwrap(),
            match_path: None,
            rules: crate::state::MatchRules::default(),
        }
    }

    #[test]
    fn detects_grpc_requests_by_content_type() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/grpc+proto"),
        );
        assert!(is_grpc(&headers));

        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        assert!(!is_grpc(&headers));
    }

    #[test]
    fn joins_backend_base_path_and_request_path() {
        let uri: Uri = "/hello?x=1".parse().unwrap();
        assert_eq!(
            build_upstream_uri(&backend("http://127.0.0.1:9000/base"), &uri)
                .unwrap()
                .to_string(),
            "http://127.0.0.1:9000/base/hello?x=1"
        );
    }

    #[test]
    fn keeps_the_request_query_string() {
        let uri: Uri = "/?a=b&c=d".parse().unwrap();
        assert_eq!(
            build_upstream_uri(&backend("http://127.0.0.1:9000"), &uri)
                .unwrap()
                .to_string(),
            "http://127.0.0.1:9000/?a=b&c=d"
        );
    }

    #[test]
    fn ipv6_backends_are_bracketed() {
        let uri: Uri = "/x".parse().unwrap();
        assert_eq!(
            build_upstream_uri(&backend("http://[::1]:9000"), &uri)
                .unwrap()
                .to_string(),
            "http://[::1]:9000/x"
        );
    }
}
