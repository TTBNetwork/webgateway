use axum::Router;

pub mod access;
pub mod certificate;
pub mod dnsprovider;
pub mod log;
pub mod settings;
pub mod website;

pub fn get_router() -> Router {
    Router::new()
        .nest("/websites", website::router())
        .nest("/logs", log::router())
        .nest("/dnsproviders", dnsprovider::router())
        .nest("/certificates", certificate::router())
        .nest("/access", access::router())
        .nest("/settings", settings::router())
}
