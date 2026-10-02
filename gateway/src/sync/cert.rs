use std::{
    sync::{Arc, LazyLock, RwLock as SyncRwLock},
    time::Duration,
};

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
use tracing::{Level, event};
pub static FULL_CERTIFICATES: LazyLock<DashMap<String, Arc<rustls::sign::CertifiedKey>>> =
    LazyLock::new(DashMap::default);

pub static CERTIFICATES: LazyLock<DashMap<ObjectId, Arc<rustls::sign::CertifiedKey>>> =
    LazyLock::new(DashMap::default);

/// 通配符证书：域名模式 → (预编译正则, 证书)。
///
/// 修复（ISSUES.md P1-6）：原先 `LAZY_CERTIFICATES` 存的是原始字符串，
/// `lookup_certificate` 在**每次握手**都要为每个候选模式重新 `Regex::new`。
/// 这里改为与 `sync/websites.rs` 一致的预编译存储。
pub static LAZY_CERTIFICATES: LazyLock<DashMap<String, (Regex, Arc<rustls::sign::CertifiedKey>)>> =
    LazyLock::new(DashMap::default);

static DEFAULT_CERTIFICATE: LazyLock<Arc<CertifiedKey>> = LazyLock::new(|| {
    let (fullchain, privatekey) = sign_default_certificates().unwrap();
    Arc::new(CertifiedKey::from_der(fullchain, privatekey, &PROVIDER).unwrap())
});
static PROVIDER: LazyLock<Arc<CryptoProvider>> =
    LazyLock::new(|| ServerConfig::builder().crypto_provider().clone());
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

/// 全量重建证书表。
///
/// 修复（ISSUES.md P0-8）：原实现只 insert 不 delete，删除或吊销证书后
/// `FULL_CERTIFICATES` / `LAZY_CERTIFICATES` / `CERTIFICATES` 只增不减，
/// **已吊销的证书会继续被使用**。改为对账式重建。
///
/// 同时不再依赖 `updated_at` 水位（P0-9 会让增量同步漏更新）。
pub async fn sync_certificates() -> anyhow::Result<()> {
    let certificates = get_database().get_certificates().await?;

    let mut next_by_id = Vec::new();
    let mut next_full = Vec::new();
    let mut next_lazy = Vec::new();

    for certificate in certificates {
        // 手工上传但内容缺失的证书会导致解析失败：跳过它而不是让整轮同步失败，
        // 否则一张坏证书会让网关完全无法加载其它证书。
        let config = match (certificate.get_fullchain(), certificate.get_private_key()) {
            (Ok(fullchain), Ok(privatekey)) => {
                match CertifiedKey::from_der(fullchain, privatekey, &PROVIDER) {
                    Ok(config) => Arc::new(config),
                    Err(e) => {
                        event!(
                            Level::ERROR,
                            "Skipping malformed certificate {}: {e}",
                            certificate.id
                        );
                        continue;
                    }
                }
            }
            _ => {
                event!(
                    Level::WARN,
                    "Skipping certificate {} because fullchain or private key is missing",
                    certificate.id
                );
                continue;
            }
        };

        next_by_id.push((certificate.id, config.clone()));
        for domain in certificate.hostnames {
            let domain = domain.to_lowercase();
            if domain.contains('*') {
                let pattern = domain.replace('.', "\\.").replace('*', r"[-\w]+");
                match Regex::new(&format!("^{pattern}$")) {
                    Ok(re) => next_lazy.push((domain, re, config.clone())),
                    Err(e) => event!(
                        Level::WARN,
                        "Invalid certificate wildcard pattern '{domain}': {e} — skipped"
                    ),
                }
            } else {
                next_full.push((domain, config.clone()));
            }
        }
    }

    CERTIFICATES.clear();
    FULL_CERTIFICATES.clear();
    LAZY_CERTIFICATES.clear();
    for (id, cert) in next_by_id {
        CERTIFICATES.insert(id, cert);
    }
    for (domain, cert) in next_full {
        FULL_CERTIFICATES.insert(domain, cert);
    }
    for (domain, re, cert) in next_lazy {
        LAZY_CERTIFICATES.insert(domain, (re, cert));
    }

    // 缓存可能指向已被删除的证书，清空最安全。
    CACHE_CERTIFICATES
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    Ok(())
}

fn lookup_certificate(host: &str) -> Option<Arc<CertifiedKey>> {
    let host = host.to_lowercase();

    // 1. 精确匹配
    if let Some(cert) = FULL_CERTIFICATES.get(&host) {
        insert_cache(&host, cert.clone());
        return Some(cert.clone());
    }
    // 2. 检查缓存
    if let Some(cached) = CACHE_CERTIFICATES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&host)
    {
        return Some(cached.clone());
    }

    // 3. 通配符匹配：按模式长度降序（更具体的优先），使用预编译正则
    let mut candidates: Vec<_> = LAZY_CERTIFICATES
        .iter()
        .map(|entry| {
            (
                entry.key().clone(),
                entry.value().0.clone(),
                entry.value().1.clone(),
            )
        })
        .collect();
    candidates.sort_by_key(|(pattern, _, _)| std::cmp::Reverse(pattern.len()));

    for (_, re, cert) in candidates {
        if re.is_match(&host) {
            insert_cache(&host, cert.clone());
            return Some(cert.clone());
        }
    }

    None
}

fn insert_cache(host: &str, cert: Arc<CertifiedKey>) {
    let mut cache = CACHE_CERTIFICATES
        .write()
        .unwrap_or_else(|e| e.into_inner());
    if !cache.contains_key(host) {
        cache.insert(host.to_string(), cert, **CACHE_CERTIFICATES_EXPIRE);
    }
}
