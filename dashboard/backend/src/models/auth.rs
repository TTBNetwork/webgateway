use axum::{extract::FromRequestParts, http::request::Parts};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shared::objectid::ObjectId;
use sqlx::{FromRow, Row, postgres::PgRow};

use crate::{
    auth::get_user_info_from_verify_jwt,
    response::{APIResponse, AppError},
};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthPostBody {
    pub username: String,
    pub totp: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthToBindQRCodePostBody {
    pub totp: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthToRefreshBindQRCodePostBody {
    pub secret_id: ObjectId,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthToVerifyBindQRCodePostBody {
    pub secret_id: ObjectId,
    pub totp: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthResponse {
    pub token: String,
    pub exp_at: DateTime<Utc>,
}

/// 账号角色。
///
/// 修复（ISSUES.md P1-16）：此前系统**没有任何角色/权限模型**，
/// 任何一个已登录账号都等同管理员，可枚举全部账号、管理全部站点/证书，
/// 并读取全部 DNS 服务商凭据（可用于劫持任意域名）。
///
/// 权限由低到高：
///
/// | 角色 | 只读 | 创建/修改 | 删除 |
/// |------|------|-----------|------|
/// | `view`  | ✅ | ❌ | ❌ |
/// | `user`  | ✅ | ✅ | ❌ |
/// | `admin` | ✅ | ✅ | ✅ |
///
/// 另：`/auth/users`（账号枚举）与角色修改**仅限 admin**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    View,
    User,
    Admin,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::View => "view",
            Role::User => "user",
            Role::Admin => "admin",
        }
    }

    /// 是否可以执行只读操作。所有角色都可以。
    pub fn can_read(&self) -> bool {
        true
    }

    /// 是否可以创建 / 修改资源。
    pub fn can_write(&self) -> bool {
        matches!(self, Role::User | Role::Admin)
    }

    /// 是否可以删除资源，以及管理账号。
    pub fn can_delete(&self) -> bool {
        matches!(self, Role::Admin)
    }

    /// 解析数据库中的角色字符串；无法识别时回退到权限最小的 `view`。
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "admin" => Role::Admin,
            "user" => Role::User,
            _ => Role::View,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 数据库 `role` 列的读取：非法值一律降级为 `view`，绝不因为脏数据放权。
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Role {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> std::result::Result<Self, sqlx::error::BoxDynError> {
        let raw = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Role::parse(raw))
    }
}

impl sqlx::Type<sqlx::Postgres> for Role {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <&str as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <&str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthJWT {
    pub id: ObjectId,
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseAuthentication {
    pub id: ObjectId,
    pub username: String,
    pub totp_secret: String,
    pub jwt_secret: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_login: Option<DateTime<Utc>>,
    pub last_ip: Option<String>,
    pub addresses: Vec<String>,
    pub bound_totp: bool,
}

impl From<DatabaseAuthentication> for AuthInfo {
    fn from(auth: DatabaseAuthentication) -> Self {
        Self {
            id: auth.id,
            username: auth.username,
            role: auth.role,
            created_at: auth.created_at,
            updated_at: auth.updated_at,
            bound_totp: auth.bound_totp,
        }
    }
}

impl<'r> FromRow<'r, PgRow> for DatabaseAuthentication {
    fn from_row(row: &'r PgRow) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            totp_secret: row.try_get("totp_secret")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            last_login: row.try_get("last_login")?,
            last_ip: row.try_get("last_ip")?,
            addresses: row.try_get("addresses")?,
            jwt_secret: row.try_get("jwt_secret")?,
            role: row.try_get("role")?,
            bound_totp: row.try_get("bound_totp")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct AuthJWTInfo {
    pub user: DatabaseAuthentication,
    pub jwt: AuthJWT,
}

#[derive(Debug, Clone)]
pub struct AuthJWTInfoExtract(pub AuthJWTInfo);

impl<S> FromRequestParts<S> for AuthJWTInfoExtract
where
    S: Send + Sync,
{
    type Rejection = APIResponse<()>;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        // first get from extsions
        let info = parts.extensions.get::<AuthJWTInfo>();
        if let Some(info) = info {
            return Ok(Self(info.clone()));
        }
        let authorization = parts
            .headers
            .get("Authorization")
            .ok_or(AppError::Unauthorized)?;
        // remove "Bearer " from the header
        let token = authorization.to_str().unwrap().replace("Bearer ", "");
        let info = get_user_info_from_verify_jwt(&token)
            .await
            .ok()
            .ok_or(AppError::Unauthorized)?;
        parts.extensions.insert(info.clone());
        Ok(Self(info))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthInfo {
    pub id: ObjectId,
    pub username: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub bound_totp: bool,
}

impl<'r> FromRow<'r, PgRow> for AuthInfo {
    fn from_row(row: &'r PgRow) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            role: row.try_get("role")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            bound_totp: row.try_get("bound_totp")?,
        })
    }
}

/// 授权提取器：在认证的基础上提供角色判定。
///
/// 用法（handler 里显式检查，失败即 403）：
///
/// ```ignore
/// pub async fn create(auth: Authorizer, ...) -> APIResponse<...> {
///     auth.require_write()?;
///     ...
/// }
/// ```
///
/// 之所以不做成中间件：axum 的中间件拿不到 handler 的语义，而"创建 vs 删除"
/// 的权限差异需要按路由区分；显式检查更不容易漏，也更容易在评审时看清楚。
#[derive(Debug, Clone)]
pub struct Authorizer(pub AuthJWTInfo);

impl Authorizer {
    pub fn role(&self) -> Role {
        self.0.user.role
    }

    pub fn user_id(&self) -> ObjectId {
        self.0.user.id
    }

    pub fn username(&self) -> &str {
        &self.0.user.username
    }

    pub fn is_admin(&self) -> bool {
        self.role().can_delete()
    }

    /// 要求创建 / 修改权限（`user` 及以上）。
    pub fn require_write(&self) -> std::result::Result<(), AppError> {
        if self.role().can_write() {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    /// 要求删除 / 账号管理权限（仅 `admin`）。
    pub fn require_admin(&self) -> std::result::Result<(), AppError> {
        if self.role().can_delete() {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}

impl<S> FromRequestParts<S> for Authorizer
where
    S: Send + Sync,
{
    type Rejection = APIResponse<()>;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let info = AuthJWTInfoExtract::from_request_parts(parts, state).await?;
        Ok(Self(info.0))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthQueryInfo {
    pub user_id: ObjectId,
}

/// 修改账号角色的请求体（仅 admin）。见 ISSUES.md P1-16。
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthSetRoleBody {
    pub user_id: ObjectId,
    pub role: Role,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthBindQRCodeResponse {
    pub secret_id: ObjectId,
    pub qr_url: String,
}

#[derive(Debug, Clone)]
pub struct AuthTempSecret {
    pub id: ObjectId,
    pub secret: String,
}

#[derive(Debug, Clone)]
pub struct AuthVerifyTOTP {
    pub username: String,
    pub totp: String,
    pub verify_type: AuthVerifyTOTPType,
    pub addr: String,
}

impl AuthVerifyTOTP {
    pub fn new(
        username: impl Into<String>,
        totp: impl Into<String>,
        verify_type: AuthVerifyTOTPType,
        addr: impl Into<String>,
    ) -> Self {
        Self {
            username: username.into(),
            totp: totp.into(),
            verify_type,
            addr: addr.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthVerifyTOTPType {
    Login,
    WantBind,
}
