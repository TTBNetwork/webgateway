//! Per-client-IP token-bucket rate limiting for the data plane.
//!
//! The limiter is intentionally in-process: it protects a single gateway from
//! abusive clients without an extra network hop. Counters are kept in 16
//! shards so concurrent requests for different IPs do not contend on one lock.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::bail;
use serde::Deserialize;

const SHARDS: usize = 16;
const STALE_AFTER: Duration = Duration::from_secs(60);
const MAX_TRACKED_FLOOR: usize = 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub enabled: bool,
    /// Sustained requests per second allowed per client IP.
    pub requests_per_second: f64,
    /// Token bucket capacity; 0 means "one second worth of requests".
    pub burst: u32,
    /// Upper bound on tracked client IPs; stale entries are pruned first.
    pub max_tracked_ips: usize,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            requests_per_second: 0.0,
            burst: 0,
            max_tracked_ips: 100_000,
        }
    }
}

impl RateLimitConfig {
    /// Reject an enabled limiter that can never allow traffic and normalize
    /// the burst/table defaults.
    pub fn sanitized(mut self) -> anyhow::Result<Self> {
        if !self.requests_per_second.is_finite() || self.requests_per_second < 0.0 {
            bail!("rate limit requests_per_second must be a finite, non-negative number");
        }
        if self.requests_per_second > 1_000_000.0 {
            bail!("rate limit requests_per_second is unreasonably high");
        }
        if self.enabled && self.requests_per_second == 0.0 {
            bail!("rate limit is enabled but requests_per_second is 0");
        }
        if self.burst == 0 {
            self.burst = self.requests_per_second.ceil().max(1.0) as u32;
        }
        self.max_tracked_ips = self.max_tracked_ips.max(MAX_TRACKED_FLOOR);
        Ok(self)
    }
}

/// Result of one limiter decision.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub allowed: bool,
    pub retry_after_secs: u64,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

pub struct RateLimiter {
    config: RateLimitConfig,
    burst: f64,
    shards: Vec<Mutex<HashMap<IpAddr, Bucket>>>,
    tracked: AtomicUsize,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        let burst = if config.burst == 0 {
            config.requests_per_second.ceil().max(1.0)
        } else {
            f64::from(config.burst)
        };
        Self {
            config,
            burst,
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
            tracked: AtomicUsize::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled && self.config.requests_per_second > 0.0
    }

    /// Allow or reject one request from `ip`. Returns `None` when disabled.
    pub fn check(&self, ip: IpAddr) -> Option<Decision> {
        self.check_at(ip, Instant::now())
    }

