//! In-memory HTTP cache for cacheable GET responses.

use std::{
    collections::{HashMap, VecDeque},
    sync::RwLock,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};

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

/// Header values covered by a response's `Vary` list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VaryKey(Vec<(HeaderName, Vec<HeaderValue>)>);

impl VaryKey {
    /// None when the response must not be cached (`Vary: *`).
    fn from_response(response_headers: &HeaderMap, request_headers: &HeaderMap) -> Option<Self> {
        let mut names = Vec::new();
        for value in response_headers.get_all(header::VARY) {
            let Ok(value) = value.to_str() else {
                continue;
            };
            for name in value.split(',') {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if name == "*" {
                    return None;
                }
                if let Ok(name) = HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes())
                    && !names.contains(&name)
                {
                    names.push(name);
                }
            }
        }

        let mut fields = Vec::with_capacity(names.len());
        for name in names {
            let values: Vec<HeaderValue> = request_headers.get_all(&name).iter().cloned().collect();
            fields.push((name, values));
        }
        Some(Self(fields))
    }

    fn matches(&self, request_headers: &HeaderMap) -> bool {
        self.0.iter().all(|(name, expected)| {
            let actual: Vec<HeaderValue> = request_headers.get_all(name).iter().cloned().collect();
            &actual == expected
        })
    }
}

struct Entry {
    response: CachedResponse,
    stored: Instant,
    expires: Instant,
    size: u64,
    vary: VaryKey,
    token: u64,
}

