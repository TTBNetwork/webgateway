//! HTTP reverse proxy.

use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::Context;
use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, Uri, Version, header,
};
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use tracing::{Level, event};

use crate::access;
use crate::cache::{CacheConfig, CachedResponse};
use crate::state::{AppState, Backend, BoxError, GatewayBody, MatchContext};

pub type ProxyBody = GatewayBody;

/// A fully buffered request, used by the HTTP/3 path.
pub struct BufferedRequest {
    pub method: http::Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub peer: SocketAddr,
}

const RATE_LIMIT_BODY: &str = "Too Many Requests";

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
    crate::metrics::record_request();
    let _inflight = crate::metrics::InFlightGuard::new();
    if request.method() == http::Method::CONNECT {
        return crate::upgrade::handle_connect(state, request, peer, proxy_source).await;
    }
    if crate::upgrade::is_websocket(&request) {
        return crate::upgrade::handle_websocket(state, request, peer, scheme, proxy_source).await;
    }

    let raw_host = request_host(&request);
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
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

    if let Some(response) = rate_limit_response(&state, &request_log, client_ip, request_version) {
        return response;
    }

    if let Some(length) = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && length > state.max_request_body_bytes
    {
        event!(
            Level::WARN,
            length,
            limit = state.max_request_body_bytes,
            "request body exceeds the configured limit"
        );
        log_event(
            &state,
            &request_log,
            StatusCode::PAYLOAD_TOO_LARGE,
            &HeaderMap::new(),
            Some(15),
            request_version,
        );
        return text_response(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large", &state);
    }

    let cache_lookup_key = cache_key_for(&raw_host, request.uri());
    let cache_lookup = (method_label == "GET" || method_label == "HEAD")
        && site.cache.enabled
        && site.cache.ttl_secs > 0
        && !request.headers().contains_key(header::AUTHORIZATION)
        && !request.headers().contains_key(header::COOKIE)
        && request
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| !value.to_ascii_lowercase().contains("no-cache"));
    // Only GET responses are stored; HEAD requests are answered from them.
    let request_cacheable = cache_lookup && method_label == "GET";
    let cache_request_headers = request.headers().clone();

    if cache_lookup && let Some(cached) = state.cache.get(&cache_lookup_key, &cache_request_headers)
    {
        return cache_hit_response(
            &state,
            &request_log,
            cached,
            request.headers(),
            request_version,
            method_label == "HEAD",
        );
    }

    let match_context = crate::state::MatchContext {
        path: &path,
        query: query.as_deref(),
        method: &method_label,
        user_agent: user_agent.as_deref(),
    };
    let Some(backend) = site.next_backend(&match_context, Some(&state.backend_health)) else {
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
    let body_limit = usize::try_from(state.max_request_body_bytes).unwrap_or(usize::MAX);
    let body = Limited::new(request.into_body(), body_limit).boxed();

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
            Ok(response) => {
                crate::health::record_passive(&state, &site, backend, true);
                response
            }
            Err(error) => {
                crate::health::record_passive(&state, &site, backend, false);
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
            Ok(Ok(response)) => {
                crate::health::record_passive(&state, &site, backend, true);
                response
            }
            Ok(Err(error)) => {
                crate::health::record_passive(&state, &site, backend, false);
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
                crate::health::record_passive(&state, &site, backend, false);
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
    let mut response_headers = upstream_response.headers().clone();
    remove_hop_by_hop(&mut response_headers);
    response_headers.insert(header::SERVER, HeaderValue::from_static("WebGateway"));
    if let Ok(value) = HeaderValue::from_str(&state.node.header_label()) {
        response_headers.insert(HeaderName::from_static("x-webgateway-node"), value);
    }

    // Only buffer responses whose declared size fits the cache and the global
    // response limit. Chunked (unknown-length) responses stream uncached, which
    // also fixes the previous "collect first, check later" behaviour.
    let declared_length = response_headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let response_limit = site.cache.max_entry_size.min(state.max_response_body_bytes);
    let cacheable = request_cacheable
        && status == StatusCode::OK
        && response_cacheable(&response_headers, &site.cache)
        && declared_length.is_some_and(|length| length <= response_limit);

    if cacheable {
        let limit = usize::try_from(response_limit).unwrap_or(usize::MAX);
        let bytes = match Limited::new(upstream_response.into_body(), limit)
            .collect()
            .await
        {
            Ok(body) => body.to_bytes(),
            Err(error) => {
                event!(Level::WARN, %error, "failed to read the cacheable upstream body");
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

        if bytes.len() as u64 <= response_limit {
            let etag = response_headers
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
                .unwrap_or_else(|| weak_etag(&bytes));
            if let Ok(value) = HeaderValue::from_str(&etag) {
                response_headers.insert(header::ETAG, value);
            }

            state.cache.insert(
                cache_lookup_key,
                CachedResponse {
                    status,
                    headers: response_headers.clone(),
                    body: bytes.clone(),
                    etag: Some(etag),
                },
                cache_ttl(&site.cache, &response_headers),
                &cache_request_headers,
            );
        }

        response_headers.insert(cache_header(), HeaderValue::from_static("MISS"));
        log_event(
            &state,
            &request_log,
            status,
            &response_headers,
            Some(bytes.len() as u64),
            version,
        );
        return full_response(status, version, response_headers, bytes);
    }

    // Count bytes while streaming and only write the access row when the body
    // ends (or the client disconnects). Chunked responses then have a real size
    // instead of NULL, and the log is not emitted before the transfer ends.
    let body = upstream_response
        .into_body()
        .map_err(|error| -> BoxError { Box::new(error) })
        .boxed();
    let body = logged_stream_body(
        state.clone(),
        request_log,
        status,
        response_headers.clone(),
        version,
        body,
    );

    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.version_mut() = version;
    *response.headers_mut() = response_headers;
    response
}

pub async fn handle_buffered(
    state: Arc<AppState>,
    request: BufferedRequest,
    scheme: &'static str,
) -> Response<GatewayBody> {
    let BufferedRequest {
        method,
        uri,
        version,
        headers,
        body,
        peer,
    } = request;

    let raw_host = parts_host(&headers, &uri);
    let path = uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let query = uri.query().map(str::to_string);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut request_log = access::RequestLog {
        id: access::next_id(),
        host: raw_host.clone(),
        method: method.as_str().to_string(),
        path: path.clone(),
        headers: access::headers_json(&headers),
        http_version: access::version_label(version),
        remote_addr: peer.ip().to_string(),
        body_length: body.len() as u64,
        website_id: None,
    };

    let Some(site) = state
        .routing
        .read()
        .expect("routing lock poisoned")
        .lookup(&raw_host)
    else {
        return buffered_error(
            &state,
            &request_log,
            StatusCode::NOT_FOUND,
            "Not Found",
            version,
        );
    };

    let client_ip = site.client_ip.resolve(peer, None, &headers);
    request_log.remote_addr = client_ip.to_string();
    request_log.website_id = Some(site.id.clone());

    if let Some(response) = buffered_rate_limit_response(&state, &request_log, client_ip, version) {
        return response;
    }

    let context = MatchContext {
        path: &path,
        query: query.as_deref(),
        method: method.as_str(),
        user_agent: user_agent.as_deref(),
    };
    let Some(backend) = site
        .next_backend(&context, Some(&state.backend_health))
        .cloned()
    else {
        return buffered_error(
            &state,
            &request_log,
            StatusCode::BAD_GATEWAY,
            "Bad Gateway",
            version,
        );
    };

    let upstream_uri = match build_upstream_uri(&backend, &uri) {
        Ok(uri) => uri,
        Err(error) => {
            event!(Level::ERROR, %error, "failed to build the buffered upstream uri");
            return buffered_error(
                &state,
                &request_log,
                StatusCode::BAD_GATEWAY,
                "Bad Gateway",
                version,
            );
        }
    };

    let mut upstream_headers = headers;
    remove_hop_by_hop(&mut upstream_headers);
    if !raw_host.is_empty()
        && let Ok(value) = HeaderValue::from_str(&raw_host)
    {
        upstream_headers.insert(header::HOST, value);
    }
    site.headers
        .apply(&mut upstream_headers, &raw_host, scheme, client_ip);

    let mut upstream_request = match Request::builder()
        .method(method)
        .uri(upstream_uri)
        .version(Version::HTTP_11)
        .body(full_gateway_body(body))
    {
        Ok(request) => request,
        Err(error) => {
            event!(Level::ERROR, %error, "failed to build the buffered upstream request");
            return buffered_error(
                &state,
                &request_log,
                StatusCode::BAD_GATEWAY,
                "Bad Gateway",
                version,
            );
        }
    };
    *upstream_request.headers_mut() = upstream_headers;

    let upstream_response = match tokio::time::timeout(
        Duration::from_secs(60),
        state.client.request(upstream_request),
    )
    .await
    {
        Ok(Ok(response)) => {
            crate::health::record_passive(&state, &site, &backend, true);
            response
        }
        Ok(Err(error)) => {
            crate::health::record_passive(&state, &site, &backend, false);
            event!(Level::WARN, %error, "buffered upstream request failed");
            return buffered_error(
                &state,
                &request_log,
                StatusCode::BAD_GATEWAY,
                "Bad Gateway",
                version,
            );
        }
        Err(_) => {
            crate::health::record_passive(&state, &site, &backend, false);
            event!(Level::WARN, "buffered upstream request timed out");
            return buffered_error(
                &state,
                &request_log,
                StatusCode::GATEWAY_TIMEOUT,
                "Gateway Timeout",
                version,
            );
        }
    };

    let upstream_response = upstream_response.map(|body| {
        body.map_err(|error| -> BoxError { Box::new(error) })
            .boxed()
    });
    let status = upstream_response.status();
    let mut response_headers = upstream_response.headers().clone();
    remove_hop_by_hop(&mut response_headers);
    response_headers.insert(header::SERVER, HeaderValue::from_static("WebGateway"));
    if let Ok(value) = HeaderValue::from_str(&state.node.header_label()) {
        response_headers.insert(HeaderName::from_static("x-webgateway-node"), value);
    }

    // Stream the upstream body; HTTP/3 no longer buffers the full response.
    let body = logged_stream_body(
        state.clone(),
        request_log,
        status,
        response_headers.clone(),
        version,
        upstream_response.into_body(),
    );
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.version_mut() = version;
    *response.headers_mut() = response_headers;
    response
}

/// Wrap a fully buffered body in the boxed body type used by the proxy.
fn full_gateway_body(bytes: Bytes) -> GatewayBody {
    Full::new(bytes)
        .map_err(|never: Infallible| -> BoxError { match never {} })
        .boxed()
}

fn parts_host(headers: &HeaderMap, uri: &Uri) -> String {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            uri.authority()
                .map(|authority| authority.as_str().to_string())
        })
        .unwrap_or_default()
}

fn buffered_error(
    state: &AppState,
    request: &access::RequestLog,
    status: StatusCode,
    message: &'static str,
    version: Version,
) -> Response<GatewayBody> {
    log_event(
        state,
        request,
        status,
        &HeaderMap::new(),
        Some(message.len() as u64),
        version,
    );
    buffered_text_response(state, status, message, version)
}

fn buffered_text_response(
    state: &AppState,
    status: StatusCode,
    message: &'static str,
    version: Version,
) -> Response<GatewayBody> {
    let mut response = Response::new(full_gateway_body(Bytes::from_static(message.as_bytes())));
    *response.status_mut() = status;
    *response.version_mut() = version;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    headers.insert(header::SERVER, HeaderValue::from_static("WebGateway"));
    if let Ok(value) = HeaderValue::from_str(&state.node.header_label()) {
        headers.insert(HeaderName::from_static("x-webgateway-node"), value);
    }
    response
}

fn cache_key_for(host: &str, uri: &Uri) -> String {
    format!(
        "{}|{}",
        host.to_ascii_lowercase(),
        uri.path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/")
    )
}

fn cache_header() -> HeaderName {
    HeaderName::from_static("x-webgateway-cache")
}

fn cache_hit_response(
    state: &AppState,
    request: &crate::access::RequestLog,
    mut cached: CachedResponse,
    request_headers: &HeaderMap,
    version: Version,
    head: bool,
) -> Response<ProxyBody> {
    let not_modified = not_modified(request_headers, cached.etag.as_deref());
    let status = if not_modified {
        StatusCode::NOT_MODIFIED
    } else {
        cached.status
    };

    if not_modified {
        cached.headers.remove(header::CONTENT_LENGTH);
        cached.headers.remove(header::CONTENT_TYPE);
        cached.body = Bytes::new();
    } else if head {
        // HEAD keeps the cached GET headers (including Content-Length) but
        // never sends a body.
        cached.body = Bytes::new();
    }

    cached
        .headers
        .insert(cache_header(), HeaderValue::from_static("HIT"));
    log_event(
        state,
        request,
        status,
        &cached.headers,
        Some(cached.body.len() as u64),
        version,
    );
    full_response(status, version, cached.headers, cached.body)
}

fn not_modified(headers: &HeaderMap, etag: Option<&str>) -> bool {
    let Some(etag) = etag else {
        return false;
    };
    let etag = etag.trim_start_matches("W/");

    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|candidate| candidate.trim().trim_start_matches("W/") == etag)
        })
}

