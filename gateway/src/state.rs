//! Routing tables, per-site proxy headers, certificate store and process state.

use std::{
    collections::HashMap,
    io::Cursor,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::Context;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue};
use http_body_util::{Full, combinators::BoxBody};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::sign::CertifiedKey;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use url::Url;

use crate::access::AccessEvent;
use crate::cache::{Cache, CacheConfig};
use crate::node::NodeInfo;
use crate::ratelimit::{RateLimitConfig, RateLimiter};

pub type UpstreamConnector = HttpsConnector<HttpConnector>;
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type GatewayBody = BoxBody<Bytes, BoxError>;
pub type UpstreamClient = Client<UpstreamConnector, GatewayBody>;
pub type BufferedUpstreamClient = Client<UpstreamConnector, Full<Bytes>>;

/// Build the upstream TLS client configuration (webpki roots + optional PEM CA).
///
/// HTTP clients let hyper-rustls configure ALPN; the WebSocket upgrade path
/// derives its own config with ALPN fixed to http/1.1.
pub fn upstream_tls_config(ca_file: Option<&Path>) -> anyhow::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    if let Some(path) = ca_file {
        let pem = std::fs::read(path)
            .with_context(|| format!("failed to read upstream CA file {}", path.display()))?;
        let mut cursor = Cursor::new(pem);
        let certificates = rustls_pemfile::certs(&mut cursor)
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to parse upstream CA file {}", path.display()))?;
        for certificate in certificates {
            roots
                .add(certificate)
                .with_context(|| format!("failed to add a CA from {}", path.display()))?;
        }
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("failed to select TLS protocol versions")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tls)
}

/// Build the Alt-Svc value that advertises the HTTP/3 UDP port.
pub fn alt_svc_value(port: u16) -> HeaderValue {
    HeaderValue::from_str(&format!("h3=\":{port}\"; ma=86400"))
        .expect("the Alt-Svc value is always valid")
}

/// Build the upstream connector used for both HTTP/1.1 and HTTP/2 backends.
///
/// hyper-rustls configures ALPN itself; pre-setting it panics.
pub fn upstream_connector(tls: rustls::ClientConfig) -> UpstreamConnector {
    HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build()
}

/// Runtime limits assembled from [server] configuration.
pub struct StateLimits {
    pub cache_max_size_bytes: u64,
    pub max_request_body_bytes: u64,
    pub max_response_body_bytes: u64,
    pub connect_ports: Vec<u16>,
    pub h3_port: Option<u16>,
    pub rate_limit: RateLimitConfig,
}

pub struct AppState {
    pub routing: RwLock<Arc<Routing>>,
    pub certs: Arc<RwLock<Arc<CertStore>>>,
    pub tls_acceptor: TlsAcceptor,
    pub client: UpstreamClient,
    /// HTTP/2-only client used for gRPC backends (h2c).
    pub grpc_client: UpstreamClient,
    /// HTTP/1.1 client with a buffered body, used by the HTTP/3 path.
    pub buffered_client: BufferedUpstreamClient,
    /// TLS client configuration for WebSocket upgrades (ALPN http/1.1).
    pub websocket_tls: Arc<rustls::ClientConfig>,
    pub cache: Cache,
    pub logs: mpsc::Sender<AccessEvent>,
    pub node: NodeInfo,
    pub max_request_body_bytes: u64,
    pub max_response_body_bytes: u64,
    /// Ports allowed as HTTP CONNECT tunnel targets.
    pub connect_ports: Arc<[u16]>,
    /// Alt-Svc header advertised on TLS responses when HTTP/3 is enabled.
    pub alt_svc: Option<HeaderValue>,
    /// Per-client-IP rate limiter (disabled by default).
    pub rate_limiter: RateLimiter,
}

