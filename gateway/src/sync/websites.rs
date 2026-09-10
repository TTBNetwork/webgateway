use std::{
    collections::HashSet,
    sync::{Arc, LazyLock, RwLock as SyncRwLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use shared::{
    database::{get_database, websites::DatabaseWebsiteRepository},
    models::websites::DatabaseWebsiteBackend,
    objectid::ObjectId,
};
use tokio::sync::RwLock;
use tracing::{Level, event};

use crate::state::WebSiteRunner;

static LAST_SYNC: LazyLock<RwLock<DateTime<Utc>>> =
    LazyLock::new(|| RwLock::new(DateTime::from_timestamp_secs(0).unwrap()));
static WEBSITES: LazyLock<DashMap<ObjectId, Arc<WebSiteRunner>>> = LazyLock::new(DashMap::default);
static FULL_WEBSITES: LazyLock<DashMap<String, Arc<WebSiteRunner>>> =
    LazyLock::new(DashMap::default);

// 变更：存储 (预编译正则, 网站) 用于通配符匹配
static LAZY_WEBSITES: LazyLock<DashMap<String, (Regex, Arc<WebSiteRunner>)>> =
    LazyLock::new(DashMap::default);

static CACHE_WEBSITES: LazyLock<SyncRwLock<ttl_cache::TtlCache<String, Arc<WebSiteRunner>>>> =
    LazyLock::new(|| SyncRwLock::new(ttl_cache::TtlCache::new((u16::MAX as usize) * 16)));
static CACHE_WEBSITES_EXPIRE: LazyLock<Arc<Duration>> =
    LazyLock::new(|| Arc::new(Duration::from_hours(2)));

pub async fn sync_websites() -> anyhow::Result<Vec<u16>> {
    let mut last_sync = { *LAST_SYNC.read().await };
    event!(Level::DEBUG, "Last sync websites time: {last_sync}");
    let websites = get_database()
        .get_websites_before_updated_at(&last_sync)
        .await?;
    let mut ports = HashSet::new();
    for website in websites {
        let site = Arc::new(WebSiteRunner::new(website).await?);
        ports.extend(&site.inner().ports);
        WEBSITES.insert(site.inner().id, site.clone());

        for domain in &site.inner().hosts {
            let domain = domain.to_lowercase();
            if domain.contains('*') {
                // 预编译正则表达式
                let regex_pattern = domain.replace('.', "\\.").replace('*', r"[-\w]+");
                match Regex::new(&format!("^{}$", regex_pattern)) {
                    Ok(re) => {
                        event!(
                            Level::INFO,
                            "Insert lazy website: {} -> {}",
                            domain,
                            site.inner().id
                        );
                        LAZY_WEBSITES.insert(domain.to_owned(), (re, site.clone()));
                    }
                    Err(e) => {
                        event!(
                            Level::WARN,
                            "Invalid wildcard pattern '{}': {} — skipped",
                            domain,
                            e
                        );
                    }
                }
            } else {
                event!(
                    Level::INFO,
                    "Insert full website: {} -> {}, {:?}",
                    domain,
                    site.inner().id,
                    site.inner()
                );
                FULL_WEBSITES.insert(domain.to_owned(), site.clone());
            }
        }

        if site.inner().updated_at > last_sync {
            last_sync = site.inner().updated_at;
        }
    }
    *LAST_SYNC.write().await = last_sync;
    Ok(ports.iter().copied().collect::<Vec<u16>>())
}

/// 根据域名和路径查找匹配的网站（支持精确匹配、通配符、缓存）
pub async fn get_website(
    domain: impl Into<String>,
    path: Option<&str>,
) -> Option<Arc<WebSiteRunner>> {
    let domain = domain.into().to_lowercase();
    let path = path.unwrap_or("/");

    // 检查某个网站是否包含匹配当前路径的 backend
    let has_matching_backend = |site: &Arc<WebSiteRunner>| -> bool {
        site.inner().backends.iter().any(|b| path_matches(path, b))
    };

    // 1. 精确匹配
    if let Some(entry) = FULL_WEBSITES.get(&domain) {
        if has_matching_backend(&entry) {
            insert_cache(&domain, entry.clone());
            return Some(entry.clone());
        }
        // 精确匹配但路径不符合，继续尝试通配符（不能直接返回）
    }

    // 2. 缓存（缓存中可能包含任意网站，需验证路径）
    if let Some(cached) = CACHE_WEBSITES.read().unwrap().get(&domain) {
        if has_matching_backend(&cached) {
            return Some(cached.clone());
        }
        // 缓存不匹配路径，继续尝试
    }

    // 3. 通配符匹配（使用预编译正则）
    let mut candidates: Vec<_> = LAZY_WEBSITES
        .iter()
        .map(|entry| {
            (
                entry.key().clone(),
                entry.value().0.clone(),
                entry.value().1.clone(),
            )
        })
        .collect();
    // 按模式长度降序（更具体的优先）
    candidates.sort_by_key(|(pattern, _, _)| std::cmp::Reverse(pattern.len()));

    for (_, re, site) in candidates {
        if re.is_match(&domain) && has_matching_backend(&site) {
            insert_cache(&domain, site.clone());
            return Some(site);
        }
    }

    None
}

/// 路径匹配辅助函数
fn path_matches(path: &str, backend: &DatabaseWebsiteBackend) -> bool {
    let pattern = backend.match_path.as_deref().unwrap_or("/");
    if pattern == "/" {
        true
    } else {
        path.starts_with(pattern) || path == pattern
    }
}

/// 插入缓存
fn insert_cache(domain: &str, site: Arc<WebSiteRunner>) {
    let mut cache = CACHE_WEBSITES.write().unwrap();
    cache.insert(domain.to_string(), site, **CACHE_WEBSITES_EXPIRE);
}

// 注意：原 regex_match 函数已删除，不再使用
