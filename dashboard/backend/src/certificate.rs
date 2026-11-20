//! Certificate uploads and ACME (DNS-01) issuance.
//!
//! Certificates can be uploaded manually (fullchain + private key) or created
//! as a pending ACME order. The background scheduler claims pending/expiring
//! rows through certificates.signing_started_at and signs one at a time.
//!
//! ACME uses DNS-01 with Tencent Cloud DNSPod. Credentials MUST come from a
//! dns_providers row managed in the panel; environment credentials are not
//! accepted. A provider can list several zones, so one certificate may cover
//! hostnames from multiple zones (for example *.atxa.top + *.txit.top).
//! ZeroSSL is tried first when EAB credentials are configured, otherwise
//! Let's Encrypt is used.

use std::{env, io::Cursor, sync::Arc, time::Duration};

use acmex::{
    AcmeClient, AcmeConfig, CertificateBundle, ChallengeSolverRegistry, Contact, Dns01Solver,
    DnsProvider, KeyPair, TencentCloudDnsProvider,
};
use anyhow::{Context, bail};
use serde::Deserialize;
use shared::database::Database;
use sqlx::Row;
use tracing::{Level, event};
use x509_parser::prelude::*;

/// How long a signing claim is valid before another run may retry the row.
const SIGNING_LEASE_SECS: u64 = 600;
/// Renew automatic certificates this many seconds before expiry (30 days).
const RENEW_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;
const TICK_SECS: u64 = 60;
const MAX_CONCURRENT_SIGNINGS: usize = 2;
const ZEROSSL_DIRECTORY: &str = "https://acme.zerossl.com/v2/DV90";
const LETSENCRYPT_DIRECTORY: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// Environment pairs accepted for ZeroSSL External Account Binding.
const ZEROSSL_EAB_PAIRS: &[(&str, &str)] = &[
    ("ZEROSSL_EAB_KID", "ZEROSSL_EAB_HMAC_KEY"),
    ("ZEROSSL_KID", "ZEROSSL_HMAC_KEY"),
    ("ZEROSSL_EAB_KEY_ID", "ZEROSSL_EAB_KEY"),
];

/// A certificate row claimed for signing.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SigningCertificate {
    pub id: String,
    pub hostnames: Vec<String>,
    pub email: Option<String>,
    pub dns_provider_id: Option<String>,
    pub dns_zone: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DnsProviderConfig {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    from_env: bool,
    #[serde(default)]
    secret_id: Option<String>,
    #[serde(default)]
    secret_key: Option<String>,
}

#[derive(Debug, Clone)]
struct DnsCredentials {
    secret_id: String,
    secret_key: String,
    zones: Vec<String>,
    source: String,
}

#[derive(Debug, Clone)]
struct CaCandidate {
    name: &'static str,
    slug: &'static str,
    directory: &'static str,
    eab: Option<(String, String)>,
}

/// Parsed properties of a PEM certificate (first certificate in the chain).
#[derive(Debug, Clone)]
pub struct ParsedCertificate {
    pub hostnames: Vec<String>,
    pub expires_at: i64,
}

/// Start the background issuance scheduler.
pub fn spawn(database: Database) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(TICK_SECS));
        loop {
            interval.tick().await;
            if let Err(error) = tick(&database).await {
                event!(Level::WARN, error = %format!("{error:#}"), "certificate scheduler tick failed");
            }
        }
    })
}

async fn tick(database: &Database) -> anyhow::Result<()> {
    for _ in 0..MAX_CONCURRENT_SIGNINGS {
        let Some(certificate) = claim_due(database).await? else {
            break;
        };
        let database = database.clone();
        tokio::spawn(async move { sign_certificate(&database, certificate).await });
    }
    Ok(())
}

