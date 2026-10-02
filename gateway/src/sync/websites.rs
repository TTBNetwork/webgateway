use std::{
    sync::{Arc, LazyLock, RwLock as SyncRwLock},
    time::Duration,
};

use dashmap::DashMap;
use regex::Regex;
use shared::{
    database::{get_database, websites::DatabaseWebsiteRepository},
    models::websites::DatabaseWebsiteBackend,
    objectid::ObjectId,
};
use tracing::{Level, event};

use crate::state::WebSiteRunner;

pub static WEBSITES: LazyLock<DashMap<ObjectId, Arc<WebSiteRunner>>> =
    LazyLock::new(DashMap::default);
pub static FULL_WEBSITES: LazyLock<DashMap<String, Arc<WebSiteRunner>>> =
    LazyLock::new(DashMap::default);

/// 通配符站点：域名模式 → (预编译正则, 站点)。
static LAZY_WEBSITES: LazyLock<DashMap<String, (Regex, Arc<WebSiteRunner>)>> =
    LazyLock::new(DashMap::default);

static CACHE_WEBSITES: LazyLock<SyncRwLock<ttl_cache::TtlCache<String, Arc<WebSiteRunner>>>> =
    LazyLock::new(|| SyncRwLock::new(ttl_cache::TtlCache::new((u16::MAX as usize) * 16)));
static CACHE_WEBSITES_EXPIRE: LazyLock<Arc<Duration>> =
    LazyLock::new(|| Arc::new(Duration::from_hours(2)));

/// 全量重建站点表，返回当前**应当**监听的全部端口。
///
/// 修复（ISSUES.md P0-8）：原实现只做增量 insert，从不删除本地条目，
/// 因此「删除站点」「修改站点 hosts 移除域名」都不会生效 —— 已下线的域名仍可访问。
/// 另外水位依赖 `updated_at > last_sync`，而 `updated_at` 由触发器用事务开始时间
/// （原为 `NOW()`）写入，长事务提交后会永久落后于水位（P0-9），增量同步本身就会漏更新。
///
/// 站点表规模很小，因此改为**对账式全量重建**：每轮同步都从数据库取全量配置，
/// 重建三张内存表，天然消除「多出来的条目」。调用方拿到端口集合后再做监听器 diff。
pub async fn sync_websites() -> anyhow::Result<Vec<u16>> {
    let websites = get_database().get_websites().await?;

    let mut next_full: Vec<(String, Arc<WebSiteRunner>)> = Vec::new();
    let mut next_lazy: Vec<(String, Regex, Arc<WebSiteRunner>)> = Vec::new();
    let mut next_by_id: Vec<(ObjectId, Arc<WebSiteRunner>)> = Vec::new();
    let mut ports = std::collections::HashSet::new();

    for website in websites {
        let site = Arc::new(WebSiteRunner::new(website).await?);
        ports.extend(&site.inner().ports);
        next_by_id.push((site.inner().id, site.clone()));

        for domain in &site.inner().hosts {
            let domain = domain.to_lowercase();
            if domain.contains('*') {
                let regex_pattern = domain.replace('.', "\\.").replace('*', r"[-\w]+");
                match Regex::new(&format!("^{}$", regex_pattern)) {
                    Ok(re) => next_lazy.push((domain, re, site.clone())),
                    Err(e) => event!(
                        Level::WARN,
                        "Invalid wildcard pattern '{}': {} — skipped",
                        domain,
                        e
                    ),
                }
            } else {
                next_full.push((domain, site.clone()));
            }
        }
    }

    // 以下替换是原子的（逐表 clear + 重建）。网关的路由查询在读侧，
    // 短暂的空窗只会让个别请求落到 404，不会返回错误的上游。
    WEBSITES.clear();
    FULL_WEBSITES.clear();
    LAZY_WEBSITES.clear();
    for (id, site) in next_by_id {
        WEBSITES.insert(id, site);
    }
    for (domain, site) in next_full {
        FULL_WEBSITES.insert(domain, site);
    }
    for (domain, re, site) in next_lazy {
        LAZY_WEBSITES.insert(domain, (re, site));
    }

    // 缓存中的条目可能指向已被删除的站点，直接清空最安全。
    CACHE_WEBSITES
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    event!(
        Level::DEBUG,
        "Reconciled websites: {} exact, {} wildcard, ports {:?}",
        FULL_WEBSITES.len(),
        LAZY_WEBSITES.len(),
        ports
    );

    let mut ports = ports.into_iter().collect::<Vec<u16>>();
    ports.sort_unstable();
    Ok(ports)
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
    if let Some(cached) = CACHE_WEBSITES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&domain)
    {
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
    let mut cache = CACHE_WEBSITES.write().unwrap_or_else(|e| e.into_inner());
    cache.insert(domain.to_string(), site, **CACHE_WEBSITES_EXPIRE);
}