impl AppState {
    pub fn new(
        node: NodeInfo,
        certs: Arc<RwLock<Arc<CertStore>>>,
        logs: mpsc::Sender<AccessEvent>,
        upstream_ca_file: Option<&Path>,
        limits: StateLimits,
    ) -> anyhow::Result<Self> {
        let StateLimits {
            cache_max_size_bytes,
            max_request_body_bytes,
            max_response_body_bytes,
            connect_ports,
            h3_port,
            rate_limit,
        } = limits;
        let tls_acceptor = crate::tls::acceptor(certs.clone())?;
        let upstream_tls = upstream_tls_config(upstream_ca_file)?;
        let connector = upstream_connector(upstream_tls.clone());
        let client: UpstreamClient =
            Client::builder(hyper_util::rt::TokioExecutor::new()).build(connector.clone());

        let mut websocket_tls = upstream_tls;
        websocket_tls.alpn_protocols = vec![b"http/1.1".to_vec()];

        let mut grpc_builder = Client::builder(hyper_util::rt::TokioExecutor::new());
        grpc_builder.http2_only(true);
        let grpc_client: UpstreamClient = grpc_builder.build(connector.clone());
        let buffered_client: BufferedUpstreamClient =
            Client::builder(hyper_util::rt::TokioExecutor::new()).build(connector);

        Ok(Self {
            routing: RwLock::new(Arc::new(Routing::default())),
            certs,
            tls_acceptor,
            client,
            grpc_client,
            buffered_client,
            websocket_tls: Arc::new(websocket_tls),
            cache: Cache::new(cache_max_size_bytes),
            rate_limiter: RateLimiter::new(rate_limit),
            logs,
            node,
            max_request_body_bytes,
            max_response_body_bytes,
            connect_ports: connect_ports.into(),
            alt_svc: h3_port.map(alt_svc_value),
        })
    }

    pub fn current_default_cert(&self) -> Arc<CertifiedKey> {
        self.certs
            .read()
            .expect("certificate store lock poisoned")
            .default_key()
    }

    pub fn apply_config(&self, routing: Arc<Routing>, certs: Arc<CertStore>) {
        self.cache.clear();
        *self.routing.write().expect("routing lock poisoned") = routing;
        *self.certs.write().expect("certificate store lock poisoned") = certs;
    }
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub url: Url,
    /// Legacy prefix match. Kept for compatibility with existing rows.
    pub match_path: Option<String>,
    pub rules: MatchRules,
    /// Relative share inside the active priority group (minimum 1).
    pub weight: u32,
    /// Lower values are preferred; higher values form standby groups.
    pub priority: u32,
}

/// Per-backend routing rules; all configured conditions must match.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct MatchRules {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub query: HashMap<String, String>,
    #[serde(default)]
    pub method: Vec<String>,
}

impl MatchRules {
    pub fn matches(&self, context: &MatchContext<'_>) -> bool {
        if let Some(pattern) = &self.path
            && !glob_match(pattern, context.path)
        {
            return false;
        }

        if let Some(pattern) = &self.user_agent {
            let Some(user_agent) = context.user_agent else {
                return false;
            };
            if !glob_match(
                &pattern.to_ascii_lowercase(),
                &user_agent.to_ascii_lowercase(),
            ) {
                return false;
            }
        }

        if !self.query.is_empty() {
            let pairs: Vec<(String, String)> = context
                .query
                .map(|query| {
                    url::form_urlencoded::parse(query.as_bytes())
                        .into_owned()
                        .collect()
                })
                .unwrap_or_default();

            for (key, expected) in &self.query {
                let matched = pairs
                    .iter()
                    .any(|(name, value)| name == key && (expected.is_empty() || value == expected));
                if !matched {
                    return false;
                }
            }
        }

        self.method.is_empty()
            || self
                .method
                .iter()
                .any(|method| method.eq_ignore_ascii_case(context.method))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MatchContext<'a> {
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub method: &'a str,
    pub user_agent: Option<&'a str>,
}

/// Minimal glob matcher: `*` matches any sequence, `?` matches one character.
fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();

    let (mut pattern_index, mut value_index) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_value_index = 0usize;

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some(pattern_index);
            star_value_index = value_index;
            pattern_index += 1;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }

    pattern_index == pattern.len()
}

#[derive(Debug, Clone)]
pub struct HeaderRule {
    pub name: HeaderName,
    pub template: String,
}

/// Per-website request header rules.
///
/// The defaults mirror the previous hard-coded behaviour; a website config can
/// replace them with its own map or disable them by providing an empty map.
#[derive(Debug, Clone)]
pub struct ProxyHeaders {
    pub set: Vec<HeaderRule>,
    pub remove: Vec<HeaderName>,
}

impl Default for ProxyHeaders {
    fn default() -> Self {
        Self::defaults()
    }
}

