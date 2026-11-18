//! Minimal HTTP surface for the master process.
//!
//! The management API arrives in later steps. For now the process exposes a
//! health endpoint so deployments can verify that the schema is ready.

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde_json::json;
use shared::database::{Database, schema_version};

pub fn router(database: Database) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(database)
}

async fn health(State(database): State<Database>) -> impl IntoResponse {
    match database.health_check().await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "database": "up",
                "schema_version": schema_version(),
            })),
        ),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "error",
                "database": "down",
                "message": error.to_string(),
            })),
        ),
    }
}
