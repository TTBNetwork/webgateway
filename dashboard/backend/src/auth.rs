use std::time::SystemTime;

use axum::{
    Router,
    body::Body,
    extract::{Json, Query, Request},
    http::{HeaderValue, Response},
    middleware::Next,
    routing::{get, post},
};
use shared::database::get_database;

use crate::{
    auth::{
        bind::{generate_random_to_key, get_secret_from_key, refresh, remove_key},
        totp::get_totp_url,
    },
    database::{auth::Authentication, log::WebLogManager},
    foundation::RemoteAddr,
    models::{
        auth::{
            AuthBindQRCodeResponse, AuthInfo, AuthJWTInfoExtract, AuthPostBody, AuthQueryInfo,
            AuthResponse, AuthSetRoleBody, AuthToBindQRCodePostBody,
            AuthToRefreshBindQRCodePostBody, AuthToVerifyBindQRCodePostBody, AuthVerifyTOTP,
            Authorizer,
        },
        log::LogAddr,
    },
    response::APIResponse,
};

// use totp give a token
mod bind;
mod foundation;
mod jwt;
mod totp;

pub use foundation::{DEFAULT_ADMIN_USERNAME, generate_random_secret};
pub use jwt::{get_user_info_from_verify_jwt, sign_jwt};
pub use totp::get_totp_code;

#[axum::debug_handler]
pub async fn login(
    RemoteAddr(addr): RemoteAddr,
    Json(body): Json<AuthPostBody>,
) -> APIResponse<AuthResponse> {
    let now = SystemTime::now();
    let res = inner_login(body, &LogAddr::from(addr)).await;
    if res.status() == 200 {
        return res;
    }
    // sleep random, maybe 2 ~ 5 sec
    let duration = now.elapsed().unwrap().as_millis();
    let sleep_time = std::cmp::max(0, rand::random_range(2000..5000) - duration);
    tokio::time::sleep(std::time::Duration::from_millis(sleep_time as u64)).await;
    res
}

async fn inner_login(body: AuthPostBody, addr: &LogAddr) -> APIResponse<AuthResponse> {
    if body.username.trim().is_empty() || body.totp.trim().is_empty() {
        return APIResponse::error(None, 401, "Invalid username or totp code");
    }
    match get_database()
        .verify_totp(AuthVerifyTOTP {
            username: body.username.clone(),
            totp: body.totp,
            verify_type: crate::models::auth::AuthVerifyTOTPType::Login,
            addr: addr.0.clone(),
        })
        .await
    {
        Ok(true) => match sign_jwt(&body.username).await {
            Ok(res) => APIResponse::ok(res),
            Err(e) => APIResponse::error(None, 500, e.to_string()),
        },
        Ok(false) => APIResponse::error(None, 401, "Invalid TOTP code"),
        Err(e) => APIResponse::error(None, 500, e.to_string()),
    }
}

pub async fn info(AuthJWTInfoExtract(info): AuthJWTInfoExtract) -> APIResponse<AuthInfo> {
    APIResponse::ok(AuthInfo::from(info.user))
}

/// 查询用户信息。
///
/// 修复（ISSUES.md P1-15，IDOR）：原先只要持有合法令牌，就能通过任意
/// `?user_id=` 读取他人信息；中间件校验完身份后直接把它丢弃了。
/// 现在的规则是：
///   * 请求自己的信息 → 允许；
///   * 请求他人信息 → 仅 admin 允许（面板的日志页需要按 user_id 反查用户名）。
///
/// 注意 `AuthInfo` 本身不含 `jwt_secret` / `totp_secret`，泄露面是用户 ID 与用户名，
/// 但这仍属越权读取，必须修。
pub async fn get_userinfo(
    auth: Authorizer,
    Query(info): Query<AuthQueryInfo>,
) -> APIResponse<AuthInfo> {
    if auth.user_id() != info.user_id
        && let Err(e) = auth.require_admin()
    {
        return APIResponse::from(e);
    }
    let user = get_database().get_user_from_id(&info.user_id).await;
    match user {
        Ok(user) => APIResponse::ok(AuthInfo::from(user)),
        Err(_) => APIResponse::error(None, 404, "User not found"),
    }
}
pub async fn refresh_token(
    AuthJWTInfoExtract(info): AuthJWTInfoExtract,
) -> APIResponse<AuthResponse> {
    match sign_jwt(&info.user.username).await {
        Ok(res) => APIResponse::ok(res),
        Err(e) => APIResponse::error(None, 500, e.to_string()),
    }
}

// middle response
pub async fn middle_refresh_token(
    AuthJWTInfoExtract(info): AuthJWTInfoExtract,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let mut resp = next.run(req).await;
    // refresh token
    if resp.status() == 200 {
        let token = sign_jwt(&info.user.username).await.unwrap();
        resp.headers_mut().insert(
            "Refresh-Token",
            HeaderValue::from_str(&token.token).unwrap(),
        );
        resp.headers_mut().insert(
            "Refresh-Token-Expired",
            HeaderValue::from_str(&token.exp_at.to_rfc3339()).unwrap(),
        );
    }
    resp
}

