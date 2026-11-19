//! Gateway configuration: config.toml plus environment overrides.

use std::io;

use anyhow::{Context, bail};
use serde::Deserialize;
use shared::database::DatabaseConfig;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database: DatabaseConfig,
    pub node: NodeConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    pub name: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub address: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub ports: Vec<u16>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_string(),
            ports: vec![80, 443],
        }
    }
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let mut config: Config = match std::fs::read_to_string("config.toml") {
            Ok(raw) => toml::from_str(&raw).context("failed to parse config.toml")?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Config::default(),
            Err(error) => return Err(error).context("failed to read config.toml"),
        };

        config.database.apply_env_overrides();
        overlay_string("GATEWAY_NODE_NAME", &mut config.node.name);
        overlay_string("GATEWAY_NODE_COUNTRY", &mut config.node.country);
        overlay_string("GATEWAY_NODE_CITY", &mut config.node.city);
        overlay_string("GATEWAY_NODE_ADDRESS", &mut config.node.address);

        if let Ok(bind) = std::env::var("GATEWAY_BIND") {
            config.server.bind = bind;
        }
        if let Ok(raw) = std::env::var("GATEWAY_PORTS") {
            let ports: Vec<u16> = raw
                .split(',')
                .filter_map(|item| item.trim().parse().ok())
                .collect();
            if !ports.is_empty() {
                config.server.ports = ports;
            }
        }

        if config.database.url.trim().is_empty() {
            bail!(
                "database url is not configured: set DATABASE_URL or [database].url in config.toml"
            );
        }

        Ok(config)
    }
}

fn overlay_string(key: &str, target: &mut Option<String>) {
    if let Ok(value) = std::env::var(key) {
        *target = Some(value);
    }
}
