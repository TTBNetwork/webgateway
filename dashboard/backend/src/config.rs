//! Dashboard configuration: config.toml plus environment overrides.

use std::{io, path::PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;
use shared::{database::DatabaseConfig, stats::StatsConfig};
use tracing::{Level, event};

/// Minimum accepted lifetime for a panel session.
pub const MIN_SESSION_TTL_SECS: u64 = 300;
/// Maximum accepted lifetime for a panel session (30 days).
pub const MAX_SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// Default panel session lifetime (7 days).
pub const DEFAULT_SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// Service tokens shorter than this are reported as weak (not rejected).
const MIN_RECOMMENDED_TOKEN_LEN: usize = 24;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database: DatabaseConfig,
    pub server: ServerConfig,
    pub stats: StatsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub port: u16,
    /// Deprecated single service token. Merged into the service token list.
    pub api_token: Option<String>,
    /// Service tokens accepted as Authorization: Bearer <token>. More than one
    /// entry keeps the old token valid while a new one is rolled out.
    pub api_tokens: Vec<String>,
    /// Panel session (JWT + HttpOnly cookie) lifetime in seconds.
    pub session_ttl_secs: u64,
    /// Serve the built panel from this directory (SPA fallback). When unset the
    /// dashboard only exposes the API and /health.
    pub static_dir: Option<PathBuf>,
    /// Optional Prometheus metrics listener; disabled when absent.
    pub metrics_port: Option<u16>,
    /// Bind address for the metrics listener (loopback by default).
    pub metrics_bind: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 3000,
            api_token: None,
            api_tokens: Vec::new(),
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            static_dir: None,
            metrics_port: None,
            metrics_bind: "127.0.0.1".to_string(),
        }
    }
}

impl ServerConfig {
    /// Every configured service token, de-duplicated and without empty values.
    pub fn service_tokens(&self) -> Vec<String> {
        let mut tokens: Vec<String> = std::iter::once(&self.api_token)
            .flatten()
            .chain(self.api_tokens.iter())
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
            .collect();
        tokens.sort();
        tokens.dedup();
        tokens
    }
}

pub fn load() -> anyhow::Result<Config> {
    let mut config: Config = match std::fs::read_to_string("config.toml") {
        Ok(raw) => toml::from_str(&raw).context("failed to parse config.toml")?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(error) => return Err(error).context("failed to read config.toml"),
    };

    config.database.apply_env_overrides();

    if let Ok(raw) = std::env::var("STATS_RAW_RETENTION_DAYS") {
        config.stats.raw_retention_days = raw
            .trim()
            .parse()
            .context("STATS_RAW_RETENTION_DAYS is not a number")?;
    }
    let requested_days = config.stats.raw_retention_days;
    config.stats = config.stats.sanitized();
    if config.stats.raw_retention_days != requested_days {
        event!(
            Level::WARN,
            requested = requested_days,
            applied = config.stats.raw_retention_days,
            "clamped the raw access-log retention"
        );
    }

    if let Ok(raw) = std::env::var("DASHBOARD_API_PORT") {
        config.server.port = raw
            .parse()
            .context("DASHBOARD_API_PORT is not a valid port")?;
    }

    if let Ok(tokens) = std::env::var("DASHBOARD_API_TOKENS") {
        config
            .server
            .api_tokens
            .extend(tokens.split(',').map(str::to_string));
    }
    if let Ok(token) = std::env::var("DASHBOARD_API_TOKEN") {
        config.server.api_tokens.push(token);
    }
    if let Ok(dir) = std::env::var("DASHBOARD_STATIC_DIR") {
        config.server.static_dir = Some(PathBuf::from(dir));
    }
    if let Ok(bind) = std::env::var("DASHBOARD_METRICS_BIND") {
        config.server.metrics_bind = bind;
    }
    if let Ok(raw) = std::env::var("DASHBOARD_METRICS_PORT") {
        config.server.metrics_port = Some(
            raw.trim()
                .parse()
                .context("DASHBOARD_METRICS_PORT is not a valid port")?,
        );
    }
    if let Ok(raw) = std::env::var("DASHBOARD_TOKEN_EXPIRES") {
        config.server.session_ttl_secs = raw
            .parse()
            .context("DASHBOARD_TOKEN_EXPIRES is not a number of seconds")?;
    }
    config.server.session_ttl_secs = config
        .server
        .session_ttl_secs
        .clamp(MIN_SESSION_TTL_SECS, MAX_SESSION_TTL_SECS);

    if config.database.url.trim().is_empty() {
        bail!("database url is not configured: set DATABASE_URL or [database].url in config.toml");
    }

    if let Some(dir) = &config.server.static_dir {
        if !dir.is_dir() {
            bail!("static dir is not a directory: {}", dir.display());
        }
        if !dir.join("index.html").is_file() {
            bail!("static dir does not contain index.html: {}", dir.display());
        }
    }

    for token in config.server.service_tokens() {
        if token.len() < MIN_RECOMMENDED_TOKEN_LEN {
            event!(
                Level::WARN,
                token_len = token.len(),
                "service token is shorter than the recommended length"
            );
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_and_deduplicates_service_tokens() {
        let server = ServerConfig {
            api_token: Some(" legacy ".to_string()),
            api_tokens: vec![
                "new-token".to_string(),
                "legacy".to_string(),
                "  ".to_string(),
            ],
            ..ServerConfig::default()
        };
        assert_eq!(server.service_tokens(), vec!["legacy", "new-token"]);
    }

    #[test]
    fn clamps_the_session_ttl() {
        assert_eq!(
            1_u64.clamp(MIN_SESSION_TTL_SECS, MAX_SESSION_TTL_SECS),
            MIN_SESSION_TTL_SECS
        );
        assert_eq!(
            u64::MAX.clamp(MIN_SESSION_TTL_SECS, MAX_SESSION_TTL_SECS),
            MAX_SESSION_TTL_SECS
        );
    }
}
