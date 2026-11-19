//! PostgreSQL connection handling, master-side migrations and gateway readiness.
//!
//! Only the dashboard (master) process runs migrations. The gateway builds a
//! lazy pool and waits until the master has published a schema version in the
//! single-row schema_state table; it never executes DDL.

use std::{
    fmt,
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde::Deserialize;
use sqlx::{
    PgPool,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tracing::{Level, event};

/// Transaction-scoped advisory lock that serializes master migrations.
///
/// WGV2 + 0001 in ASCII, chosen to avoid collisions with other applications.
pub const SCHEMA_INIT_LOCK: i64 = 0x5747_5632_0000_0001;

/// Migrations are embedded at compile time so deployment is a single binary.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Schema version this binary expects, derived from the embedded migrations.
pub fn schema_version() -> i64 {
    MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

const REQUIRED_EXTENSIONS: &[&str] = &["uint128", "btree_gin"];

const REQUIRED_TABLES: &[&str] = &[
    "schema_state",
    "configurations",
    "users",
    "users_client_secrets",
    "web_log",
    "dns_providers",
    "certificates",
    "websites",
    "gateways",
    "access_request_logs",
    "access_response_logs",
    "access_request_size_logs",
    "access_response_size_logs",
    "access_stats_second",
    "access_stats_minute",
    "access_stats_daily",
    "stats_state",
];

const REQUIRED_COLUMNS: &[(&str, &str)] = &[
    ("users", "role"),
    ("certificates", "signing_started_at"),
    ("access_request_logs", "remote_addr"),
    ("gateways", "machine_id"),
    ("access_stats_second", "second_at"),
    ("access_stats_minute", "minute_at"),
    ("stats_state", "last_aggregated_at"),
];

/// Database settings shared by the master and the gateway.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub connect_timeout_secs: u64,
    pub init_lock_timeout_secs: u64,
    pub wait_timeout_secs: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: 10,
            connect_timeout_secs: 10,
            init_lock_timeout_secs: 60,
            wait_timeout_secs: 300,
        }
    }
}

impl DatabaseConfig {
    /// Overlay DATABASE_* environment variables on top of the TOML values.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(value) = std::env::var("DATABASE_URL") {
            self.url = value;
        }
        overlay_u32("DATABASE_MAX_CONNECTIONS", &mut self.max_connections);
        overlay_u64("DATABASE_CONNECT_TIMEOUT", &mut self.connect_timeout_secs);
        overlay_u64(
            "DATABASE_INIT_LOCK_TIMEOUT",
            &mut self.init_lock_timeout_secs,
        );
        overlay_u64("DATABASE_WAIT_TIMEOUT", &mut self.wait_timeout_secs);
    }
}

fn overlay_u32(key: &str, target: &mut u32) {
    let Ok(raw) = std::env::var(key) else {
        return;
    };
    match raw.parse() {
        Ok(value) => *target = value,
        Err(_) => {
            event!(Level::WARN, env = key, value = %raw, "ignoring non-numeric environment variable")
        }
    }
}

fn overlay_u64(key: &str, target: &mut u64) {
    let Ok(raw) = std::env::var(key) else {
        return;
    };
    match raw.parse() {
        Ok(value) => *target = value,
        Err(_) => {
            event!(Level::WARN, env = key, value = %raw, "ignoring non-numeric environment variable")
        }
    }
}

