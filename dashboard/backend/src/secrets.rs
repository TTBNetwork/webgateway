//! Encrypt plaintext secrets that were stored before encryption was enabled.
//!
//! Runs on every dashboard start when `SECRET_ENCRYPTION_KEY` is configured;
//! rows that already carry the `enc:v1:` prefix are skipped, so the operation
//! is idempotent. Gateways keep working with either form while the migration
//! is in progress.

use anyhow::Context;
use shared::{crypto, database::Database};
use sqlx::Row;
use tracing::{Level, event};

/// Encrypt every plaintext secret column. No-op unless a key is configured.
pub async fn encrypt_legacy_secrets(database: &Database) -> anyhow::Result<()> {
    if !crypto::enabled() {
        return Ok(());
    }
    let mut updated = 0usize;
    updated += reencrypt_users(database).await?;
    updated += reencrypt_client_secrets(database).await?;
    updated += reencrypt_certificate_keys(database).await?;
    updated += reencrypt_acme_accounts(database).await?;
    updated += reencrypt_dns_providers(database).await?;
    if updated > 0 {
        event!(
            Level::INFO,
            count = updated,
            "encrypted plaintext secrets in the database"
        );
    }
    Ok(())
}

async fn reencrypt_users(database: &Database) -> anyhow::Result<usize> {
    let rows = sqlx::query(
        "SELECT id, totp_secret, jwt_secret FROM users
          WHERE totp_secret NOT LIKE $1 OR jwt_secret NOT LIKE $1",
    )
    .bind(encrypted_pattern())
    .fetch_all(database.pool())
    .await
    .context("failed to load users for secret encryption")?;

    let mut updated = 0;
    for row in rows {
        let id: String = row.try_get("id")?;
        let result = async {
            let totp: String = row.try_get("totp_secret")?;
            let jwt: String = row.try_get("jwt_secret")?;
            let totp = seal_if_plaintext(&totp)?;
            let jwt = seal_if_plaintext(&jwt)?;
            sqlx::query("UPDATE users SET totp_secret = $2, jwt_secret = $3 WHERE id = $1")
                .bind(&id)
                .bind(&totp)
                .bind(&jwt)
                .execute(database.pool())
                .await
                .context("failed to encrypt the user secrets")?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => updated += 1,
            Err(error) => event!(
                Level::ERROR,
                error = %format!("{error:#}"),
                user = %id,
                "failed to encrypt user secrets"
            ),
        }
    }
    Ok(updated)
}

async fn reencrypt_client_secrets(database: &Database) -> anyhow::Result<usize> {
    let rows =
        sqlx::query("SELECT user_id, secret FROM users_client_secrets WHERE secret NOT LIKE $1")
            .bind(encrypted_pattern())
            .fetch_all(database.pool())
            .await
            .context("failed to load client secrets for encryption")?;

    let mut updated = 0;
    for row in rows {
        let user_id: String = row.try_get("user_id")?;
        let result = async {
            let secret: String = row.try_get("secret")?;
            let sealed = crypto::seal(&secret)?;
            sqlx::query(
                "UPDATE users_client_secrets SET secret = $3 WHERE user_id = $1 AND secret = $2",
            )
            .bind(&user_id)
            .bind(&secret)
            .bind(&sealed)
            .execute(database.pool())
            .await
            .context("failed to encrypt a client secret")?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => updated += 1,
            Err(error) => event!(
                Level::ERROR,
                error = %format!("{error:#}"),
                user = %user_id,
                "failed to encrypt a client secret"
            ),
        }
    }
    Ok(updated)
}

