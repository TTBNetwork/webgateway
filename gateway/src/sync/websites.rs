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
static LAZY_WEBSITES: LazyLock<DashMap<String, Arc<WebSiteRunner>>> =
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
            if domain.contains("*") {
                event!(
                    Level::INFO,
                    "Insert website: {domain} to lazy websites {}",
                    site.inner().id
                );
                LAZY_WEBSITES.insert(domain.to_owned(), site.clone());
            } else {
                event!(
                    Level::INFO,
                    "Insert website: {domain} to full websites {}",
                    site.inner().id
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

pub async fn get_website(domain: impl Into<String>) -> Option<Arc<WebSiteRunner>> {
    let domain = domain.into().to_lowercase();

    // 精确匹配
    if let Some(entry) = FULL_WEBSITES.get(&domain) {
        insert_cache(&domain, entry.clone());
        return Some(entry.clone());
    }

    // 缓存
    if let Some(cached) = CACHE_WEBSITES.read().unwrap().get(&domain) {
        return Some(cached.clone());
    }

    // 通配符匹配（按模式长度降序）
    let mut candidates: Vec<_> = LAZY_WEBSITES
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect();
    candidates.sort_by_key(|(b, _)| std::cmp::Reverse(b.len()));

    for (pattern, site) in candidates {
        if regex_match(&domain, &pattern) {
            insert_cache(&domain, site.clone());
            return Some(site);
        }
    }

    None
}

fn insert_cache(domain: &str, site: Arc<WebSiteRunner>) {
    let mut cache = CACHE_WEBSITES.write().unwrap();
    cache.insert(domain.to_string(), site, **CACHE_WEBSITES_EXPIRE);
}

fn regex_match(host: &str, pattern: &str) -> bool {
    // 转义点号，将 * 替换为正则中的通配符（允许字母、数字、连字符）
    let pattern = pattern.replace('.', "\\.").replace('*', r"[-\w]+");
    // 编译正则，如果失败则返回 false（一般不会发生）
    let Ok(re) = Regex::new(&format!("^{}$", pattern)) else {
        return false;
    };
    re.is_match(host)
}
