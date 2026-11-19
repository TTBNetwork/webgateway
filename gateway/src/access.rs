//! Access log writer.
//!
//! The proxy hands completed request/response pairs to a background writer.
//! Rows are batched every 500ms and inserted in one transaction: request rows
//! first, response rows second, then per-second size rows (no foreign keys, so
//! the main rows must exist first). All timestamps come from PostgreSQL now(),
//! which keeps log time aligned with the master database clock.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use http::HeaderMap;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::{Level, event};

/// Events dropped because the channel was full or the database rejected a batch.
static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);
/// Batches that exhausted their retries.
static FAILED_BATCHES: AtomicU64 = AtomicU64::new(0);

use shared::database::Database;

#[derive(Debug, Clone)]
pub struct RequestLog {
    pub id: String,
    pub host: String,
    pub method: String,
    pub path: String,
    pub headers: String,
    pub http_version: String,
    pub remote_addr: String,
    pub body_length: u64,
    pub website_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResponseLog {
    pub id: String,
    pub status: u16,
    pub headers: String,
    pub http_version: String,
    pub body_length: Option<u64>,
    pub website_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AccessEvent {
    pub request: RequestLog,
    pub response: ResponseLog,
}

/// Handle used to flush the writer during graceful shutdown.
pub struct AccessWriter {
    shutdown: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

impl AccessWriter {
    /// Ask the writer to drain queued events, flush, and exit.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        if let Err(error) = self.handle.await {
            event!(Level::WARN, %error, "access log writer stopped with an error");
        }
    }
}

/// Start the background writer. Shutdown is explicit so the last batch is
/// flushed before the process exits.
pub fn spawn(database: Database) -> (mpsc::Sender<AccessEvent>, AccessWriter) {
    let (sender, receiver) = mpsc::channel(4096);
    let (shutdown, receiver_shutdown) = watch::channel(false);
    let handle = tokio::spawn(write_loop(database, receiver, receiver_shutdown));
    (sender, AccessWriter { shutdown, handle })
}

/// Record a dropped event (channel full). Logs a warning every 100 drops.
pub fn record_dropped() {
    let total = DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed) + 1;
    if total.is_multiple_of(100) {
        event!(
            Level::WARN,
            dropped = total,
            "access log channel is full; events are being dropped"
        );
    }
}

/// Counters for tests/logging.
#[cfg(test)]
fn metrics() -> (u64, u64) {
    (
        DROPPED_EVENTS.load(Ordering::Relaxed),
        FAILED_BATCHES.load(Ordering::Relaxed),
    )
}

async fn write_loop(
    database: Database,
    mut receiver: mpsc::Receiver<AccessEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    let mut buffer = Vec::with_capacity(256);

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                // Drain anything already queued and flush before leaving.
                while let Ok(event) = receiver.try_recv() {
                    buffer.push(event);
                }
                let count = buffer.len();
                flush(&database, &mut buffer).await;
                event!(Level::INFO, count, "access log writer stopped");
                return;
            }
            maybe = receiver.recv() => {
                match maybe {
                    Some(event) => {
                        buffer.push(event);
                        if buffer.len() >= 256 {
                            flush(&database, &mut buffer).await;
                        }
                    }
                    None => {
                        flush(&database, &mut buffer).await;
                        return;
                    }
                }
            }
            _ = interval.tick() => {
                flush(&database, &mut buffer).await;
            }
        }
    }
}

async fn flush(database: &Database, buffer: &mut Vec<AccessEvent>) {
    if buffer.is_empty() {
        return;
    }

    let events = std::mem::take(buffer);
    let count = events.len();
    let mut last_error = None;

    // The database is the only sink; retry transient failures before giving up
    // so a short outage does not lose the batch outright.
    for attempt in 0..3 {
        match write_batch(database, &events).await {
            Ok(()) => return,
            Err(error) => {
                last_error = Some(error);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                }
            }
        }
    }

    let failures = FAILED_BATCHES.fetch_add(1, Ordering::Relaxed) + 1;
    let dropped = DROPPED_EVENTS.fetch_add(count as u64, Ordering::Relaxed) + count as u64;
    let error = last_error
        .map(|error| format!("{error:#}"))
        .unwrap_or_else(|| "unknown error".to_string());
    event!(
        Level::WARN,
        %error,
        count,
        failed_batches = failures,
        dropped,
        "access log batch failed after retries; events dropped"
    );
}

