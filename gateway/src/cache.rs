//! In-memory HTTP cache for cacheable GET responses.

use std::{
    collections::{HashMap, VecDeque},
    sync::RwLock,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{HeaderMap, StatusCode};

use crate::metrics;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ttl", rename = "ttl")]
    pub ttl_secs: u64,
    #[serde(default)]
    pub content_types: Vec<String>,
    #[serde(default = "default_max_entry")]
    pub max_entry_size: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: default_ttl(),
            content_types: Vec::new(),
            max_entry_size: default_max_entry(),
        }
    }
}

impl CacheConfig {
    /// Empty list means every content type is cacheable.
    pub fn allows_content_type(&self, content_type: Option<&str>) -> bool {
        if self.content_types.is_empty() {
            return true;
        }
        let Some(content_type) = content_type else {
            return false;
        };
        let content_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();

        self.content_types.iter().any(|allowed| {
            let allowed = allowed.trim().to_ascii_lowercase();
            if allowed.ends_with('/') {
                content_type.starts_with(&allowed)
            } else {
                content_type == allowed
            }
        })
    }
}

fn default_ttl() -> u64 {
    60
}

fn default_max_entry() -> u64 {
    1024 * 1024
}

#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub etag: Option<String>,
}

struct Entry {
    response: CachedResponse,
    stored: Instant,
    expires: Instant,
    size: u64,
}

struct Inner {
    entries: HashMap<String, Entry>,
    order: VecDeque<String>,
    size: u64,
}

pub struct Cache {
    inner: RwLock<Inner>,
    max_size: u64,
}

impl Cache {
    pub fn new(max_size: u64) -> Self {
        Self {
            inner: RwLock::new(Inner {
                entries: HashMap::new(),
                order: VecDeque::new(),
                size: 0,
            }),
            max_size,
        }
    }

    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        self.get_at(key, Instant::now())
    }

    pub fn get_at(&self, key: &str, now: Instant) -> Option<CachedResponse> {
        let mut inner = self.inner.write().expect("cache lock poisoned");
        let expired = inner
            .entries
            .get(key)
            .is_some_and(|entry| entry.expires <= now);
        if expired {
            remove(&mut inner, key);
            metrics::cache_expired();
            metrics::cache_miss();
            return None;
        }

        let Some(entry) = inner.entries.get(key) else {
            metrics::cache_miss();
            return None;
        };
        metrics::cache_hit();
        let response = entry.response.clone();
        let age = now.saturating_duration_since(entry.stored).as_secs();

        // Move the key to the back so eviction behaves like an LRU list.
        if let Some(position) = inner.order.iter().position(|item| item == key) {
            inner.order.remove(position);
            inner.order.push_back(key.to_string());
        }

        let mut response = response;
        if let Ok(value) = http::HeaderValue::from_str(&age.to_string()) {
            response.headers.insert(http::header::AGE, value);
        }
        Some(response)
    }

    pub fn insert(&self, key: String, response: CachedResponse, ttl: Duration) {
        let size = response.body.len() as u64;
        if ttl.is_zero() || size > self.max_size {
            metrics::cache_rejected();
            return;
        }

        let now = Instant::now();
        let mut inner = self.inner.write().expect("cache lock poisoned");

        if let Some(previous) = inner.entries.remove(&key) {
            inner.size = inner.size.saturating_sub(previous.size);
            if let Some(position) = inner.order.iter().position(|item| item == &key) {
                inner.order.remove(position);
            }
        }

        while inner.size.saturating_add(size) > self.max_size {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some(removed) = inner.entries.remove(&oldest) {
                inner.size = inner.size.saturating_sub(removed.size);
                metrics::cache_eviction();
            }
        }

        inner.size = inner.size.saturating_add(size);
        inner.order.push_back(key.clone());
        inner.entries.insert(
            key,
            Entry {
                response,
                stored: now,
                expires: now + ttl,
                size,
            },
        );
        metrics::cache_store();
    }

    pub fn clear(&self) {
        let mut inner = self.inner.write().expect("cache lock poisoned");
        inner.entries.clear();
        inner.order.clear();
        inner.size = 0;
        metrics::cache_clear();
    }

    /// Occupancy snapshot used by the metrics endpoint.
    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.read().expect("cache lock poisoned");
        CacheStats {
            entries: inner.entries.len(),
            bytes: inner.size,
            max_bytes: self.max_size,
        }
    }
}

/// Cache occupancy reported as gauges.
pub struct CacheStats {
    pub entries: usize,
    pub bytes: u64,
    pub max_bytes: u64,
}

fn remove(inner: &mut Inner, key: &str) {
    if let Some(entry) = inner.entries.remove(key) {
        inner.size = inner.size.saturating_sub(entry.size);
    }
    if let Some(position) = inner.order.iter().position(|item| item == key) {
        inner.order.remove(position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: &'static [u8]) -> CachedResponse {
        CachedResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from_static(body),
            etag: Some("\"v1\"".to_string()),
        }
    }

    #[test]
    fn entries_expire() {
        let cache = Cache::new(1024);
        cache.insert("a".to_string(), response(b"hello"), Duration::from_secs(10));
        let now = Instant::now();
        assert!(cache.get_at("a", now).is_some());

        let later = now + Duration::from_secs(11);
        assert!(cache.get_at("a", later).is_none());
        assert!(cache.get_at("a", later).is_none());
    }

    #[test]
    fn evicts_the_oldest_entry() {
        let cache = Cache::new(10);
        cache.insert("a".to_string(), response(b"12345"), Duration::from_secs(10));
        cache.insert("b".to_string(), response(b"1234"), Duration::from_secs(10));
        assert!(cache.get("a").is_some());

        // Touching "a" makes "b" the LRU entry; "c" evicts it.
        cache.insert("c".to_string(), response(b"12345"), Duration::from_secs(10));
        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn filters_content_types() {
        let config = CacheConfig {
            enabled: true,
            content_types: vec!["text/css".to_string(), "image/".to_string()],
            ..CacheConfig::default()
        };
        assert!(config.allows_content_type(Some("text/css; charset=utf-8")));
        assert!(config.allows_content_type(Some("image/png")));
        assert!(!config.allows_content_type(Some("text/html")));
        assert!(!config.allows_content_type(None));

        let all = CacheConfig {
            enabled: true,
            ..CacheConfig::default()
        };
        assert!(all.allows_content_type(None));
    }

    #[test]
    fn clear_removes_everything() {
        let cache = Cache::new(1024);
        cache.insert("a".to_string(), response(b"body"), Duration::from_secs(10));
        cache.clear();
        assert!(cache.get("a").is_none());
    }
}
