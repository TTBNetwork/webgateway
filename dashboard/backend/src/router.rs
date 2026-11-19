//! Dashboard API and health endpoint.

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use shared::database::{Database, schema_version};
use sqlx::Row;

#[derive(Clone)]
struct AppState {
    database: Database,
    api_token: Option<String>,
}

pub fn router(database: Database, api_token: Option<String>) -> Router {
    let state = AppState {
        database,
        api_token,
    };

    let api = Router::new()
        .route("/summary", get(summary))
        .route("/websites", get(websites))
        .route("/certificates", get(certificates))
        .route("/gateways", get(gateways))
        .route("/logs", get(logs))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/health", get(health))
        .nest("/api", api)
        .with_state(state)
}

async fn require_auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(expected) = state.api_token.as_deref() else {
        return next.run(request).await;
    };

    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == expected);

    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response()
    }
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.database.health_check().await {
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

async fn summary(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let counts = sqlx::query(
        "SELECT
            (SELECT count(*) FROM websites) AS websites,
            (SELECT count(*) FROM certificates) AS certificates,
            (SELECT count(*) FROM gateways_online) AS gateways_online,
            (SELECT count(*) FROM gateways) AS gateways_total",
    )
    .fetch_one(state.database.pool())
    .await
    .map_err(internal)?;

    let today = sqlx::query(
        "WITH stats AS (
            SELECT total_requests, total_responses, e4xx_requests, e5xx_requests,
                   total_requests_size, total_response_size
              FROM access_stats_minute
             WHERE minute_at >= date_trunc('day', now())
            UNION ALL
            SELECT total_requests, total_responses, e4xx_requests, e5xx_requests,
                   total_requests_size, total_response_size
              FROM access_stats_second
             WHERE second_at >= date_trunc('day', now())
         )
         SELECT COALESCE(sum(total_requests), 0)::bigint AS requests,
                COALESCE(sum(total_responses), 0)::bigint AS responses,
                COALESCE(sum(e4xx_requests), 0)::bigint AS e4xx,
                COALESCE(sum(e5xx_requests), 0)::bigint AS e5xx,
                COALESCE(sum(total_requests_size::numeric), 0)::text AS request_size,
                COALESCE(sum(total_response_size::numeric), 0)::text AS response_size
           FROM stats",
    )
    .fetch_one(state.database.pool())
    .await
    .map_err(internal)?;

    let traffic = sqlx::query(
        "WITH points AS (
            SELECT date_trunc('hour', minute_at) AS bucket, total_requests AS requests
              FROM access_stats_minute
             WHERE minute_at >= now() - interval '24 hours'
            UNION ALL
            SELECT date_trunc('hour', second_at), total_requests
              FROM access_stats_second
             WHERE second_at >= now() - interval '1 hour'
         )
         SELECT to_char(bucket, 'YYYY-MM-DD\"T\"HH24:00:00\"Z\"') AS bucket,
                sum(requests)::bigint AS requests
           FROM points
          GROUP BY bucket
          ORDER BY bucket",
    )
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;

    let traffic: Vec<Value> = traffic
        .into_iter()
        .filter_map(|row| {
            let bucket: String = row.try_get("bucket").ok()?;
            let requests: i64 = row.try_get("requests").ok()?;
            Some(json!({ "bucket": bucket, "requests": requests }))
        })
        .collect();

    Ok(Json(json!({
        "websites": counts.try_get::<i64, _>("websites").map_err(internal)?,
        "certificates": counts.try_get::<i64, _>("certificates").map_err(internal)?,
        "gateways_online": counts.try_get::<i64, _>("gateways_online").map_err(internal)?,
        "gateways_total": counts.try_get::<i64, _>("gateways_total").map_err(internal)?,
        "requests_today": today.try_get::<i64, _>("requests").map_err(internal)?,
        "responses_today": today.try_get::<i64, _>("responses").map_err(internal)?,
        "e4xx_today": today.try_get::<i64, _>("e4xx").map_err(internal)?,
        "e5xx_today": today.try_get::<i64, _>("e5xx").map_err(internal)?,
        "requests_size_today": today.try_get::<String, _>("request_size").map_err(internal)?,
        "response_size_today": today.try_get::<String, _>("response_size").map_err(internal)?,
        "traffic": traffic,
    })))
}