fn response_cacheable(headers: &HeaderMap, config: &CacheConfig) -> bool {
    if headers.contains_key(header::SET_COOKIE) {
        return false;
    }

    if let Some(value) = headers
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
    {
        let value = value.to_ascii_lowercase();
        if value.contains("no-store") || value.contains("private") || value.contains("no-cache") {
            return false;
        }
    }

    config.allows_content_type(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
    )
}

fn cache_ttl(config: &CacheConfig, headers: &HeaderMap) -> Duration {
    let mut ttl = config.ttl_secs;
    if let Some(value) = headers
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
    {
        for directive in value.split(',') {
            let directive = directive.trim();
            let seconds = directive
                .strip_prefix("s-maxage=")
                .or_else(|| directive.strip_prefix("max-age="));
            if let Some(seconds) = seconds
                && let Ok(seconds) = seconds.parse::<u64>()
            {
                ttl = ttl.min(seconds);
            }
        }
    }
    Duration::from_secs(ttl)
}

fn weak_etag(body: &[u8]) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    format!("W/\"{:016x}\"", hasher.finish())
}

fn full_response(
    status: StatusCode,
    version: Version,
    headers: HeaderMap,
    body: Bytes,
) -> Response<ProxyBody> {
    let body = Full::new(body)
        .map_err(|never: Infallible| -> BoxError { match never {} })
        .boxed();

    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.version_mut() = version;
    *response.headers_mut() = headers;
    response
}

