//! Prometheus-compatible metrics for the gateway data plane.
//!
//! Counters are process-wide atomics; the listener is a small plain-HTTP
//! endpoint bound to `[server].metrics_bind` (loopback by default) so it does
//! not need authentication and is not exposed through the public listeners.

use std::{
    convert::Infallible,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use bytes::Bytes;
use http::{HeaderValue, Response, header};
use http_body_util::Full;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{Level, event};

use crate::{access, state::AppState};

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static HTTP3_REQUESTS: AtomicU64 = AtomicU64::new(0);
static WEBSOCKET_REQUESTS: AtomicU64 = AtomicU64::new(0);
static CONNECT_REQUESTS: AtomicU64 = AtomicU64::new(0);

static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static CACHE_EXPIRED: AtomicU64 = AtomicU64::new(0);
static CACHE_STORES: AtomicU64 = AtomicU64::new(0);
static CACHE_EVICTIONS: AtomicU64 = AtomicU64::new(0);
static CACHE_REJECTED: AtomicU64 = AtomicU64::new(0);
static CACHE_CLEARS: AtomicU64 = AtomicU64::new(0);

static CONNECTIONS_ACTIVE: AtomicU64 = AtomicU64::new(0);
static REQUESTS_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
static SYNC_SUCCESS: AtomicU64 = AtomicU64::new(0);
static SYNC_FAILURES: AtomicU64 = AtomicU64::new(0);

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

pub fn record_request() {
    bump(&REQUESTS);
}

pub fn record_http3_request() {
    bump(&HTTP3_REQUESTS);
}

pub fn record_websocket_request() {
    bump(&WEBSOCKET_REQUESTS);
}

pub fn record_connect_request() {
    bump(&CONNECT_REQUESTS);
}

pub fn record_sync_result(success: bool) {
    if success {
        bump(&SYNC_SUCCESS);
    } else {
        bump(&SYNC_FAILURES);
    }
}

pub fn cache_hit() {
    bump(&CACHE_HITS);
}

pub fn cache_miss() {
    bump(&CACHE_MISSES);
}

pub fn cache_expired() {
    bump(&CACHE_EXPIRED);
}

pub fn cache_store() {
    bump(&CACHE_STORES);
}

pub fn cache_eviction() {
    bump(&CACHE_EVICTIONS);
}

pub fn cache_rejected() {
    bump(&CACHE_REJECTED);
}

pub fn cache_clear() {
    bump(&CACHE_CLEARS);
}

pub fn connection_started() {
    bump(&CONNECTIONS_ACTIVE);
}

pub fn connection_finished() {
    CONNECTIONS_ACTIVE.fetch_sub(1, Ordering::Relaxed);
}

/// Keeps `webgateway_requests_in_flight` accurate for the request lifetime.
#[must_use]
pub struct InFlightGuard;

impl InFlightGuard {
    pub fn new() -> Self {
        bump(&REQUESTS_IN_FLIGHT);
        Self
    }
}

impl Default for InFlightGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        REQUESTS_IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) struct Snapshot {
    machine_id: String,
    node: String,
    version: String,
    requests: u64,
    http3_requests: u64,
    websocket_requests: u64,
    connect_requests: u64,
    cache_hits: u64,
    cache_misses: u64,
    cache_expired: u64,
    cache_stores: u64,
    cache_evictions: u64,
    cache_rejected: u64,
    cache_clears: u64,
    cache_entries: u64,
    cache_bytes: u64,
    cache_max_bytes: u64,
    connections_active: u64,
    requests_in_flight: u64,
    access_dropped: u64,
    access_failed_batches: u64,
    access_queue_available: u64,
    sync_success: u64,
    sync_failures: u64,
    route_hosts: u64,
    certificate_hosts: u64,
}

fn counter(out: &mut String, name: &str, value: u64) {
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" counter\n");
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

fn gauge(out: &mut String, name: &str, value: u64) {
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" gauge\n");
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

pub(crate) fn format_metrics(snapshot: &Snapshot) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("# TYPE webgateway_info gauge\n");
    out.push_str(&format!(
        "webgateway_info{{machine_id=\"{}\",node=\"{}\",version=\"{}\"}} 1\n",
        escape_label(&snapshot.machine_id),
        escape_label(&snapshot.node),
        escape_label(&snapshot.version)
    ));
    counter(&mut out, "webgateway_requests_total", snapshot.requests);
    counter(
        &mut out,
        "webgateway_http3_requests_total",
        snapshot.http3_requests,
    );
    counter(
        &mut out,
        "webgateway_websocket_requests_total",
        snapshot.websocket_requests,
    );
    counter(
        &mut out,
        "webgateway_connect_requests_total",
        snapshot.connect_requests,
    );
    counter(&mut out, "webgateway_cache_hits_total", snapshot.cache_hits);
    counter(
        &mut out,
        "webgateway_cache_misses_total",
        snapshot.cache_misses,
    );
    counter(
        &mut out,
        "webgateway_cache_expired_total",
        snapshot.cache_expired,
    );
    counter(
        &mut out,
        "webgateway_cache_stores_total",
        snapshot.cache_stores,
    );
    counter(
        &mut out,
        "webgateway_cache_evictions_total",
        snapshot.cache_evictions,
    );
    counter(
        &mut out,
        "webgateway_cache_rejected_total",
        snapshot.cache_rejected,
    );
    counter(
        &mut out,
        "webgateway_cache_clears_total",
        snapshot.cache_clears,
    );
    counter(
        &mut out,
        "webgateway_access_log_dropped_total",
        snapshot.access_dropped,
    );
    counter(
        &mut out,
        "webgateway_access_log_failed_batches_total",
        snapshot.access_failed_batches,
    );
    counter(
        &mut out,
        "webgateway_config_sync_success_total",
        snapshot.sync_success,
    );
    counter(
        &mut out,
        "webgateway_config_sync_failures_total",
        snapshot.sync_failures,
    );
    gauge(&mut out, "webgateway_cache_entries", snapshot.cache_entries);
    gauge(&mut out, "webgateway_cache_bytes", snapshot.cache_bytes);
    gauge(
        &mut out,
        "webgateway_cache_max_bytes",
        snapshot.cache_max_bytes,
    );
    gauge(
        &mut out,
        "webgateway_connections_active",
        snapshot.connections_active,
    );
    gauge(
        &mut out,
        "webgateway_requests_in_flight",
        snapshot.requests_in_flight,
    );
    gauge(
        &mut out,
        "webgateway_access_log_queue_available",
        snapshot.access_queue_available,
    );
    gauge(&mut out, "webgateway_route_hosts", snapshot.route_hosts);
    gauge(
        &mut out,
        "webgateway_certificate_hosts",
        snapshot.certificate_hosts,
    );
    out
}

