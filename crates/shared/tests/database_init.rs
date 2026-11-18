//! Integration test for master initialization and gateway readiness.
//!
//! Runs only when DATABASE_URL is set; otherwise it is a no-op so CI (which
//! cannot reach the internal database) stays green. A throwaway database is
//! created and dropped, so the target database is left untouched.

use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use shared::database::{Database, DatabaseConfig, schema_version, wait_for_master_schema};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

#[tokio::test]
async fn master_init_unlocks_gateway_readiness() -> anyhow::Result<()> {
    let Some(base_url) = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
    else {
        eprintln!("skipping database integration test: DATABASE_URL is not set");
        return Ok(());
    };

    let admin = admin_pool(&base_url).await?;
    let database_name = format!(
        "webgateway_it_{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros()
    );

    if let Err(error) = sqlx::query(&format!("CREATE DATABASE \"{database_name}\""))
        .execute(&admin)
        .await
    {
        eprintln!(
            "skipping database integration test: cannot create a temporary database: {error}"
        );
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

    // A gateway that starts before the master must wait and then time out.
    let error = match wait_for_master_schema(&config).await {
        Ok(_) => panic!("an uninitialized schema must not be considered ready"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(message.contains("timed out"), "unexpected error: {message}");

    // The master applies migrations.
    let master = Database::connect(&config).await?;
    master.initialize_master(&config).await?;
    master.verify_schema().await?;

    let version: Option<i32> = sqlx::query_scalar("SELECT version FROM schema_state WHERE id = 1")
        .fetch_optional(master.pool())
        .await?;
    assert_eq!(version.map(i64::from), Some(schema_version()));

    let extensions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_extension WHERE extname IN ('uint128', 'btree_gin')",
    )
    .fetch_one(master.pool())
    .await?;
    assert_eq!(extensions, 2);

    let gateways: bool = sqlx::query_scalar("SELECT to_regclass('public.gateways') IS NOT NULL")
        .fetch_one(master.pool())
        .await?;
    assert!(gateways);

    let partitions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_inherits i JOIN pg_class p ON p.oid = i.inhparent WHERE p.relname = 'access_request_logs'",
    )
    .fetch_one(master.pool())
    .await?;
    assert!(partitions > 0, "weekly partitions were not pre-created");

    // Re-running initialization is a no-op (sqlx records applied migrations).
    master.initialize_master(&config).await?;

    // A gateway now becomes ready immediately.
    let gateway = wait_for_master_schema(&DatabaseConfig {
        wait_timeout_secs: 5,
        ..config.clone()
    })
    .await?;
    gateway.verify_schema().await?;

    master.pool().close().await;
    gateway.pool().close().await;

    sqlx::query(&format!(
        "DROP DATABASE IF EXISTS \"{database_name}\" WITH (FORCE)"
    ))
    .execute(&admin)
    .await
    .context("failed to drop the temporary test database")?;
    admin.close().await;

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

#[test]
fn replaces_the_database_segment_of_a_url() {
    assert_eq!(
        replace_database("postgres://user:pass@host:5432/original", "temp"),
        "postgres://user:pass@host:5432/temp"
    );
    assert_eq!(
        replace_database(
            "postgres://user:pass@host:5432/original?sslmode=disable",
            "temp"
        ),
        "postgres://user:pass@host:5432/temp?sslmode=disable"
    );
    assert_eq!(
        replace_database("postgres://host:5432", "temp"),
        "postgres://host:5432/temp"
    );
}
