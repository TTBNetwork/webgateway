//! Access log rollups: per-second stats, minute/daily rollups, backfill and pruning.
//!
//! The master is the only long-lived writer-side process that is guaranteed to be
//! unique, so the task lives here and is started by the dashboard. A transaction
//! advisory lock keeps multiple dashboard instances from aggregating the same
//! window twice.
//!
//! Windows are driven by stats_state.last_aggregated_at:
//!
//! * the recent window always recomputes at least the last 90 seconds (the old
//!   fixed window), which keeps delayed log batches idempotent;
//! * the gap between the watermark and that recent window is backfilled from raw
//!   logs: per-second rows for the newest hour, minute rows directly from raw
//!   logs for everything older. This survives arbitrarily long master outages
//!   (up to the 7-day raw log retention).
//!
//! Weekly partitions are created on the first tick and then hourly; dead
//! partitions are dropped whole once they are older than the retention window.

use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use tracing::{Level, event};

use crate::database::Database;

/// Arbitrary stable key for the stats rollup advisory lock.
const STATS_LOCK: i64 = 0x7767_7374_6174_7301;
const INTERVAL_SECS: u64 = 10;
const PRUNE_EVERY_TICKS: u64 = 360; // about one hour
const UPCOMING_WEEKS: i32 = 3;
const MIN_RAW_RETENTION_DAYS: i32 = 7;
const MAX_RAW_RETENTION_DAYS: i32 = 90;

/// Rollup settings supplied by the master.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct StatsConfig {
    /// Raw access log (and weekly partition) retention in days.
    pub raw_retention_days: i32,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            raw_retention_days: 7,
        }
    }
}

impl StatsConfig {
    /// Weekly partitions can only be dropped whole, so retention stays at or
    /// above one week (the SQL helper enforces the same floor).
    pub fn sanitized(mut self) -> Self {
        self.raw_retention_days = self
            .raw_retention_days
            .clamp(MIN_RAW_RETENTION_DAYS, MAX_RAW_RETENTION_DAYS);
        self
    }
}

/// Start the rollup loop. The first tick runs immediately (creating partitions
/// right after a restart) and also performs the hourly maintenance work.
pub fn spawn(database: Database, config: StatsConfig) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(INTERVAL_SECS));
        let mut ticks = 0u64;

        loop {
            interval.tick().await;
            let maintain = ticks.is_multiple_of(PRUNE_EVERY_TICKS);
            ticks = ticks.wrapping_add(1);

            if let Err(error) = run_once(&database, maintain, config).await {
                event!(Level::WARN, error = %format!("{error:#}"), "access stats rollup failed");
            }
        }
    })
}

