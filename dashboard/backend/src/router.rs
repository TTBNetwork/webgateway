//! Dashboard API and health endpoint.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use shared::database::{Database, schema_version};
use sqlx::Row;
use tracing::{Level, event};

use crate::auth::{self, User};
use crate::certificate;

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

    let protected = Router::new()
        .route("/api/summary", get(summary))
        .route("/api/websites", get(websites))
        .route(
            "/api/certificates",
            get(certificates).post(create_certificate),
        )
        .route("/api/certificates/issue", post(issue_certificate))
        .route("/api/certificates/{id}", delete(delete_certificate))
        .route("/api/certificates/{id}/issue", post(reissue_certificate))
        .route(
            "/api/dns-providers",
            get(dns_providers).post(create_dns_provider),
        )
        .route("/api/dns-providers/{id}", delete(delete_dns_provider))
        .route("/api/gateways", get(gateways))
        .route("/api/logs", get(logs))
        .route("/api/auth/me", get(me))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/health", get(health))
        .route("/api/auth/login", post(login))
        .merge(protected)
        .with_state(state)
}

async fn require_auth(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string);

    let user = match token.as_deref() {
        Some(token) if state.api_token.as_deref() == Some(token) => Some(User {
            id: "service".to_string(),
            username: "service".to_string(),
            role: "service".to_string(),
        }),
        Some(token) => auth::authenticate(&state.database, token).await,
        None => None,
    };

    match user {
        Some(user) => {
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    code: String,
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<Value>, ApiError> {
    if payload.username.trim().is_empty() || payload.code.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "username and code are required" })),
        ));
    }

    let last_ip = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').next().unwrap_or(value).trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        });

    match auth::login(
        &state.database,
        payload.username.trim(),
        payload.code.trim(),
        last_ip,
    )
    .await
    .map_err(internal)?
    {
        Some(success) => Ok(Json(json!({
            "token": success.token,
            "user": success.user,
        }))),
        None => {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "invalid username or code" })),
            ))
        }
    }
}

async fn me(Extension(user): Extension<User>) -> Json<Value> {
    Json(json!({ "user": user }))
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
        "SELECT id, name, hostnames, email, dns_provider_id, auto_renew, dns_zone, last_error,
                CASE
                    WHEN fullchain IS NULL THEN 'pending'
                    WHEN expires_at IS NOT NULL AND expires_at < now() THEN 'expired'
                    WHEN expires_at IS NOT NULL AND expires_at < now() + interval '30 days' THEN 'expiring'
                    ELSE 'valid'
                END AS status,
                to_char(signing_started_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS signing_started_at,
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
                "auto_renew": row.try_get::<bool, _>("auto_renew").map_err(internal)?,
                "dns_zone": row.try_get::<Option<String>, _>("dns_zone").map_err(internal)?,
                "last_error": row.try_get::<Option<String>, _>("last_error").map_err(internal)?,
                "status": row.try_get::<String, _>("status").map_err(internal)?,
                "signing_started_at": row.try_get::<Option<String>, _>("signing_started_at").map_err(internal)?,
                "expires_at": row.try_get::<Option<String>, _>("expires_at").map_err(internal)?,
                "created_at": row.try_get::<String, _>("created_at").map_err(internal)?,
            }))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct UploadCertificateRequest {
    name: Option<String>,
    hostnames: Option<Vec<String>>,
    fullchain: String,
    private_key: String,
    email: Option<String>,
}

async fn create_certificate(
    State(state): State<AppState>,
    Json(payload): Json<UploadCertificateRequest>,
) -> Result<Json<Value>, ApiError> {
    let uploaded = certificate::upload_certificate(
        &state.database,
        payload.name,
        payload.hostnames,
        payload.fullchain,
        payload.private_key,
        payload.email,
    )
    .await
    .map_err(certificate_error)?;

    Ok(Json(json!({
        "id": uploaded.id,
        "hostnames": uploaded.hostnames,
        "expires_at": uploaded.expires_at,
        "status": "valid",
    })))
}

#[derive(Debug, Deserialize)]
struct IssueCertificateRequest {
    name: Option<String>,
    hostnames: Vec<String>,
    email: Option<String>,
    dns_provider_id: Option<String>,
    dns_zone: Option<String>,
}

async fn issue_certificate(
    State(state): State<AppState>,
    Json(payload): Json<IssueCertificateRequest>,
) -> Result<Json<Value>, ApiError> {
    let id = certificate::create_pending(
        &state.database,
        payload.name,
        payload.hostnames,
        payload.email,
        payload.dns_provider_id,
        payload.dns_zone,
    )
    .await
    .map_err(certificate_error)?;

    certificate::trigger(state.database.clone(), id.clone());
    Ok(Json(json!({ "id": id, "status": "queued" })))
}

async fn reissue_certificate(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    certificate::mark_for_issue(&state.database, &id)
        .await
        .map_err(certificate_error)?;
    certificate::trigger(state.database.clone(), id.clone());
    Ok(Json(json!({ "id": id, "status": "queued" })))
}

async fn delete_certificate(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match certificate::delete_certificate(&state.database, &id)
        .await
        .map_err(certificate_error)?
    {
        true => Ok(Json(json!({ "deleted": true }))),
        false => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "certificate not found" })),
        )),
    }
}

async fn dns_providers(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let providers = certificate::list_dns_providers(&state.database)
        .await
        .map_err(certificate_error)?;
    let items: Vec<Value> = providers
        .into_iter()
        .map(|provider| {
            json!({
                "id": provider.id,
                "name": provider.name,
                "domains": provider.domains,
                "kind": provider.kind,
                "from_env": provider.from_env,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct CreateDnsProviderRequest {
    name: Option<String>,
    domains: Vec<String>,
    kind: Option<String>,
    #[serde(default)]
    from_env: bool,
    secret_id: Option<String>,
    secret_key: Option<String>,
}

async fn create_dns_provider(
    State(state): State<AppState>,
    Json(payload): Json<CreateDnsProviderRequest>,
) -> Result<Json<Value>, ApiError> {
    let id = certificate::create_dns_provider(
        &state.database,
        payload.name,
        payload.domains,
        payload.kind,
        payload.from_env,
        payload.secret_id,
        payload.secret_key,
    )
    .await
    .map_err(certificate_error)?;
    Ok(Json(json!({ "id": id })))
}

async fn delete_dns_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match certificate::delete_dns_provider(&state.database, &id)
        .await
        .map_err(certificate_error)?
    {
        true => Ok(Json(json!({ "deleted": true }))),
        false => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "DNS provider not found" })),
        )),
    }
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

fn certificate_error(error: anyhow::Error) -> ApiError {
    event!(Level::WARN, error = %format!("{error:#}"), "certificate API request failed");
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": error.to_string() })),
    )
}

type ApiError = (StatusCode, Json<Value>);

fn internal(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}
