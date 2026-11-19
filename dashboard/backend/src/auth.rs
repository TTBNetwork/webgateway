//! Dashboard authentication: TOTP login and per-user JWT.

use std::{
    env,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use shared::database::Database;
use sqlx::Row;
use totp_rs::{Builder, Secret, Totp};
use tracing::{Level, event};

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    username: String,
    role: String,
    /// Bumped on logout/admin revoke/role change/TOTP reset. Tokens issued with
    /// an older value are rejected by authenticate().
    #[serde(default)]
    token_version: i64,
    iat: usize,
    exp: usize,
}

/// User row returned by the admin user-management API (never includes secrets).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UserSummary {
    pub id: String,
    pub username: String,
    pub role: String,
    pub token_version: i32,
    pub last_login: Option<String>,
    pub last_ip: Option<String>,
    pub created_at: String,
}

pub const USER_ROLES: &[&str] = &["admin", "user", "view"];

pub fn is_valid_role(role: &str) -> bool {
    USER_ROLES.contains(&role)
}

/// Service tokens act as an administrator.
pub fn is_admin(user: &User) -> bool {
    user.role == "admin" || user.role == "service"
}

pub fn can_write_certificates(user: &User) -> bool {
    is_admin(user) || user.role == "user"
}

pub struct LoginSuccess {
    pub token: String,
    pub user: User,
}

/// Create a default admin on an empty users table. The TOTP secret is printed
/// once; an operator must store it in an authenticator app.
pub async fn ensure_default_admin(database: &Database) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(database.pool())
        .await
        .context("failed to count users")?;
    if count > 0 {
        return Ok(());
    }

    let username = env::var("DASHBOARD_ADMIN_USERNAME").unwrap_or_else(|_| "admin".to_string());
    let totp_secret = match env::var("DASHBOARD_ADMIN_TOTP_SECRET") {
        Ok(secret) => {
            let secret = secret.trim().to_ascii_uppercase();
            if build_totp(&secret).is_none() {
                bail!("DASHBOARD_ADMIN_TOTP_SECRET is not valid base32");
            }
            secret
        }
        Err(_) => {
            let mut bytes = [0u8; 20];
            getrandom::fill(&mut bytes)
                .map_err(|error| anyhow::anyhow!("randomness failed: {error}"))?;
            Secret::from(bytes).to_base32()
        }
    };
    let jwt_secret = match env::var("DASHBOARD_JWT_SECRET") {
        Ok(secret) if !secret.trim().is_empty() => secret,
        _ => random_hex(32)?,
    };

    let id = random_hex(12)?;
    sqlx::query(
        "INSERT INTO users (id, username, totp_secret, jwt_secret, role, addresses)
         VALUES ($1, $2, $3, $4, 'admin', ARRAY[]::text[])",
    )
    .bind(&id)
    .bind(&username)
    .bind(&totp_secret)
    .bind(&jwt_secret)
    .execute(database.pool())
    .await
    .context("failed to create the default admin")?;

    event!(
        Level::WARN,
        username = %username,
        totp_secret = %totp_secret,
        "created default admin; store the TOTP secret and remove it from logs"
    );
    Ok(())
}

/// Verify a bearer JWT and return the current user.
pub async fn authenticate(database: &Database, token: &str) -> Option<User> {
    let claims = unverified_claims(token)?;
    let row = sqlx::query(
        "SELECT id, username, role, jwt_secret, token_version FROM users WHERE id = $1",
    )
    .bind(&claims.sub)
    .fetch_optional(database.pool())
    .await
    .ok()??;

    let jwt_secret: String = row.try_get("jwt_secret").ok()?;
    let validation = Validation::new(Algorithm::HS256);
    let key = DecodingKey::from_secret(jwt_secret.as_bytes()).ok()?;
    let decoded = decode::<Claims>(token, &key, &validation).ok()?;
    let token_version: i32 = row.try_get("token_version").ok()?;
    if decoded.claims.token_version != i64::from(token_version) {
        return None;
    }

    Some(User {
        id: row.try_get("id").ok()?,
        username: row.try_get("username").ok()?,
        role: row.try_get("role").ok()?,
    })
}

