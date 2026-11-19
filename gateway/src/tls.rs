//! TLS certificate loading and SNI resolution.

use std::{
    fmt,
    io::Cursor,
    sync::{Arc, RwLock},
};

use anyhow::Context;
use rcgen::generate_simple_self_signed;
use rustls::{
    ServerConfig,
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use tokio_rustls::TlsAcceptor;

use crate::state::CertStore;

struct Resolver {
    certs: Arc<RwLock<Arc<CertStore>>>,
}

impl fmt::Debug for Resolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Resolver").finish_non_exhaustive()
    }
}

impl ResolvesServerCert for Resolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let store = self.certs.read().ok()?;
        store.lookup(hello.server_name())
    }
}

pub fn acceptor(certs: Arc<RwLock<Arc<CertStore>>>) -> anyhow::Result<TlsAcceptor> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("failed to select TLS protocol versions")?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Resolver { certs }));

    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Generate the self-signed fallback certificate used when SNI has no match.
pub fn default_certified_key() -> anyhow::Result<Arc<CertifiedKey>> {
    let generated = generate_simple_self_signed(vec!["localhost".to_string()])
        .context("failed to generate the default self-signed certificate")?;

    let cert = generated.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        generated.signing_key.serialize_der(),
    ));

    certified_key(vec![cert], key)
}

pub fn certified_key_from_pem(
    fullchain: &str,
    private_key: &str,
) -> anyhow::Result<Arc<CertifiedKey>> {
    let certs = rustls_pemfile::certs(&mut Cursor::new(fullchain.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse the certificate chain")?;
    if certs.is_empty() {
        anyhow::bail!("certificate chain is empty");
    }

    let key = rustls_pemfile::private_key(&mut Cursor::new(private_key.as_bytes()))
        .context("failed to parse the private key")?
        .context("no private key found")?;

    certified_key(certs, key)
}

fn certified_key(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> anyhow::Result<Arc<CertifiedKey>> {
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
        .context("unsupported private key type")?;
    Ok(Arc::new(CertifiedKey::new(certs, signing_key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pem_certificate_and_key() {
        let generated = generate_simple_self_signed(vec!["example.com".to_string()]).unwrap();
        let cert_pem = generated.cert.pem();
        let key_pem = generated.signing_key.serialize_pem();

        let key = certified_key_from_pem(&cert_pem, &key_pem).unwrap();
        assert_eq!(key.cert.len(), 1);
    }
}
