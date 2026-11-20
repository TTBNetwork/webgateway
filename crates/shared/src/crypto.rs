//! Symmetric encryption for secrets stored in PostgreSQL.
//!
//! Secrets are stored as `enc:v1:<base64url(nonce || AES-256-GCM ciphertext)>`.
//! The key is read from `SECRET_ENCRYPTION_KEY` (64 hex characters or 32 bytes
//! in base64); `SECRET_ENCRYPTION_KEY_PREVIOUS` is accepted for decryption
//! while a key is rotated. Without a key, values stay in plaintext (legacy
//! behaviour) and encrypted values fail to open, so production must configure
//! the key on the dashboard and on every gateway.

use std::{
    env,
    sync::{Arc, OnceLock},
};

use anyhow::{Context, anyhow, bail};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use tracing::{Level, event};

/// Prefix that marks an encrypted value in the database.
pub const PREFIX: &str = "enc:v1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// AEAD wrapper holding the current key and an optional key being rotated out.
#[derive(Clone)]
pub struct SecretBox {
    current: Arc<LessSafeKey>,
    previous: Option<Arc<LessSafeKey>>,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretBox")
            .field("rotation", &self.previous.is_some())
            .finish()
    }
}

impl SecretBox {
    /// Build the box from `SECRET_ENCRYPTION_KEY` and the optional previous key.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let current = env::var("SECRET_ENCRYPTION_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let previous = env::var("SECRET_ENCRYPTION_KEY_PREVIOUS")
            .ok()
            .filter(|value| !value.trim().is_empty());
        match (current, previous) {
            (None, None) => Ok(None),
            (None, Some(_)) => {
                bail!("SECRET_ENCRYPTION_KEY_PREVIOUS is set but SECRET_ENCRYPTION_KEY is missing")
            }
            (Some(current), previous) => Ok(Some(Self::from_key_material(
                &current,
                previous.as_deref(),
            )?)),
        }
    }

    /// Build a box from raw key material (hex or base64). Used by tests and
    /// by the environment loader.
    pub fn from_key_material(current: &str, previous: Option<&str>) -> anyhow::Result<Self> {
        Ok(Self {
            current: Arc::new(parse_key(current, "SECRET_ENCRYPTION_KEY")?),
            previous: previous
                .map(|material| parse_key(material, "SECRET_ENCRYPTION_KEY_PREVIOUS"))
                .transpose()?
                .map(Arc::new),
        })
    }

    /// True when the value carries the encryption prefix.
    pub fn is_encrypted(value: &str) -> bool {
        value.starts_with(PREFIX)
    }

    /// Encrypt one plaintext value with a fresh nonce.
    pub fn seal(&self, plaintext: &str) -> anyhow::Result<String> {
        if Self::is_encrypted(plaintext) {
            bail!("refusing to encrypt an already encrypted value");
        }
        let rng = SystemRandom::new();
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill(&mut nonce)
            .map_err(|_| anyhow!("failed to generate an encryption nonce"))?;
        let mut payload = plaintext.as_bytes().to_vec();
        self.current
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut payload,
            )
            .map_err(|_| anyhow!("failed to encrypt a secret"))?;
        let mut encoded = Vec::with_capacity(NONCE_LEN + payload.len());
        encoded.extend_from_slice(&nonce);
        encoded.extend_from_slice(&payload);
        Ok(format!(
            "{PREFIX}{}",
            data_encoding::BASE64URL_NOPAD.encode(&encoded)
        ))
    }

    /// Decrypt a value; plaintext values are returned unchanged so databases
    /// can be migrated incrementally.
    pub fn open(&self, stored: &str) -> anyhow::Result<String> {
        if !Self::is_encrypted(stored) {
            return Ok(stored.to_string());
        }
        let encoded = stored.strip_prefix(PREFIX).unwrap_or_default();
        let payload = data_encoding::BASE64URL_NOPAD
            .decode(encoded.as_bytes())
            .context("encrypted secret is not valid base64url")?;
        if payload.len() <= NONCE_LEN {
            bail!("encrypted secret is truncated");
        }
        let nonce_bytes: [u8; NONCE_LEN] = payload[..NONCE_LEN]
            .try_into()
            .expect("nonce length was checked");

        let attempt = |key: &LessSafeKey| -> Option<String> {
            let mut buffer = payload[NONCE_LEN..].to_vec();
            let plaintext = key
                .open_in_place(
                    Nonce::assume_unique_for_key(nonce_bytes),
                    Aad::empty(),
                    &mut buffer,
                )
                .ok()?;
            String::from_utf8(plaintext.to_vec()).ok()
        };

        if let Some(plaintext) = attempt(&self.current) {
            return Ok(plaintext);
        }
        if let Some(plaintext) = self.previous.as_deref().and_then(attempt) {
            return Ok(plaintext);
        }
        bail!("failed to decrypt a secret: the configured key does not match")
    }
}