pub(crate) fn log_event(
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
        crate::access::record_dropped();
        tracing::debug!(%error, "dropping access log event");
    }
}

/// Streams a body while counting bytes, then writes one access-log row.
///
/// Logging on completion (instead of before streaming) records the real body
/// size for chunked responses. A client disconnect still logs the bytes read so
/// far via `Drop`.
struct LoggedBody {
    inner: Pin<Box<GatewayBody>>,
    state: Arc<AppState>,
    request: access::RequestLog,
    status: StatusCode,
    headers: HeaderMap,
    version: Version,
    bytes: u64,
    logged: bool,
}

impl LoggedBody {
    fn finish(&mut self) {
        if self.logged {
            return;
        }
        self.logged = true;
        log_event(
            &self.state,
            &self.request,
            self.status,
            &self.headers,
            Some(self.bytes),
            self.version,
        );
    }
}

impl Body for LoggedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.bytes = this.bytes.saturating_add(data.len() as u64);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.finish();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.finish();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.as_ref().size_hint()
    }
}

impl Drop for LoggedBody {
    fn drop(&mut self) {
        self.finish();
    }
}

fn logged_stream_body(
    state: Arc<AppState>,
    request: access::RequestLog,
    status: StatusCode,
    headers: HeaderMap,
    version: Version,
    body: GatewayBody,
) -> GatewayBody {
    LoggedBody {
        inner: Box::pin(body),
        state,
        request,
        status,
        headers,
        version,
        bytes: 0,
        logged: false,
    }
    .boxed()
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

/// Shared decision + access log for a rejected request. Returns the number of
/// seconds to advertise in Retry-After, or None when the request may proceed.
fn rate_limit_decision(
    state: &AppState,
    request_log: &crate::access::RequestLog,
    client_ip: IpAddr,
    version: Version,
) -> Option<u64> {
    let decision = state.rate_limiter.check(client_ip)?;
    if decision.allowed {
        return None;
    }
    crate::metrics::record_rate_limited();
    event!(
        Level::DEBUG,
        client = %client_ip,
        retry_after_secs = decision.retry_after_secs,
        "request rate limited"
    );
    let mut extra = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&decision.retry_after_secs.to_string()) {
        extra.insert(header::RETRY_AFTER, value);
    }
    log_event(
        state,
        request_log,
        StatusCode::TOO_MANY_REQUESTS,
        &extra,
        Some(RATE_LIMIT_BODY.len() as u64),
        version,
    );
    Some(decision.retry_after_secs)
}