impl ProxyHeaders {
    pub fn defaults() -> Self {
        let defaults = [
            ("X-Real-IP", "$remote_addr"),
            ("X-Forwarded-For", "$remote_addr"),
            ("X-Forwarded-Proto", "$scheme"),
            ("X-Forwarded-Host", "$host"),
        ]
        .into_iter()
        .map(|(name, template)| (name.to_string(), template.to_string()))
        .collect();

        Self::from_parts(defaults, Vec::new())
    }

    pub fn from_config(set: HashMap<String, String>, remove: Vec<String>) -> Self {
        Self::from_parts(set, remove)
    }

    fn from_parts(set: HashMap<String, String>, remove: Vec<String>) -> Self {
        let mut rules: Vec<HeaderRule> = set
            .into_iter()
            .filter_map(|(name, template)| {
                parse_header_name(&name).map(|name| HeaderRule { name, template })
            })
            .collect();
        rules.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));

        let remove = remove
            .into_iter()
            .filter_map(|name| parse_header_name(&name))
            .collect();

        Self { set: rules, remove }
    }

    /// Apply the configured set/remove rules to outgoing upstream headers.
    pub fn apply(&self, headers: &mut HeaderMap, host: &str, scheme: &str, client_ip: IpAddr) {
        let remote_addr = client_ip.to_string();

        for rule in &self.set {
            let value = rule
                .template
                .replace("$remote_addr", &remote_addr)
                .replace("$scheme", scheme)
                .replace("$host", host);

            match HeaderValue::from_str(&value) {
                Ok(value) => {
                    headers.insert(rule.name.clone(), value);
                }
                Err(error) => {
                    tracing::debug!(header = %rule.name, value = %value, %error, "ignoring invalid proxy header value");
                }
            }
        }

        for name in &self.remove {
            headers.remove(name);
        }
    }
}

/// How the real client IP is derived for a website.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientIpMode {
    #[serde(rename = "raw", alias = "Raw")]
    #[default]
    Raw,
    #[serde(rename = "proxyProtocol", alias = "proxy_protocol")]
    ProxyProtocol,
    #[serde(rename = "xForwardedFor", alias = "x-forwarded-for")]
    XForwardedFor(Option<usize>),
    #[serde(rename = "xRealIP", alias = "xRealIp", alias = "x-real-ip")]
    XRealIP,
}

impl ClientIpMode {
    pub fn resolve(
        &self,
        peer: SocketAddr,
        proxy_source: Option<SocketAddr>,
        headers: &HeaderMap,
    ) -> IpAddr {
        match self {
            Self::Raw => peer.ip(),
            Self::ProxyProtocol => proxy_source
                .map(|source| source.ip())
                .unwrap_or_else(|| peer.ip()),
            Self::XForwardedFor(index) => {
                forwarded_for(headers, *index).unwrap_or_else(|| peer.ip())
            }
            Self::XRealIP => header_ip(headers, "x-real-ip").unwrap_or_else(|| peer.ip()),
        }
    }
}

fn forwarded_for(headers: &HeaderMap, index: Option<usize>) -> Option<IpAddr> {
    let value = headers.get("x-forwarded-for")?.to_str().ok()?;
    let hops: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|hop| !hop.is_empty())
        .collect();

    let index = index.unwrap_or(0);
    hops.get(index)
        .or_else(|| hops.last())
        .and_then(|hop| hop.parse().ok())
}

fn header_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

fn parse_header_name(name: &str) -> Option<HeaderName> {
    match HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes()) {
        Ok(name) => Some(name),
        Err(error) => {
            tracing::warn!(header = %name, %error, "ignoring invalid proxy header name");
            None
        }
    }
}

#[derive(Debug)]
pub struct Site {
    pub id: String,
    pub name: Option<String>,
    pub hosts: Vec<String>,
    pub ports: Vec<u16>,
    pub backends: Vec<Backend>,
    pub headers: ProxyHeaders,
    pub client_ip: ClientIpMode,
    pub cache: CacheConfig,
    selector: AtomicUsize,
}