async fn reencrypt_certificate_keys(database: &Database) -> anyhow::Result<usize> {
    let rows = sqlx::query(
        "SELECT id, private_key FROM certificates
          WHERE private_key IS NOT NULL AND private_key NOT LIKE $1",
    )
    .bind(encrypted_pattern())
    .fetch_all(database.pool())
    .await
    .context("failed to load certificate keys for encryption")?;

    let mut updated = 0;
    for row in rows {
        let id: String = row.try_get("id")?;
        let result = async {
            let private_key: String = row.try_get("private_key")?;
            let sealed = crypto::seal(&private_key)?;
            sqlx::query("UPDATE certificates SET private_key = $2 WHERE id = $1")
                .bind(&id)
                .bind(&sealed)
                .execute(database.pool())
                .await
                .context("failed to encrypt the certificate key")?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => updated += 1,
            Err(error) => event!(
                Level::ERROR,
                error = %format!("{error:#}"),
                certificate = %id,
                "failed to encrypt a certificate key"
            ),
        }
    }
    Ok(updated)
}

async fn reencrypt_acme_accounts(database: &Database) -> anyhow::Result<usize> {
    let rows = sqlx::query(
        "SELECT key, value ->> 'key_pem' AS key_pem FROM configurations
          WHERE key LIKE 'acme_account:%'
            AND value ->> 'key_pem' IS NOT NULL
            AND value ->> 'key_pem' NOT LIKE $1",
    )
    .bind(encrypted_pattern())
    .fetch_all(database.pool())
    .await
    .context("failed to load ACME account keys for encryption")?;

    let mut updated = 0;
    for row in rows {
        let key: String = row.try_get("key")?;
        let result = async {
            let pem: String = row.try_get("key_pem")?;
            let sealed = crypto::seal(&pem)?;
            sqlx::query(
                "UPDATE configurations SET value = jsonb_set(value, '{key_pem}', to_jsonb($2::text))
                  WHERE key = $1",
            )
            .bind(&key)
            .bind(&sealed)
            .execute(database.pool())
            .await
            .context("failed to encrypt the ACME account key")?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => updated += 1,
            Err(error) => event!(
                Level::ERROR,
                error = %format!("{error:#}"),
                key = %key,
                "failed to encrypt the ACME account key"
            ),
        }
    }
    Ok(updated)
}

async fn reencrypt_dns_providers(database: &Database) -> anyhow::Result<usize> {
    let rows = sqlx::query(
        "SELECT id, provider::text AS provider FROM dns_providers
          WHERE (provider ? 'secret_id' AND provider ->> 'secret_id' NOT LIKE $1)
             OR (provider ? 'secret_key' AND provider ->> 'secret_key' NOT LIKE $1)",
    )
    .bind(encrypted_pattern())
    .fetch_all(database.pool())
    .await
    .context("failed to load DNS providers for encryption")?;

    let mut updated = 0;
    for row in rows {
        let id: String = row.try_get("id")?;
        let result = async {
            let raw: String = row.try_get("provider")?;
            let mut provider: serde_json::Value =
                serde_json::from_str(&raw).context("invalid dns_providers.provider JSON")?;
            for field in ["secret_id", "secret_key"] {
                if let Some(value) = provider.get(field).and_then(|value| value.as_str())
                    && !crypto::is_encrypted(value)
                {
                    provider[field] = serde_json::Value::String(crypto::seal(value)?);
                }
            }
            sqlx::query("UPDATE dns_providers SET provider = $2::jsonb WHERE id = $1")
                .bind(&id)
                .bind(provider.to_string())
                .execute(database.pool())
                .await
                .context("failed to encrypt the DNS provider credentials")?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match result {
            Ok(()) => updated += 1,
            Err(error) => event!(
                Level::ERROR,
                error = %format!("{error:#}"),
                provider = %id,
                "failed to encrypt DNS provider credentials"
            ),
        }
    }
    Ok(updated)
}

fn seal_if_plaintext(value: &str) -> anyhow::Result<String> {
    if crypto::is_encrypted(value) {
        Ok(value.to_string())
    } else {
        crypto::seal(value)
    }
}

fn encrypted_pattern() -> String {
    format!("{}%", crypto::PREFIX)
}
