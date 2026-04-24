use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use shared::default::default_database_max_connections;
use tracing::{Level, event};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainConfig {
    #[serde(default = "config_database_url")]
    pub database: String,
    #[serde(
        default = "config_max_connections",
        rename = "database_max_connections"
    )]
    pub max_connections: u32,
}

impl Default for MainConfig {
    fn default() -> Self {
        Self {
            database: config_database_url(),
            max_connections: config_max_connections(),
        }
    }
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
    match env {
        Ok(res) => res,
        Err(e) => {
            event!(Level::WARN, "DATABASE_URL is not set. Error: {e:?}");
            "".to_string()
        }
    }
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
