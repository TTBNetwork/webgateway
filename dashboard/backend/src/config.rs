//! Dashboard configuration: config.toml plus environment overrides.

use std::io;

use anyhow::{Context, bail};
use serde::Deserialize;
use shared::database::DatabaseConfig;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database: DatabaseConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub port: u16,
    /// When set, /api routes require `Authorization: Bearer <token>`.
    pub api_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 3000,
            api_token: None,
        }
    }
}

pub fn load() -> anyhow::Result<Config> {
    let mut config: Config = match std::fs::read_to_string("config.toml") {
        Ok(raw) => toml::from_str(&raw).context("failed to parse config.toml")?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(error) => return Err(error).context("failed to read config.toml"),
    };

    config.database.apply_env_overrides();

    if let Ok(raw) = std::env::var("DASHBOARD_API_PORT") {
        config.server.port = raw
            .parse()
            .context("DASHBOARD_API_PORT is not a valid port")?;
    }

    if let Ok(token) = std::env::var("DASHBOARD_API_TOKEN") {
        config.server.api_token = Some(token);
    }

    if config.database.url.trim().is_empty() {
        bail!("database url is not configured: set DATABASE_URL or [database].url in config.toml");
    }

    Ok(config)
}
