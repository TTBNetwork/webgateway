use std::sync::LazyLock;

use anyhow::Result;
use jsonwebtoken::{DecodingKey, EncodingKey, Validation, dangerous::insecure_decode, decode};
use shared::{database::get_database, objectid::ObjectId};

use crate::{
    config::get_config,
    database::auth::Authentication,
    models::auth::{AuthJWT, AuthJWTInfo, AuthResponse},
};

static EXPIRES: LazyLock<i64> = LazyLock::new(|| get_config().token_exp as i64);

pub async fn sign_jwt(username: &str) -> Result<AuthResponse> {
    let user = get_database().get_user(username).await?;
    let now = chrono::Utc::now().timestamp();
    // 修复（ISSUES.md P1-4）：`exp` 原本用秒、`exp_at` 却用分钟，
    // 于是令牌实际 7 天有效，却告诉客户端 420 天。这里统一为**秒**。
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(*EXPIRES);
    let payload = AuthJWT {
        id: user.id,
        iat: now,
        exp: expires_at.timestamp(),
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &payload,
        &EncodingKey::from_secret(user.jwt_secret.as_bytes()),
    )?;
    Ok(AuthResponse {
        token,
        exp_at: expires_at,
    })
}

fn get_user_id_from_unsafe_jwt(token: &str) -> Result<ObjectId> {
    let unsafe_data = insecure_decode::<AuthJWT>(token)?;
    let user_id = unsafe_data.claims.id;
    Ok(user_id)
}

pub async fn get_user_info_from_verify_jwt(token: &str) -> Result<AuthJWTInfo> {
    let user_id = get_user_id_from_unsafe_jwt(token)?;
    let user = get_database().get_user_from_id(&user_id).await?;
    let jwt = decode::<AuthJWT>(
        token,
        &DecodingKey::from_secret(user.jwt_secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(AuthJWTInfo {
        user,
        jwt: jwt.claims,
    })
}
