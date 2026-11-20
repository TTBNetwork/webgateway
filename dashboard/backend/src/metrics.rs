//! Prometheus metrics for the dashboard control plane.
//!
//! Served from a separate plain-HTTP listener (`[server].metrics_port`, bound
//! to loopback by default) so it can be scraped without exposing the API.

use std::{
    net::IpAddr,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use axum::{Router, extract::State, response::Response, routing::get};
use shared::database::{Database, schema_version};
use sqlx::Row;
use tracing::{Level, event};

static AUTH_FAILURES: AtomicU64 = AtomicU64::new(0);
static AUTH_THROTTLED: AtomicU64 = AtomicU64::new(0);
static API_ERRORS: AtomicU64 = AtomicU64::new(0);
static STARTED: OnceLock<Instant> = OnceLock::new();

pub fn mark_start() {
    let _ = STARTED.set(Instant::now());
}

pub fn record_auth_failure() {
    AUTH_FAILURES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_auth_throttled() {
    AUTH_THROTTLED.fetch_add(1, Ordering::Relaxed);
}

pub fn record_api_error() {
    API_ERRORS.fetch_add(1, Ordering::Relaxed);
}

fn uptime_seconds() -> u64 {
    STARTED
        .get()
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(0)
}

pub(crate) struct Snapshot {
    database_up: bool,
    websites: i64,
    certificates: i64,
    certificates_expiring: i64,
    gateways_online: i64,
    gateways_total: i64,
    users: i64,
    stats_lag_seconds: Option<f64>,
}

impl Snapshot {
    fn unavailable() -> Self {
        Self {
            database_up: false,
            websites: 0,
            certificates: 0,
            certificates_expiring: 0,
            gateways_online: 0,
            gateways_total: 0,
            users: 0,
            stats_lag_seconds: None,
        }
    }
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

fn gauge(out: &mut String, name: &str, value: impl std::fmt::Display) {
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push_str(" gauge\n");
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

pub(crate) fn format_metrics(snapshot: &Snapshot) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("# TYPE dashboard_info gauge\n");
    out.push_str(&format!(
        "dashboard_info{{schema_version=\"{}\"}} 1\n",
        schema_version()
    ));
    gauge(&mut out, "dashboard_up", 1);
    gauge(
        &mut out,
        "dashboard_database_up",
        u8::from(snapshot.database_up),
    );
    gauge(&mut out, "dashboard_uptime_seconds", uptime_seconds());
    gauge(&mut out, "dashboard_websites_total", snapshot.websites);
    gauge(
        &mut out,
        "dashboard_certificates_total",
        snapshot.certificates,
    );
    gauge(
        &mut out,
        "dashboard_certificates_expiring_total",
        snapshot.certificates_expiring,
    );
    gauge(
        &mut out,
        "dashboard_gateways_online",
        snapshot.gateways_online,
    );
    gauge(
        &mut out,
        "dashboard_gateways_total",
        snapshot.gateways_total,
    );
    gauge(&mut out, "dashboard_users_total", snapshot.users);
    gauge(
        &mut out,
        "dashboard_stats_ready",
        u8::from(snapshot.stats_lag_seconds.is_some()),
    );
    gauge(
        &mut out,
        "dashboard_stats_lag_seconds",
        snapshot.stats_lag_seconds.unwrap_or(0.0),
    );
    counter(
        &mut out,
        "dashboard_auth_failures_total",
        AUTH_FAILURES.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "dashboard_auth_throttled_total",
        AUTH_THROTTLED.load(Ordering::Relaxed),
    );
    counter(
        &mut out,
        "dashboard_api_errors_total",
        API_ERRORS.load(Ordering::Relaxed),
    );
    out
}

async fn collect(database: &Database) -> anyhow::Result<Snapshot> {
    let row = sqlx::query(
        "SELECT
            (SELECT count(*) FROM websites) AS websites,
            (SELECT count(*) FROM certificates) AS certificates,
            (SELECT count(*) FROM certificates
              WHERE expires_at IS NOT NULL
                AND expires_at < now() + interval '30 days') AS certificates_expiring,
            (SELECT count(*) FROM gateways
              WHERE status = 'online'
                AND last_seen_at >= now() - interval '30 seconds') AS gateways_online,
            (SELECT count(*) FROM gateways) AS gateways_total,
            (SELECT count(*) FROM users) AS users,
            (SELECT EXTRACT(EPOCH FROM now() - last_aggregated_at)::float8
               FROM stats_state LIMIT 1) AS stats_lag_seconds",
    )
    .fetch_one(database.pool())
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))?;

    Ok(Snapshot {
        database_up: true,
        websites: row.try_get("websites")?,
        certificates: row.try_get("certificates")?,
        certificates_expiring: row.try_get("certificates_expiring")?,
        gateways_online: row.try_get("gateways_online")?,
        gateways_total: row.try_get("gateways_total")?,
        users: row.try_get("users")?,
        stats_lag_seconds: row.try_get("stats_lag_seconds")?,
    })
}

async fn handler(State(database): State<Database>) -> Response<String> {
    let snapshot = match collect(&database).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            event!(
                Level::WARN,
                error = %format!("{error:#}"),
                "dashboard metrics collection failed"
            );
            Snapshot::unavailable()
        }
    };
    let body = format_metrics(&snapshot);
    let mut response = Response::new(body);
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

/// Bind and serve `/metrics` until the task is aborted.
pub async fn spawn(
    bind: IpAddr,
    port: u16,
    database: Database,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind((bind, port))
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to bind the metrics listener on {bind}:{port}: {error}")
        })?;
    let app = Router::new()
        .route("/metrics", get(handler))
        .with_state(database);
    event!(Level::INFO, %bind, port, "metrics listener started");
    Ok(tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            event!(Level::ERROR, %error, "dashboard metrics listener stopped");
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_control_plane_metrics() {
        let text = format_metrics(&Snapshot {
            database_up: true,
            websites: 3,
            certificates: 2,
            certificates_expiring: 1,
            gateways_online: 2,
            gateways_total: 3,
            users: 4,
            stats_lag_seconds: Some(12.5),
        });
        assert!(text.contains("dashboard_info{schema_version="));
        assert!(text.contains("dashboard_database_up 1\n"));
        assert!(text.contains("dashboard_websites_total 3\n"));
        assert!(text.contains("dashboard_certificates_expiring_total 1\n"));
        assert!(text.contains("dashboard_gateways_online 2\n"));
        assert!(text.contains("dashboard_stats_ready 1\n"));
        assert!(text.contains("dashboard_stats_lag_seconds 12.5\n"));
        assert!(text.contains("# TYPE dashboard_auth_failures_total counter"));
    }

    #[test]
    fn reports_unavailable_database() {
        let text = format_metrics(&Snapshot::unavailable());
        assert!(text.contains("dashboard_database_up 0\n"));
        assert!(text.contains("dashboard_stats_ready 0\n"));
    }
}