/// Claim one pending or expiring certificate (multi-instance safe).
async fn claim_due(database: &Database) -> anyhow::Result<Option<SigningCertificate>> {
    let row = sqlx::query_as::<_, SigningCertificate>(
        "UPDATE certificates
            SET signing_started_at = now()
          WHERE id = (
                SELECT id
                  FROM certificates
                 WHERE auto_renew
                   AND (fullchain IS NULL
                        OR (expires_at IS NOT NULL
                            AND expires_at < now() + make_interval(secs => $1)))
                   AND (signing_started_at IS NULL
                        OR signing_started_at < now() - make_interval(secs => $2))
                 ORDER BY expires_at NULLS FIRST
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
          )
         RETURNING id, hostnames, email, dns_provider_id, dns_zone",
    )
    .bind(RENEW_WINDOW_SECS as f64)
    .bind(SIGNING_LEASE_SECS as f64)
    .fetch_optional(database.pool())
    .await
    .context("failed to claim a certificate for signing")?;
    Ok(row)
}

/// Claim a specific certificate for an immediate reissue.
async fn claim_by_id(database: &Database, id: &str) -> anyhow::Result<Option<SigningCertificate>> {
    let row = sqlx::query_as::<_, SigningCertificate>(
        "UPDATE certificates
            SET signing_started_at = now()
          WHERE id = $1
            AND (signing_started_at IS NULL
                 OR signing_started_at < now() - make_interval(secs => $2))
         RETURNING id, hostnames, email, dns_provider_id, dns_zone",
    )
    .bind(id)
    .bind(SIGNING_LEASE_SECS as f64)
    .fetch_optional(database.pool())
    .await
    .context("failed to claim the certificate for signing")?;
    Ok(row)
}

/// Trigger an immediate issuance attempt for one certificate.
pub fn trigger(database: Database, id: String) {
    tokio::spawn(async move {
        match claim_by_id(&database, &id).await {
            Ok(Some(certificate)) => sign_certificate(&database, certificate).await,
            Ok(None) => {
                event!(Level::DEBUG, certificate = %id, "certificate is already being signed")
            }
            Err(error) => event!(
                Level::WARN,
                certificate = %id,
                error = %format!("{error:#}"),
                "failed to claim the certificate"
            ),
        }
    });
}