pub async fn get_bind_qrcode(
    AuthJWTInfoExtract(info): AuthJWTInfoExtract,
    RemoteAddr(addr): RemoteAddr,
    Json(body): Json<AuthToBindQRCodePostBody>,
) -> APIResponse<AuthBindQRCodeResponse> {
    // verify totp
    if body.totp.trim().is_empty() {
        return APIResponse::error(None, 401, "Invalid TOTP code");
    }

    match get_database()
        .verify_totp(AuthVerifyTOTP {
            username: info.user.username.clone(),
            totp: body.totp,
            verify_type: crate::models::auth::AuthVerifyTOTPType::WantBind,
            addr: addr.to_string(),
        })
        .await
    {
        Ok(true) => {}
        Ok(false) => return APIResponse::error(None, 401, "Invalid TOTP code"),
        Err(e) => return APIResponse::error(None, 500, e.to_string()),
    }

    let secret = generate_random_to_key();
    let url = match get_totp_url(info.user.username, secret.secret) {
        Ok(res) => res,
        Err(e) => return APIResponse::error(None, 500, e.to_string()),
    };

    let _ = get_database()
        .add_web_log(
            &info.user.id,
            &crate::models::log::LogContent::Raw("auth.user.totp.bind.want.qr_code".to_string()),
            &LogAddr(addr.to_string()),
        )
        .await;

    APIResponse::ok(AuthBindQRCodeResponse {
        secret_id: secret.id,
        qr_url: url,
    })
}

pub async fn refresh_bind_qrcode(
    AuthJWTInfoExtract(info): AuthJWTInfoExtract,
    RemoteAddr(addr): RemoteAddr,
    Json(body): Json<AuthToRefreshBindQRCodePostBody>,
) -> APIResponse<AuthBindQRCodeResponse> {
    let id = body.secret_id;
    if get_secret_from_key(id).is_none() {
        return APIResponse::error(None, 403, "Totp is invalid");
    }

    let secret = match refresh(id) {
        Ok(secret) => secret,
        Err(e) => return APIResponse::error(None, 500, e.to_string()),
    };

    let url = match get_totp_url(info.user.username, secret.secret) {
        Ok(res) => res,
        Err(e) => return APIResponse::error(None, 500, e.to_string()),
    };

    let _ = get_database()
        .add_web_log(
            &info.user.id,
            &crate::models::log::LogContent::Raw("auth.user.totp.bind.want.qr_code".to_string()),
            &LogAddr(addr.to_string()),
        )
        .await;

    APIResponse::ok(AuthBindQRCodeResponse {
        secret_id: secret.id,
        qr_url: url,
    })
}

pub async fn verify_bind_qrcode(
    AuthJWTInfoExtract(info): AuthJWTInfoExtract,
    RemoteAddr(addr): RemoteAddr,
    Json(body): Json<AuthToVerifyBindQRCodePostBody>,
) -> APIResponse<()> {
    if body.totp.trim().is_empty() {
        return APIResponse::error(None, 401, "Invalid TOTP code");
    }
    let secret = match get_secret_from_key(body.secret_id) {
        Some(res) => res,
        None => return APIResponse::error(None, 404, "Secret not found"),
    };

    let totp_code = match get_totp_code(info.user.username, secret.as_str()) {
        Ok(res) => res,
        Err(e) => return APIResponse::error(None, 500, e.to_string()),
    };
    // println!("totp code: {}, {}", totp_code, body.totp);
    let result = totp_code.eq(&body.totp);
    let _ = get_database()
        .add_web_log(
            &info.user.id,
            &crate::models::log::LogContent::Raw(format!(
                "auth.user.totp.bind.verify.qr_code.{}",
                match result {
                    true => "success",
                    false => "fail",
                }
            )),
            &LogAddr(addr.to_string()),
        )
        .await;
    match result {
        true => {
            let r = get_database()
                .add_client_secret(&info.user.id, secret)
                .await;
            if r.is_ok() {
                remove_key(body.secret_id);
            }
            APIResponse::result(r)
        }
        false => APIResponse::error(None, 401, "Invalid TOTP code"),
    }
}

/// 列出全部账号。
///
/// 修复（ISSUES.md P1-16）：原先任何已登录账号都能枚举全部账号（含 `bound_totp`
/// 状态，可用于筛选未绑定 TOTP 的目标）。现在仅 admin 可用。
pub async fn all_users(auth: Authorizer) -> APIResponse<Vec<AuthInfo>> {
    if let Err(e) = auth.require_admin() {
        return APIResponse::from(e);
    }
    APIResponse::result(get_database().get_info_of_users().await)
}

/// 修改指定账号的角色。仅 admin 可用。
///
/// 安全护栏：不允许把自己降级（避免管理员误操作后无人能再管理面板）；
/// 也不允许通过该接口修改自己的角色，改角色请由另一名 admin 操作。
pub async fn set_user_role(
    auth: Authorizer,
    Json(body): Json<AuthSetRoleBody>,
) -> APIResponse<AuthInfo> {
    if let Err(e) = auth.require_admin() {
        return APIResponse::from(e);
    }
    if auth.user_id() == body.user_id {
        return APIResponse::error(None, 400, "Refusing to change your own role");
    }
    match get_database().set_user_role(&body.user_id, body.role).await {
        Ok(()) => match get_database().get_user_from_id(&body.user_id).await {
            Ok(user) => APIResponse::ok(AuthInfo::from(user)),
            Err(_) => APIResponse::error(None, 404, "User not found"),
        },
        Err(e) => APIResponse::error(None, 404, e.to_string()),
    }
}

pub fn get_router() -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/info", get(get_userinfo))
        .route("/totp/qrcode/get", post(get_bind_qrcode))
        .route("/totp/qrcode/verify", post(verify_bind_qrcode))
        .route("/totp/qrcode/refresh", post(refresh_bind_qrcode))
        .route("/refresh", get(refresh_token))
        .route("/users", get(all_users))
        .route("/users/role", post(set_user_role))
        .route("/", get(info))
}