async fn websites(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, name, hosts,
                ARRAY(SELECT p::int FROM unnest(ports) AS p)::int[] AS ports,
                backends::text AS backends,
                config::text AS config,
                to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at,
                to_char(updated_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at
           FROM websites
          ORDER BY updated_at DESC",
    )
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            Ok(json!({
                "id": row.try_get::<String, _>("id").map_err(internal)?,
                "name": row.try_get::<Option<String>, _>("name").map_err(internal)?,
                "hosts": row.try_get::<Vec<String>, _>("hosts").map_err(internal)?,
                "ports": row.try_get::<Vec<i32>, _>("ports").map_err(internal)?,
                "backends": parse_json(&row.try_get::<String, _>("backends").map_err(internal)?),
                "config": parse_json(&row.try_get::<String, _>("config").map_err(internal)?),
                "created_at": row.try_get::<String, _>("created_at").map_err(internal)?,
                "updated_at": row.try_get::<String, _>("updated_at").map_err(internal)?,
            }))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(Json(json!({ "items": items })))
}

async fn certificates(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, name, hostnames, email, dns_provider_id,
                to_char(expires_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at,
                to_char(created_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at
           FROM certificates
          ORDER BY expires_at ASC NULLS LAST",
    )
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            Ok(json!({
                "id": row.try_get::<String, _>("id").map_err(internal)?,
                "name": row.try_get::<Option<String>, _>("name").map_err(internal)?,
                "hostnames": row.try_get::<Vec<String>, _>("hostnames").map_err(internal)?,
                "email": row.try_get::<Option<String>, _>("email").map_err(internal)?,
                "dns_provider_id": row.try_get::<Option<String>, _>("dns_provider_id").map_err(internal)?,
                "expires_at": row.try_get::<Option<String>, _>("expires_at").map_err(internal)?,
                "created_at": row.try_get::<String, _>("created_at").map_err(internal)?,
            }))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(Json(json!({ "items": items })))
}

async fn gateways(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, name, country, city, version, status,
                to_char(last_seen_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS last_seen_at,
                to_char(started_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS started_at,
                (status = 'online' AND last_seen_at >= now() - interval '30 seconds') AS online
           FROM gateways
          ORDER BY last_seen_at DESC NULLS LAST",
    )
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            Ok(json!({
                "id": row.try_get::<String, _>("id").map_err(internal)?,
                "name": row.try_get::<Option<String>, _>("name").map_err(internal)?,
                "country": row.try_get::<Option<String>, _>("country").map_err(internal)?,
                "city": row.try_get::<Option<String>, _>("city").map_err(internal)?,
                "version": row.try_get::<Option<String>, _>("version").map_err(internal)?,
                "status": row.try_get::<String, _>("status").map_err(internal)?,
                "last_seen_at": row.try_get::<Option<String>, _>("last_seen_at").map_err(internal)?,
                "started_at": row.try_get::<Option<String>, _>("started_at").map_err(internal)?,
                "online": row.try_get::<bool, _>("online").map_err(internal)?,
            }))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct LogQuery {
    limit: Option<i64>,
}

async fn logs(
    State(state): State<AppState>,
    Query(query): Query<LogQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);

    let rows = sqlx::query(
        "SELECT r.id, r.host, r.method, r.path, r.remote_addr, r.http_version,
                r.body_length::text AS request_size,
                to_char(r.requested_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS requested_at,
                s.status::int AS status,
                s.body_length::text AS response_size
           FROM access_request_logs r
           LEFT JOIN access_response_logs s ON s.id = r.id
          ORDER BY r.requested_at DESC
          LIMIT $1",
    )
    .bind(limit)
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            Ok(json!({
                "id": row.try_get::<String, _>("id").map_err(internal)?,
                "host": row.try_get::<String, _>("host").map_err(internal)?,
                "method": row.try_get::<String, _>("method").map_err(internal)?,
                "path": row.try_get::<String, _>("path").map_err(internal)?,
                "remote_addr": row.try_get::<String, _>("remote_addr").map_err(internal)?,
                "http_version": row.try_get::<String, _>("http_version").map_err(internal)?,
                "request_size": row.try_get::<String, _>("request_size").map_err(internal)?,
                "response_size": row.try_get::<Option<String>, _>("response_size").map_err(internal)?,
                "requested_at": row.try_get::<String, _>("requested_at").map_err(internal)?,
                "status": row.try_get::<Option<i32>, _>("status").map_err(internal)?,
            }))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(Json(json!({ "items": items })))
}

fn parse_json(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or(Value::Null)
}

type ApiError = (StatusCode, Json<Value>);

fn internal(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}