fn snapshot(state: &AppState) -> Snapshot {
    let cache = state.cache.stats();
    let (access_dropped, access_failed_batches) = access::counters();
    Snapshot {
        machine_id: state.node.machine_id.clone(),
        node: state.node.label(),
        version: state.node.version.to_string(),
        requests: REQUESTS.load(Ordering::Relaxed),
        http3_requests: HTTP3_REQUESTS.load(Ordering::Relaxed),
        websocket_requests: WEBSOCKET_REQUESTS.load(Ordering::Relaxed),
        connect_requests: CONNECT_REQUESTS.load(Ordering::Relaxed),
        cache_hits: CACHE_HITS.load(Ordering::Relaxed),
        cache_misses: CACHE_MISSES.load(Ordering::Relaxed),
        cache_expired: CACHE_EXPIRED.load(Ordering::Relaxed),
        cache_stores: CACHE_STORES.load(Ordering::Relaxed),
        cache_evictions: CACHE_EVICTIONS.load(Ordering::Relaxed),
        cache_rejected: CACHE_REJECTED.load(Ordering::Relaxed),
        cache_clears: CACHE_CLEARS.load(Ordering::Relaxed),
        cache_entries: cache.entries as u64,
        cache_bytes: cache.bytes,
        cache_max_bytes: cache.max_bytes,
        connections_active: CONNECTIONS_ACTIVE.load(Ordering::Relaxed),
        requests_in_flight: REQUESTS_IN_FLIGHT.load(Ordering::Relaxed),
        access_dropped,
        access_failed_batches,
        access_queue_available: state.logs.capacity() as u64,
        sync_success: SYNC_SUCCESS.load(Ordering::Relaxed),
        sync_failures: SYNC_FAILURES.load(Ordering::Relaxed),
        route_hosts: state
            .routing
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .host_count() as u64,
        certificate_hosts: state
            .certs
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .hostname_count() as u64,
    }
}

fn metrics_response(state: &AppState) -> Response<Full<Bytes>> {
    let body = format_metrics(&snapshot(state));
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

/// Bind and serve the metrics endpoint until the task is aborted.
pub async fn spawn(
    bind: IpAddr,
    port: u16,
    state: Arc<AppState>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = TcpListener::bind((bind, port))
        .await
        .with_context(|| format!("failed to bind the metrics listener on {bind}:{port}"))?;
    event!(Level::INFO, %bind, port, "metrics listener started");
    Ok(tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(value) => value,
                Err(error) => {
                    event!(Level::WARN, %error, "metrics listener accept failed");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            let state = state.clone();
            tokio::spawn(async move {
                let service = service_fn(move |_request| {
                    let state = state.clone();
                    async move { Ok::<_, Infallible>(metrics_response(&state)) }
                });
                if let Err(error) = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                {
                    event!(Level::DEBUG, %peer, %error, "metrics connection failed");
                }
            });
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Snapshot {
        Snapshot {
            machine_id: "abc-123".to_string(),
            node: "CN-Shanghai".to_string(),
            version: "0.1.0".to_string(),
            requests: 7,
            http3_requests: 2,
            websocket_requests: 1,
            connect_requests: 1,
            cache_hits: 3,
            cache_misses: 4,
            cache_expired: 1,
            cache_stores: 4,
            cache_evictions: 2,
            cache_rejected: 1,
            cache_clears: 1,
            cache_entries: 2,
            cache_bytes: 42,
            cache_max_bytes: 1024,
            connections_active: 5,
            requests_in_flight: 3,
            access_dropped: 0,
            access_failed_batches: 1,
            access_queue_available: 255,
            sync_success: 9,
            sync_failures: 2,
            route_hosts: 6,
            certificate_hosts: 4,
        }
    }

    #[test]
    fn renders_prometheus_text() {
        let text = format_metrics(&sample());
        assert!(text.contains("# TYPE webgateway_cache_hits_total counter"));
        assert!(text.contains("webgateway_cache_hits_total 3\n"));
        assert!(text.contains("webgateway_cache_bytes 42\n"));
        assert!(text.contains("webgateway_access_log_failed_batches_total 1\n"));
        assert!(text.contains("webgateway_route_hosts 6\n"));
        assert!(text.contains(
            "webgateway_info{machine_id=\"abc-123\",node=\"CN-Shanghai\",version=\"0.1.0\"} 1"
        ));
    }

    #[test]
    fn escapes_label_values() {
        let mut snapshot = sample();
        snapshot.node = "line\nwith \"quotes\"".to_string();
        let text = format_metrics(&snapshot);
        assert!(text.contains("line\\nwith \\\"quotes\\\""));
    }
}
