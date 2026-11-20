//! Integration test for the watermark-based stats rollup.
//!
//! Runs only when DATABASE_URL is set; otherwise it is a no-op so CI (which
//! cannot reach the internal database) stays green. A throwaway database is
//! created and dropped, so the target database is left untouched.

use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use shared::{
    database::{Database, DatabaseConfig},
    stats::{StatsConfig, run_once},
};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};

#[tokio::test]
async fn backfills_gaps_and_maintains_weekly_partitions() -> anyhow::Result<()> {
    let Some(base_url) = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
    else {
        eprintln!("skipping stats integration test: DATABASE_URL is not set");
        return Ok(());
    };

    let admin = admin_pool(&base_url).await?;
    let database_name = format!(
        "webgateway_stats_it_{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros()
    );

    if let Err(error) = sqlx::query(&format!("CREATE DATABASE \"{database_name}\""))
        .execute(&admin)
        .await
    {
        eprintln!("skipping stats integration test: cannot create a temporary database: {error}");
        admin.close().await;
        return Ok(());
    }

    let config = DatabaseConfig {
        url: replace_database(&base_url, &database_name),
        max_connections: 2,
        connect_timeout_secs: 5,
        init_lock_timeout_secs: 15,
        wait_timeout_secs: 2,
    };

    let master = Database::connect(&config).await?;
    master.initialize_master(&config).await?;
    master.verify_schema().await?;

    let result = run_scenario(&master).await;

    master.pool().close().await;
    sqlx::query(&format!(
        "DROP DATABASE IF EXISTS \"{database_name}\" WITH (FORCE)"
    ))
    .execute(&admin)
    .await
    .context("failed to drop the temporary test database")?;
    admin.close().await;

    result
}