async fn write_batch(database: &Database, events: &[AccessEvent]) -> anyhow::Result<()> {
    let mut tx = database
        .pool()
        .begin()
        .await
        .context("failed to start the access log transaction")?;

    let ids: Vec<String> = events
        .iter()
        .map(|event| event.request.id.clone())
        .collect();
    let hosts: Vec<String> = events
        .iter()
        .map(|event| event.request.host.clone())
        .collect();
    let methods: Vec<String> = events
        .iter()
        .map(|event| event.request.method.clone())
        .collect();
    let paths: Vec<String> = events
        .iter()
        .map(|event| event.request.path.clone())
        .collect();
    let request_headers: Vec<String> = events
        .iter()
        .map(|event| event.request.headers.clone())
        .collect();
    let request_versions: Vec<String> = events
        .iter()
        .map(|event| event.request.http_version.clone())
        .collect();
    let remote_addrs: Vec<String> = events
        .iter()
        .map(|event| event.request.remote_addr.clone())
        .collect();
    let request_lengths: Vec<i64> = events
        .iter()
        .map(|event| event.request.body_length.min(i64::MAX as u64) as i64)
        .collect();
    let website_ids: Vec<String> = events
        .iter()
        .map(|event| event.request.website_id.clone().unwrap_or_default())
        .collect();

    sqlx::query(
        "INSERT INTO access_request_logs
             (id, host, method, path, headers, http_version, remote_addr, body_length, website_id)
         SELECT t.id, t.host, t.method, t.path, t.headers::jsonb, t.http_version, t.remote_addr,
                t.body_length::uint8, NULLIF(t.website_id, '')
           FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::bigint[], $9::text[])
                AS t(id, host, method, path, headers, http_version, remote_addr, body_length, website_id)",
    )
    .bind(&ids)
    .bind(&hosts)
    .bind(&methods)
    .bind(&paths)
    .bind(&request_headers)
    .bind(&request_versions)
    .bind(&remote_addrs)
    .bind(&request_lengths)
    .bind(&website_ids)
    .execute(&mut *tx)
    .await
    .context("failed to insert access_request_logs")?;

    let statuses: Vec<i32> = events
        .iter()
        .map(|event| i32::from(event.response.status))
        .collect();
    let response_headers: Vec<String> = events
        .iter()
        .map(|event| event.response.headers.clone())
        .collect();
    let response_versions: Vec<String> = events
        .iter()
        .map(|event| event.response.http_version.clone())
        .collect();
    let response_lengths: Vec<i64> = events
        .iter()
        .map(|event| event.response.body_length.unwrap_or(0).min(i64::MAX as u64) as i64)
        .collect();
    let response_website_ids: Vec<String> = events
        .iter()
        .map(|event| event.response.website_id.clone().unwrap_or_default())
        .collect();

    sqlx::query(
        "INSERT INTO access_response_logs
             (id, status, headers, http_version, body_length, website_id, backend_responsed_at)
         SELECT t.id, t.status::uint2, t.headers::jsonb, t.http_version, NULLIF(t.body_length, 0)::uint8,
                NULLIF(t.website_id, ''), now()
           FROM unnest($1::text[], $2::int[], $3::text[], $4::text[], $5::bigint[], $6::text[])
                AS t(id, status, headers, http_version, body_length, website_id)",
    )
    .bind(&ids)
    .bind(&statuses)
    .bind(&response_headers)
    .bind(&response_versions)
    .bind(&response_lengths)
    .bind(&response_website_ids)
    .execute(&mut *tx)
    .await
    .context("failed to insert access_response_logs")?;

    // Per-second byte counters. Main rows are inserted above; the unique
    // (request_id/response_id, at_second) index makes concurrent batches safe.
    let request_size_rows: Vec<(String, String, i64)> = events
        .iter()
        .filter(|event| event.request.body_length > 0)
        .map(|event| {
            (
                next_id(),
                event.request.id.clone(),
                event.request.body_length.min(i64::MAX as u64) as i64,
            )
        })
        .collect();

    if !request_size_rows.is_empty() {
        let size_ids: Vec<String> = request_size_rows.iter().map(|row| row.0.clone()).collect();
        let request_ids: Vec<String> = request_size_rows.iter().map(|row| row.1.clone()).collect();
        let lengths: Vec<i64> = request_size_rows.iter().map(|row| row.2).collect();

        sqlx::query(
            "INSERT INTO access_request_size_logs (id, request_id, body_length, at_second)
             SELECT t.id, t.request_id, t.body_length::uint8, date_trunc('second', now())
               FROM unnest($1::text[], $2::text[], $3::bigint[]) AS t(id, request_id, body_length)
             ON CONFLICT (request_id, at_second)
             DO UPDATE SET body_length = access_request_size_logs.body_length + EXCLUDED.body_length",
        )
        .bind(&size_ids)
        .bind(&request_ids)
        .bind(&lengths)
        .execute(&mut *tx)
        .await
        .context("failed to insert access_request_size_logs")?;
    }

    let response_size_rows: Vec<(String, String, i64)> = events
        .iter()
        .filter_map(|event| {
            event
                .response
                .body_length
                .filter(|length| *length > 0)
                .map(|length| {
                    (
                        next_id(),
                        event.response.id.clone(),
                        length.min(i64::MAX as u64) as i64,
                    )
                })
        })
        .collect();

    if !response_size_rows.is_empty() {
        let size_ids: Vec<String> = response_size_rows.iter().map(|row| row.0.clone()).collect();
        let response_ids: Vec<String> =
            response_size_rows.iter().map(|row| row.1.clone()).collect();
        let lengths: Vec<i64> = response_size_rows.iter().map(|row| row.2).collect();

        sqlx::query(
            "INSERT INTO access_response_size_logs (id, response_id, body_length, at_second)
             SELECT t.id, t.response_id, t.body_length::uint8, date_trunc('second', now())
               FROM unnest($1::text[], $2::text[], $3::bigint[]) AS t(id, response_id, body_length)
             ON CONFLICT (response_id, at_second)
             DO UPDATE SET body_length = access_response_size_logs.body_length + EXCLUDED.body_length",
        )
        .bind(&size_ids)
        .bind(&response_ids)
        .bind(&lengths)
        .execute(&mut *tx)
        .await
        .context("failed to insert access_response_size_logs")?;
    }

    tx.commit()
        .await
        .context("failed to commit the access log transaction")?;

    Ok(())
}

