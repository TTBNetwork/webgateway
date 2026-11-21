//! HTTP/3 (QUIC) listener.
//!
//! Request bodies are still buffered up to `max_request_body_bytes`; responses
//! are streamed from the upstream so large downloads do not consume memory.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use http::StatusCode;
use http_body_util::BodyExt;
use tokio::{sync::Semaphore, task::JoinHandle};
use tracing::{Level, event};

use crate::{
    listeners::Inflight,
    proxy::{self, BufferedRequest},
    state::AppState,
    tls,
};

/// Handles for the HTTP/3 listener so shutdown can close the endpoints, drain
/// in-flight requests and then stop the accept loops.
pub struct Http3 {
    tasks: Vec<JoinHandle<()>>,
    endpoints: Vec<quinn::Endpoint>,
}

impl Http3 {
    /// Stop accepting new QUIC connections while accepted streams keep draining.
    pub fn stop_accepting(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }

    /// Close the endpoints after the drain window; sends CONNECTION_CLOSE.
    pub fn close(&self) {
        for endpoint in &self.endpoints {
            endpoint.close(0u32.into(), b"shutdown");
        }
    }

    /// Abort the accept loops after the drain window elapsed.
    pub fn abort(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}

pub fn spawn(
    bind: IpAddr,
    ports: &[u16],
    state: Arc<AppState>,
    inflight: Inflight,
    max_connections: usize,
    max_concurrent_streams: u32,
) -> anyhow::Result<Http3> {
    let mut tasks = Vec::new();
    let mut endpoints = Vec::new();
    let connection_limit = Arc::new(Semaphore::new(max_connections.max(1)));

    for &port in ports {
        let mut config = tls::quic_server_config(state.certs.clone())?;
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(max_concurrent_streams.max(1).into());
        config.transport_config(Arc::new(transport));

        let addr = SocketAddr::new(bind, port);
        let endpoint = quinn::Endpoint::server(config, addr)
            .with_context(|| format!("failed to bind HTTP/3 listener on {addr}"))?;
        event!(
            Level::INFO,
            port,
            max_connections,
            max_concurrent_streams,
            "HTTP/3 listener started"
        );

        let state = state.clone();
        let inflight = inflight.clone();
        let accept_endpoint = endpoint.clone();
        let connection_limit = connection_limit.clone();
        tasks.push(tokio::spawn(async move {
            accept_loop(accept_endpoint, state, inflight, connection_limit).await;
        }));
        endpoints.push(endpoint);
    }

    Ok(Http3 { tasks, endpoints })
}

async fn accept_loop(
    endpoint: quinn::Endpoint,
    state: Arc<AppState>,
    inflight: Inflight,
    connection_limit: Arc<Semaphore>,
) {
    while let Some(incoming) = endpoint.accept().await {
        let permit = match connection_limit.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                event!(
                    Level::WARN,
                    "HTTP/3 connection limit reached; dropping new connection"
                );
                drop(incoming);
                continue;
            }
        };
        let state = state.clone();
        let inflight = inflight.clone();
        tokio::spawn(async move {
            let _permit = permit;
            match incoming.await {
                Ok(connection) => {
                    if let Err(error) = serve_connection(connection, state, inflight).await {
                        event!(Level::DEBUG, %error, "HTTP/3 connection closed");
                    }
                }
                Err(error) => event!(Level::DEBUG, %error, "HTTP/3 handshake failed"),
            }
        });
    }
}

async fn serve_connection(
    connection: quinn::Connection,
    state: Arc<AppState>,
    inflight: Inflight,
) -> anyhow::Result<()> {
    let peer = connection.remote_address();
    let mut h3 = h3::server::Connection::new(h3_quinn::Connection::new(connection))
        .await
        .context("HTTP/3 handshake failed")?;

    loop {
        let resolver = match h3.accept().await.context("HTTP/3 accept failed")? {
            Some(resolver) => resolver,
            None => return Ok(()),
        };

        // Each request stream is independent; serve them concurrently so one
        // slow upstream does not block the other multiplexed streams.
        let state = state.clone();
        let inflight = inflight.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_request(resolver, state, peer, inflight).await {
                event!(Level::DEBUG, %error, "HTTP/3 request failed");
            }
        });
    }
}

