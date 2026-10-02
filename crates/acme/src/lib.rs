use crate::util::hash_domains;

mod util;

#[derive(Debug, Default)]
pub struct CertificateBuilder {
    domains: Vec<String>,
    private_key: Vec<u8>,
    public_key: Vec<u8>,
    fullchain_key: Vec<u8>,
}

impl CertificateBuilder {
    pub fn push_domain(mut self, domain: impl Into<String>) -> Self {
        self.domains.push(domain.into());
        self
    }

    pub fn private_key(mut self, key: impl Into<Vec<u8>>) -> Self {
        let key = key.into();
        println!("key: {:?}", hex::decode(&key));
        self.private_key = key;
        self
    }

    pub fn build(self) -> Certificate {
        let mut domains = self
            .domains
            .into_iter()
            .map(|it| it.into())
            .collect::<Vec<String>>();
        domains.sort();
        Certificate::new(domains)
    }
}

#[derive(Debug, Clone)]
pub struct Certificate {
    hash_id: String,
    domains: Vec<String>,
}

impl Certificate {
    fn new(domains: Vec<impl Into<String>>) -> Self {
        let mut inner_domains = domains
            .into_iter()
            .map(|it| it.into())
            .collect::<Vec<String>>();
        inner_domains.sort();
        Self {
            hash_id: hash_domains(&inner_domains),
            domains: inner_domains,
        }
    }
}
