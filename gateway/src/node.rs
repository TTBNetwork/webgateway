//! Node identity and gateway registration (online / heartbeat / offline).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use machine_id::MachineId;
use shared::database::Database;
use tracing::{Level, event};

use crate::config::NodeConfig;

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub machine_id: String,
    pub name: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub address: Option<String>,
    pub version: &'static str,
}

impl NodeInfo {
    pub fn new(config: &NodeConfig) -> Self {
        Self {
            machine_id: MachineId::get().to_string(),
            name: config.name.clone(),
            country: config.country.clone(),
            city: config.city.clone(),
            address: config.address.clone(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    pub fn label(&self) -> String {
        match (&self.country, &self.city) {
            (Some(country), Some(city)) if !country.is_empty() && !city.is_empty() => {
                format!("{country}-{city}")
            }
            _ => self
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| self.machine_id.clone()),
        }
    }

    /// HTTP header value: "<country-city>-<hash>" where the hash mixes the
    /// location and the machine id, e.g. "cn-shanghai-1f3c9a...".
    pub fn header_label(&self) -> String {
        let location = self.location_label();
        let hash = fnv1a64(format!("{location}{}", self.machine_id).as_bytes());
        format!("{location}-{hash:016x}")
    }

    fn location_label(&self) -> String {
        let raw = match (&self.country, &self.city) {
            (Some(country), Some(city)) if !country.is_empty() && !city.is_empty() => {
                format!("{country}-{city}")
            }
            _ => self
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "node".to_string()),
        };

        let sanitized: String = raw
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        let sanitized = sanitized.trim_matches('-');
        if sanitized.is_empty() {
            "node".to_string()
        } else {
            sanitized.to_string()
        }
    }

    pub async fn register_online(&self, database: &Database) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO gateways (id, machine_id, name, country, city, address, version, status, last_seen_at, started_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'online', now(), now())
             ON CONFLICT (machine_id) DO UPDATE SET
                 name = EXCLUDED.name,
                 country = EXCLUDED.country,
                 city = EXCLUDED.city,
                 address = EXCLUDED.address,
                 version = EXCLUDED.version,
                 status = 'online',
                 last_seen_at = now(),
                 started_at = now(),
                 stopped_at = NULL",
        )
        .bind(generate_id())
        .bind(&self.machine_id)
        .bind(self.name.as_deref())
        .bind(self.country.as_deref())
        .bind(self.city.as_deref())
        .bind(self.address.as_deref())
        .bind(self.version)
        .execute(database.pool())
        .await
        .context("failed to register this gateway in PostgreSQL")?;

        event!(
            Level::INFO,
            machine_id = %self.machine_id,
            node = %self.label(),
            version = self.version,
            "gateway marked online"
        );
        Ok(())
    }

    pub async fn heartbeat(&self, database: &Database) -> anyhow::Result<()> {
        sqlx::query("UPDATE gateways SET last_seen_at = now() WHERE machine_id = $1")
            .bind(&self.machine_id)
            .execute(database.pool())
            .await
            .context("failed to update the gateway heartbeat")?;
        Ok(())
    }

    pub async fn mark_offline(&self, database: &Database) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE gateways SET status = 'offline', stopped_at = now(), last_seen_at = now()
              WHERE machine_id = $1",
        )
        .bind(&self.machine_id)
        .execute(database.pool())
        .await
        .context("failed to mark the gateway offline")?;

        event!(Level::INFO, machine_id = %self.machine_id, "gateway marked offline");
        Ok(())
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn generate_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:016x}{:08x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> NodeInfo {
        NodeInfo {
            machine_id: "machine-1".to_string(),
            name: Some("edge".to_string()),
            country: Some("CN".to_string()),
            city: Some("Shanghai".to_string()),
            address: None,
            version: "0.1.0",
        }
    }

    #[test]
    fn label_prefers_country_city() {
        assert_eq!(node().label(), "CN-Shanghai");
    }

    #[test]
    fn header_label_contains_location_and_hash() {
        let node = node();
        let label = node.header_label();
        assert!(label.starts_with("cn-shanghai-"));
        assert_eq!(label.len(), "cn-shanghai-".len() + 16);
        assert_eq!(label, node.header_label());
    }

    #[test]
    fn header_label_hashes_the_machine_id() {
        let first = node().header_label();
        let mut other = node();
        other.machine_id = "machine-2".to_string();
        assert_ne!(first, other.header_label());
    }

    #[test]
    fn header_label_falls_back_to_node_for_non_ascii_location() {
        let mut node = node();
        node.city = Some("上海".to_string());
        assert!(node.header_label().starts_with("cn-"));
    }
}