/// 24 hex characters: seconds + nanoseconds + in-process counter.
pub fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() as u32;
    let nanos = now.subsec_nanos();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed) as u32;

    format!("{seconds:08x}{nanos:08x}{counter:08x}")
}

pub fn headers_json(headers: &HeaderMap) -> String {
    let mut map = serde_json::Map::with_capacity(headers.len());
    for (name, value) in headers {
        let value = value
            .to_str()
            .map(str::to_string)
            .unwrap_or_else(|_| String::from_utf8_lossy(value.as_bytes()).into_owned());
        map.insert(name.as_str().to_string(), serde_json::Value::String(value));
    }
    serde_json::Value::Object(map).to_string()
}

pub fn content_length(headers: &HeaderMap) -> u64 {
    headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

pub fn version_label(version: http::Version) -> String {
    match version {
        http::Version::HTTP_09 => "HTTP/0.9".to_string(),
        http::Version::HTTP_10 => "HTTP/1.0".to_string(),
        http::Version::HTTP_11 => "HTTP/1.1".to_string(),
        http::Version::HTTP_2 => "HTTP/2.0".to_string(),
        http::Version::HTTP_3 => "HTTP/3.0".to_string(),
        _ => format!("{version:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_events_are_counted() {
        let (before, _) = metrics();
        record_dropped();
        let (after, _) = metrics();
        assert!(after > before);
    }

    #[test]
    fn ids_are_24_hex_and_unique() {
        let first = next_id();
        let second = next_id();
        assert_eq!(first.len(), 24);
        assert_ne!(first, second);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn headers_are_serialized_as_json() {
        let mut headers = HeaderMap::new();
        headers.insert("x-test", "value".parse().unwrap());
        let json = headers_json(&headers);
        assert!(json.contains("\"x-test\":\"value\""));
    }

    #[test]
    fn parses_content_length() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, "42".parse().unwrap());
        assert_eq!(content_length(&headers), 42);
        assert_eq!(content_length(&HeaderMap::new()), 0);
    }
}
