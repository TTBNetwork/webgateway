use std::{
    sync::{Arc, LazyLock, RwLock as SyncRwLock},
    time::Duration,
};

use chrono::Utc;
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

    // 先解出所有可用证书，并标注它是否已过期。
    //
    // 需求（STEP.md）：**过期的证书不加载**，除非加载完发现一张能用的都没有 ——
    // 那时宁可继续用过期证书顶着（浏览器会告警但至少能连），也不要无证书可用。
    let now = Utc::now();
    let mut parsed: Vec<(ObjectId, Arc<CertifiedKey>, bool, Vec<String>)> = Vec::new();
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

        // 模型里 `expires_at` 是必填（`DateTime<Utc>`，不是 Option），因此这里只判"是否已过期"。
        let expired = certificate.expires_at < now;
        if expired {
            event!(
                Level::WARN,
                "Certificate {} is expired (expires_at = {:?}); it will be ignored unless \
                 no valid certificate is available",
                certificate.id,
                certificate.expires_at
            );
        }
        parsed.push((
            certificate.id,
            config,
            expired,
            certificate.hostnames,
        ));
    }

    // 决定哪些证书参与装载：优先只用未过期的；若一张未过期的都没有，则回退到全部
    // （宁可继续用过期证书顶着，也不要无证书可用）。
    let expired_flags: Vec<bool> = parsed.iter().map(|(_, _, expired, _)| *expired).collect();
    let loadable = loadable_certificates(&expired_flags);
    if !expired_flags.is_empty() && !expired_flags.iter().any(|e| !*e) {
        event!(
            Level::ERROR,
            "No valid (unexpired) certificate available; falling back to {} expired \
             certificate(s) so that TLS keeps working — please renew them",
            parsed.len()
        );
    }

    for (idx, (id, config, _expired, hostnames)) in parsed.into_iter().enumerate() {
        if !loadable.get(idx).copied().unwrap_or(false) {
            continue;
        }
        next_by_id.push((id, config.clone()));
        for domain in hostnames {
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

/// 决定哪些证书参与装载。
///
/// 规则（STEP.md：「过期的证书不加载，除非真的没有可以的证书再说」）：
/// * 只要存在**未过期**的证书，就**只**装载未过期的；
/// * 若全部都已过期，则全部装载（有证书总比没证书强，浏览器会提示但不至于连不上）；
/// * 空列表返回空。
///
/// 抽成独立函数是为了能直接单测这条规则 —— 它决定线上 TLS 用哪张证书。
fn loadable_certificates(expired: &[bool]) -> Vec<bool> {
    if expired.is_empty() {
        return Vec::new();
    }
    let has_valid = expired.iter().any(|e| !*e);
    expired.iter().map(|e| if has_valid { !*e } else { true }).collect()
}

#[cfg(test)]
mod tests {
    use super::loadable_certificates;

    #[test]
    fn skips_expired_when_a_valid_one_exists() {
        // [未过期, 已过期, 已过期] -> 只装载第一个
        assert_eq!(loadable_certificates(&[false, true, true]), vec![true, false, false]);
    }

    #[test]
    fn falls_back_to_all_when_everything_expired() {
        // 全过期 -> 全部装载（否则无证书可用）
        assert_eq!(loadable_certificates(&[true, true]), vec![true, true]);
    }

    #[test]
    fn handles_empty_and_all_valid() {
        assert!(loadable_certificates(&[]).is_empty());
        assert_eq!(loadable_certificates(&[false, false]), vec![true, true]);
    }
}
