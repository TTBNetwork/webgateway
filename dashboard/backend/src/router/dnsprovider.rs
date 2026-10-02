use axum::{
    Json, Router,
    extract::Query,
    middleware,
    routing::{get, post},
};
use shared::{
    database::{
        dnsprovider::{DatabaseDNSProviderModifyRepository, DatabaseDNSProviderRepository},
        get_database,
    },
    models::dnsprovider::{CreateDatabaseDNSProvider, DNSProviderQueryParams, DatabaseDNSProvider},
    secret::RemovedSensitiveInfo,
};

use crate::{auth::middle_refresh_token, models::auth::Authorizer, response::APIResponse};

pub async fn info() -> APIResponse<usize> {
    APIResponse::result(get_database().get_total_of_dns_providers().await)
}

pub async fn paged(
    Query(query): Query<DNSProviderQueryParams>,
) -> APIResponse<Vec<DatabaseDNSProvider>> {
    let res = get_database()
        .get_dns_providers_by_page(
            query.page.unwrap_or(0),
            std::cmp::min(query.limit.unwrap_or(20), 100),
        )
        .await
        .map(|v| {
            v.iter()
                .map(|v| v.remove_sensitive_info())
                .collect::<Vec<_>>()
        });
    APIResponse::result(res)
}

pub async fn create(
    auth: Authorizer,
    Json(dns_provider): Json<CreateDatabaseDNSProvider>,
) -> APIResponse<DatabaseDNSProvider> {
    if let Err(e) = auth.require_write() {
        return APIResponse::from(e);
    }
    if dns_provider.domains.is_empty() {
        return APIResponse::error(None, 422, "DNS provider must have at least one domain");
    }
    APIResponse::result(
        get_database()
            .create_dns_provider(&dns_provider)
            .await
            .map(|v| v.remove_sensitive_info()),
    )
}

pub fn router() -> Router {
    Router::new()
        .route("/total", get(info))
        .route("/page", get(paged))
        .route("/create", post(create))
        .layer(middleware::from_fn(middle_refresh_token))
}
