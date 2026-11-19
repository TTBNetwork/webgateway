/// JWS (JSON Web Signature) signing for ACME
use crate::error::{AcmeError, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rcgen::{KeyPair, SigningKey};
use serde_json::Value;

/// JWS Signer for signing ACME requests
pub struct JwsSigner<'a> {
    key_pair: &'a KeyPair,
}

impl<'a> JwsSigner<'a> {
    /// Create a new JWS signer with a KeyPair reference
    pub fn new(key_pair: &'a KeyPair) -> Self {
        Self { key_pair }
    }

    /// Sign a JWS with the given header and payload
    pub fn sign(&self, header: &Value, payload: &Value) -> Result<String> {
        let header_json = header.to_string();
        let payload_json = if payload.is_null() {
            String::new()
        } else {
            payload.to_string()
        };

        let header_encoded = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
        let payload_encoded = if payload.is_null() {
            String::new()
        } else {
            URL_SAFE_NO_PAD.encode(payload_json.as_bytes())
        };

        let signing_input = format!("{}.{}", header_encoded, payload_encoded);

        // Sign using rcgen's KeyPair (requires SigningKey trait)
        let mut signature = self
            .key_pair
            .sign(signing_input.as_bytes())
            .map_err(|e| AcmeError::crypto(format!("JWS signing failed: {}", e)))?;

        // JWS ES256 signatures are raw fixed-size r || s values, while rcgen
        // returns an ASN.1 DER ECDSA-Sig-Value.
        if header.get("alg").and_then(Value::as_str) == Some("ES256") && signature.len() != 64 {
            signature = ecdsa_der_to_raw(&signature)
                .map(|raw| raw.to_vec())
                .ok_or_else(|| AcmeError::crypto("invalid ECDSA signature encoding"))?;
        }

        let signature_encoded = URL_SAFE_NO_PAD.encode(&signature);

        // ACME expects a JWS JSON object (RFC 7515 flattened serialization),
        // not the compact "header.payload.signature" string.
        Ok(serde_json::json!({
            "protected": header_encoded,
            "payload": payload_encoded,
            "signature": signature_encoded,
        })
        .to_string())
    }

    /// Sign empty payload (for some ACME operations)
    pub fn sign_empty(&self, header: &Value) -> Result<String> {
        self.sign(header, &Value::Null)
    }

    /// Get reference to the key pair
    pub fn key_pair(&self) -> &KeyPair {
        self.key_pair
    }
}

/// Convert an ASN.1 DER ECDSA signature into JWS raw r || s (64 bytes).
fn ecdsa_der_to_raw(der: &[u8]) -> Option<[u8; 64]> {
    let mut index = 0usize;
    if *der.get(index)? != 0x30 {
        return None;
    }
    index += 1;
    let sequence_length = der_read_length(der, &mut index)?;
    if index + sequence_length > der.len() {
        return None;
    }

    if *der.get(index)? != 0x02 {
        return None;
    }
    index += 1;
    let r_length = der_read_length(der, &mut index)?;
    let r = der.get(index..index + r_length)?;
    index += r_length;

    if *der.get(index)? != 0x02 {
        return None;
    }
    index += 1;
    let s_length = der_read_length(der, &mut index)?;
    let s = der.get(index..index + s_length)?;

    let mut raw = [0u8; 64];
    copy_right_aligned(&mut raw[..32], r)?;
    copy_right_aligned(&mut raw[32..], s)?;
    Some(raw)
}

fn der_read_length(bytes: &[u8], index: &mut usize) -> Option<usize> {
    let first = *bytes.get(*index)?;
    *index += 1;
    if first & 0x80 == 0 {
        return Some(first as usize);
    }
    let count = (first & 0x7f) as usize;
    if count == 0 || count > 2 {
        return None;
    }
    let mut length = 0usize;
    for _ in 0..count {
        length = (length << 8) | (*bytes.get(*index)? as usize);
        *index += 1;
    }
    Some(length)
}

fn copy_right_aligned(target: &mut [u8], value: &[u8]) -> Option<()> {
    let value = value.strip_prefix(&[0u8][..]).unwrap_or(value);
    if value.len() > target.len() {
        return None;
    }
    let offset = target.len() - value.len();
    target[offset..].copy_from_slice(value);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jws_sign() {
        let key_pair = KeyPair::generate().expect("Failed to generate key pair");
        let signer = JwsSigner::new(&key_pair);

        let header = serde_json::json!({
            "alg": "ES256",
            "nonce": "test-nonce",
            "url": "https://example.com/acme/new-account"
        });

        let payload = serde_json::json!({
            "termsOfServiceAgreed": true
        });

        let jws = signer.sign(&header, &payload).expect("Failed to sign JWS");
        let parts: Vec<&str> = jws.split('.').collect();
        assert_eq!(parts.len(), 1, "JWS JSON object is not dot separated");
        let value: Value = serde_json::from_str(&jws).expect("JWS should be JSON");
        for field in ["protected", "payload", "signature"] {
            let encoded = value[field].as_str().expect("field should be a string");
            assert!(URL_SAFE_NO_PAD.decode(encoded).is_ok());
        }
    }

    #[test]
    fn test_jws_sign_empty() {
        let key_pair = KeyPair::generate().expect("Failed to generate key pair");
        let signer = JwsSigner::new(&key_pair);

        let header = serde_json::json!({
            "alg": "ES256",
            "nonce": "test-nonce",
            "url": "https://example.com/acme/new-nonce"
        });

        let jws = signer
            .sign_empty(&header)
            .expect("Failed to sign empty JWS");
        let value: Value = serde_json::from_str(&jws).expect("JWS should be JSON");
        assert_eq!(value["payload"], "", "Payload part should be empty");
    }
}