/// An owned handle around the PostgreSQL connection pool.
#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Connect eagerly. Used by the master, which must fail fast on bad config.
    pub async fn connect(config: &DatabaseConfig) -> anyhow::Result<Self> {
        let options = connect_options(config, "webgateway-dashboard")?;
        let pool = PgPoolOptions::new()
            // Migrations hold one connection for the advisory lock while the
            // migrator uses another one, so always keep at least two.
            .max_connections(config.max_connections.max(2))
            .acquire_timeout(Duration::from_secs(config.connect_timeout_secs.max(1)))
            .connect_with(options)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to PostgreSQL ({})",
                    redact_url(&config.url)
                )
            })?;

        event!(Level::INFO, database = %redact_url(&config.url), "connected to PostgreSQL");
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Read the readiness marker. None means the row has not been written yet.
    async fn marker_version(&self) -> Result<Option<i32>, sqlx::Error> {
        sqlx::query_scalar("SELECT version FROM schema_state WHERE id = 1")
            .fetch_optional(&self.pool)
            .await
    }

    /// Apply all pending migrations under a transaction-scoped advisory lock.
    ///
    /// This is the only migration entry point in the project; the gateway has no
    /// way to call it. The lock timeout keeps a slow peer from blocking startup
    /// forever.
    pub async fn initialize_master(&self, config: &DatabaseConfig) -> anyhow::Result<()> {
        let expected = schema_version();
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start the migration transaction")?;

        sqlx::query("SELECT set_config('lock_timeout', $1, true)")
            .bind(format!("{}s", config.init_lock_timeout_secs.max(1)))
            .execute(&mut *tx)
            .await
            .context("failed to set lock_timeout for the migration transaction")?;

        if let Err(error) = sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SCHEMA_INIT_LOCK)
            .execute(&mut *tx)
            .await
        {
            let timed_out = matches!(
                &error,
                sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03")
            );
            if timed_out {
                bail!(
                    "timed out after {}s waiting for the schema migration lock; another master instance is probably applying migrations. Do not restart in a loop; wait for it to finish",
                    config.init_lock_timeout_secs
                );
            }
            return Err(error).context("failed to acquire the schema migration lock");
        }

        event!(
            Level::INFO,
            expected_version = expected,
            "applying database migrations"
        );
        MIGRATOR
            .run(&self.pool)
            .await
            .context("database migration failed")?;

        tx.commit()
            .await
            .context("failed to commit the migration transaction")?;

        let applied = self.marker_version().await?.unwrap_or_default();
        event!(
            Level::INFO,
            schema_version = applied,
            "database migrations applied"
        );
        Ok(())
    }

    /// Read-only schema assertion used by the master after migrating and by the
    /// gateway after it is allowed to start.
    pub async fn verify_schema(&self) -> anyhow::Result<()> {
        let expected = schema_version();
        let version = self
            .marker_version()
            .await
            .context("failed to read schema_state")?;
        let Some(version) = version else {
            bail!("schema_state is empty: the database was not initialized by the master");
        };

        if i64::from(version) < expected {
            bail!(
                "database schema version {version} is older than expected {expected}; start the dashboard (master) to apply migrations"
            );
        }
        if i64::from(version) > expected {
            event!(
                Level::WARN,
                found = version,
                expected,
                "database schema is newer than this binary expects"
            );
        }

        let extensions: Vec<String> = REQUIRED_EXTENSIONS
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let missing_extensions: Vec<String> = sqlx::query_scalar(
            "SELECT e.name
               FROM unnest($1::text[]) AS e(name)
              WHERE NOT EXISTS (SELECT 1 FROM pg_extension x WHERE x.extname = e.name)",
        )
        .bind(extensions)
        .fetch_all(&self.pool)
        .await
        .context("failed to check required extensions")?;

        let tables: Vec<String> = REQUIRED_TABLES
            .iter()
            .map(|table| format!("public.{table}"))
            .collect();
        let missing_tables: Vec<String> = sqlx::query_scalar(
            "SELECT t.name FROM unnest($1::text[]) AS t(name) WHERE to_regclass(t.name) IS NULL",
        )
        .bind(tables)
        .fetch_all(&self.pool)
        .await
        .context("failed to check required tables")?;

        let column_tables: Vec<String> = REQUIRED_COLUMNS
            .iter()
            .map(|(table, _)| (*table).to_string())
            .collect();
        let column_names: Vec<String> = REQUIRED_COLUMNS
            .iter()
            .map(|(_, column)| (*column).to_string())
            .collect();
        let missing_columns: Vec<String> = sqlx::query_scalar(
            "SELECT c.table_name || '.' || c.column_name
               FROM unnest($1::text[], $2::text[]) AS c(table_name, column_name)
              WHERE NOT EXISTS (
                    SELECT 1 FROM information_schema.columns i
                     WHERE i.table_schema = 'public'
                       AND i.table_name = c.table_name
                       AND i.column_name = c.column_name)",
        )
        .bind(column_tables)
        .bind(column_names)
        .fetch_all(&self.pool)
        .await
        .context("failed to check required columns")?;

        if !missing_extensions.is_empty()
            || !missing_tables.is_empty()
            || !missing_columns.is_empty()
        {
            bail!(
                "database schema is incomplete (missing extensions: {missing_extensions:?}, missing tables: {missing_tables:?}, missing columns: {missing_columns:?}); start the dashboard (master) to apply migrations"
            );
        }

        event!(
            Level::INFO,
            schema_version = version,
            "database schema verified"
        );
        Ok(())
    }

    /// Lightweight liveness probe used by the health endpoint.
    pub async fn health_check(&self) -> anyhow::Result<()> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .context("database health check failed")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum WaitState {
    Connecting,
    MarkerMissing,
    Version { found: i32, expected: i64 },
    Unavailable,
}

impl fmt::Display for WaitState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting => write!(formatter, "connecting to the database"),
            Self::MarkerMissing => write!(formatter, "schema_state row is missing"),
            Self::Version { found, expected } => {
                write!(formatter, "schema version {found} (expected {expected})")
            }
            Self::Unavailable => write!(formatter, "database unreachable"),
        }
    }
}