async fn sign_certificate(database: &Database, certificate: SigningCertificate) {
    let id = certificate.id.clone();
    match sign_inner(database, &certificate).await {
        Ok((fullchain, private_key, expires_at)) => {
            let sealed_private_key = match shared::crypto::seal(&private_key) {
                Ok(sealed) => sealed,
                Err(error) => {
                    event!(
                        Level::ERROR,
                        certificate = %id,
                        error = %format!("{error:#}"),
                        "failed to encrypt the issued certificate key"
                    );
                    return;
                }
            };
            match sqlx::query(
                "UPDATE certificates
                    SET fullchain = $2,
                        private_key = $3,
                        expires_at = to_timestamp($4),
                        signing_started_at = NULL,
                        last_error = NULL
                  WHERE id = $1",
            )
            .bind(&id)
            .bind(&fullchain)
            .bind(&sealed_private_key)
            .bind(expires_at as f64)
            .execute(database.pool())
            .await
            {
                Ok(_) => event!(Level::INFO, certificate = %id, "certificate issued"),
                Err(error) => event!(
                    Level::ERROR,
                    certificate = %id,
                    error = %format!("{error:#}"),
                    "failed to store the issued certificate"
                ),
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            event!(
                Level::WARN,
                certificate = %id,
                error = %message,
                "certificate issuance failed"
            );
            let short = truncate(&message, 2000);
            if let Err(update_error) = sqlx::query(
                "UPDATE certificates
                    SET last_error = $2, signing_started_at = now()
                  WHERE id = $1",
            )
            .bind(&id)
            .bind(short)
            .execute(database.pool())
            .await
            {
                event!(
                    Level::WARN,
                    certificate = %id,
                    error = %format!("{update_error:#}"),
                    "failed to record the certificate error"
                );
            }
        }
    }
}

async fn sign_inner(
    database: &Database,
    certificate: &SigningCertificate,
) -> anyhow::Result<(String, String, i64)> {
    let credentials = resolve_dns(
        database,
        certificate.dns_provider_id.as_deref(),
        &certificate.hostnames,
        certificate.dns_zone.as_deref(),
    )
    .await?;
    let email = certificate
        .email
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var("ACME_EMAIL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });

    let mut failures = Vec::new();
    for candidate in ca_candidates() {
        event!(
            Level::INFO,
            certificate = %certificate.id,
            ca = candidate.name,
            zones = ?credentials.zones,
            dns_source = %credentials.source,
            "requesting certificate"
        );
        match issue_with_ca(
            database,
            &candidate,
            &credentials,
            &certificate.hostnames,
            email.as_deref(),
        )
        .await
        {
            Ok(bundle) => {
                let parsed = parse_certificate(&bundle.certificate_pem)
                    .context("issued certificate could not be parsed")?;
                return Ok((
                    bundle.certificate_pem,
                    bundle.private_key_pem,
                    parsed.expires_at,
                ));
            }
            Err(error) => {
                let message = format!("{error:#}");
                event!(
                    Level::WARN,
                    certificate = %certificate.id,
                    ca = candidate.name,
                    error = %message,
                    "ACME issuance attempt failed"
                );
                failures.push(format!("{}: {message}", candidate.name));
            }
        }
    }

    bail!(
        "all ACME certificate authorities failed: {}",
        failures.join("; ")
    );
}

async fn issue_with_ca(
    database: &Database,
    candidate: &CaCandidate,
    credentials: &DnsCredentials,
    hostnames: &[String],
    email: Option<&str>,
) -> anyhow::Result<CertificateBundle> {
    let mut config = AcmeConfig::new(candidate.directory).with_tos_agreed(true);
    if let Some(email) = email {
        config = config.with_contact(Contact::email(email.trim()));
    }
    if let Some((key_id, hmac_key)) = &candidate.eab {
        config = config.with_external_account_binding(key_id.clone(), hmac_key.clone());
    }
    if let Some(proxy) = acme_proxy(database).await? {
        config = config.with_proxy(proxy);
    }

    // Reuse one ACME account key per CA. Creating a fresh key on every attempt
    // exhausts Let's Encrypt's new-account rate limit and loses the account
    // history; the private key is stored in the configurations table.
    let key_slot = format!("acme_account:{}", candidate.slug);
    let stored: Option<String> =
        sqlx::query_scalar("SELECT value ->> 'key_pem' FROM configurations WHERE key = $1")
            .bind(&key_slot)
            .fetch_optional(database.pool())
            .await
            .context("failed to load the ACME account key")?;
    let key_pair = match stored {
        Some(pem) => {
            let pem = shared::crypto::open(&pem)
                .context("failed to decrypt the stored ACME account key")?;
            KeyPair::from_pem(&pem)
                .map_err(|error| anyhow::anyhow!("invalid stored ACME account key: {error}"))?
        }
        None => {
            let key_pair = KeyPair::generate().map_err(|error| {
                anyhow::anyhow!("failed to generate an ACME account key: {error}")
            })?;
            let pem = key_pair.serialize_pem();
            let sealed_pem = shared::crypto::seal(&pem)?;
            sqlx::query(
                "INSERT INTO configurations (key, value)
                 VALUES ($1, jsonb_build_object('key_pem', $2::text))
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(&key_slot)
            .bind(&sealed_pem)
            .execute(database.pool())
            .await
            .context("failed to persist the ACME account key")?;
            key_pair
        }
    };

    let mut client = AcmeClient::with_key_pair(config, key_pair);
    let provider: Arc<dyn DnsProvider> = Arc::new(TencentCloudDnsProvider::new(
        credentials.secret_id.clone(),
        credentials.secret_key.clone(),
        String::new(),
    ));
    let mut registry = ChallengeSolverRegistry::new();
    registry.register(Dns01Solver::new(
        provider,
        credentials.zones.first().cloned().unwrap_or_default(),
    ));

    let result = client
        .issue_certificate(hostnames.to_vec(), &mut registry)
        .await;
    // Remove TXT records even when validation/finalization fails; the success
    // path already cleans up inside acmex.
    registry.cleanup_all().await;
    result.map_err(|error| anyhow::anyhow!("{error}"))
}

/// Optional HTTP(S) proxy for ACME CA requests, stored in configurations.
///
/// The URL is sealed like other control-plane secrets because a proxy may carry
/// credentials. DNS provider API calls are not affected.
async fn acme_proxy(database: &Database) -> anyhow::Result<Option<String>> {
    let sealed: Option<String> = sqlx::query_scalar(
        "SELECT value ->> 'url' FROM configurations WHERE key = 'acme_http_proxy'",
    )
    .fetch_optional(database.pool())
    .await
    .context("failed to load the ACME proxy setting")?;
    sealed
        .map(|value| shared::crypto::open(&value).context("failed to decrypt the ACME proxy URL"))
        .transpose()
}

fn ca_candidates() -> Vec<CaCandidate> {
    let mut candidates = Vec::new();
    match env_pair(ZEROSSL_EAB_PAIRS) {
        Some((_, _, key_id, hmac_key)) => candidates.push(CaCandidate {
            name: "ZeroSSL",
            slug: "zerossl",
            directory: ZEROSSL_DIRECTORY,
            eab: Some((key_id, hmac_key)),
        }),
        None => event!(
            Level::INFO,
            "ZeroSSL skipped: set ZEROSSL_EAB_KID and ZEROSSL_EAB_HMAC_KEY to enable it"
        ),
    }
    candidates.push(CaCandidate {
        name: "Let's Encrypt",
        slug: "letsencrypt",
        directory: LETSENCRYPT_DIRECTORY,
        eab: None,
    });
    candidates
}

async fn resolve_dns(
    database: &Database,
    dns_provider_id: Option<&str>,
    hostnames: &[String],
    dns_zone: Option<&str>,
) -> anyhow::Result<DnsCredentials> {
    let provider_id = dns_provider_id
        .filter(|value| !value.trim().is_empty())
        .context(
            "select a DNS provider in the panel; certificate issuance no longer reads DNS credentials from the environment",
        )?;
    let row =
        sqlx::query("SELECT provider::text AS provider, domains FROM dns_providers WHERE id = $1")
            .bind(provider_id)
            .fetch_optional(database.pool())
            .await
            .context("failed to load the DNS provider")?
            .with_context(|| format!("DNS provider {provider_id} does not exist"))?;
    let raw: String = row.try_get("provider")?;
    let domains: Vec<String> = row.try_get("domains")?;
    let config: DnsProviderConfig =
        serde_json::from_str(&raw).context("invalid dns_providers.provider JSON")?;
    if let Some(kind) = config.kind.as_deref()
        && kind != "tencent"
    {
        bail!("unsupported DNS provider kind: {kind}");
    }
    if config.from_env {
        bail!(
            "DNS provider {provider_id} uses environment credentials; create a panel provider with SecretId/SecretKey instead"
        );
    }
    let (secret_id, secret_key) = match (config.secret_id, config.secret_key) {
        (Some(secret_id), Some(secret_key)) => (
            shared::crypto::open(&secret_id)
                .context("failed to decrypt the DNS provider secret id")?,
            shared::crypto::open(&secret_key)
                .context("failed to decrypt the DNS provider secret key")?,
        ),
        _ => bail!(
            "DNS provider {provider_id} has no SecretId/SecretKey; add them in the DNS provider page"
        ),
    };

    let mut zones: Vec<String> = domains
        .iter()
        .map(|zone| normalize_zone(zone))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if zones.is_empty() {
        bail!("DNS provider {provider_id} has no zones");
    }

    // `dns_zone` is a hint for the single-zone case. A certificate may span
    // several provider zones (for example *.atxa.top + *.txit.top); in that case
    // the provider's domain list decides where each TXT record goes.
    if let Some(zone) = dns_zone.filter(|value| !value.trim().is_empty()) {
        let zone = normalize_zone(zone)?;
        if !zones.iter().any(|candidate| candidate == &zone) {
            bail!("DNS zone {zone} is not configured on the selected DNS provider");
        }
        if hostnames_within_zone(hostnames, &zone) {
            zones = vec![zone];
        } else {
            event!(
                Level::WARN,
                certificate = %hostnames.first().map(String::as_str).unwrap_or_default(),
                zone = %zone,
                "dns_zone does not cover every hostname; using the provider domains instead"
            );
        }
    }
    ensure_hostnames_in_zones(hostnames, &zones)?;

    Ok(DnsCredentials {
        secret_id,
        secret_key,
        zones,
        source: String::from("dns_providers row"),
    })
}

fn env_pair(
    pairs: &[(&'static str, &'static str)],
) -> Option<(&'static str, &'static str, String, String)> {
    for (id_name, key_name) in pairs {
        let id = env::var(id_name).ok();
        let key = env::var(key_name).ok();
        if let (Some(id), Some(key)) = (id, key)
            && !id.trim().is_empty()
            && !key.trim().is_empty()
        {
            return Some((
                id_name,
                key_name,
                id.trim().to_string(),
                key.trim().to_string(),
            ));
        }
    }
    None
}

fn normalize_zone(zone: &str) -> anyhow::Result<String> {
    let zone = zone.trim().trim_matches('.').to_ascii_lowercase();
    if zone.is_empty() {
        bail!("DNS zone must not be empty");
    }
    Ok(zone)
}

/// Derive the parent zone by dropping the leftmost label. This mirrors the
/// DNSPod provider and works for zones such as atxa.top; pass dns_zone for
/// multi-label public suffixes.
#[cfg(test)]
pub fn derive_zone(hostname: &str) -> anyhow::Result<String> {
    let host = hostname.trim().trim_start_matches("*.");
    let labels: Vec<&str> = host.split('.').filter(|label| !label.is_empty()).collect();
    if labels.len() < 2 {
        bail!("cannot derive a DNS zone from {hostname}");
    }
    Ok(labels[labels.len() - 2..].join(".").to_ascii_lowercase())
}

fn hostname_body(hostname: &str) -> String {
    hostname
        .trim()
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn hostname_in_zone(hostname: &str, zone: &str) -> bool {
    let host = hostname_body(hostname);
    host == zone || host.ends_with(&format!(".{zone}"))
}

fn hostnames_within_zone(hostnames: &[String], zone: &str) -> bool {
    hostnames
        .iter()
        .all(|hostname| hostname_in_zone(hostname, zone))
}

/// Validate that every hostname belongs to at least one of the provider zones.
/// The zones are independent: DNS-01 can solve each authorization in its own
/// parent zone even for a multi-domain certificate.
fn ensure_hostnames_in_zones(hostnames: &[String], zones: &[String]) -> anyhow::Result<()> {
    for hostname in hostnames {
        if !zones.iter().any(|zone| hostname_in_zone(hostname, zone)) {
            bail!(
                "hostname {hostname} is not inside any DNS zone configured on the selected provider ({})",
                zones.join(", ")
            );
        }
    }
    Ok(())
}

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        value.to_string()
    } else {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &value[..end])
    }
}

/// Parse the first certificate of a PEM chain and extract SANs / expiry.
pub fn parse_certificate(fullchain: &str) -> anyhow::Result<ParsedCertificate> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(fullchain.trim().as_bytes())
        .map_err(|error| anyhow::anyhow!("invalid certificate PEM: {error}"))?;
    let (_, certificate) = X509Certificate::from_der(&pem.contents)
        .map_err(|error| anyhow::anyhow!("invalid certificate DER: {error}"))?;

    let mut hostnames = Vec::new();
    for extension in certificate.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = extension.parsed_extension() {
            for name in &san.general_names {
                match name {
                    GeneralName::DNSName(dns) => hostnames.push((*dns).to_string()),
                    GeneralName::IPAddress(bytes) => {
                        if let Some(ip) = ip_from_bytes(bytes) {
                            hostnames.push(ip.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if hostnames.is_empty()
        && let Some(common_name) = certificate.subject().iter_common_name().next()
        && let Ok(name) = common_name.as_str()
    {
        hostnames.push(name.to_string());
    }

    Ok(ParsedCertificate {
        hostnames,
        expires_at: certificate.validity().not_after.timestamp(),
    })
}

fn ip_from_bytes(bytes: &[u8]) -> Option<std::net::IpAddr> {
    match bytes.len() {
        4 => Some(std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).into()),
        16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(bytes);
            Some(std::net::Ipv6Addr::from(octets).into())
        }
        _ => None,
    }
}

/// Check that the private key is valid PEM (the gateway re-validates the pair).
pub fn check_private_key(private_key: &str) -> anyhow::Result<()> {
    let mut cursor = Cursor::new(private_key.trim().as_bytes());
    let key = rustls_pemfile::private_key(&mut cursor)
        .context("invalid private key PEM")?
        .context("no private key found in the PEM input")?;
    let _ = key;
    Ok(())
}

pub fn hostnames_covered(requested: &[String], certificate_names: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|requested| {
            !certificate_names
                .iter()
                .any(|name| hostname_matches(name, requested))
        })
        .cloned()
        .collect()
}

/// Minimal wildcard matcher: * matches exactly one label.
pub fn hostname_matches(pattern: &str, hostname: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    let hostname = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return hostname
            .split_once('.')
            .is_some_and(|(label, rest)| !label.is_empty() && rest == suffix);
    }
    pattern == hostname
}

/// Manual certificate upload.
pub struct UploadedCertificate {
    pub id: String,
    pub hostnames: Vec<String>,
    pub expires_at: i64,
}

#[allow(clippy::too_many_arguments)]
pub async fn upload_certificate(
    database: &Database,
    name: Option<String>,
    hostnames: Option<Vec<String>>,
    fullchain: String,
    private_key: String,
    email: Option<String>,
) -> anyhow::Result<UploadedCertificate> {
    if fullchain.trim().is_empty() || private_key.trim().is_empty() {
        bail!("fullchain and private_key are required");
    }
    check_private_key(&private_key)?;
    let parsed = parse_certificate(&fullchain)?;
    if parsed.hostnames.is_empty() {
        bail!("the certificate does not contain any DNS/IP subject alternative names");
    }

    let requested: Vec<String> = hostnames
        .unwrap_or_default()
        .into_iter()
        .map(|hostname| hostname.trim().to_ascii_lowercase())
        .filter(|hostname| !hostname.is_empty())
        .collect();
    let hostnames = if requested.is_empty() {
        parsed.hostnames.clone()
    } else {
        let missing = hostnames_covered(&requested, &parsed.hostnames);
        if !missing.is_empty() {
            bail!("the certificate does not cover: {}", missing.join(", "));
        }
        requested
    };

    let id = new_id()?;
    let name = name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| hostnames.join(", "));
    let sealed_private_key = shared::crypto::seal(private_key.trim())?;
    sqlx::query(
        "INSERT INTO certificates
             (id, name, hostnames, fullchain, private_key, email, expires_at,
              auto_renew, dns_zone, last_error)
         VALUES ($1, $2, $3, $4, $5, $6, to_timestamp($7), FALSE, NULL, NULL)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&hostnames)
    .bind(fullchain.trim())
    .bind(&sealed_private_key)
    .bind(email.filter(|value| !value.trim().is_empty()))
    .bind(parsed.expires_at as f64)
    .execute(database.pool())
    .await
    .context("failed to store the uploaded certificate")?;

    Ok(UploadedCertificate {
        id,
        hostnames,
        expires_at: parsed.expires_at,
    })
}

/// Create a pending ACME order. The caller should call trigger() afterwards.
pub async fn create_pending(
    database: &Database,
    name: Option<String>,
    hostnames: Vec<String>,
    email: Option<String>,
    dns_provider_id: Option<String>,
    dns_zone: Option<String>,
) -> anyhow::Result<String> {
    let hostnames: Vec<String> = hostnames
        .into_iter()
        .map(|hostname| hostname.trim().to_ascii_lowercase())
        .filter(|hostname| !hostname.is_empty())
        .collect();
    if hostnames.is_empty() {
        bail!("at least one hostname is required");
    }

    let provider_id = dns_provider_id
        .filter(|value| !value.trim().is_empty())
        .context(
            "select a DNS provider in the panel; certificates are no longer issued with environment DNS credentials",
        )?;
    let domains: Vec<String> =
        sqlx::query_scalar("SELECT domains FROM dns_providers WHERE id = $1")
            .bind(&provider_id)
            .fetch_optional(database.pool())
            .await
            .context("failed to load the DNS provider")?
            .with_context(|| format!("DNS provider {provider_id} does not exist"))?;
    let zones: Vec<String> = domains
        .iter()
        .map(|zone| normalize_zone(zone))
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure_hostnames_in_zones(&hostnames, &zones)?;

    let requested_zone = dns_zone
        .filter(|value| !value.trim().is_empty())
        .map(|zone| normalize_zone(&zone))
        .transpose()?;
    if let Some(zone) = requested_zone.as_deref()
        && !zones.iter().any(|candidate| candidate == zone)
    {
        bail!("DNS zone {zone} is not configured on the selected DNS provider");
    }
    // Store a zone only when it covers the whole certificate; multi-zone certs
    // leave dns_zone empty and resolve each authorization from the provider list.
    let stored_zone = requested_zone.filter(|zone| hostnames_within_zone(&hostnames, zone));

    let id = new_id()?;
    let name = name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| hostnames.join(", "));
    sqlx::query(
        "INSERT INTO certificates
             (id, name, hostnames, fullchain, private_key, dns_provider_id, email,
              expires_at, auto_renew, dns_zone, last_error)
         VALUES ($1, $2, $3, NULL, NULL, $4, $5, NULL, TRUE, $6, NULL)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&hostnames)
    .bind(Some(provider_id))
    .bind(email.filter(|value| !value.trim().is_empty()))
    .bind(&stored_zone)
    .execute(database.pool())
    .await
    .context("failed to create the pending certificate")?;

    Ok(id)
}

