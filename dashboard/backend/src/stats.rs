//! Access log rollups: per-second stats, minute/daily rollups and pruning.
//!
//! Runs in the master process, which is the only long-lived writer-side process
//! that is guaranteed to be unique. A transaction-level advisory lock keeps
//! multiple dashboard instances from aggregating the same window twice.

use std::time::Duration;

use anyhow::Context;
use shared::database::Database;
use tracing::{Level, event};

/// Arbitrary stable key for the stats rollup advisory lock.
const STATS_LOCK: i64 = 0x7767_7374_6174_7301;
const INTERVAL_SECS: u64 = 10;
const PRUNE_EVERY_TICKS: u64 = 360; // about one hour

pub fn spawn(database: Database) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(INTERVAL_SECS));
        let mut ticks = 0u64;

        loop {
            interval.tick().await;
            let prune = ticks.is_multiple_of(PRUNE_EVERY_TICKS);
            ticks = ticks.wrapping_add(1);

            if let Err(error) = run_once(&database, prune).await {
                event!(Level::WARN, error = %format!("{error:#}"), "access stats rollup failed");
            }
        }
    })
}

async fn run_once(database: &Database, prune: bool) -> anyhow::Result<()> {
    let mut tx = database
        .pool()
        .begin()
        .await
        .context("failed to start the stats transaction")?;

    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(STATS_LOCK)
        .fetch_one(&mut *tx)
        .await
        .context("failed to acquire the stats advisory lock")?;
    if !locked {
        tx.rollback().await.ok();
        return Ok(());
    }

    sqlx::query(SECOND_ROLLUP)
        .execute(&mut *tx)
        .await
        .context("failed to roll up per-second stats")?;
    sqlx::query(MINUTE_ROLLUP)
        .execute(&mut *tx)
        .await
        .context("failed to roll up minute stats")?;
    sqlx::query(DAILY_ROLLUP)
        .execute(&mut *tx)
        .await
        .context("failed to roll up daily stats")?;

    if prune {
        for query in PRUNE_QUERIES {
            sqlx::query(query)
                .execute(&mut *tx)
                .await
                .context("failed to prune access logs")?;
        }
    }

    tx.commit()
        .await
        .context("failed to commit the stats transaction")?;
    Ok(())
}

/// Recompute the recent per-second window. Upserts replace values, so the
/// window can be replayed safely after delayed log batches.
const SECOND_ROLLUP: &str = r#"
WITH req AS (
    SELECT date_trunc('second', requested_at) AS second_at,
           COALESCE(website_id, '__unknown__') AS website_id,
           COUNT(*) AS total_requests,
           COUNT(DISTINCT remote_addr) AS total_ips,
           COALESCE(SUM(body_length::numeric), 0) AS request_size
      FROM access_request_logs
     WHERE requested_at >= now() - interval '90 seconds'
       AND requested_at < now() - interval '2 seconds'
     GROUP BY 1, 2
),
resp AS (
    SELECT date_trunc('second', responsed_at) AS second_at,
           COALESCE(website_id, '__unknown__') AS website_id,
           COUNT(*) AS total_responses,
           COUNT(*) FILTER (WHERE status::int BETWEEN 400 AND 499) AS e4xx,
           COUNT(*) FILTER (WHERE status::int BETWEEN 500 AND 599) AS e5xx,
           COUNT(*) FILTER (WHERE status::int IN (502, 504)) AS backend_errors,
           COALESCE(SUM(COALESCE(body_length, 0::bigint::uint8)::numeric), 0) AS response_size
      FROM access_response_logs
     WHERE responsed_at >= now() - interval '90 seconds'
       AND responsed_at < now() - interval '2 seconds'
     GROUP BY 1, 2
)
INSERT INTO access_stats_second (
    second_at, website_id, total_requests, total_responses, total_ips,
    e4xx_requests, e5xx_requests, backend_error_requests,
    total_requests_size, total_response_size)
SELECT COALESCE(req.second_at, resp.second_at),
       COALESCE(req.website_id, resp.website_id),
       COALESCE(req.total_requests, 0),
       COALESCE(resp.total_responses, 0),
       COALESCE(req.total_ips, 0),
       COALESCE(resp.e4xx, 0),
       COALESCE(resp.e5xx, 0),
       COALESCE(resp.backend_errors, 0),
       LEAST(COALESCE(req.request_size, 0), 9223372036854775807::numeric)::bigint::uint8,
       LEAST(COALESCE(resp.response_size, 0), 9223372036854775807::numeric)::bigint::uint8
  FROM req
  FULL OUTER JOIN resp
    ON req.second_at = resp.second_at
   AND req.website_id = resp.website_id