/// Validate username + TOTP. Returns None for both "unknown user" and "bad code"
/// so the API does not disclose which one failed.
pub async fn login(
    database: &Database,
    username: &str,
    code: &str,
    last_ip: Option<String>,
) -> anyhow::Result<Option<LoginSuccess>> {
    let row = sqlx::query(
        "SELECT id, username, role, totp_secret, jwt_secret, token_version
           FROM users
          WHERE lower(username) = lower($1)",
    )
    .bind(username)
    .fetch_optional(database.pool())
    .await
    .context("failed to load user")?;

    let Some(row) = row else {
        return Ok(None);
    };

    let id: String = row.try_get("id")?;
    let mut secrets: Vec<String> = vec![row.try_get("totp_secret")?];
    let extra: Vec<String> =
        sqlx::query_scalar("SELECT secret FROM users_client_secrets WHERE user_id = $1")
            .bind(&id)
            .fetch_all(database.pool())
            .await
            .unwrap_or_default();
    secrets.extend(extra);

    if !secrets.iter().any(|secret| verify_totp(secret, code)) {
        return Ok(None);
    }

    sqlx::query(
        "UPDATE users SET last_login = now(), last_ip = COALESCE($2, last_ip) WHERE id = $1",
    )
    .bind(&id)
    .bind(last_ip)
    .execute(database.pool())
    .await
    .context("failed to update last_login")?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs() as usize;
    let expires = env::var("DASHBOARD_TOKEN_EXPIRES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(7 * 24 * 60 * 60);

    let username: String = row.try_get("username")?;
    let role: String = row.try_get("role")?;
    let token_version: i32 = row.try_get("token_version")?;
    let claims = Claims {
        sub: id.clone(),
        username: username.clone(),
        role: role.clone(),
        token_version: i64::from(token_version),
        iat: now,
        exp: now.saturating_add(expires),
    };
    let jwt_secret: String = row.try_get("jwt_secret")?;
    let key = EncodingKey::from_secret(jwt_secret.as_bytes())
        .map_err(|error| anyhow::anyhow!("failed to create the JWT key: {error}"))?;
    let token = encode(&Header::default(), &claims, &key).context("failed to sign the JWT")?;

    event!(Level::INFO, user = %username, "dashboard login succeeded");
    Ok(Some(LoginSuccess {
        token,
        user: User { id, username, role },
    }))
}

fn unverified_claims(token: &str) -> Option<Claims> {
    let payload = token.split('.').nth(1)?;
    let bytes = data_encoding::BASE64URL_NOPAD
        .decode(payload.as_bytes())
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn verify_totp(secret: &str, code: &str) -> bool {
    build_totp(secret)
        .and_then(|totp| totp.check_current(code.trim()))
        .is_some()
}

fn build_totp(secret: &str) -> Option<Totp> {
    let secret = Secret::try_from_base32(secret).ok()?;
    Builder::new().with_secret(secret).with_skew(1).build().ok()
}

pub(crate) fn random_id() -> anyhow::Result<String> {
    random_hex(12)
}

fn random_hex(bytes: usize) -> anyhow::Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|error| anyhow::anyhow!("randomness failed: {error}"))?;
    Ok(data_encoding::HEXLOWER.encode(&buffer))
}

fn new_totp_secret() -> anyhow::Result<String> {
    let mut bytes = [0u8; 20];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("randomness failed: {error}"))?;
    Ok(Secret::from(bytes).to_base32())
}

