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
) -> Response<ProxyBody> {
    let raw_host = request_host(&request);
    let path = request.uri().path().to_string();

    let Some(site) = state
        .routing
        .read()
        .expect("routing lock poisoned")
        .lookup(&raw_host)
    else {
        return text_response(StatusCode::NOT_FOUND, "Not Found", &state);
    };

    let Some(backend) = site.next_backend(&path) else {
        return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
    };

    let upstream_uri = match build_upstream_uri(backend, request.uri()) {
        Ok(uri) => uri,
        Err(error) => {
            event!(Level::ERROR, %error, "failed to build the upstream uri");
            return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };

    let mut headers = request.headers().clone();
    remove_hop_by_hop(&mut headers);

    // Keep the original Host unless the site proxy headers override it.
    if !raw_host.is_empty()
        && let Ok(value) = HeaderValue::from_str(&raw_host)
    {
        headers.insert(header::HOST, value);
    }
    site.headers.apply(&mut headers, &raw_host, scheme, peer);

    let method = request.method().clone();
    let body = request.into_body();

    let mut upstream_request = match Request::builder()
        .method(method)
        .uri(upstream_uri)
        .version(Version::HTTP_11)
        .body(body)
    {
        Ok(request) => request,
        Err(error) => {
            event!(Level::ERROR, %error, "failed to build the upstream request");
            return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
    };
    *upstream_request.headers_mut() = headers;

    let upstream_response = match tokio::time::timeout(
        Duration::from_secs(60),
        state.client.request(upstream_request),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            event!(Level::WARN, %error, "upstream request failed");
            return text_response(StatusCode::BAD_GATEWAY, "Bad Gateway", &state);
        }
        Err(_) => {
            event!(Level::WARN, "upstream request timed out");
            return text_response(StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout", &state);
        }
    };

    let status = upstream_response.status();
    let version = upstream_response.version();

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
        }
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
