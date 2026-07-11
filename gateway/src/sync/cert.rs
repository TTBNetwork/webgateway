use std::{
    sync::{Arc, LazyLock, RwLock as SyncRwLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use rustls::{
    ServerConfig, crypto::CryptoProvider, server::ResolvesServerCert, sign::CertifiedKey,
};
use shared::{
    database::{certificate::DatabaseCertificateRepository, get_database},
    default::sign_default_certificates,
    objectid::ObjectId,
};
use tokio::sync::RwLock;
use tracing::{Level, event};
pub static FULL_CERTIFICATES: LazyLock<DashMap<String, Arc<rustls::sign::CertifiedKey>>> =
    LazyLock::new(DashMap::default);

pub static CERTIFICATES: LazyLock<DashMap<ObjectId, Arc<rustls::sign::CertifiedKey>>> =
    LazyLock::new(DashMap::default);

pub static LAZY_CERTIFICATES: LazyLock<DashMap<String, Arc<rustls::sign::CertifiedKey>>> =
    LazyLock::new(DashMap::default);

static DEFAULT_CERTIFICATE: LazyLock<Arc<CertifiedKey>> = LazyLock::new(|| {
    let (fullchain, privatekey) = sign_default_certificates().unwrap();
    Arc::new(CertifiedKey::from_der(fullchain, privatekey, &PROVIDER).unwrap())
});
static PROVIDER: LazyLock<Arc<CryptoProvider>> =
    LazyLock::new(|| ServerConfig::builder().crypto_provider().clone());
static LAST_SYNC: LazyLock<RwLock<DateTime<Utc>>> =
    LazyLock::new(|| RwLock::new(DateTime::from_timestamp_secs(0).unwrap()));
static CACHE_CERTIFICATES: LazyLock<
    SyncRwLock<ttl_cache::TtlCache<String, Arc<rustls::sign::CertifiedKey>>>,
> = LazyLock::new(|| SyncRwLock::new(ttl_cache::TtlCache::new((u16::MAX as usize) * 16)));
static CACHE_CERTIFICATES_EXPIRE: LazyLock<Arc<Duration>> =
    LazyLock::new(|| Arc::new(Duration::from_hours(2)));

#[derive(Debug, Default)]
pub struct AutoCertificate;

impl ResolvesServerCert for AutoCertificate {
    fn resolve(
        &self,
        client_hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        // TODO: implement
        let sni = client_hello.server_name();
        if let Some(sni) = sni
            && let Some(cert) = lookup_certificate(&sni.to_lowercase())
        {
            return Some(cert);
        }
        DEFAULT_CERTIFICATE.clone().into()
    }
}

pub async fn sync_certificates() -> anyhow::Result<()> {
    let mut last_sync = { *LAST_SYNC.read().await };
    event!(Level::DEBUG, "Last sync certificates time: {last_sync}");
    let certificates = get_database()
        .get_certificates_before_updated_at(&last_sync)
        .await?;
    for certificate in certificates {
        let fullchain = certificate.get_fullchain()?;
        let privatekey = certificate.get_private_key()?;
        let config = Arc::new(CertifiedKey::from_der(fullchain, privatekey, &PROVIDER)?);
        CERTIFICATES.insert(certificate.id, config.clone());
        for domain in certificate.hostnames {
            if domain.contains("*") {
                LAZY_CERTIFICATES.insert(domain, config.clone());
            } else {
                FULL_CERTIFICATES.insert(domain, config.clone());
            }
        }
        // compare
        if certificate.updated_at > last_sync {
            last_sync = certificate.updated_at;
        }
    }
    *LAST_SYNC.write().await = last_sync;
    Ok(())
}

fn lookup_certificate(host: &str) -> Option<Arc<CertifiedKey>> {
    let host = host.to_lowercase();

    // 2. 精确匹配
    if let Some(cert) = FULL_CERTIFICATES.get(&host) {
        insert_cache(&host, cert.clone());
        return Some(cert.clone());
    }
    // 1. 检查缓存
    if let Some(cached) = CACHE_CERTIFICATES.read().unwrap().get(&host) {
        return Some(cached.clone());
    }

    // 3. 通配符匹配：按模式长度降序（更具体的优先）
    let mut candidates: Vec<_> = LAZY_CERTIFICATES
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect();
    candidates.sort_by_key(|(b, _)| std::cmp::Reverse(b.len()));

    for (pattern, cert) in candidates {
        if regex_match(&host, &pattern) {
            insert_cache(&host, cert.clone());
            return Some(cert.clone());
        }
    }

    None
}

fn insert_cache(host: &str, cert: Arc<CertifiedKey>) {
    let mut cache = CACHE_CERTIFICATES.write().unwrap();
    if !cache.contains_key(host) {
        cache.insert(host.to_string(), cert, **CACHE_CERTIFICATES_EXPIRE);
    }
}

fn regex_match(host: &str, pattern: &str) -> bool {
    let pattern = pattern.replace('.', "\\.").replace('*', r"[-\w]+");
    let re = Regex::new(&format!("^{pattern}$")).unwrap();
    re.is_match(host)
}