type H3Stream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

async fn reject(
    stream: &mut H3Stream,
    status: StatusCode,
    message: &'static str,
) -> anyhow::Result<()> {
    let body = Bytes::from_static(message.as_bytes());
    let response = http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(http::header::CONTENT_LENGTH, body.len().to_string())
        .body(())
        .context("failed to build the HTTP/3 error response")?;
    stream
        .send_response(response)
        .await
        .context("failed to send the HTTP/3 error response")?;
    stream
        .send_data(body)
        .await
        .context("failed to send the HTTP/3 error body")?;
    stream
        .finish()
        .await
        .context("failed to finish the HTTP/3 error response")?;
    Ok(())
}

async fn handle_request(
    resolver: h3::server::RequestResolver<h3_quinn::Connection, Bytes>,
    state: Arc<AppState>,
    peer: SocketAddr,
    inflight: Inflight,
) -> anyhow::Result<()> {
    crate::metrics::record_http3_request();
    let _metrics_inflight = crate::metrics::InFlightGuard::new();
    // Shared with the TCP listeners so SIGTERM drains HTTP/3 requests too.
    let _drain_guard = inflight.enter();
    let (request, mut stream) = resolver
        .resolve_request()
        .await
        .context("HTTP/3 request failed")?;

    let header_bytes: usize = request
        .headers()
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len() + 32)
        .sum();
    if header_bytes > state.max_header_bytes {
        event!(
            Level::WARN,
            header_bytes,
            limit = state.max_header_bytes,
            "rejecting oversized HTTP/3 request headers"
        );
        reject(
            &mut stream,
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "Request Header Fields Too Large",
        )
        .await?;
        return Ok(());
    }

    let declared_length = request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > state.max_request_body_bytes) {
        event!(
            Level::WARN,
            length = ?declared_length,
            limit = state.max_request_body_bytes,
            "rejecting oversized HTTP/3 request body"
        );
        reject(
            &mut stream,
            StatusCode::PAYLOAD_TOO_LARGE,
            "Payload Too Large",
        )
        .await?;
        return Ok(());
    }

    let mut body = BytesMut::new();
    while let Some(mut chunk) = stream
        .recv_data()
        .await
        .context("failed to read the HTTP/3 request body")?
    {
        let length = chunk.remaining();
        if (body.len() as u64).saturating_add(length as u64) > state.max_request_body_bytes {
            event!(
                Level::WARN,
                limit = state.max_request_body_bytes,
                "rejecting streamed HTTP/3 request body over the limit"
            );
            reject(
                &mut stream,
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large",
            )
            .await?;
            return Ok(());
        }
        let data = chunk.copy_to_bytes(length);
        body.extend_from_slice(&data);
    }

    let (parts, _) = request.into_parts();
    let buffered = BufferedRequest {
        method: parts.method,
        uri: parts.uri,
        version: parts.version,
        headers: parts.headers,
        body: body.freeze(),
        peer,
    };
    let response = proxy::handle_buffered(state, buffered, "https").await;
    let (response_parts, response_body) = response.into_parts();
    let response = http::Response::from_parts(response_parts, ());

    stream
        .send_response(response)
        .await
        .context("failed to send the HTTP/3 response headers")?;

    // Stream the upstream body instead of collecting it: HTTP/3 downloads of
    // any size stay O(chunk) in memory.
    let mut response_body = response_body;
    while let Some(frame) = response_body
        .frame()
        .await
        .transpose()
        .map_err(|error| anyhow::anyhow!("HTTP/3 upstream body failed: {error}"))?
    {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
        {
            stream
                .send_data(data.clone())
                .await
                .context("failed to send the HTTP/3 response body")?;
        }
    }
    stream
        .finish()
        .await
        .context("failed to finish the HTTP/3 response")?;

    Ok(())
}