    pub(crate) fn check_at(&self, ip: IpAddr, now: Instant) -> Option<Decision> {
        if !self.enabled() {
            return None;
        }
        let shard_index = self.shard_index(ip);
        let mut buckets = self.shards[shard_index]
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let at_capacity = self.tracked.load(Ordering::Relaxed) >= self.config.max_tracked_ips;
        if at_capacity && !buckets.contains_key(&ip) {
            drop(buckets);
            self.prune_stale(now);
            buckets = self.shards[shard_index]
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if self.tracked.load(Ordering::Relaxed) >= self.config.max_tracked_ips {
                // A full table of active clients must not block unrelated
                // traffic; fail open and let the next scrape/admin notice.
                return Some(Decision {
                    allowed: true,
                    retry_after_secs: 0,
                });
            }
        }

        let is_new = !buckets.contains_key(&ip);
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.burst,
            last: now,
        });
        if is_new {
            self.tracked.fetch_add(1, Ordering::Relaxed);
        }
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.config.requests_per_second).min(self.burst);
        bucket.last = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Some(Decision {
                allowed: true,
                retry_after_secs: 0,
            })
        } else {
            let retry_after_secs =
                ((1.0 - bucket.tokens) / self.config.requests_per_second).ceil() as u64;
            Some(Decision {
                allowed: false,
                retry_after_secs: retry_after_secs.max(1),
            })
        }
    }

    /// Number of client IPs currently tracked (used by the metrics endpoint).
    pub fn tracked_ips(&self) -> usize {
        self.tracked.load(Ordering::Relaxed)
    }

    /// Drop entries that have not been seen for a while; used before failing
    /// open or accepting a new client once the table is full.
    fn prune_stale(&self, now: Instant) {
        for shard in &self.shards {
            let mut buckets = shard
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let before = buckets.len();
            buckets.retain(|_, bucket| now.saturating_duration_since(bucket.last) < STALE_AFTER);
            let removed = before.saturating_sub(buckets.len());
            if removed > 0 {
                self.tracked.fetch_sub(removed, Ordering::Relaxed);
            }
        }
    }

    fn shard_index(&self, ip: IpAddr) -> usize {
        let octets = match ip {
            IpAddr::V4(address) => address.octets().to_vec(),
            IpAddr::V6(address) => address.octets().to_vec(),
        };
        let mut hash = 0usize;
        for octet in octets {
            hash = hash.wrapping_mul(31).wrapping_add(usize::from(octet));
        }
        hash % self.shards.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(requests_per_second: f64, burst: u32) -> RateLimiter {
        RateLimiter::new(RateLimitConfig {
            enabled: true,
            requests_per_second,
            burst,
            ..RateLimitConfig::default()
        })
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn disabled_limiter_never_limits() {
        let limiter = RateLimiter::new(RateLimitConfig::default());
        assert!(!limiter.enabled());
        assert!(limiter.check(ip("192.0.2.1")).is_none());
    }

    #[test]
    fn burst_then_deny_and_refill() {
        let limiter = limiter(1.0, 2);
        let start = Instant::now();
        assert!(limiter.check_at(ip("192.0.2.1"), start).unwrap().allowed);
        assert!(limiter.check_at(ip("192.0.2.1"), start).unwrap().allowed);
        let denied = limiter.check_at(ip("192.0.2.1"), start).unwrap();
        assert!(!denied.allowed);
        assert_eq!(denied.retry_after_secs, 1);
        let refilled = limiter.check_at(ip("192.0.2.1"), start + Duration::from_secs(1));
        assert!(refilled.unwrap().allowed);
    }

    #[test]
    fn separate_ips_have_separate_buckets() {
        let limiter = limiter(1.0, 1);
        let start = Instant::now();
        assert!(limiter.check_at(ip("192.0.2.1"), start).unwrap().allowed);
        assert!(!limiter.check_at(ip("192.0.2.1"), start).unwrap().allowed);
        assert!(limiter.check_at(ip("192.0.2.2"), start).unwrap().allowed);
        assert_eq!(limiter.tracked_ips(), 2);
    }

    #[test]
    fn tracks_entries_globally_across_shards() {
        let limiter = limiter(100.0, 1);
        let start = Instant::now();
        for octet in 1..=32 {
            assert!(
                limiter
                    .check_at(ip(&format!("198.51.100.{octet}")), start)
                    .unwrap()
                    .allowed
            );
        }
        assert_eq!(limiter.tracked_ips(), 32);
    }

    #[test]
    fn stale_entries_are_pruned_when_the_table_is_full() {
        let limiter = RateLimiter::new(RateLimitConfig {
            enabled: true,
            requests_per_second: 1.0,
            burst: 1,
            max_tracked_ips: 2,
        });
        let start = Instant::now();
        assert!(limiter.check_at(ip("192.0.2.1"), start).unwrap().allowed);
        assert!(limiter.check_at(ip("192.0.2.2"), start).unwrap().allowed);
        // Table full with fresh entries: fail open, do not grow. Shared
        // shards mean tracked_ips is the sum across all shards.
        let allowed = limiter.check_at(ip("192.0.2.3"), start).unwrap();
        assert!(allowed.allowed);
        assert_eq!(limiter.tracked_ips(), 2);
        // After the stale window the old entries are pruned and tracking resumes.
        let later = start + Duration::from_secs(61);
        assert!(limiter.check_at(ip("192.0.2.3"), later).unwrap().allowed);
        assert_eq!(limiter.tracked_ips(), 1);
    }

    #[test]
    fn sanitize_rejects_zero_rate_and_normalizes_defaults() {
        assert!(
            RateLimitConfig {
                enabled: true,
                requests_per_second: 0.0,
                ..RateLimitConfig::default()
            }
            .sanitized()
            .is_err()
        );
        let config = RateLimitConfig {
            enabled: true,
            requests_per_second: 2.5,
            ..RateLimitConfig::default()
        }
        .sanitized()
        .unwrap();
        assert_eq!(config.burst, 3);
        assert_eq!(config.max_tracked_ips, 100_000);
    }
}