async fn run_scenario(master: &Database) -> anyhow::Result<()> {
    // The migration creates and seeds the singleton watermark row.
    let state_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM stats_state")
        .fetch_one(master.pool())
        .await?;
    assert_eq!(state_rows, 1, "stats_state must be a seeded singleton");

    // Fixed timestamps are passed as text so request and response rows for the
    // same site share exactly the same wall-clock instant.
    let old_ts: String = sqlx::query_scalar("SELECT (now() - interval '2 days')::text")
        .fetch_one(master.pool())
        .await?;
    let mid_ts: String = sqlx::query_scalar("SELECT (now() - interval '2 hours')::text")
        .fetch_one(master.pool())
        .await?;
    let recent_ts: String = sqlx::query_scalar("SELECT (now() - interval '10 seconds')::text")
        .fetch_one(master.pool())
        .await?;

    insert_request(master, "req-old", "site-old", 10, &old_ts).await?;
    insert_response(master, "res-old", "site-old", 100, 200, &old_ts).await?;
    insert_request(master, "req-mid", "site-mid", 20, &mid_ts).await?;
    insert_response(master, "res-mid", "site-mid", 200, 200, &mid_ts).await?;
    insert_request(master, "req-recent", "site-recent", 30, &recent_ts).await?;
    insert_response(master, "res-recent", "site-recent", 300, 404, &recent_ts).await?;

    // Same client twice plus a second client inside one minute: the minute
    // total_ips must be 2 distinct addresses, not 3 requests.
    for (id, ip, seconds) in [
        ("req-ips-1", "198.51.100.21", 0),
        ("req-ips-2", "198.51.100.21", 5),
        ("req-ips-3", "198.51.100.22", 9),
    ] {
        sqlx::query(
            "INSERT INTO access_request_logs
                 (id, host, method, path, headers, http_version, remote_addr,
                  body_length, website_id, requested_at)
             VALUES ($1, 'stats.test', 'GET', '/', '{}'::jsonb, 'HTTP/1.1', $2,
                     1::uint8, 'site-ips',
                     date_trunc('minute', now() - interval '2 hours')
                     + make_interval(secs => $3))",
        )
        .bind(id)
        .bind(ip)
        .bind(seconds)
        .execute(master.pool())
        .await?;
    }

    // Two days ago, two distinct clients across two minutes: the daily
    // total_ips must be 2 distinct addresses, not the sum of two minute counts.
    for (id, ip, seconds) in [
        ("req-day-1", "198.51.100.31", 0),
        ("req-day-2", "198.51.100.31", 5),
        ("req-day-3", "198.51.100.32", 70),
    ] {
        sqlx::query(
            "INSERT INTO access_request_logs
                 (id, host, method, path, headers, http_version, remote_addr,
                  body_length, website_id, requested_at)
             VALUES ($1, 'stats.test', 'GET', '/', '{}'::jsonb, 'HTTP/1.1', $2,
                     1::uint8, 'site-day-ips',
                     date_trunc('day', now() - interval '2 days')
                     + interval '1 hour' + make_interval(secs => $3))",
        )
        .bind(id)
        .bind(ip)
        .bind(seconds)
        .execute(master.pool())
        .await?;
    }

    // A dead partition from eight weeks ago must be dropped by maintenance, even
    // though it still contains a row.
    let old_partition: String = sqlx::query_scalar(
        "SELECT format('access_request_logs_%s_w%s',
                       to_char(date_trunc('week', now() - interval '8 weeks'), 'IYYY'),
                       to_char(date_trunc('week', now() - interval '8 weeks'), 'IW'))",
    )
    .fetch_one(master.pool())
    .await?;
    sqlx::query(
        "SELECT wg_ensure_weekly_partition(
             'access_request_logs',
             date_trunc('week', now() - interval '8 weeks')::date)",
    )
    .execute(master.pool())
    .await?;
    sqlx::query(
        "INSERT INTO access_request_logs
             (id, host, method, path, headers, http_version, remote_addr, body_length,
              website_id, requested_at)
         VALUES ('req-expired', 'expired.test', 'GET', '/', '{}'::jsonb, 'HTTP/1.1',
                 '198.51.100.9', 40::uint8, 'site-old', now() - interval '8 weeks')",
    )
    .execute(master.pool())
    .await?;

    let old_partition_before: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("public.{old_partition}"))
        .fetch_one(master.pool())
        .await?;
    assert!(old_partition_before, "old partition was not created");

    // Simulate a three-day master outage. The rollup must backfill the whole gap.
    sqlx::query("UPDATE stats_state SET last_aggregated_at = now() - interval '3 days'")
        .execute(master.pool())
        .await?;

    run_once(master, true, StatsConfig::default()).await?;

    let old_partition_after: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("public.{old_partition}"))
        .fetch_one(master.pool())
        .await?;
    assert!(!old_partition_after, "expired partition was not dropped");

    // Current week + the next three weeks must exist after maintenance.
    let upcoming: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM generate_series(date_trunc('week', now()),
                                date_trunc('week', now()) + interval '3 weeks',
                                interval '1 week') AS weeks(ts)
          WHERE to_regclass(format('public.access_request_logs_%s_w%s',
                                   to_char(ts, 'IYYY'), to_char(ts, 'IW'))) IS NOT NULL",
    )
    .fetch_one(master.pool())
    .await?;
    assert_eq!(upcoming, 4, "expected the current week and three ahead");

    // Two-day-old logs were rolled directly into the daily table.
    let old = sqlx::query(
        "SELECT total_requests, total_requests_size::text AS request_size,
                total_response_size::text AS response_size
           FROM access_stats_daily
          WHERE website_id = 'site-old'",
    )
    .fetch_one(master.pool())
    .await?;
    assert_eq!(old.try_get::<i64, _>("total_requests")?, 1);
    assert_eq!(old.try_get::<String, _>("request_size")?, "10");
    assert_eq!(old.try_get::<String, _>("response_size")?, "100");

    // Two-hour-old logs may live in minutes or in daily depending on midnight,
    // so aggregate both tiers.
    let mid = sqlx::query(
        "SELECT COALESCE(SUM(total_requests), 0)::bigint AS requests,
                COALESCE(SUM(total_requests_size::numeric), 0)::text AS request_size,
                COALESCE(SUM(total_response_size::numeric), 0)::text AS response_size
           FROM (SELECT total_requests, total_requests_size, total_response_size
                   FROM access_stats_minute WHERE website_id = 'site-mid'
                 UNION ALL
                 SELECT total_requests, total_requests_size, total_response_size
                   FROM access_stats_daily WHERE website_id = 'site-mid') AS stats",
    )
    .fetch_one(master.pool())
    .await?;
    assert_eq!(mid.try_get::<i64, _>("requests")?, 1);
    assert_eq!(mid.try_get::<String, _>("request_size")?, "20");
    assert_eq!(mid.try_get::<String, _>("response_size")?, "200");

    // The newest second stays in the per-second table.
    let recent = sqlx::query(
        "SELECT total_requests, total_responses, e4xx_requests,
                total_requests_size::text AS request_size,
                total_response_size::text AS response_size
           FROM access_stats_second
          WHERE website_id = 'site-recent'",
    )
    .fetch_one(master.pool())
    .await?;
    assert_eq!(recent.try_get::<i64, _>("total_requests")?, 1);
    assert_eq!(recent.try_get::<i64, _>("total_responses")?, 1);
    assert_eq!(recent.try_get::<i64, _>("e4xx_requests")?, 1);
    assert_eq!(recent.try_get::<String, _>("request_size")?, "30");
    assert_eq!(recent.try_get::<String, _>("response_size")?, "300");

    let minute_ips: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(total_ips), 0)::bigint
           FROM access_stats_minute
          WHERE website_id = 'site-ips'",
    )
    .fetch_one(master.pool())
    .await?;
    assert_eq!(
        minute_ips, 2,
        "minute total_ips must count distinct clients"
    );

    let daily_ips: i64 = sqlx::query_scalar(
        "SELECT total_ips FROM access_stats_daily WHERE website_id = 'site-day-ips'",
    )
    .fetch_one(master.pool())
    .await?;
    assert_eq!(daily_ips, 2, "daily total_ips must count distinct clients");

    let watermark_recent: bool = sqlx::query_scalar(
        "SELECT last_aggregated_at > now() - interval '1 minute' FROM stats_state",
    )
    .fetch_one(master.pool())
    .await?;
    assert!(watermark_recent, "the rollup watermark did not advance");

    // The expired partition took its row with it; the three fresh rows survive
    // because they are inside the 7-day raw log retention.
    let raw_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM access_request_logs")
        .fetch_one(master.pool())
        .await?;
    assert_eq!(raw_rows, 9);

    // A second run must not double count.
    let before = total_requests(master).await?;
    run_once(master, false, StatsConfig::default()).await?;
    let after = total_requests(master).await?;
    assert_eq!(before, after, "replaying the rollup double counted stats");

    Ok(())
}