impl Site {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        name: Option<String>,
        hosts: Vec<String>,
        ports: Vec<u16>,
        backends: Vec<Backend>,
        headers: ProxyHeaders,
        client_ip: ClientIpMode,
        cache: CacheConfig,
    ) -> Self {
        Self {
            id,
            name,
            hosts,
            ports,
            backends,
            headers,
            client_ip,
            cache,
            selector: AtomicUsize::new(0),
        }
    }

    /// Pick the next backend in round-robin order, preferring matching rules.
    pub fn next_backend(&self, context: &MatchContext<'_>) -> Option<&Backend> {
        let matched: Vec<&Backend> = self
            .backends
            .iter()
            .filter(|backend| {
                let path_matches = match backend.match_path.as_deref() {
                    Some(prefix) if !prefix.is_empty() => context.path.starts_with(prefix),
                    _ => true,
                };
                path_matches && backend.rules.matches(context)
            })
            .collect();

        let candidates = if matched.is_empty() {
            self.backends.iter().collect::<Vec<_>>()
        } else {
            matched
        };
        if candidates.is_empty() {
            return None;
        }

        // Prefer the lowest priority group (primary); higher values are standby.
        let priority = candidates
            .iter()
            .map(|backend| backend.priority)
            .min()
            .unwrap_or(0);
        let primary: Vec<&Backend> = candidates
            .into_iter()
            .filter(|backend| backend.priority == priority)
            .collect();
        let total_weight: u64 = primary
            .iter()
            .map(|backend| u64::from(backend.weight.max(1)))
            .sum();
        if total_weight == 0 {
            return primary.first().copied();
        }

        let mut index = self.selector.fetch_add(1, Ordering::Relaxed) as u64 % total_weight;
        let mut selected = primary[0];
        for backend in &primary {
            let weight = u64::from(backend.weight.max(1));
            if index < weight {
                selected = backend;
                break;
            }
            index -= weight;
        }

        Some(selected)
    }
}

#[derive(Default)]
pub struct Routing {
    exact: HashMap<String, Arc<Site>>,
    wildcard: Vec<(String, Arc<Site>)>,
}

impl Routing {
    pub fn build(sites: Vec<Arc<Site>>) -> Self {
        let mut exact = HashMap::new();
        let mut wildcard = Vec::new();

        for site in sites {
            for host in &site.hosts {
                let pattern = normalize_host(host);
                if pattern.is_empty() {
                    continue;
                }
                if let Some(suffix) = pattern.strip_prefix("*.") {
                    wildcard.push((format!(".{suffix}"), site.clone()));
                } else {
                    exact.insert(pattern, site.clone());
                }
            }
        }

        // Most specific wildcard first.
        wildcard.sort_by_key(|pattern| std::cmp::Reverse(pattern.0.len()));
        Self { exact, wildcard }
    }

    pub fn lookup(&self, host: &str) -> Option<Arc<Site>> {
        let host = normalize_host(host);
        if host.is_empty() {
            return None;
        }

        if let Some(site) = self.exact.get(&host) {
            return Some(site.clone());
        }

        for (suffix, site) in &self.wildcard {
            if let Some(prefix) = host.strip_suffix(suffix.as_str())
                && !prefix.is_empty()
                && !prefix.contains('.')
            {
                return Some(site.clone());
            }
        }

        None
    }

    pub fn host_count(&self) -> usize {
        self.exact.len() + self.wildcard.len()
    }
}

pub struct CertStore {
    exact: HashMap<String, Arc<CertifiedKey>>,
    wildcard: Vec<(String, Arc<CertifiedKey>)>,
    default: Arc<CertifiedKey>,
}

impl CertStore {
    pub fn build(
        entries: Vec<(Vec<String>, Arc<CertifiedKey>)>,
        default: Arc<CertifiedKey>,
    ) -> Self {
        let mut store = Self {
            exact: HashMap::new(),
            wildcard: Vec::new(),
            default,
        };

        for (hostnames, key) in entries {
            for hostname in hostnames {
                let hostname = normalize_host(&hostname);
                if hostname.is_empty() {
                    continue;
                }
                if let Some(suffix) = hostname.strip_prefix("*.") {
                    store.wildcard.push((format!(".{suffix}"), key.clone()));
                } else {
                    store.exact.insert(hostname, key.clone());
                }
            }
        }

        store
            .wildcard
            .sort_by_key(|pattern| std::cmp::Reverse(pattern.0.len()));
        store
    }

    pub fn default_key(&self) -> Arc<CertifiedKey> {
        self.default.clone()
    }

    pub fn hostname_count(&self) -> usize {
        self.exact.len() + self.wildcard.len()
    }

