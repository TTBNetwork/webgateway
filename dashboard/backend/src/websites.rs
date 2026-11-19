//! Website (site) create/update/delete for the dashboard API.
//!
//! The gateway reads the same table through LISTEN/NOTIFY; validation here is
//! deliberately conservative so a bad row cannot take a listener down.

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::Value;
use shared::database::Database;
use url::Url;

use crate::auth;

#[derive(Debug, Clone, Deserialize)]
pub struct WebsiteInput {
    #[serde(default)]
    pub name: Option<String>,
    pub hosts: Vec<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
    pub backends: Value,
    #[serde(default = "empty_object")]
    pub config: Value,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

pub fn validate(input: &WebsiteInput) -> anyhow::Result<()> {
    if input.hosts.is_empty() {
        bail!("at least one host is required");
    }
    for host in &input.hosts {
        validate_host(host)?;
    }
    for port in &input.ports {
        if *port == 0 {
            bail!("port 0 is not allowed");
        }
    }
    if !input.config.is_object() {
        bail!("config must be a JSON object");
    }
    if !input.backends.is_array() {
        bail!("backends must be a JSON array");
    }
    let backends = input.backends.as_array().expect("checked above");
    if backends.is_empty() {
        bail!("at least one backend is required");
    }
    for backend in backends {
        validate_backend(backend)?;
    }
    Ok(())
}

fn validate_host(host: &str) -> anyhow::Result<()> {
    let host = host.trim();
    if host.is_empty() {
        bail!("host must not be empty");
    }
    if host.contains('/') || host.contains(':') || host.contains(char::is_whitespace) {
        bail!("invalid host: {host}");
    }
    let host = host.trim_start_matches("*.");
    if host.is_empty() || host.starts_with('.') || host.ends_with('.') {
        bail!("invalid host: {host}");
    }
    Ok(())
}

fn validate_backend(backend: &Value) -> anyhow::Result<()> {
    let object = backend
        .as_object()
        .context("each backend must be a JSON object")?;
    let url = object
        .get("url")
        .and_then(Value::as_str)
        .context("each backend needs a string url")?;
    let parsed = Url::parse(url).with_context(|| format!("invalid backend url: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("backend url must be http(s) with a host: {url}");
    }
    if let Some(weight) = object.get("weight")
        && weight.as_u64().is_none_or(|weight| weight == 0)
    {
        bail!("backend weight must be a positive integer");
    }
    if let Some(priority) = object.get("priority")
        && priority.as_u64().is_none()
    {
        bail!("backend priority must be a non-negative integer");
    }
    if let Some(rules) = object.get("match")
        && !rules.is_object()
    {
        bail!("backend match rules must be a JSON object");
    }
    Ok(())
}

fn ports_as_i32(input: &WebsiteInput) -> Vec<i32> {
    let mut ports: Vec<i32> = input.ports.iter().map(|port| i32::from(*port)).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

pub async fn create(database: &Database, input: WebsiteInput) -> anyhow::Result<String> {
    validate(&input)?;
    let id = auth::random_id()?;
    let hosts: Vec<String> = input
        .hosts
        .iter()
        .map(|host| host.trim().to_ascii_lowercase())
        .collect();
    sqlx::query(
        "INSERT INTO websites (id, name, hosts, ports, backends, config)
         VALUES ($1, $2, $3, $4::int[]::uint2[], $5::jsonb, $6::jsonb)",
    )
    .bind(&id)
    .bind(
        input
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty()),
    )
    .bind(&hosts)
    .bind(ports_as_i32(&input))
    .bind(input.backends.to_string())
    .bind(input.config.to_string())
    .execute(database.pool())
    .await
    .context("failed to create the website")?;
    Ok(id)
}

pub async fn update(database: &Database, id: &str, input: WebsiteInput) -> anyhow::Result<bool> {
    validate(&input)?;
    let hosts: Vec<String> = input
        .hosts
        .iter()
        .map(|host| host.trim().to_ascii_lowercase())
        .collect();
    let result = sqlx::query(
        "UPDATE websites
            SET name = $2, hosts = $3, ports = $4::int[]::uint2[],
                backends = $5::jsonb, config = $6::jsonb
          WHERE id = $1",
    )
    .bind(id)
    .bind(
        input
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty()),
    )
    .bind(&hosts)
    .bind(ports_as_i32(&input))
    .bind(input.backends.to_string())
    .bind(input.config.to_string())
    .execute(database.pool())
    .await
    .context("failed to update the website")?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete(database: &Database, id: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM websites WHERE id = $1")
        .bind(id)
        .execute(database.pool())
        .await
        .context("failed to delete the website")?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(hosts: Vec<&str>, ports: Vec<u16>, backends: Value) -> WebsiteInput {
        WebsiteInput {
            name: None,
            hosts: hosts.into_iter().map(str::to_string).collect(),
            ports,
            backends,
            config: json!({}),
        }
    }

    #[test]
    fn accepts_a_minimal_site() {
        let site = input(
            vec!["example.com", "*.example.com"],
            vec![80, 443],
            json!([{ "url": "http://127.0.0.1:9000" }]),
        );
        assert!(validate(&site).is_ok());
    }

    #[test]
    fn rejects_bad_hosts_backends_and_ports() {
        assert!(validate(&input(vec![], vec![], json!([]))).is_err());
        assert!(
            validate(&input(
                vec!["http://example.com"],
                vec![],
                json!([{ "url": "http://127.0.0.1:9000" }])
            ))
            .is_err()
        );
        assert!(
            validate(&input(
                vec!["example.com"],
                vec![0],
                json!([{ "url": "http://127.0.0.1:9000" }])
            ))
            .is_err()
        );
        assert!(validate(&input(vec!["example.com"], vec![], json!([]))).is_err());
        assert!(
            validate(&input(
                vec!["example.com"],
                vec![],
                json!([{ "url": "ftp://example.com" }])
            ))
            .is_err()
        );
    }
}