/// One rollup transaction. Exposed for database integration tests.
pub async fn run_once(
    database: &Database,
    maintain: bool,
    config: StatsConfig,
) -> anyhow::Result<()> {
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

    // Defensive: the migration inserts this row, but a manual cleanup should not
    // disable the watermark forever.
    sqlx::query("INSERT INTO stats_state (id) VALUES (1) ON CONFLICT (id) DO NOTHING")
        .execute(&mut *tx)
        .await
        .context("failed to initialize stats_state")?;

    if maintain {
        sqlx::query("SELECT wg_ensure_upcoming_weeks($1)")
            .bind(UPCOMING_WEEKS)
            .execute(&mut *tx)
            .await
            .context("failed to create upcoming access partitions")?;
        sqlx::query("SELECT wg_drop_expired_weekly_partitions($1)")
            .bind(config.raw_retention_days)
            .execute(&mut *tx)
            .await
            .context("failed to drop expired access partitions")?;
    }

    sqlx::query(BULK_MINUTE_ROLLUP)
        .execute(&mut *tx)
        .await
        .context("failed to backfill minute stats")?;
    sqlx::query(RECENT_SECOND_ROLLUP)
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
    sqlx::query(WATERMARK_UPDATE)
        .execute(&mut *tx)
        .await
        .context("failed to advance the stats watermark")?;

    if maintain {
        for query in PRUNE_QUERIES {
            sqlx::query(query)
                .bind(config.raw_retention_days)
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

/// Backfill minutes directly from raw logs for everything older than the newest
/// hour. The range is a prefix of the watermark gap, so the per-second table
/// never has to hold more than one hour of backfill.
const BULK_MINUTE_ROLLUP: &str = r#"
WITH rollup_window AS (
    SELECT LEAST(
               COALESCE((SELECT last_aggregated_at FROM stats_state WHERE id = 1),
                        now() - interval '90 seconds'),
               now() - interval '90 seconds'
           ) AS start_at,
           now() - interval '2 seconds' AS end_at,
           date_trunc('minute', now() - interval '1 hour') AS recent_floor
),
bounded AS (
    SELECT start_at, LEAST(end_at, recent_floor) AS stop_at
      FROM rollup_window
     WHERE start_at < LEAST(end_at, recent_floor)
),
req AS (
    SELECT date_trunc('minute', r.requested_at) AS minute_at,
           COALESCE(r.website_id, '__unknown__') AS website_id,
           COUNT(*) AS total_requests,
           COUNT(DISTINCT r.remote_addr) AS total_ips,
           COALESCE(SUM(r.body_length::numeric), 0) AS request_size
      FROM access_request_logs r
      JOIN bounded b
        ON r.requested_at >= b.start_at
       AND r.requested_at < b.stop_at
     GROUP BY 1, 2
),
resp AS (
    SELECT date_trunc('minute', s.responsed_at) AS minute_at,
           COALESCE(s.website_id, '__unknown__') AS website_id,
           COUNT(*) AS total_responses,
           COUNT(*) FILTER (WHERE s.status::int BETWEEN 400 AND 499) AS e4xx,
           COUNT(*) FILTER (WHERE s.status::int BETWEEN 500 AND 599) AS e5xx,
           COUNT(*) FILTER (WHERE s.status::int IN (502, 504)) AS backend_errors,
           COALESCE(SUM(COALESCE(s.body_length, 0::bigint::uint8)::numeric), 0) AS response_size
      FROM access_response_logs s
      JOIN bounded b
        ON s.responsed_at >= b.start_at
       AND s.responsed_at < b.stop_at
     GROUP BY 1, 2
)
INSERT INTO access_stats_minute (
    minute_at, website_id, total_requests, total_responses, total_ips,
    e4xx_requests, e5xx_requests, backend_error_requests,
    total_requests_size, total_response_size)
SELECT COALESCE(req.minute_at, resp.minute_at),
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
    ON req.minute_at = resp.minute_at
   AND req.website_id = resp.website_id
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

/// Recompute per-second rows for the newest hour of the watermark gap (but at
/// least the last 90 seconds). Upserts replace values, so the window can be
/// replayed safely after delayed log batches.
const RECENT_SECOND_ROLLUP: &str = r#"
WITH rollup_window AS (
    SELECT LEAST(
               COALESCE((SELECT last_aggregated_at FROM stats_state WHERE id = 1),
                        now() - interval '90 seconds'),
               now() - interval '90 seconds'
           ) AS start_at,
           now() - interval '2 seconds' AS end_at,
           date_trunc('minute', now() - interval '1 hour') AS recent_floor
),
bounded AS (
    SELECT GREATEST(start_at, recent_floor) AS start_at, end_at AS stop_at
      FROM rollup_window
     WHERE GREATEST(start_at, recent_floor) < end_at
),
req AS (
    SELECT date_trunc('second', r.requested_at) AS second_at,
           COALESCE(r.website_id, '__unknown__') AS website_id,
           COUNT(*) AS total_requests,
           COUNT(DISTINCT r.remote_addr) AS total_ips,
           COALESCE(SUM(r.body_length::numeric), 0) AS request_size
      FROM access_request_logs r
      JOIN bounded b
        ON r.requested_at >= b.start_at
       AND r.requested_at < b.stop_at
     GROUP BY 1, 2
),
resp AS (
    SELECT date_trunc('second', s.responsed_at) AS second_at,
           COALESCE(s.website_id, '__unknown__') AS website_id,
           COUNT(*) AS total_responses,
           COUNT(*) FILTER (WHERE s.status::int BETWEEN 400 AND 499) AS e4xx,
           COUNT(*) FILTER (WHERE s.status::int BETWEEN 500 AND 599) AS e5xx,
           COUNT(*) FILTER (WHERE s.status::int IN (502, 504)) AS backend_errors,
           COALESCE(SUM(COALESCE(s.body_length, 0::bigint::uint8)::numeric), 0) AS response_size
      FROM access_response_logs s
      JOIN bounded b
        ON s.responsed_at >= b.start_at
       AND s.responsed_at < b.stop_at
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

/// Move seconds older than one hour into the minute table. `total_ips` is
/// recomputed from raw access logs for the rolled window so the minute value is
/// a real distinct-client count; the per-second sum is only a fallback when raw
/// logs (already pruned) are unavailable.
const MINUTE_ROLLUP: &str = r#"
WITH rolled AS (
    DELETE FROM access_stats_second
     WHERE second_at < date_trunc('minute', now() - interval '1 hour')
    RETURNING second_at, website_id, total_requests, total_responses, total_ips,
              e4xx_requests, e5xx_requests, backend_error_requests,
              total_requests_size, total_response_size
),
agg AS (
    SELECT date_trunc('minute', second_at) AS minute_at,
           website_id,
           SUM(total_requests) AS total_requests,
           SUM(total_responses) AS total_responses,
           SUM(total_ips) AS approx_ips,
           SUM(e4xx_requests) AS e4xx_requests,
           SUM(e5xx_requests) AS e5xx_requests,
           SUM(backend_error_requests) AS backend_error_requests,
           LEAST(SUM(total_requests_size::numeric), 9223372036854775807::numeric)::bigint::uint8 AS total_requests_size,
           LEAST(SUM(total_response_size::numeric), 9223372036854775807::numeric)::bigint::uint8 AS total_response_size
      FROM rolled
     GROUP BY 1, 2
),
rollup_range AS (
    SELECT min(second_at) AS start_at,
           max(second_at) + interval '1 second' AS stop_at
      FROM rolled
),
exact AS (
    SELECT date_trunc('minute', r.requested_at) AS minute_at,
           COALESCE(r.website_id, '__unknown__') AS website_id,
           COUNT(DISTINCT r.remote_addr) AS total_ips
      FROM access_request_logs r
      JOIN rollup_range w
        ON r.requested_at >= w.start_at
       AND r.requested_at < w.stop_at
     GROUP BY 1, 2
)
INSERT INTO access_stats_minute (
    minute_at, website_id, total_requests, total_responses, total_ips,
    e4xx_requests, e5xx_requests, backend_error_requests,
    total_requests_size, total_response_size)
SELECT agg.minute_at,
       agg.website_id,
       agg.total_requests,
       agg.total_responses,
       COALESCE(exact.total_ips, agg.approx_ips),
       agg.e4xx_requests,
       agg.e5xx_requests,
       agg.backend_error_requests,
       agg.total_requests_size,
       agg.total_response_size
  FROM agg
  LEFT JOIN exact
    ON agg.minute_at = exact.minute_at
   AND agg.website_id = exact.website_id
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
/// removed in the same statement. Daily `total_ips` is recomputed from raw logs
/// when the day is still inside the raw retention window, otherwise it falls
/// back to the sum of per-minute distinct counts.
const DAILY_ROLLUP: &str = r#"
WITH rolled AS (
    DELETE FROM access_stats_minute
     WHERE minute_at < date_trunc('day', now())
    RETURNING minute_at, website_id, total_requests, total_responses, total_ips,
              e4xx_requests, e5xx_requests, backend_error_requests,
              total_requests_size, total_response_size
),
agg AS (
    SELECT minute_at::date AS day, website_id,
           SUM(total_requests) AS total_requests,
           SUM(total_responses) AS total_responses,
           SUM(total_ips) AS approx_ips,
           SUM(e4xx_requests) AS e4xx_requests,
           SUM(e5xx_requests) AS e5xx_requests,
           SUM(backend_error_requests) AS backend_error_requests,
           LEAST(SUM(total_requests_size::numeric), 9223372036854775807::numeric)::bigint::uint8 AS total_requests_size,
           LEAST(SUM(total_response_size::numeric), 9223372036854775807::numeric)::bigint::uint8 AS total_response_size
      FROM rolled
     GROUP BY 1, 2
),
rollup_range AS (
    SELECT min(minute_at) AS start_at,
           max(minute_at) + interval '1 minute' AS stop_at
      FROM rolled
),
exact AS (
    SELECT date_trunc('day', r.requested_at)::date AS day,
           COALESCE(r.website_id, '__unknown__') AS website_id,
           COUNT(DISTINCT r.remote_addr) AS total_ips
      FROM access_request_logs r
      JOIN rollup_range w
        ON r.requested_at >= w.start_at
       AND r.requested_at < w.stop_at
     GROUP BY 1, 2
)
INSERT INTO access_stats_daily (
    day, website_id, total_requests, total_responses, total_ips,
    e4xx_requests, e5xx_requests, backend_error_requests,
    total_requests_size, total_response_size)
SELECT agg.day,
       agg.website_id,
       agg.total_requests,
       agg.total_responses,
       COALESCE(exact.total_ips, agg.approx_ips),
       agg.e4xx_requests,
       agg.e5xx_requests,
       agg.backend_error_requests,
       agg.total_requests_size,
       agg.total_response_size
  FROM agg
  LEFT JOIN exact
    ON agg.day = exact.day
   AND agg.website_id = exact.website_id
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

/// Advance the watermark only after every aggregation statement succeeded.
const WATERMARK_UPDATE: &str = r#"
UPDATE stats_state
   SET last_aggregated_at = now() - interval '2 seconds'
 WHERE id = 1
"#;

/// Raw access logs are kept for the configured retention (default 7 days). Whole
/// weekly partitions are dropped by wg_drop_expired_weekly_partitions; these
/// deletes only trim the edge rows of partitions still partially inside the
/// window. The bind parameter is the retention in days.
const PRUNE_QUERIES: &[&str] = &[
    "DELETE FROM access_request_logs WHERE requested_at < now() - make_interval(days => $1)",
    "DELETE FROM access_response_logs WHERE responsed_at < now() - make_interval(days => $1)",
    "DELETE FROM access_request_size_logs WHERE at_second < now() - make_interval(days => $1)",
    "DELETE FROM access_response_size_logs WHERE at_second < now() - make_interval(days => $1)",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_raw_retention_to_at_least_one_week() {
        assert_eq!(
            StatsConfig {
                raw_retention_days: 1,
            }
            .sanitized()
            .raw_retention_days,
            7
        );
        assert_eq!(
            StatsConfig {
                raw_retention_days: 120,
            }
            .sanitized()
            .raw_retention_days,
            90
        );
        assert_eq!(StatsConfig::default().sanitized().raw_retention_days, 7);
    }
}