fn retry_after_header(seconds: u64) -> Option<HeaderValue> {
    HeaderValue::from_str(&seconds.to_string()).ok()
}

/// Reject a request with 429 when the per-client-IP token bucket is empty.
/// Returns None when the request may proceed (including when limiting is off).
pub(crate) fn rate_limit_response(
    state: &AppState,
    request_log: &crate::access::RequestLog,
    client_ip: IpAddr,
    version: Version,
) -> Option<Response<ProxyBody>> {
    let retry_after = rate_limit_decision(state, request_log, client_ip, version)?;
    let mut response = text_response(StatusCode::TOO_MANY_REQUESTS, RATE_LIMIT_BODY, state);
    if let Some(value) = retry_after_header(retry_after) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    Some(response)
}

/// HTTP/3 variant: the buffered path returns a byte body instead of a stream.
fn buffered_rate_limit_response(
    state: &AppState,
    request_log: &crate::access::RequestLog,
    client_ip: IpAddr,
    version: Version,
) -> Option<Response<GatewayBody>> {
    let retry_after = rate_limit_decision(state, request_log, client_ip, version)?;
    let mut response = buffered_text_response(
        state,
        StatusCode::TOO_MANY_REQUESTS,
        RATE_LIMIT_BODY,
        version,
    );
    if let Some(value) = retry_after_header(retry_after) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    Some(response)
}

pub(crate) fn text_response(
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
            weight: 1,
            priority: 0,
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
