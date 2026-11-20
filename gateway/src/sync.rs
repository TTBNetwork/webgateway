//! Load websites and certificates from PostgreSQL and track a change watermark.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use anyhow::Context;
use tracing::{Level, event};
use url::Url;

use crate::cache::CacheConfig;
use crate::state::{
    AppState, Backend, CertStore, ClientIpMode, MatchRules, ProxyHeaders, Routing, Site,
};
use shared::database::Database;

#[derive(Debug, sqlx::FromRow)]
struct WebsiteRow {
    id: String,
    name: Option<String>,
    hosts: Vec<String>,
    ports: Vec<i32>,
    backends: String,
    config: String,
}

#[derive(Debug, serde::Deserialize)]
struct BackendRow {
    url: String,
    #[serde(default)]
    match_path: Option<String>,
    #[serde(default, rename = "match")]
    rules: MatchRules,
    #[serde(default = "default_weight")]
    weight: u32,
    #[serde(default)]
    priority: u32,
}

fn default_weight() -> u32 {
    1
}

#[derive(Debug, serde::Deserialize, Default)]
struct WebsiteConfigRow {
    #[serde(default)]
    headers: Option<HeaderConfigRow>,
    #[serde(default)]
    get_request_ip: ClientIpMode,
    #[serde(default)]
    cache: CacheConfig,
}

#[derive(Debug, serde::Deserialize, Default)]
struct HeaderConfigRow {
    #[serde(default)]
    set: HashMap<String, String>,
    #[serde(default)]
    remove: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct CertificateRow {
    id: String,
    hostnames: Vec<String>,
    fullchain: String,
    private_key: String,
}

/// Lightweight fingerprint used to decide whether a full reload is needed.
///
/// Counts catch inserts and deletes; MAX(updated_at) catches updates. Comparing
/// both avoids polling the full configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    websites_count: i64,
    websites_updated: String,
    certificates_count: i64,
    certificates_updated: String,
}

pub async fn current_watermark(database: &Database) -> anyhow::Result<Watermark> {
    let row: (i64, String, i64, String) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM websites),
                (SELECT COALESCE(MAX(updated_at)::text, '') FROM websites),
                (SELECT COUNT(*) FROM certificates),
                (SELECT COALESCE(MAX(updated_at)::text, '') FROM certificates)",
    )
    .fetch_one(database.pool())
    .await
    .context("failed to read the configuration watermark")?;

    Ok(Watermark {
        websites_count: row.0,
        websites_updated: row.1,
        certificates_count: row.2,
        certificates_updated: row.3,
    })
}

/// Reload the full configuration and swap it into the running state.
pub async fn sync_once(
    database: &Database,
    state: &AppState,
    config_ports: &[u16],
) -> anyhow::Result<(BTreeSet<u16>, Watermark)> {
    let default_cert = state.current_default_cert();

    let websites = sqlx::query_as::<_, WebsiteRow>(
        "SELECT id, name, hosts,
                ARRAY(SELECT x::int FROM unnest(ports) AS x)::int[] AS ports,
                backends::text AS backends,
                config::text AS config
           FROM websites",
    )
    .fetch_all(database.pool())
    .await
    .context("failed to load websites")?;

    let mut sites = Vec::new();
    let mut database_ports = BTreeSet::new();

    for row in websites {
        let specs: Vec<BackendRow> = match serde_json::from_str(&row.backends) {
            Ok(specs) => specs,
            Err(error) => {
                event!(Level::WARN, website = %row.id, %error, "ignoring website with invalid backends");
                continue;
            }
        };

        let config: WebsiteConfigRow = match serde_json::from_str(&row.config) {
            Ok(config) => config,
            Err(error) => {
                event!(Level::WARN, website = %row.id, %error, "invalid website config; using defaults");
                WebsiteConfigRow::default()
            }
        };
        let headers = match config.headers {
            Some(headers) => ProxyHeaders::from_config(headers.set, headers.remove),
            None => ProxyHeaders::defaults(),
        };

        let mut backends = Vec::new();
        for spec in specs {
            match Url::parse(&spec.url) {
                Ok(url) if matches!(url.scheme(), "http" | "https") => {
                    if url.host_str().is_none() {
                        event!(Level::WARN, website = %row.id, url = %spec.url, "ignoring backend without a host");
                        continue;
                    }
                    backends.push(Backend {
                        url,
                        match_path: spec.match_path,
                        rules: spec.rules,
                        weight: spec.weight.max(1),
                        priority: spec.priority,
                    });
                }
                Ok(url) => {
                    event!(
                        Level::WARN,
                        website = %row.id,
                        url = %url,
                        "ignoring backend with an unsupported scheme (expected http or https)"
                    );
                }
                Err(error) => {
                    event!(Level::WARN, website = %row.id, url = %spec.url, %error, "ignoring invalid backend url");
                }
            }
        }

        let ports: Vec<u16> = row
            .ports
            .iter()
            .filter_map(|port| u16::try_from(*port).ok())
            .collect();
        database_ports.extend(ports.iter().copied());

        let site = Arc::new(Site::new(
            row.id,
            row.name,
            row.hosts,
            ports,
            backends,
            headers,
            config.get_request_ip,
            config.cache,
        ));
        event!(
            Level::DEBUG,
            website = %site.id,
            name = ?site.name,
            hosts = ?site.hosts,
            ports = ?site.ports,
            "loaded website"
        );
        sites.push(site);
    }

    let routing = Arc::new(Routing::build(sites));

    let certificates = sqlx::query_as::<_, CertificateRow>(
        "SELECT id, hostnames, fullchain, private_key
           FROM certificates
          WHERE fullchain IS NOT NULL
            AND private_key IS NOT NULL
            AND (expires_at IS NULL OR expires_at > now())",
    )
    .fetch_all(database.pool())
    .await
    .context("failed to load certificates")?;

    let mut entries = Vec::new();
    for row in certificates {
        let private_key = match shared::crypto::open(&row.private_key) {
            Ok(private_key) => private_key,
            Err(error) => {
                event!(
                    Level::WARN,
                    certificate = %row.id,
                    error = %format!("{error:#}"),
                    "cannot decrypt the certificate key; ignoring certificate"
                );
                continue;
            }
        };
        match crate::tls::certified_key_from_pem(&row.fullchain, &private_key) {
            Ok(key) => entries.push((row.hostnames, key)),
            Err(error) => {
                event!(Level::WARN, certificate = %row.id, %error, "ignoring unusable certificate");
            }
        }
    }
    let certificates = Arc::new(CertStore::build(entries, default_cert));

    let mut desired_ports: BTreeSet<u16> = config_ports.iter().copied().collect();
    desired_ports.extend(database_ports.iter().copied());

    let host_count = routing.host_count();
    let certificate_host_count = certificates.hostname_count();
    state.apply_config(routing, certificates);

    let watermark = current_watermark(database).await?;

    event!(
        Level::INFO,
        website_hosts = host_count,
        certificate_hosts = certificate_host_count,
        ports = ?desired_ports,
        "gateway configuration synced"
    );

    Ok((desired_ports, watermark))
}