ON CONFLICT (second_at, website_id) DO UPDATE SET
    total_requests = EXCLUDED.total_requests,
    total_responses = EXCLUDED.total_responses,
    total_ips = EXCLUDED.total_ips,
    e4xx_requests = EXCLUDED.e4xx_requests,
    e5xx_requests = EXCLUDED.e5xx_requests,
    backend_error_requests = EXCLUDED.backend_error_requests,
    total_requests_size = EXCLUDED.total_requests_size,
    total_response_size = EXCLUDED.total_response_size
"#;

/// Move complete minutes older than one hour into the minute table and delete
/// their second rows in the same statement.
const MINUTE_ROLLUP: &str = r#"
WITH rolled AS (
    DELETE FROM access_stats_second
     WHERE second_at < date_trunc('minute', now() - interval '1 hour')
    RETURNING second_at, website_id, total_requests, total_responses, total_ips,
              e4xx_requests, e5xx_requests, backend_error_requests,
              total_requests_size, total_response_size
)
INSERT INTO access_stats_minute (
    minute_at, website_id, total_requests, total_responses, total_ips,
    e4xx_requests, e5xx_requests, backend_error_requests,
    total_requests_size, total_response_size)
SELECT date_trunc('minute', second_at),
       website_id,
       SUM(total_requests),
       SUM(total_responses),
       SUM(total_ips),
       SUM(e4xx_requests),
       SUM(e5xx_requests),
       SUM(backend_error_requests),
       LEAST(SUM(total_requests_size::numeric), 9223372036854775807::numeric)::bigint::uint8,
       LEAST(SUM(total_response_size::numeric), 9223372036854775807::numeric)::bigint::uint8
  FROM rolled
 GROUP BY 1, 2
ON CONFLICT (minute_at, website_id) DO UPDATE SET
    total_requests = EXCLUDED.total_requests,
    total_responses = EXCLUDED.total_responses,
    total_ips = EXCLUDED.total_ips,
    e4xx_requests = EXCLUDED.e4xx_requests,
    e5xx_requests = EXCLUDED.e5xx_requests,
    backend_error_requests = EXCLUDED.backend_error_requests,
    total_requests_size = EXCLUDED.total_requests_size,
    total_response_size = EXCLUDED.total_response_size
"#;

/// Move complete days into the daily table; minute rows for older days are
/// removed in the same statement. Daily rows are kept indefinitely.
const DAILY_ROLLUP: &str = r#"
WITH rolled AS (
    DELETE FROM access_stats_minute
     WHERE minute_at < date_trunc('day', now())
    RETURNING minute_at, website_id, total_requests, total_responses, total_ips,
              e4xx_requests, e5xx_requests, backend_error_requests,
              total_requests_size, total_response_size
)
INSERT INTO access_stats_daily (
    day, website_id, total_requests, total_responses, total_ips,
    e4xx_requests, e5xx_requests, backend_error_requests,
    total_requests_size, total_response_size)
SELECT minute_at::date,
       website_id,
       SUM(total_requests),
       SUM(total_responses),
       SUM(total_ips),
       SUM(e4xx_requests),
       SUM(e5xx_requests),
       SUM(backend_error_requests),
       LEAST(SUM(total_requests_size::numeric), 9223372036854775807::numeric)::bigint::uint8,
       LEAST(SUM(total_response_size::numeric), 9223372036854775807::numeric)::bigint::uint8
  FROM rolled
 GROUP BY 1, 2
ON CONFLICT (day, website_id) DO UPDATE SET
    total_requests = EXCLUDED.total_requests,
    total_responses = EXCLUDED.total_responses,
    total_ips = EXCLUDED.total_ips,
    e4xx_requests = EXCLUDED.e4xx_requests,
    e5xx_requests = EXCLUDED.e5xx_requests,
    backend_error_requests = EXCLUDED.backend_error_requests,
    total_requests_size = EXCLUDED.total_requests_size,
    total_response_size = EXCLUDED.total_response_size
"#;

/// Raw access logs are kept for 7 days. Weekly partitions are left in place;
/// pruning removes rows inside them.
const PRUNE_QUERIES: &[&str] = &[
    "DELETE FROM access_request_logs WHERE requested_at < now() - interval '7 days'",
    "DELETE FROM access_response_logs WHERE responsed_at < now() - interval '7 days'",
    "DELETE FROM access_request_size_logs WHERE at_second < now() - interval '7 days'",
    "DELETE FROM access_response_size_logs WHERE at_second < now() - interval '7 days'",
];
