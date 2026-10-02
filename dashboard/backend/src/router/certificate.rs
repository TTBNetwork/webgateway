use axum::{
    Json, Router,
    extract::Query,
    middleware,
    routing::{get, post},
};
use shared::{
    database::{
        certificate::{DatabaseCertificateModifiyRepository, DatabaseCertificateRepository},
        get_database,
    },
    models::certificate::{
        CertificateQueryParams, CreateCertificate, CreateCertificateMethod, DatabaseCertificate,
    },
    secret::RemovedSensitiveInfo,
};

use crate::{auth::middle_refresh_token, models::auth::Authorizer, response::APIResponse};

pub async fn info() -> APIResponse<usize> {
    APIResponse::result(get_database().get_total_of_certificates().await)
}

pub async fn paged(
    Query(query): Query<CertificateQueryParams>,
) -> APIResponse<Vec<DatabaseCertificate>> {
    let res = get_database()
        .get_certificates_by_page(
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
    Json(certificate): Json<CreateCertificate>,
) -> APIResponse<DatabaseCertificate> {
    if let Err(e) = auth.require_write() {
        return APIResponse::from(e);
    }
    match &certificate.content {
        CreateCertificateMethod::AUTO(context) => {
            if context.hostnames.is_empty() {
                return APIResponse::error(None, 422, "hostnames is empty");
            }
        }
        CreateCertificateMethod::MANUAL(context) => {
            // TODO: check if certificate is valid
            if let Err(e) = context.vaild() {
                return APIResponse::error(None, 422, e.to_string());
            }
        }
    };
    APIResponse::result(
        get_database()
            .create_certificate(&certificate)
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