    pub fn lookup(&self, server_name: Option<&str>) -> Option<Arc<CertifiedKey>> {
        let Some(server_name) = server_name else {
            return Some(self.default_key());
        };

        let host = normalize_host(server_name);
        if let Some(key) = self.exact.get(&host) {
            return Some(key.clone());
        }

        for (suffix, key) in &self.wildcard {
            if let Some(prefix) = host.strip_suffix(suffix.as_str())
                && !prefix.is_empty()
                && !prefix.contains('.')
            {
                return Some(key.clone());
            }
        }

        Some(self.default_key())
    }
}

/// Normalize a Host header or certificate hostname for matching.
pub fn normalize_host(host: &str) -> String {
    let host = host.trim();

    let host = if let Some(rest) = host.strip_prefix('[') {
        match rest.find(']') {
            Some(index) => &rest[..index],
            None => host,
        }
    } else if let Some((name, port)) = host.rsplit_once(':')
        && !name.contains(':')
        && !port.is_empty()
        && port.chars().all(|character| character.is_ascii_digit())
    {
        name
    } else {
        host
    };

    host.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_alt_svc_for_h3() {
        assert_eq!(
            alt_svc_value(8443).to_str().unwrap(),
            "h3=\":8443\"; ma=86400"
        );
    }

    fn backend(url: &str, match_path: Option<&str>) -> Backend {
        Backend {
            url: Url::parse(url).unwrap(),
            match_path: match_path.map(str::to_string),
            rules: MatchRules::default(),
            weight: 1,
            priority: 0,
        }
    }

    fn context<'a>(path: &'a str) -> MatchContext<'a> {
        MatchContext {
            path,
            query: None,
            method: "GET",
            user_agent: None,
        }
    }

    fn site(hosts: &[&str], backends: Vec<Backend>) -> Arc<Site> {
        Arc::new(Site::new(
            "site".to_string(),
            None,
            hosts.iter().map(|host| (*host).to_string()).collect(),
            vec![80],
            backends,
            ProxyHeaders::defaults(),
            ClientIpMode::Raw,
            CacheConfig::default(),
        ))
    }

    #[test]
    fn normalizes_host_and_strips_ports() {
        assert_eq!(normalize_host("Example.COM:443"), "example.com");
        assert_eq!(normalize_host("example.com."), "example.com");
        assert_eq!(normalize_host("[::1]:8080"), "::1");
        assert_eq!(normalize_host("127.0.0.1:80"), "127.0.0.1");
    }

    #[test]
    fn routes_exact_and_wildcard_hosts() {
        let routing = Routing::build(vec![
            site(
                &["exact.example.com"],
                vec![backend("http://127.0.0.1:9001", None)],
            ),
            site(
                &["*.example.com"],
                vec![backend("http://127.0.0.1:9002", None)],
            ),
        ]);

        assert!(routing.lookup("exact.example.com:443").is_some());
        assert!(routing.lookup("other.example.com").is_some());
        assert!(routing.lookup("a.b.example.com").is_none());
        assert!(routing.lookup("example.org").is_none());
    }

    #[test]
    fn round_robin_prefers_path_matches() {
        let site = site(
            &["example.com"],
            vec![
                backend("http://127.0.0.1:9001", Some("/api")),
                backend("http://127.0.0.1:9002", None),
            ],
        );

        let api = site.next_backend(&context("/api/v1")).unwrap();
        assert_eq!(api.url.port(), Some(9001));
        let web = site.next_backend(&context("/index.html")).unwrap();
        assert_eq!(web.url.port(), Some(9002));
    }

    #[test]
    fn matches_path_user_agent_query_and_method() {
        let rules = MatchRules {
            path: Some("/api/*".to_string()),
            user_agent: Some("*mobile*".to_string()),
            query: HashMap::from([("version".to_string(), "v2".to_string())]),
            method: vec!["POST".to_string()],
        };

        let matching = MatchContext {
            path: "/api/users",
            query: Some("version=v2&x=1"),
            method: "POST",
            user_agent: Some("Mozilla/5.0 (Mobile)"),
        };

        assert!(rules.matches(&matching));
        assert!(!rules.matches(&MatchContext {
            path: "/web",
            ..matching
        }));
        assert!(!rules.matches(&MatchContext {
            query: Some("version=v1"),
            ..matching
        }));
        assert!(!rules.matches(&MatchContext {
            method: "GET",
            ..matching
        }));
        assert!(!rules.matches(&MatchContext {
            user_agent: Some("curl/8"),
            ..matching
        }));
    }

    #[test]
    fn weights_and_priority_select_backends() {
        let backend = |port: u16, weight: u32, priority: u32| Backend {
            url: Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            match_path: None,
            rules: MatchRules::default(),
            weight,
            priority,
        };

        let site = site(
            &["example.com"],
            vec![
                backend(9001, 3, 0),
                backend(9002, 1, 0),
                backend(9003, 1, 1),
            ],
        );

        let mut counts = HashMap::new();
        for _ in 0..40 {
            let selected = site.next_backend(&context("/")).unwrap();
            *counts.entry(selected.url.port()).or_insert(0) += 1;
        }

        assert_eq!(counts.get(&Some(9001)), Some(&30));
        assert_eq!(counts.get(&Some(9002)), Some(&10));
        assert!(!counts.contains_key(&Some(9003)));
    }

    #[test]
    fn glob_matcher_supports_wildcards() {
        assert!(glob_match("/api/*", "/api/v1/users"));
        assert!(glob_match("*mobile*", "mozilla mobile safari"));
        assert!(!glob_match("*mobile*", "Mozilla Mobile Safari"));
        assert!(!glob_match("/api/*", "/web"));
        assert!(glob_match("/v?", "/v2"));
    }

    #[test]
    fn selects_backend_by_rules() {
        let api_rules = MatchRules {
            path: Some("/api/*".to_string()),
            ..MatchRules::default()
        };

        let site = site(
            &["example.com"],
            vec![
                Backend {
                    url: Url::parse("http://127.0.0.1:9001").unwrap(),
                    match_path: None,
                    rules: api_rules,
                    weight: 1,
                    priority: 0,
                },
                backend("http://127.0.0.1:9002", None),
            ],
        );

        let api = site.next_backend(&context("/api/v1")).unwrap();
        assert_eq!(api.url.port(), Some(9001));
        let web = site.next_backend(&context("/web")).unwrap();
        assert_eq!(web.url.port(), Some(9002));
    }

    #[test]
    fn picks_the_longest_wildcard_certificate() {
        let default = crate::tls::default_certified_key().unwrap();
        let first = crate::tls::default_certified_key().unwrap();
        let second = crate::tls::default_certified_key().unwrap();
        let store = CertStore::build(
            vec![
                (vec!["*.example.com".to_string()], first),
                (vec!["*.api.example.com".to_string()], second),
            ],
            default,
        );

        assert!(store.lookup(Some("a.api.example.com")).is_some());
        assert!(store.lookup(Some("unknown.example.org")).is_some());
    }

    #[test]
    fn default_headers_are_rendered() {
        let peer: SocketAddr = "203.0.113.9:1234".parse().unwrap();
        let mut headers = HeaderMap::new();
        ProxyHeaders::defaults().apply(&mut headers, "example.com", "https", peer.ip());

        assert_eq!(headers["x-real-ip"], "203.0.113.9");
        assert_eq!(headers["x-forwarded-for"], "203.0.113.9");
        assert_eq!(headers["x-forwarded-proto"], "https");
        assert_eq!(headers["x-forwarded-host"], "example.com");
    }

    #[test]
    fn custom_headers_replace_defaults() {
        let peer: SocketAddr = "203.0.113.9:1234".parse().unwrap();
        let mut set = HashMap::new();
        set.insert("X-Node".to_string(), "node=$host".to_string());

        let headers_config = ProxyHeaders::from_config(set, vec!["X-Forwarded-For".to_string()]);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4"));

        headers_config.apply(&mut headers, "example.com", "http", peer.ip());

        assert_eq!(headers["x-node"], "node=example.com");
        assert!(headers.get("x-forwarded-for").is_none());
        assert!(headers.get("x-forwarded-proto").is_none());
    }

    #[test]
    fn resolves_client_ip_modes() {
        let peer: SocketAddr = "10.0.0.1:1234".parse().unwrap();
        let proxied: SocketAddr = "192.0.2.7:5555".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.5, 10.0.0.1"),
        );
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.9"));

        assert_eq!(
            ClientIpMode::Raw.resolve(peer, Some(proxied), &headers),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            ClientIpMode::ProxyProtocol.resolve(peer, Some(proxied), &headers),
            "192.0.2.7".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            ClientIpMode::XForwardedFor(Some(0)).resolve(peer, None, &headers),
            "203.0.113.5".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            ClientIpMode::XRealIP.resolve(peer, None, &headers),
            "198.51.100.9".parse::<IpAddr>().unwrap()
        );
    }
}
