//! Routing tables, per-site proxy headers, certificate store and process state.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use http::{HeaderMap, HeaderName, HeaderValue};
use hyper::body::Incoming;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::sign::CertifiedKey;
use tokio_rustls::TlsAcceptor;
use url::Url;

use crate::node::NodeInfo;

pub type UpstreamClient = Client<HttpConnector, Incoming>;

pub struct AppState {
    pub routing: RwLock<Arc<Routing>>,
    pub certs: Arc<RwLock<Arc<CertStore>>>,
    pub tls_acceptor: TlsAcceptor,
    pub client: UpstreamClient,
    pub node: NodeInfo,
}

impl AppState {
    pub fn new(node: NodeInfo, certs: Arc<RwLock<Arc<CertStore>>>) -> anyhow::Result<Self> {
        let tls_acceptor = crate::tls::acceptor(certs.clone())?;
        let client =
            Client::builder(hyper_util::rt::TokioExecutor::new()).build(HttpConnector::new());

        Ok(Self {
            routing: RwLock::new(Arc::new(Routing::default())),
            certs,
            tls_acceptor,
            client,
            node,
        })
    }

    pub fn current_default_cert(&self) -> Arc<CertifiedKey> {
        self.certs
            .read()
            .expect("certificate store lock poisoned")
            .default_key()
    }

    pub fn apply_config(&self, routing: Arc<Routing>, certs: Arc<CertStore>) {
        *self.routing.write().expect("routing lock poisoned") = routing;
        *self.certs.write().expect("certificate store lock poisoned") = certs;
    }
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub url: Url,
    pub match_path: Option<String>,
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
    pub fn apply(&self, headers: &mut HeaderMap, host: &str, scheme: &str, peer: SocketAddr) {
        let remote_addr = peer.ip().to_string();

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
    selector: AtomicUsize,
}

impl Site {
    pub fn new(
        id: String,
        name: Option<String>,
        hosts: Vec<String>,
        ports: Vec<u16>,
        backends: Vec<Backend>,
        headers: ProxyHeaders,
    ) -> Self {
        Self {
            id,
            name,
            hosts,
            ports,
            backends,
            headers,
            selector: AtomicUsize::new(0),
        }
    }

    /// Pick the next backend in round-robin order, preferring path matches.
    pub fn next_backend(&self, path: &str) -> Option<&Backend> {
        let matched: Vec<&Backend> = self
            .backends
            .iter()
            .filter(|backend| match backend.match_path.as_deref() {
                Some(prefix) if !prefix.is_empty() => path.starts_with(prefix),
                _ => true,
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

        let index = self.selector.fetch_add(1, Ordering::Relaxed) % candidates.len();
        Some(candidates[index])
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

    fn backend(url: &str, match_path: Option<&str>) -> Backend {
        Backend {
            url: Url::parse(url).unwrap(),
            match_path: match_path.map(str::to_string),
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

        let api = site.next_backend("/api/v1").unwrap();
        assert_eq!(api.url.port(), Some(9001));
        let web = site.next_backend("/index.html").unwrap();
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
        ProxyHeaders::defaults().apply(&mut headers, "example.com", "https", peer);

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

        headers_config.apply(&mut headers, "example.com", "http", peer);

        assert_eq!(headers["x-node"], "node=example.com");
        assert!(headers.get("x-forwarded-for").is_none());
        assert!(headers.get("x-forwarded-proto").is_none());
    }
}
