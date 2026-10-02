use axum::{
    Json, Router, middleware,
    extract::Path,
    routing::{get, post},
};
use shared::{
    database::{
        get_database,
        websites::{DatabaseWebsiteModifyRepository, DatabaseWebsiteRepository},
    },
    models::websites::{CreateDatabaseWebsite, DatabaseWebsite},
    objectid::ObjectId,
};

use crate::{auth::middle_refresh_token, models::auth::Authorizer, response::APIResponse};

pub async fn get_all() -> APIResponse<Vec<DatabaseWebsite>> {
    APIResponse::result(get_database().get_websites().await)
}

pub async fn create(
    auth: Authorizer,
    Json(data): Json<CreateDatabaseWebsite>,
) -> APIResponse<DatabaseWebsite> {
    if let Err(e) = auth.require_write() {
        return APIResponse::from(e);
    }
    APIResponse::result(get_database().create_website(&data).await)
}

/// 读取单个站点（编辑对话框回填用）。
pub async fn get_one(Path(id): Path<String>) -> APIResponse<DatabaseWebsite> {
    let id = match id.parse::<ObjectId>() {
        Ok(id) => id,
        Err(_) => return APIResponse::error(None, 400, "invalid website id"),
    };
    APIResponse::result(get_database().get_website(&id).await)
}

/// 整条覆盖站点配置。需要 `user` 及以上角色。
pub async fn update(
    auth: Authorizer,
    Path(id): Path<String>,
    Json(data): Json<CreateDatabaseWebsite>,
) -> APIResponse<DatabaseWebsite> {
    if let Err(e) = auth.require_write() {
        return APIResponse::from(e);
    }
    let id = match id.parse::<ObjectId>() {
        Ok(id) => id,
        Err(_) => return APIResponse::error(None, 400, "invalid website id"),
    };
    match get_database().update_website(&id, &data).await {
        // 影响 0 行说明 id 不存在：必须回 404，不能假装成功 —— 否则前端会刷新出一个
        // "看起来改了但没生效"的列表，很难排查。
        Ok(0) => APIResponse::error(None, 404, "website not found"),
        Ok(_) => APIResponse::result(get_database().get_website(&id).await),
        Err(e) => APIResponse::error(None, 500, e.to_string()),
    }
}

/// 删除站点。**需要 admin**：删除会让该站点的所有域名立即停止服务，属破坏性操作。
pub async fn remove(
    auth: Authorizer,
    Path(id): Path<String>,
) -> APIResponse<bool> {
    if let Err(e) = auth.require_admin() {
        return APIResponse::from(e);
    }
    let id = match id.parse::<ObjectId>() {
        Ok(id) => id,
        Err(_) => return APIResponse::error(None, 400, "invalid website id"),
    };
    match get_database().delete_website(&id).await {
        Ok(0) => APIResponse::error(None, 404, "website not found"),
        Ok(_) => APIResponse::ok(true),
        Err(e) => APIResponse::error(None, 500, e.to_string()),
    }
}

pub fn router() -> Router {
    Router::new()
        .route("/", get(get_all))
        .route("/create", post(create))
        .route("/{id}", get(get_one))
        .route("/{id}/update", post(update))
        .route("/{id}/delete", post(remove))
        .layer(middleware::from_fn(middle_refresh_token))
}
