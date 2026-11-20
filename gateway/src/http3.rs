//! HTTP/3 (QUIC) listener. Requests are buffered before being proxied.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use http::StatusCode;
use tokio::task::JoinHandle;
use tracing::{Level, event};

use crate::{
    proxy::{self, BufferedRequest},
    state::AppState,
    tls,
};

pub fn spawn(
    bind: IpAddr,
    ports: &[u16],
    state: Arc<AppState>,
) -> anyhow::Result<Vec<JoinHandle<()>>> {
    let mut tasks = Vec::new();

    for &port in ports {
        let config = tls::quic_server_config(state.certs.clone())?;
        let addr = SocketAddr::new(bind, port);
        let endpoint = quinn::Endpoint::server(config, addr)
            .with_context(|| format!("failed to bind HTTP/3 listener on {addr}"))?;
        event!(Level::INFO, port, "HTTP/3 listener started");

        let state = state.clone();
        tasks.push(tokio::spawn(async move {
            accept_loop(endpoint, state).await;
        }));
    }

    Ok(tasks)
}

async fn accept_loop(endpoint: quinn::Endpoint, state: Arc<AppState>) {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(connection) => {
                    if let Err(error) = serve_connection(connection, state).await {
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
        tokio::spawn(async move {
            if let Err(error) = handle_request(resolver, state, peer).await {
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
) -> anyhow::Result<()> {
    crate::metrics::record_http3_request();
    let _inflight = crate::metrics::InFlightGuard::new();
    let (request, mut stream) = resolver
        .resolve_request()
        .await
        .context("HTTP/3 request failed")?;

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
    let mut response = http::Response::from_parts(response_parts, ());

    if !response
        .headers()
        .contains_key(http::header::CONTENT_LENGTH)
        && let Ok(value) = http::HeaderValue::from_str(&response_body.len().to_string())
    {
        response
            .headers_mut()
            .insert(http::header::CONTENT_LENGTH, value);
    }

    stream
        .send_response(response)
        .await
        .context("failed to send the HTTP/3 response headers")?;
    if !response_body.is_empty() {
        stream
            .send_data(response_body)
            .await
            .context("failed to send the HTTP/3 response body")?;
    }
    stream
        .finish()
        .await
        .context("failed to finish the HTTP/3 response")?;

    Ok(())
}
