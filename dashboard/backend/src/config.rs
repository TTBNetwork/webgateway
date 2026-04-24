use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use shared::default::{default_dashboard_api_port, default_database_max_connections};
use tracing::{Level, event};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainConfig {
    #[serde(default = "config_dashboard_api_port")]
    pub port: u16,
    #[serde(default = "config_database_url")]
    pub database: String,
    #[serde(
        default = "config_max_connections",
        rename = "database_max_connections"
    )]
    pub max_connections: u32,
    #[serde(rename = "token_expires", default = "config_token_expires")]
    pub token_exp: u64,
}

impl Default for MainConfig {
    fn default() -> Self {
        Self {
            database: config_database_url(),
            max_connections: config_max_connections(),
            port: config_dashboard_api_port(),
            token_exp: config_token_expires(),
        }
    }
}

fn config_token_expires() -> u64 {
    // from run env
    let env = std::env::var("TOKEN_EXPIRES");
    if let Ok(v) = env
        && let Ok(value) = v.parse::<u64>()
    {
        return value;
    }
    60 * 60 * 24 * 7
}

fn config_dashboard_api_port() -> u16 {
    // from run env
    let env = std::env::var("DASHBOARD_API_PORT");
    if let Ok(v) = env
        && let Ok(value) = v.parse::<u16>()
    {
        return value;
    }
    default_dashboard_api_port()
}

fn config_max_connections() -> u32 {
    // from run env
    let env = std::env::var("DATABASE_MAX_CONNECTIONS");
    if let Ok(v) = env
        && let Ok(value) = v.parse::<u32>()
    {
        return value;
    }
    default_database_max_connections()
}

fn config_database_url() -> String {
    // from run env
    let env = std::env::var("DATABASE_URL");
    if let Ok(v) = env {
        return v;
    }
    "".to_string()
}

pub static CONFIG: OnceLock<MainConfig> = OnceLock::new();

pub fn init_config() -> anyhow::Result<()> {
    // laod from toml
    let config = match std::fs::read_to_string("config.toml") {
        Ok(content) => match toml::from_str::<MainConfig>(&content) {
            Ok(config) => config,
            Err(_) => {
                event!(Level::WARN, "config.toml is invalid, use default config");
                MainConfig::default()
            }
        },
        Err(_) => {
            event!(Level::INFO, "config.toml not found, use default config");
            MainConfig::default()
        },
    };
    CONFIG.set(config).unwrap();

    Ok(())
}

pub fn get_config() -> &'static MainConfig {
    CONFIG.get().unwrap()
}