pub async fn list_users(database: &Database) -> anyhow::Result<Vec<UserSummary>> {
    sqlx::query_as::<_, UserSummary>(
        "SELECT id, username, role, token_version,
                to_char(last_login, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS last_login,
                last_ip,
                to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at
           FROM users
          ORDER BY created_at",
    )
    .fetch_all(database.pool())
    .await
    .context("failed to list users")
}

pub async fn get_user(database: &Database, id: &str) -> anyhow::Result<Option<UserSummary>> {
    sqlx::query_as::<_, UserSummary>(
        "SELECT id, username, role, token_version,
                to_char(last_login, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS last_login,
                last_ip,
                to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at
           FROM users
          WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(database.pool())
    .await
    .context("failed to load the user")
}

/// Create a user and return the one-time TOTP secret. The secret is never
/// returned again; an admin must hand it to the user immediately.
pub async fn create_user(
    database: &Database,
    username: &str,
    role: &str,
) -> anyhow::Result<(UserSummary, String)> {
    let username = username.trim();
    if username.is_empty() {
        bail!("username is required");
    }
    if !is_valid_role(role) {
        bail!("invalid role: {role}");
    }

    let id = random_hex(12)?;
    let totp_secret = new_totp_secret()?;
    let jwt_secret = random_hex(32)?;
    match sqlx::query(
        "INSERT INTO users (id, username, totp_secret, jwt_secret, role, addresses)
         VALUES ($1, $2, $3, $4, $5, ARRAY[]::text[])",
    )
    .bind(&id)
    .bind(username)
    .bind(&totp_secret)
    .bind(&jwt_secret)
    .bind(role)
    .execute(database.pool())
    .await
    {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505") => {
            bail!("username already exists")
        }
        Err(error) => return Err(error).context("failed to create the user"),
    }

    let summary = get_user(database, &id)
        .await?
        .context("created user disappeared")?;
    Ok((summary, totp_secret))
}

/// Update the username and/or role. A role change bumps token_version so the
/// previous JWT (which carries the old role) stops working immediately.
pub async fn update_user(
    database: &Database,
    id: &str,
    username: Option<&str>,
    role: Option<&str>,
) -> anyhow::Result<Option<UserSummary>> {
    if let Some(username) = username {
        let username = username.trim();
        if username.is_empty() {
            bail!("username is required");
        }
        match sqlx::query("UPDATE users SET username = $2 WHERE id = $1")
            .bind(id)
            .bind(username)
            .execute(database.pool())
            .await
        {
            Ok(_) => {}
            Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505") => {
                bail!("username already exists")
            }
            Err(error) => return Err(error).context("failed to update the username"),
        }
    }

    if let Some(role) = role {
        if !is_valid_role(role) {
            bail!("invalid role: {role}");
        }
        sqlx::query(
            "UPDATE users
                SET role = $2, token_version = token_version + 1
              WHERE id = $1",
        )
        .bind(id)
        .bind(role)
        .execute(database.pool())
        .await
        .context("failed to update the role")?;
    }

    get_user(database, id).await
}

pub async fn delete_user(database: &Database, id: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(database.pool())
        .await
        .context("failed to delete the user")?;
    Ok(result.rows_affected() > 0)
}

/// Replace a user's TOTP secret and invalidate existing sessions.
pub async fn reset_totp(database: &Database, id: &str) -> anyhow::Result<Option<String>> {
    let secret = new_totp_secret()?;
    let result = sqlx::query(
        "UPDATE users
            SET totp_secret = $2, token_version = token_version + 1
          WHERE id = $1",
    )
    .bind(id)
    .bind(&secret)
    .execute(database.pool())
    .await
    .context("failed to reset the TOTP secret")?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(secret))
}

pub async fn revoke_tokens(database: &Database, id: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("UPDATE users SET token_version = token_version + 1 WHERE id = $1")
        .bind(id)
        .execute(database.pool())
        .await
        .context("failed to revoke the user tokens")?;
    Ok(result.rows_affected() > 0)
}

pub async fn count_admins(database: &Database) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM users WHERE role = 'admin'")
        .fetch_one(database.pool())
        .await
        .context("failed to count admins")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_round_trip() {
        let secret = Secret::from([7u8; 20]);
        let totp = Builder::new()
            .with_secret(secret)
            .with_skew(1)
            .build()
            .unwrap();
        let code = totp.generate(1_700_000_000).to_string();
        assert!(totp.check(&code, 1_700_000_000).is_some());
    }

    #[test]
    fn rejects_invalid_totp_secret() {
        assert!(build_totp("not-base32").is_none());
    }

    #[test]
    fn validates_roles_and_permissions() {
        for role in ["admin", "user", "view"] {
            assert!(is_valid_role(role));
        }
        assert!(!is_valid_role("root"));

        let admin = User {
            id: "1".into(),
            username: "admin".into(),
            role: "admin".into(),
        };
        let user = User {
            id: "2".into(),
            username: "user".into(),
            role: "user".into(),
        };
        let view = User {
            id: "3".into(),
            username: "view".into(),
            role: "view".into(),
        };
        let service = User {
            id: "service".into(),
            username: "service".into(),
            role: "service".into(),
        };

        assert!(is_admin(&admin));
        assert!(is_admin(&service));
        assert!(!is_admin(&user));
        assert!(can_write_certificates(&admin));
        assert!(can_write_certificates(&user));
        assert!(!can_write_certificates(&view));
    }
}