/// Wait until the master has initialized the schema, then return a ready pool.
///
/// Used by the gateway. Missing tables and connection errors are retried; any
/// other SQL error fails fast because waiting will not fix it. A wait timeout of
/// zero means wait forever.
pub async fn wait_for_master_schema(config: &DatabaseConfig) -> anyhow::Result<Database> {
    let options = connect_options(config, "webgateway-gateway")?;
    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections.max(1))
        .acquire_timeout(Duration::from_secs(config.connect_timeout_secs.max(1)))
        .connect_lazy_with(options);
    let database = Database { pool };

    let expected = schema_version();
    let started = Instant::now();
    let timeout =
        (config.wait_timeout_secs > 0).then(|| Duration::from_secs(config.wait_timeout_secs));

    event!(
        Level::INFO,
        expected_version = expected,
        "waiting for the master to initialize the database schema"
    );

    let mut attempt: u32 = 0;
    let mut state = WaitState::Connecting;
    let mut last_log = Instant::now();
    event!(Level::DEBUG, %state, "starting schema readiness probe");

    loop {
        attempt += 1;
        match database.marker_version().await {
            Ok(Some(version)) if i64::from(version) >= expected => {
                event!(
                    Level::INFO,
                    schema_version = version,
                    waited_secs = started.elapsed().as_secs(),
                    "database schema is ready"
                );
                database.verify_schema().await?;
                return Ok(database);
            }
            Ok(Some(version)) => {
                state = WaitState::Version {
                    found: version,
                    expected,
                };
            }
            Ok(None) => state = WaitState::MarkerMissing,
            Err(error) if is_retryable_wait_error(&error) => {
                state = WaitState::Unavailable;
            }
            Err(error) => {
                return Err(error).context(
                    "failed while waiting for the master to initialize the database schema",
                );
            }
        }

        if let Some(timeout) = timeout
            && started.elapsed() >= timeout
        {
            bail!(
                "timed out after {}s waiting for the master to initialize the database schema; last observed state: {state}; expected schema version: {expected}. Check that the dashboard (master) is running, that DATABASE_URL points to the same database, and check the master logs for migration errors",
                timeout.as_secs()
            );
        }

        if last_log.elapsed() >= Duration::from_secs(10) {
            event!(
                Level::INFO,
                waited_secs = started.elapsed().as_secs(),
                %state,
                "still waiting for the master database schema"
            );
            last_log = Instant::now();
        } else {
            event!(Level::DEBUG, attempt, %state, "schema is not ready yet");
        }

        let delay = Duration::from_secs(u64::from(attempt.clamp(1, 5)));
        tokio::time::sleep(delay).await;
    }
}

fn is_retryable_wait_error(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        // 42P01 = undefined_table: the master has not created schema_state yet.
        sqlx::Error::Database(database_error) => database_error.code().as_deref() == Some("42P01"),
        _ => false,
    }
}

fn connect_options(
    config: &DatabaseConfig,
    application_name: &'static str,
) -> anyhow::Result<PgConnectOptions> {
    if config.url.trim().is_empty() {
        bail!("database url is not configured: set DATABASE_URL or [database].url in config.toml");
    }

    let options = PgConnectOptions::from_str(config.url.trim()).with_context(|| {
        format!(
            "invalid DATABASE_URL (host/database part: {})",
            redact_url(&config.url)
        )
    })?;

    Ok(options.application_name(application_name))
}

