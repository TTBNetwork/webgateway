use axum::{
    Json, Router, middleware,
    routing::{get, post},
};
use shared::{
    database::{
        get_database,
        websites::{DatabaseWebsiteModifyRepository, DatabaseWebsiteRepository},
    },
    models::websites::{CreateDatabaseWebsite, DatabaseWebsite},
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

pub fn router() -> Router {
    Router::new()
        .route("/", get(get_all))
        .route("/create", post(create))
        .layer(middleware::from_fn(middle_refresh_token))
}
