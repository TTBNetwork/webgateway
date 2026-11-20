//! Upstream health checking: active probes plus passive failure marking.
//!
//! Sites opt in through `config.health_check`. Healthy state lives in an
//! in-process registry keyed by site id and backend URL; the router prefers
//! healthy backends and falls back to the full set when none is healthy, so a
//! single bad probe never removes every backend.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::bail;
use bytes::Bytes;
use http::{Method, Request};
use http_body_util::Full;
use serde::Deserialize;
use tokio::task::JoinHandle;
use tracing::{Level, event};

use crate::{
    metrics,
    state::{AppState, Backend, Site},
};

/// Configuration stored in `websites.config.health_check`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HealthCheckConfig {
    pub enabled: bool,
    /// Request path used for active probes (must start with '/').
    pub path: String,
    pub interval_secs: u64,
    pub timeout_secs: u64,
    /// Consecutive failures before the backend is marked unhealthy.
    pub unhealthy_threshold: u32,
    /// Consecutive successes before it is marked healthy again.
    pub healthy_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: "/".to_string(),
            interval_secs: 10,
            timeout_secs: 2,
            unhealthy_threshold: 2,
            healthy_threshold: 2,
        }
    }
}

impl HealthCheckConfig {
    pub fn sanitized(mut self) -> anyhow::Result<Self> {
        if !self.path.starts_with('/') {
            bail!("health_check.path must start with '/'");
        }
        if self.interval_secs == 0 || self.interval_secs > 3600 {
            bail!("health_check.interval_secs must be between 1 and 3600");
        }
        if self.timeout_secs == 0 {
            bail!("health_check.timeout_secs must be at least 1");
        }
        self.timeout_secs = self.timeout_secs.min(self.interval_secs);
        self.unhealthy_threshold = self.unhealthy_threshold.clamp(1, 100);
        self.healthy_threshold = self.healthy_threshold.clamp(1, 100);
        Ok(self)
    }
}

/// Registry key: one entry per site/backend pair.
pub fn backend_key(site_id: &str, backend_url: &str) -> String {
    format!("{site_id}|{backend_url}")
}

struct BackendStatus {
    healthy: AtomicBool,
    failures: AtomicU32,
    successes: AtomicU32,
    last_checked: Mutex<Option<Instant>>,
}

impl Default for BackendStatus {
    fn default() -> Self {
        Self {
            // Unknown backends start healthy so traffic is not blackholed
            // before the first probe completes.
            healthy: AtomicBool::new(true),
            failures: AtomicU32::new(0),
            successes: AtomicU32::new(0),
            last_checked: Mutex::new(None),
        }
    }
}

#[derive(Default)]
pub struct BackendHealth {
    entries: RwLock<HashMap<String, Arc<BackendStatus>>>,
}

impl BackendHealth {
    /// Healthy unless a probe marked the backend down. Unknown keys are up.
    pub fn is_healthy(&self, key: &str) -> bool {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .is_none_or(|status| status.healthy.load(Ordering::Relaxed))
    }

    pub fn record_success(&self, key: &str, healthy_threshold: u32) {
        let status = self.entry(key);
        status.failures.store(0, Ordering::Relaxed);
        if !status.healthy.load(Ordering::Relaxed) {
            let successes = status.successes.fetch_add(1, Ordering::Relaxed) + 1;
            if successes >= healthy_threshold.max(1) {
                status.healthy.store(true, Ordering::Relaxed);
                status.successes.store(0, Ordering::Relaxed);
            }
        }
    }

    pub fn record_failure(&self, key: &str, unhealthy_threshold: u32) {
        let status = self.entry(key);
        status.successes.store(0, Ordering::Relaxed);
        if status.healthy.load(Ordering::Relaxed) {
            let failures = status.failures.fetch_add(1, Ordering::Relaxed) + 1;
            if failures >= unhealthy_threshold.max(1) {
                status.healthy.store(false, Ordering::Relaxed);
                status.failures.store(0, Ordering::Relaxed);
            }
        }
    }

    /// True when the probe interval elapsed; updates the last probe time.
    pub fn begin_check(&self, key: &str, interval_secs: u64) -> bool {
        self.begin_check_at(key, interval_secs, Instant::now())
    }

    pub(crate) fn begin_check_at(&self, key: &str, interval_secs: u64, now: Instant) -> bool {
        let status = self.entry(key);
        let mut last = status
            .last_checked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.is_some_and(|previous| {
            now.saturating_duration_since(previous) < Duration::from_secs(interval_secs.max(1))
        }) {
            return false;
        }
        *last = Some(now);
        true
    }

    pub fn unhealthy_count(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|status| !status.healthy.load(Ordering::Relaxed))
            .count()
    }

    fn retain(&self, live: &HashSet<String>) {
        self.entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, _| live.contains(key));
    }

    fn entry(&self, key: &str) -> Arc<BackendStatus> {
        if let Some(status) = self
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
        {
            return status.clone();
        }
        self.entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(BackendStatus::default()))
            .clone()
    }
}

/// Apply a passive observation from the request path. Only sites with health
/// checking enabled are tracked, because recovery needs the active probes.
pub fn record_passive(state: &AppState, site: &Site, backend: &Backend, success: bool) {
    if !site.health_check.enabled {
        return;
    }
    let key = backend_key(&site.id, backend.url.as_str());
    if success {
        state
            .backend_health
            .record_success(&key, site.health_check.healthy_threshold);
    } else {
        state
            .backend_health
            .record_failure(&key, site.health_check.unhealthy_threshold);
    }
}