/// Hide the password before a URL ends up in a log line or an error message.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let Some((userinfo, host_and_path)) = rest.rsplit_once('@') else {
        return url.to_string();
    };
    let userinfo = match userinfo.split_once(':') {
        Some((user, _)) => format!("{user}:***"),
        None => userinfo.to_string(),
    };
    format!("{scheme}://{userinfo}@{host_and_path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATION_SQL: &str = include_str!("../migrations/0001_initial_schema.sql");
    const MIGRATION_SQL_2: &str = include_str!("../migrations/0002_notify_deletes.sql");
    const MIGRATION_SQL_3: &str = include_str!("../migrations/0003_stats_maintenance.sql");

    #[test]
    fn schema_version_is_positive() {
        assert!(schema_version() >= 3);
    }

    #[test]
    fn migration_uses_internal_postgres_extensions() {
        assert!(MIGRATION_SQL.contains("CREATE EXTENSION IF NOT EXISTS uint128"));
        assert!(MIGRATION_SQL.contains("CREATE EXTENSION IF NOT EXISTS btree_gin"));
        assert!(MIGRATION_SQL.contains("uint2[]"));
        assert!(MIGRATION_SQL.contains("uint8"));
    }

    #[test]
    fn every_migration_marks_its_schema_version_last() {
        assert!(
            MIGRATION_SQL
                .trim_end()
                .ends_with("SELECT wg_mark_schema_version(1);")
        );
        assert!(
            MIGRATION_SQL_2
                .trim_end()
                .ends_with("SELECT wg_mark_schema_version(2);")
        );
        assert!(
            MIGRATION_SQL_3
                .trim_end()
                .ends_with("SELECT wg_mark_schema_version(3);")
        );
    }

    #[test]
    fn stats_maintenance_migration_tracks_watermark_and_partitions() {
        assert!(MIGRATION_SQL_3.contains("CREATE TABLE stats_state"));
        assert!(MIGRATION_SQL_3.contains("last_aggregated_at"));
        assert!(MIGRATION_SQL_3.contains("created_at"));
        assert!(MIGRATION_SQL_3.contains("updated_at"));
        assert!(MIGRATION_SQL_3.contains("wg_drop_expired_weekly_partitions"));
        assert!(MIGRATION_SQL_3.contains("wg_ensure_upcoming_weeks(3)"));
        assert!(MIGRATION_SQL_3.contains("retention days must be >= 7"));
    }

    #[test]
    fn deletes_are_notified_for_config_tables() {
        for table in [
            "configurations",
            "dns_providers",
            "certificates",
            "websites",
        ] {
            assert!(
                MIGRATION_SQL_2.contains(&format!("AFTER DELETE ON {table}")),
                "missing delete notification for {table}"
            );
        }
        assert!(MIGRATION_SQL_2.contains("pg_notify(TG_TABLE_NAME || '_updater'"));
    }

    #[test]
    fn response_status_keeps_non_rfc_codes() {
        assert!(!MIGRATION_SQL.contains("BETWEEN 100 AND 599"));
        assert!(!MIGRATION_SQL.contains("status CHECK"));
    }

    #[test]
    fn migration_creates_gateway_registry_and_stats_tables() {
        for table in [
            "gateways",
            "access_stats_second",
            "access_stats_minute",
            "access_stats_daily",
        ] {
            assert!(
                MIGRATION_SQL.contains(&format!("CREATE TABLE {table} ")),
                "missing table {table}"
            );
        }
    }

    #[test]
    fn every_table_has_created_at() {
        let tables = table_bodies(MIGRATION_SQL);
        assert!(
            tables.len() >= 15,
            "unexpected table count: {}",
            tables.len()
        );

        for (name, body) in tables {
            assert!(
                body.contains("created_at"),
                "table {name} is missing created_at"
            );
        }
    }

    #[test]
    fn access_log_tables_are_partitioned() {
        for table in [
            "access_request_logs",
            "access_response_logs",
            "access_request_size_logs",
            "access_response_size_logs",
        ] {
            let body = table_bodies(MIGRATION_SQL)
                .into_iter()
                .find(|(name, _)| name == table)
                .map(|(_, body)| body)
                .unwrap_or_else(|| panic!("missing table {table}"));
            assert!(
                !body.contains("REFERENCES"),
                "{table} must not have foreign keys"
            );
        }

        assert!(MIGRATION_SQL.contains("PARTITION BY RANGE (requested_at)"));
        assert!(MIGRATION_SQL.contains("PARTITION BY RANGE (responsed_at)"));
        assert!(MIGRATION_SQL.contains("PARTITION BY RANGE (at_second)"));
    }

    #[test]
    fn config_defaults_are_gateway_friendly() {
        let config = DatabaseConfig::default();
        assert_eq!(config.max_connections, 10);
        assert_eq!(config.init_lock_timeout_secs, 60);
        assert_eq!(config.wait_timeout_secs, 300);
    }

    #[test]
    fn redacts_password_from_database_url() {
        assert_eq!(
            redact_url("postgres://webgateway:secret@127.0.0.1:5432/db"),
            "postgres://webgateway:***@127.0.0.1:5432/db"
        );
        assert_eq!(
            redact_url("postgres://user@host/db"),
            "postgres://user@host/db"
        );
    }

    fn table_bodies(sql: &str) -> Vec<(String, String)> {
        let mut tables = Vec::new();
        for (offset, _) in sql.match_indices("CREATE TABLE ") {
            if offset != 0 && !sql[..offset].ends_with('\n') {
                continue;
            }

            let after = &sql[offset + "CREATE TABLE ".len()..];
            let name_end = after
                .find(|c: char| c.is_whitespace() || c == '(')
                .unwrap_or(after.len());
            let name = after[..name_end].to_string();

            let Some(open) = after.find('(') else {
                continue;
            };
            let mut depth = 0usize;
            let mut end = None;
            for (index, character) in after[open..].char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + index + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = end {
                tables.push((name, after[open..end].to_string()));
            }
        }
        tables
    }
}