fn parse_key(material: &str, name: &str) -> anyhow::Result<LessSafeKey> {
    let material = material.trim();
    let bytes = if material.len() == KEY_LEN * 2
        && material
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        data_encoding::HEXLOWER_PERMISSIVE
            .decode(material.as_bytes())
            .with_context(|| format!("{name} is not valid hex"))?
    } else {
        data_encoding::BASE64URL_NOPAD
            .decode(material.as_bytes())
            .or_else(|_| data_encoding::BASE64.decode(material.as_bytes()))
            .with_context(|| format!("{name} must be 32 bytes encoded as hex or base64"))?
    };
    if bytes.len() != KEY_LEN {
        bail!("{name} must decode to {KEY_LEN} bytes, got {}", bytes.len());
    }
    let key = UnboundKey::new(&AES_256_GCM, &bytes)
        .map_err(|_| anyhow!("{name} was rejected by AES-256-GCM"))?;
    Ok(LessSafeKey::new(key))
}

static INSTANCE: OnceLock<Option<SecretBox>> = OnceLock::new();

/// Read the key configuration once per process. Returns whether encryption is
/// enabled. Safe to call more than once; later calls keep the first result.
pub fn init_from_env() -> anyhow::Result<bool> {
    let secret_box = SecretBox::from_env()?;
    let _ = INSTANCE.set(secret_box);
    let enabled = enabled();
    if enabled {
        event!(Level::INFO, "secret encryption enabled for database values");
    } else {
        event!(
            Level::WARN,
            "secret encryption is disabled; set SECRET_ENCRYPTION_KEY before production"
        );
    }
    Ok(enabled)
}

/// Cheap check without touching the global instance (used by migrations).
pub fn is_encrypted(value: &str) -> bool {
    value.starts_with(PREFIX)
}

/// Whether encryption was configured for this process.
pub fn enabled() -> bool {
    INSTANCE.get().is_some_and(Option::is_some)
}

fn instance() -> anyhow::Result<Option<&'static SecretBox>> {
    match INSTANCE.get() {
        Some(value) => Ok(value.as_ref()),
        None => bail!("secret encryption is not initialized; call shared::crypto::init_from_env()"),
    }
}

/// Encrypt a value when a key is configured; otherwise return it unchanged.
pub fn seal(value: &str) -> anyhow::Result<String> {
    match instance()? {
        Some(secret_box) => secret_box.seal(value),
        None => {
            if SecretBox::is_encrypted(value) {
                bail!("cannot store an already encrypted value without an encryption key");
            }
            Ok(value.to_string())
        }
    }
}

/// Decrypt a stored value; plaintext values are returned unchanged.
pub fn open(stored: &str) -> anyhow::Result<String> {
    match instance()? {
        Some(secret_box) => secret_box.open(stored),
        None => {
            if SecretBox::is_encrypted(stored) {
                bail!("a secret is encrypted but SECRET_ENCRYPTION_KEY is not configured");
            }
            Ok(stored.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> String {
        data_encoding::HEXLOWER.encode(&[byte; KEY_LEN])
    }

    #[test]
    fn round_trips_with_a_fresh_nonce() {
        let secret_box = SecretBox::from_key_material(&key(1), None).unwrap();
        let first = secret_box.seal("super secret").unwrap();
        let second = secret_box.seal("super secret").unwrap();
        assert!(first.starts_with(PREFIX));
        assert_ne!(first, second);
        assert_eq!(secret_box.open(&first).unwrap(), "super secret");
        assert_eq!(secret_box.open("plaintext").unwrap(), "plaintext");
    }

    #[test]
    fn rejects_wrong_and_short_keys() {
        let seal = SecretBox::from_key_material(&key(1), None).unwrap();
        let other = SecretBox::from_key_material(&key(2), None).unwrap();
        let encrypted = seal.seal("value").unwrap();
        assert!(other.open(&encrypted).is_err());
        assert!(SecretBox::from_key_material("abcd", None).is_err());
        assert!(seal.seal(&encrypted).is_err());
    }

    #[test]
    fn previous_key_decrypts_during_rotation() {
        let old = SecretBox::from_key_material(&key(1), None).unwrap();
        let encrypted = old.seal("value").unwrap();
        let rotated = SecretBox::from_key_material(&key(2), Some(&key(1))).unwrap();
        assert_eq!(rotated.open(&encrypted).unwrap(), "value");
        let fresh = rotated.seal("value").unwrap();
        assert_eq!(rotated.open(&fresh).unwrap(), "value");
        let plain = SecretBox::from_key_material(&key(2), None).unwrap();
        assert!(plain.open(&encrypted).is_err());
    }

    #[test]
    fn accepts_base64_key_material() {
        let base64 = data_encoding::BASE64.encode(&[7u8; KEY_LEN]);
        assert!(SecretBox::from_key_material(&base64, None).is_ok());
    }
}