struct Inner {
    entries: HashMap<String, Vec<Entry>>,
    order: VecDeque<(String, u64)>,
    size: u64,
    next_token: u64,
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
                next_token: 0,
            }),
            max_size,
        }
    }

    pub fn get(&self, key: &str, request_headers: &HeaderMap) -> Option<CachedResponse> {
        self.get_at(key, request_headers, Instant::now())
    }

    pub fn get_at(
        &self,
        key: &str,
        request_headers: &HeaderMap,
        now: Instant,
    ) -> Option<CachedResponse> {
        let mut guard = self.inner.write().expect("cache lock poisoned");
        let inner = &mut *guard;
        let mut expired = 0u64;
        let mut expired_bytes = 0u64;
        let mut empty = false;
        let mut hit = None;
        {
            let Some(variants) = inner.entries.get_mut(key) else {
                metrics::cache_miss();
                return None;
            };
            variants.retain(|entry| {
                if entry.expires <= now {
                    expired += 1;
                    expired_bytes = expired_bytes.saturating_add(entry.size);
                    false
                } else {
                    true
                }
            });
            if variants.is_empty() {
                empty = true;
            } else if let Some(entry) = variants
                .iter()
                .find(|entry| entry.vary.matches(request_headers))
            {
                hit = Some((entry.response.clone(), entry.stored, entry.token));
            }
        }

        if expired > 0 {
            inner.size = inner.size.saturating_sub(expired_bytes);
            for _ in 0..expired {
                metrics::cache_expired();
            }
        }
        if empty {
            inner.entries.remove(key);
            metrics::cache_miss();
            return None;
        }
        let Some((response, stored, token)) = hit else {
            metrics::cache_miss();
            return None;
        };
        metrics::cache_hit();

        if let Some(position) = inner.order.iter().position(|(_, item)| *item == token) {
            inner.order.remove(position);
        }
        inner.order.push_back((key.to_string(), token));

        let age = now.saturating_duration_since(stored).as_secs();
        let mut response = response;
        if let Ok(value) = HeaderValue::from_str(&age.to_string()) {
            response.headers.insert(header::AGE, value);
        }
        Some(response)
    }

    pub fn insert(
        &self,
        key: String,
        response: CachedResponse,
        ttl: Duration,
        request_headers: &HeaderMap,
    ) {
        let size = response.body.len() as u64;
        if ttl.is_zero() || size > self.max_size {
            metrics::cache_rejected();
            return;
        }
        let Some(vary) = VaryKey::from_response(&response.headers, request_headers) else {
            metrics::cache_rejected();
            return;
        };

        let now = Instant::now();
        let mut guard = self.inner.write().expect("cache lock poisoned");
        let inner = &mut *guard;

        // Replace the entry for this variant when it already exists.
        let mut empty_key = false;
        if let Some(variants) = inner.entries.get_mut(&key) {
            if let Some(position) = variants.iter().position(|entry| entry.vary == vary) {
                let previous = variants.remove(position);
                inner.size = inner.size.saturating_sub(previous.size);
                if let Some(order_position) = inner
                    .order
                    .iter()
                    .position(|(_, item)| *item == previous.token)
                {
                    inner.order.remove(order_position);
                }
            }
            empty_key = variants.is_empty();
        }
        if empty_key {
            inner.entries.remove(&key);
        }

        while inner.size.saturating_add(size) > self.max_size {
            let Some((old_key, token)) = inner.order.pop_front() else {
                break;
            };
            let mut remove_key = false;
            if let Some(variants) = inner.entries.get_mut(&old_key) {
                if let Some(position) = variants.iter().position(|entry| entry.token == token) {
                    let removed = variants.remove(position);
                    inner.size = inner.size.saturating_sub(removed.size);
                    metrics::cache_eviction();
                }
                remove_key = variants.is_empty();
            }
            if remove_key {
                inner.entries.remove(&old_key);
            }
        }

        let token = inner.next_token;
        inner.next_token = inner.next_token.wrapping_add(1);
        inner.size = inner.size.saturating_add(size);
        inner.order.push_back((key.clone(), token));
        inner.entries.entry(key).or_default().push(Entry {
            response,
            stored: now,
            expires: now + ttl,
            size,
            vary,
            token,
        });
        metrics::cache_store();
    }

    pub fn clear(&self) {
        let mut guard = self.inner.write().expect("cache lock poisoned");
        let inner = &mut *guard;
        inner.entries.clear();
        inner.order.clear();
        inner.size = 0;
        metrics::cache_clear();
    }

    /// Occupancy snapshot used by the metrics endpoint.
    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.read().expect("cache lock poisoned");
        CacheStats {
            entries: inner.entries.values().map(Vec::len).sum(),
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

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn entries_expire() {
        let cache = Cache::new(1024);
        let request = HeaderMap::new();
        cache.insert(
            "a".to_string(),
            response(b"hello"),
            Duration::from_secs(10),
            &request,
        );
        let now = Instant::now();
        assert!(cache.get_at("a", &request, now).is_some());

        let later = now + Duration::from_secs(11);
        assert!(cache.get_at("a", &request, later).is_none());
        assert!(cache.get_at("a", &request, later).is_none());
    }

    #[test]
    fn evicts_the_oldest_entry() {
        let cache = Cache::new(10);
        let request = HeaderMap::new();
        cache.insert(
            "a".into(),
            response(b"12345"),
            Duration::from_secs(10),
            &request,
        );
        cache.insert(
            "b".into(),
            response(b"1234"),
            Duration::from_secs(10),
            &request,
        );
        assert!(cache.get("a", &request).is_some());

        // Touching "a" makes "b" the LRU entry; "c" evicts it.
        cache.insert(
            "c".into(),
            response(b"12345"),
            Duration::from_secs(10),
            &request,
        );
        assert!(cache.get("b", &request).is_none());
        assert!(cache.get("a", &request).is_some());
        assert!(cache.get("c", &request).is_some());
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
    fn vari_are_stored_separately() {
        let cache = Cache::new(1024);
        let gzip = headers(&[("accept-encoding", "gzip")]);
        let brotli = headers(&[("accept-encoding", "br")]);
        let plain = HeaderMap::new();

        let mut gzip_body = response(b"gzip-body");
        gzip_body
            .headers
            .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
        cache.insert("k".into(), gzip_body, Duration::from_secs(10), &gzip);

        assert_eq!(cache.stats().entries, 1);
        assert!(cache.get("k", &gzip).is_some());
        assert!(cache.get("k", &brotli).is_none());
        assert!(cache.get("k", &plain).is_none());

        let mut brotli_body = response(b"br-body");
        brotli_body
            .headers
            .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
        cache.insert("k".into(), brotli_body, Duration::from_secs(10), &brotli);
        assert_eq!(cache.stats().entries, 2);
        assert_eq!(
            cache.get("k", &brotli).unwrap().body,
            Bytes::from_static(b"br-body")
        );
    }

    #[test]
    fn vary_star_is_not_cached() {
        let cache = Cache::new(1024);
        let mut star = response(b"body");
        star.headers
            .insert(header::VARY, HeaderValue::from_static("*"));
        cache.insert("k".into(), star, Duration::from_secs(10), &HeaderMap::new());
        assert_eq!(cache.stats().entries, 0);
        assert!(cache.get("k", &HeaderMap::new()).is_none());
    }

    #[test]
    fn clear_removes_everything() {
        let cache = Cache::new(1024);
        cache.insert(
            "a".into(),
            response(b"body"),
            Duration::from_secs(10),
            &HeaderMap::new(),
        );
        cache.clear();
        assert!(cache.get("a", &HeaderMap::new()).is_none());
        assert_eq!(cache.stats().entries, 0);
    }
}
