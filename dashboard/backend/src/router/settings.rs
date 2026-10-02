use axum::{Json, Router, middleware, routing::{get, post}};
use serde::Deserialize;
use shared::{
    database::access::{get_retention_config, prune_access_logs_with, set_retention_config},
    models::retention::AccessLogRetention,
};

use crate::{auth::middle_refresh_token, models::auth::Authorizer, response::APIResponse};

/// 读取访问日志保留期配置。
pub async fn get_retention() -> APIResponse<AccessLogRetention> {
    APIResponse::result(get_retention_config().await)
}

#[derive(Debug, Deserialize)]
pub struct UpdateRetention {
    pub retention_days: u32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// 更新保留期配置。低于下限（90 天）会被夹紧，并在响应里返回实际生效值 ——
/// 前端据此回显，不会出现"我设了 30 天但实际是 90 天"的静默偏差。
pub async fn update_retention(
    auth: Authorizer,
    Json(data): Json<UpdateRetention>,
) -> APIResponse<AccessLogRetention> {
    if let Err(e) = auth.require_write() {
        return APIResponse::from(e);
    }
    APIResponse::result(set_retention_config(&AccessLogRetention {
        retention_days: data.retention_days,
        enabled: data.enabled,
    }).await)
}

/// 立即执行一轮清理（用于"改完保留期想马上释放空间"）。
///
/// 只推进有限批次：一次请求不会长时间占用连接。返回本次删除的行数，
/// 前端可提示"已删除 N 行，可再次点击继续"。
pub async fn prune_now(auth: Authorizer) -> APIResponse<u64> {
    if let Err(e) = auth.require_write() {
        return APIResponse::from(e);
    }
    // 单次点按最多推进 5 轮 × 5 万行 = 25 万行，避免把请求拖成分钟级。
    APIResponse::result(prune_access_logs_with(50_000, 5).await)
}

pub fn router() -> Router {
    Router::new()
        .route("/retention", get(get_retention))
        .route("/retention", post(update_retention))
        .route("/retention/prune", post(prune_now))
        .layer(middleware::from_fn(middle_refresh_token))
}