async fn total_requests(master: &Database) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT ((SELECT COALESCE(SUM(total_requests), 0) FROM access_stats_second)
               + (SELECT COALESCE(SUM(total_requests), 0) FROM access_stats_minute)
               + (SELECT COALESCE(SUM(total_requests), 0) FROM access_stats_daily))::bigint",
    )
    .fetch_one(master.pool())
    .await?)
}

async fn insert_request(
    master: &Database,
    id: &str,
    website: &str,
    body_length: i64,
    timestamp: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO access_request_logs
             (id, host, method, path, headers, http_version, remote_addr, body_length,
              website_id, requested_at)
         VALUES ($1, 'stats.test', 'GET', '/', '{}'::jsonb, 'HTTP/1.1',
                 '203.0.113.10', $2::uint8, $3, $4::timestamptz)",
    )
    .bind(id)
    .bind(body_length)
    .bind(website)
    .bind(timestamp)
    .execute(master.pool())
    .await
    .context("failed to insert a test request log")?;
    Ok(())
}

async fn insert_response(
    master: &Database,
    id: &str,
    website: &str,
    body_length: i64,
    status: i64,
    timestamp: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO access_response_logs
             (id, status, headers, http_version, body_length, website_id, responsed_at)
         VALUES ($1, $2::uint2, '{}'::jsonb, 'HTTP/1.1', $3::uint8, $4, $5::timestamptz)",
    )
    .bind(id)
    .bind(status as i32)
    .bind(body_length)
    .bind(website)
    .bind(timestamp)
    .execute(master.pool())
    .await
    .context("failed to insert a test response log")?;
    Ok(())
}

async fn admin_pool(url: &str) -> anyhow::Result<PgPool> {
    let options = PgConnectOptions::from_str(url)?.database("postgres");
    PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("failed to connect to the postgres maintenance database")
}

fn replace_database(url: &str, database: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };

    let scheme_end = base.find("://").map(|index| index + 3).unwrap_or(0);
    let path_start = base[scheme_end..].find('/').map(|index| scheme_end + index);

    let mut result = match path_start {
        Some(index) => base[..index].to_string(),
        None => base.to_string(),
    };
    result.push('/');
    result.push_str(database);
    if let Some(query) = query {
        result.push('?');
        result.push_str(query);
    }
    result
}