/// Reset a certificate so the scheduler signs it again.
pub async fn mark_for_issue(database: &Database, id: &str) -> anyhow::Result<()> {
    let auto_renew: Option<bool> =
        sqlx::query_scalar("SELECT auto_renew FROM certificates WHERE id = $1")
            .bind(id)
            .fetch_optional(database.pool())
            .await
            .context("failed to load the certificate")?;
    match auto_renew {
        Some(true) => {}
        Some(false) => bail!("manual certificates cannot be reissued automatically"),
        None => bail!("certificate not found"),
    }
    sqlx::query(
        "UPDATE certificates
            SET signing_started_at = NULL, last_error = NULL
          WHERE id = $1",
    )
    .bind(id)
    .execute(database.pool())
    .await
    .context("failed to reset the certificate")?;
    Ok(())
}

pub async fn delete_certificate(database: &Database, id: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM certificates WHERE id = $1")
        .bind(id)
        .execute(database.pool())
        .await
        .context("failed to delete the certificate")?;
    Ok(result.rows_affected() > 0)
}

/// DNS provider summary returned by the API (never includes secrets).
#[derive(Debug, Clone)]
pub struct DnsProviderSummary {
    pub id: String,
    pub name: Option<String>,
    pub domains: Vec<String>,
    pub kind: String,
    pub from_env: bool,
}

