/// DNS-01 challenge implementation
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::ChallengeSolver;
use crate::error::Result;
use crate::order::Challenge;
use crate::types::{ChallengeType, Identifier};

/// DNS provider trait for managing DNS records
#[async_trait]
pub trait DnsProvider: Send + Sync {
    /// Create a TXT record for DNS-01 challenge
    async fn create_txt_record(&self, domain: &str, value: &str) -> Result<String>;

    /// Delete a TXT record
    async fn delete_txt_record(&self, domain: &str, record_id: &str) -> Result<()>;

    /// Verify that the DNS record is propagated
    async fn verify_record(&self, domain: &str, value: &str) -> Result<bool>;
}

/// Mock DNS provider for testing
pub struct MockDnsProvider {
    records: Arc<RwLock<std::collections::HashMap<String, String>>>,
    counter: Arc<RwLock<u64>>,
}

impl MockDnsProvider {
    /// Create a new mock DNS provider
    pub fn new() -> Self {
        Self {
            records: Arc::new(RwLock::new(std::collections::HashMap::new())),
            counter: Arc::new(RwLock::new(0)),
        }
    }
}

impl Default for MockDnsProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DnsProvider for MockDnsProvider {
    async fn create_txt_record(&self, domain: &str, value: &str) -> Result<String> {
        let mut records = self.records.write().await;
        let mut counter = self.counter.write().await;
        *counter += 1;
        let record_id = format!("mock-record-{}", counter);
        records.insert(format!("{}/{}", domain, record_id), value.to_string());
        tracing::debug!("Mock DNS record created: {} = {}", domain, value);
        Ok(record_id)
    }

    async fn delete_txt_record(&self, domain: &str, record_id: &str) -> Result<()> {
        let mut records = self.records.write().await;
        records.remove(&format!("{}/{}", domain, record_id));
        tracing::debug!("Mock DNS record deleted: {}/{}", domain, record_id);
        Ok(())
    }

    async fn verify_record(&self, domain: &str, value: &str) -> Result<bool> {
        let records = self.records.read().await;
        for (key, stored_value) in records.iter() {
            if key.starts_with(domain) && stored_value == value {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// DNS-01 challenge solver
pub struct Dns01Solver {
    /// DNS provider implementation
    provider: Arc<dyn DnsProvider>,
    /// Domain name
    domain: String,
    /// TXT record name and ID created for this solver (one pair per authorization)
    record_id: Arc<RwLock<Vec<(String, String)>>>,
}

impl Dns01Solver {
    /// Create a new DNS-01 solver with custom provider
    pub fn new(provider: Arc<dyn DnsProvider>, domain: String) -> Self {
        Self {
            provider,
            domain,
            record_id: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Create with mock provider (for testing)
    pub fn with_mock(domain: String) -> Self {
        Self::new(Arc::new(MockDnsProvider::new()), domain)
    }
}

#[async_trait]
impl ChallengeSolver for Dns01Solver {
    fn challenge_type(&self) -> ChallengeType {
        ChallengeType::Dns01
    }

    async fn prepare(
        &mut self,
        challenge: &Challenge,
        identifier: &Identifier,
        key_authorization: &str,
    ) -> Result<()> {
        // Compute DNS record value (base64url of SHA256 hash)
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(key_authorization.as_bytes());
        let digest = hasher.finalize();
        let record_value = URL_SAFE_NO_PAD.encode(&digest[..]);

        // The TXT record name follows the identifier being authorized, not the
        // parent zone: for cert-test2.atxa.top it is
        // _acme-challenge.cert-test2.atxa.top. Wildcard identifiers arrive
        // without the leading "*.".
        let host = identifier
            .value
            .trim_start_matches('*')
            .trim_start_matches('.');
        let host = if host.is_empty() {
            self.domain.as_str()
        } else {
            host
        };
        let domain = format!("_acme-challenge.{host}");
        let id = self
            .provider
            .create_txt_record(&domain, &record_value)
            .await?;

        // Store the record name and ID for cleanup. A multi-hostname order
        // produces one TXT record per authorization, so keep them all.
        let mut record_id = self.record_id.write().await;
        record_id.push((domain.clone(), id));

        tracing::info!(
            "DNS-01 challenge prepared for domain: {} (token: {})",
            domain,
            challenge.token
        );

        // Give the authoritative DNS servers a moment to pick up the new TXT
        // record before the CA starts validating. Deployments that can verify
        // propagation through another channel can set this to 0.
        let propagation_secs = std::env::var("ACME_DNS_PROPAGATION_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(15);
        if propagation_secs > 0 {
            tracing::info!(
                "Waiting {}s for DNS-01 record propagation: {}",
                propagation_secs,
                domain
            );
            tokio::time::sleep(std::time::Duration::from_secs(propagation_secs)).await;
        }

        Ok(())
    }

    async fn present(&self) -> Result<()> {
        // For DNS-01, we just need to have the record created
        tracing::debug!("DNS-01 challenge presented");
        Ok(())
    }

    async fn verify(&self) -> Result<bool> {
        // Check if the record exists (in a real scenario, we'd query DNS)
        let record_id = self.record_id.read().await;
        Ok(!record_id.is_empty())
    }

    async fn cleanup(&mut self) -> Result<()> {
        let mut record_id_guard = self.record_id.write().await;
        let records = std::mem::take(&mut *record_id_guard);
        for (domain, id) in records {
            self.provider.delete_txt_record(&domain, &id).await?;
            tracing::info!("DNS-01 record cleaned up: {}", domain);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dns01_solver_creation() {
        let solver = Dns01Solver::with_mock("example.com".to_string());
        assert_eq!(solver.challenge_type(), ChallengeType::Dns01);
    }

    #[tokio::test]
    async fn test_mock_dns_provider() {
        let provider = MockDnsProvider::new();
        let record_id = provider
            .create_txt_record("example.com", "test-value")
            .await
            .unwrap();

        let verified = provider
            .verify_record("example.com", "test-value")
            .await
            .unwrap();
        assert!(verified);

        provider
            .delete_txt_record("example.com", &record_id)
            .await
            .unwrap();

        let verified = provider
            .verify_record("example.com", "test-value")
            .await
            .unwrap();
        assert!(!verified);
    }

    #[tokio::test]
    async fn test_dns01_solver_prepare() {
        let mut solver = Dns01Solver::with_mock("example.com".to_string());
        let challenge = Challenge {
            challenge_type: "dns-01".to_string(),
            url: "https://example.com/challenge/123".to_string(),
            status: "pending".to_string(),
            token: "test-token".to_string(),
            key_authorization: None,
            validation: None,
            updated: None,
            error: None,
        };
        let identifier = Identifier::dns("example.com");

        let result = solver
            .prepare(&challenge, &identifier, "test-token.test-auth")
            .await;
        assert!(result.is_ok());
    }
}