struct ProbeTarget {
    key: String,
    url: String,
    config: HealthCheckConfig,
}

/// Run the probe scheduler until the task is aborted.
pub fn spawn(state: Arc<AppState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let targets = collect_targets(&state);
            let live: HashSet<String> = targets.iter().map(|target| target.key.clone()).collect();
            state.backend_health.retain(&live);
            for target in targets {
                if !state
                    .backend_health
                    .begin_check(&target.key, target.config.interval_secs)
                {
                    continue;
                }
                let state = state.clone();
                tokio::spawn(async move { run_probe(state, target).await });
            }
        }
    })
}

fn collect_targets(state: &AppState) -> Vec<ProbeTarget> {
    let routing = state
        .routing
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut targets = Vec::new();
    for site in routing.sites() {
        if !site.health_check.enabled {
            continue;
        }
        for backend in &site.backends {
            targets.push(ProbeTarget {
                key: backend_key(&site.id, backend.url.as_str()),
                url: backend.url.to_string(),
                config: site.health_check.clone(),
            });
        }
    }
    targets
}

async fn run_probe(state: Arc<AppState>, target: ProbeTarget) {
    let url = probe_url(&target.url, &target.config.path);
    let request = match Request::builder()
        .method(Method::GET)
        .uri(&url)
        .body(Full::new(Bytes::new()))
    {
        Ok(request) => request,
        Err(error) => {
            metrics::record_health_check(false);
            state
                .backend_health
                .record_failure(&target.key, target.config.unhealthy_threshold);
            event!(
                Level::WARN,
                backend = %target.url,
                %error,
                "invalid health check request"
            );
            return;
        }
    };

    let timeout = Duration::from_secs(target.config.timeout_secs.max(1));
    let result = tokio::time::timeout(timeout, state.buffered_client.request(request)).await;
    let healthy = matches!(&result, Ok(Ok(response)) if response.status().as_u16() < 500);
    metrics::record_health_check(healthy);
    if healthy {
        state
            .backend_health
            .record_success(&target.key, target.config.healthy_threshold);
    } else {
        let reason = match &result {
            Ok(Ok(response)) => format!("status {}", response.status()),
            Ok(Err(error)) => format!("{error:#}"),
            Err(_) => "timeout".to_string(),
        };
        state
            .backend_health
            .record_failure(&target.key, target.config.unhealthy_threshold);
        event!(
            Level::WARN,
            backend = %target.url,
            reason = %reason,
            "upstream health check failed"
        );
    }
}

fn probe_url(backend: &str, path: &str) -> String {
    let base = backend.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_unhealthy_after_threshold_and_recovers() {
        let health = BackendHealth::default();
        let key = backend_key("site", "http://127.0.0.1:8080/");
        assert!(health.is_healthy(&key));
        health.record_failure(&key, 2);
        assert!(health.is_healthy(&key));
        health.record_failure(&key, 2);
        assert!(!health.is_healthy(&key));
        assert_eq!(health.unhealthy_count(), 1);
        health.record_success(&key, 2);
        assert!(!health.is_healthy(&key));
        health.record_success(&key, 2);
        assert!(health.is_healthy(&key));
        assert_eq!(health.unhealthy_count(), 0);
    }

    #[test]
    fn success_resets_failure_streak() {
        let health = BackendHealth::default();
        let key = backend_key("site", "https://backend/");
        health.record_failure(&key, 2);
        health.record_success(&key, 1);
        health.record_failure(&key, 2);
        assert!(health.is_healthy(&key));
    }

    #[test]
    fn begin_check_respects_the_interval() {
        let health = BackendHealth::default();
        let key = backend_key("site", "http://127.0.0.1:8080/");
        let start = Instant::now();
        assert!(health.begin_check_at(&key, 2, start));
        assert!(!health.begin_check_at(&key, 2, start + Duration::from_millis(500)));
        assert!(health.begin_check_at(&key, 2, start + Duration::from_secs(2)));
        // Intervals below one second are clamped to one second.
        assert!(health.begin_check_at(&key, 0, start + Duration::from_secs(3)));
    }

    #[test]
    fn retain_drops_removed_backends() {
        let health = BackendHealth::default();
        let keep = backend_key("site", "http://keep/");
        let drop_key = backend_key("site", "http://drop/");
        health.record_failure(&keep, 1);
        health.record_failure(&drop_key, 1);
        assert_eq!(health.unhealthy_count(), 2);
        health.retain(&HashSet::from([keep]));
        assert_eq!(health.unhealthy_count(), 1);
    }

    #[test]
    fn validates_and_sanitizes_config() {
        let bad = HealthCheckConfig {
            path: "healthz".to_string(),
            ..HealthCheckConfig::default()
        };
        assert!(bad.sanitized().is_err());
        let config = HealthCheckConfig {
            enabled: true,
            interval_secs: 1,
            timeout_secs: 5,
            unhealthy_threshold: 0,
            ..HealthCheckConfig::default()
        }
        .sanitized()
        .unwrap();
        assert_eq!(config.timeout_secs, 1);
        assert_eq!(config.unhealthy_threshold, 1);
        assert_eq!(
            probe_url("http://127.0.0.1:8080", "/health"),
            "http://127.0.0.1:8080/health"
        );
        assert_eq!(
            probe_url("https://upstream/base", "/"),
            "https://upstream/base/"
        );
    }
}