pub async fn list_dns_providers(database: &Database) -> anyhow::Result<Vec<DnsProviderSummary>> {
    let rows = sqlx::query(
        "SELECT id, name, domains, provider::text AS provider FROM dns_providers ORDER BY created_at",
    )
    .fetch_all(database.pool())
    .await
    .context("failed to list DNS providers")?;

    let mut providers = Vec::new();
    for row in rows {
        let raw: String = row.try_get("provider")?;
        let config: DnsProviderConfig = serde_json::from_str(&raw).unwrap_or(DnsProviderConfig {
            kind: None,
            from_env: true,
            secret_id: None,
            secret_key: None,
        });
        providers.push(DnsProviderSummary {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            domains: row.try_get("domains")?,
            kind: config.kind.unwrap_or_else(|| "tencent".to_string()),
            from_env: config.from_env,
        });
    }
    Ok(providers)
}

pub async fn create_dns_provider(
    database: &Database,
    name: Option<String>,
    domains: Vec<String>,
    kind: Option<String>,
    from_env: bool,
    secret_id: Option<String>,
    secret_key: Option<String>,
) -> anyhow::Result<String> {
    let domains: Vec<String> = domains
        .into_iter()
        .map(|domain| domain.trim().trim_matches('.').to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
        .collect();
    if domains.is_empty() {
        bail!("at least one DNS zone is required");
    }
    let kind = kind.unwrap_or_else(|| "tencent".to_string());
    if kind != "tencent" {
        bail!("unsupported DNS provider kind: {kind}");
    }
    if from_env {
        bail!(
            "environment-backed DNS credentials are disabled; enter SecretId and SecretKey in the panel"
        );
    }
    let provider = match (
        secret_id.filter(|value| !value.is_empty()),
        secret_key.filter(|value| !value.is_empty()),
    ) {
        (Some(secret_id), Some(secret_key)) => serde_json::json!({
            "kind": kind,
            "from_env": false,
            "secret_id": shared::crypto::seal(&secret_id)?,
            "secret_key": shared::crypto::seal(&secret_key)?,
        }),
        _ => bail!(
            "secret_id and secret_key are required; environment credentials are not supported"
        ),
    };

    let id = new_id()?;
    sqlx::query(
        "INSERT INTO dns_providers (id, name, provider, domains)
         VALUES ($1, $2, $3::jsonb, $4)",
    )
    .bind(&id)
    .bind(name.filter(|value| !value.trim().is_empty()))
    .bind(provider.to_string())
    .bind(&domains)
    .execute(database.pool())
    .await
    .context("failed to create the DNS provider")?;
    Ok(id)
}

pub async fn delete_dns_provider(database: &Database, id: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM dns_providers WHERE id = $1")
        .bind(id)
        .execute(database.pool())
        .await
        .context("failed to delete the DNS provider")?;
    Ok(result.rows_affected() > 0)
}

fn new_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("randomness failed: {error}"))?;
    Ok(data_encoding::HEXLOWER.encode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_parent_zone() {
        assert_eq!(derive_zone("cert-test.atxa.top").unwrap(), "atxa.top");
        assert_eq!(derive_zone("*.atxa.top").unwrap(), "atxa.top");
        assert!(derive_zone("localhost").is_err());
    }

    #[test]
    fn matches_wildcards_and_exact_names() {
        assert!(hostname_matches("*.example.com", "a.example.com"));
        assert!(!hostname_matches("*.example.com", "a.b.example.com"));
        assert!(!hostname_matches("*.example.com", "example.com"));
        assert!(hostname_matches("example.com", "EXAMPLE.com"));
    }

    #[test]
    fn accepts_hostnames_from_multiple_provider_zones() {
        let hostnames = vec![
            "*.atxa.top".to_string(),
            "*.txit.top".to_string(),
            "*.local.atxa.top".to_string(),
        ];
        let zones = vec!["atxa.top".to_string(), "txit.top".to_string()];
        assert!(ensure_hostnames_in_zones(&hostnames, &zones).is_ok());
        assert!(hostname_in_zone("*.local.atxa.top", "atxa.top"));
        assert!(!hostname_in_zone("*.other.com", "atxa.top"));
    }

    #[test]
    fn reports_uncovered_hostnames() {
        let requested = vec!["a.example.com".to_string(), "b.example.com".to_string()];
        let names = vec!["*.example.com".to_string()];
        assert!(hostnames_covered(&requested, &names).is_empty());

        let missing = hostnames_covered(&["x.other.com".to_string()], &names);
        assert_eq!(missing, vec!["x.other.com".to_string()]);
    }
}
