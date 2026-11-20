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
    pub upstream_tls: UpstreamTlsConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstreamTlsConfig {
    /// Extra PEM CA certificates trusted for https backends.
    pub ca_file: Option<String>,
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
    /// UDP ports for HTTP/3; empty disables the QUIC listener.
    pub http3_ports: Vec<u16>,
    /// Global in-memory cache limit in bytes.
    pub cache_max_size_bytes: u64,
    /// Maximum concurrently served TCP connections across all listeners.
    pub max_connections: usize,
    /// Maximum HTTP/1 header buffer and HTTP/2 header-list size.
    pub max_header_bytes: usize,
    /// Maximum declared/streamed request body size.
    pub max_request_body_bytes: u64,
    /// Maximum response body buffered for caching or HTTP/3 delivery.
    pub max_response_body_bytes: u64,
    /// Ports allowed as HTTP CONNECT tunnel targets.
    pub connect_ports: Vec<u16>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_string(),
            ports: vec![80, 443],
            http3_ports: Vec::new(),
            cache_max_size_bytes: 16 * 1024 * 1024,
            max_connections: 4096,
            max_header_bytes: 32 * 1024,
            max_request_body_bytes: 64 * 1024 * 1024,
            max_response_body_bytes: 64 * 1024 * 1024,
            connect_ports: vec![443, 80],
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
        overlay_string("GATEWAY_UPSTREAM_CA_FILE", &mut config.upstream_tls.ca_file);

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
        if let Ok(raw) = std::env::var("GATEWAY_HTTP3_PORTS") {
            config.server.http3_ports = raw
                .split(',')
                .filter_map(|item| item.trim().parse().ok())
                .collect();
        }
        if let Ok(raw) = std::env::var("GATEWAY_CACHE_MAX_SIZE")
            && let Ok(bytes) = raw.parse::<u64>()
            && bytes > 0
        {
            config.server.cache_max_size_bytes = bytes;
        }
        overlay_usize(
            "GATEWAY_MAX_CONNECTIONS",
            &mut config.server.max_connections,
        );
        overlay_usize(
            "GATEWAY_MAX_HEADER_BYTES",
            &mut config.server.max_header_bytes,
        );
        overlay_u64(
            "GATEWAY_MAX_REQUEST_BODY",
            &mut config.server.max_request_body_bytes,
        );
        overlay_u64(
            "GATEWAY_MAX_RESPONSE_BODY",
            &mut config.server.max_response_body_bytes,
        );
        if let Ok(raw) = std::env::var("GATEWAY_CONNECT_PORTS") {
            config.server.connect_ports = raw
                .split(',')
                .filter_map(|item| item.trim().parse().ok())
                .collect();
        }

        // Keep values in a range the server/builder can actually use.
        config.server.max_connections = config.server.max_connections.max(1);
        config.server.max_header_bytes = config.server.max_header_bytes.max(8192);
        config.server.max_request_body_bytes = config.server.max_request_body_bytes.max(1024);
        config.server.max_response_body_bytes = config.server.max_response_body_bytes.max(1024);

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

fn overlay_usize(key: &str, target: &mut usize) {
    if let Ok(raw) = std::env::var(key)
        && let Ok(value) = raw.trim().parse::<usize>()
    {
        *target = value;
    }
}

fn overlay_u64(key: &str, target: &mut u64) {
    if let Ok(raw) = std::env::var(key)
        && let Ok(value) = raw.trim().parse::<u64>()
    {
        *target = value;
    }
}
