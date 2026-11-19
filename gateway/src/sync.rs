//! Load websites and certificates from PostgreSQL and apply them to the state.

use std::{collections::BTreeSet, sync::Arc};

use anyhow::Context;
use tracing::{Level, event};
use url::Url;

use crate::state::{AppState, Backend, CertStore, Routing, Site};
use shared::database::Database;

#[derive(Debug, sqlx::FromRow)]
struct WebsiteRow {
    id: String,
    name: Option<String>,
    hosts: Vec<String>,
    ports: Vec<i32>,
    backends: String,
}

#[derive(Debug, serde::Deserialize)]
struct BackendRow {
    url: String,
    #[serde(default)]
    match_path: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct CertificateRow {
    id: String,
    hostnames: Vec<String>,
    fullchain: String,
    private_key: String,
}

/// Reload the full configuration and swap it into the running state.
pub async fn sync_once(
    database: &Database,
    state: &AppState,
    config_ports: &[u16],
) -> anyhow::Result<BTreeSet<u16>> {
    let default_cert = state.current_default_cert();

    let websites = sqlx::query_as::<_, WebsiteRow>(
        "SELECT id, name, hosts,
                ARRAY(SELECT x::int FROM unnest(ports) AS x)::int[] AS ports,
                backends::text AS backends
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

        let mut backends = Vec::new();
        for spec in specs {
            match Url::parse(&spec.url) {
                Ok(url) if url.scheme() == "http" => {
                    if url.host_str().is_none() {
                        event!(Level::WARN, website = %row.id, url = %spec.url, "ignoring backend without a host");
                        continue;
                    }
                    backends.push(Backend {
                        url,
                        match_path: spec.match_path,
                    });
                }
                Ok(url) => {
                    event!(
                        Level::WARN,
                        website = %row.id,
                        url = %url,
                        "ignoring non-http backend (upstream TLS is not implemented yet)"
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

        let site = Arc::new(Site::new(row.id, row.name, row.hosts, ports, backends));
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
        match crate::tls::certified_key_from_pem(&row.fullchain, &row.private_key) {
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

    event!(
        Level::INFO,
        website_hosts = host_count,
        certificate_hosts = certificate_host_count,
        ports = ?desired_ports,
        "gateway configuration synced"
    );

    Ok(desired_ports)
}
