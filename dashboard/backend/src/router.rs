//! Dashboard API and health endpoint.

use std::{
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use shared::database::{Database, schema_version};
use sqlx::Row;
use tracing::{Level, event};

use crate::auth::{self, User};
use crate::certificate;
use crate::websites;

/// Name of the HttpOnly session cookie handed out by POST /api/auth/login.
const SESSION_COOKIE: &str = "wg_session";

/// CSP for JSON API responses and health checks.
const API_CSP: &str =
    "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";
/// CSP for the bundled panel. Inline styles are required by Vue style bindings.
const PANEL_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

#[derive(Clone)]
struct AppState {
    database: Database,
    /// Service tokens accepted as bearer credentials. More than one entry keeps
    /// the previous token valid while it is rotated out.
    service_tokens: Arc<[String]>,
    session_ttl_secs: u64,
    /// Built panel directory; when unset only /health and /api are served.
    static_root: Option<Arc<PathBuf>>,
}

pub fn router(
    database: Database,
    service_tokens: Vec<String>,
    session_ttl_secs: u64,
    static_dir: Option<PathBuf>,
) -> Router {
    let state = AppState {
        database,
        service_tokens: Arc::from(service_tokens),
        session_ttl_secs,
        static_root: static_dir.map(Arc::new),
    };

    let read = Router::new()
        .route("/api/summary", get(summary))
        .route("/api/websites", get(websites))
        .route("/api/certificates", get(certificates))
        .route("/api/dns-providers", get(dns_providers))
        .route("/api/gateways", get(gateways))
        .route("/api/logs", get(logs))
        .route("/api/auth/me", get(me))
        .route("/api/auth/logout", post(logout));

    let certificate_write = Router::new()
        .route("/api/certificates", post(create_certificate))
        .route("/api/certificates/issue", post(issue_certificate))
        .route("/api/certificates/{id}", delete(delete_certificate))
        .route("/api/certificates/{id}/issue", post(reissue_certificate))
        .route_layer(middleware::from_fn(require_certificate_writer));

    let admin = Router::new()
        .route("/api/dns-providers", post(create_dns_provider))
        .route("/api/dns-providers/{id}", delete(delete_dns_provider))
        .route("/api/websites", post(create_website))
        .route(
            "/api/websites/{id}",
            put(update_website).delete(delete_website),
        )
        .route(
            "/api/gateways/{id}",
            patch(update_gateway).delete(delete_gateway),
        )
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/{id}", patch(update_user).delete(delete_user))
        .route("/api/users/{id}/totp", post(reset_user_totp))
        .route("/api/users/{id}/revoke", post(revoke_user))
        .route_layer(middleware::from_fn(require_admin));

    let protected = read
        .merge(certificate_write)
        .merge(admin)
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/health", get(health))
        .route("/api/auth/login", post(login))
        .merge(protected)
        .fallback(serve_panel)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// Serve the built panel with SPA history fallback. Unknown /api paths stay
/// JSON 404s so API clients never receive index.html.
async fn serve_panel(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    let secure = is_https(&headers);
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        let mut response = api_not_found();
        apply_security_headers(response.headers_mut(), secure);
        return response;
    }

    let response = match state.static_root.as_deref() {
        None => api_not_found(),
        Some(root) => match safe_join(root, path.trim_start_matches('/')) {
            Some(file) if file.is_file() => match tokio::fs::read(&file).await {
                Ok(bytes) => static_response(&file, bytes),
                Err(_) => panel_index(root).await,
            },
            _ => panel_index(root).await,
        },
    };

    let mut response = response;
    apply_security_headers(response.headers_mut(), secure);
    response
}

async fn panel_index(root: &FsPath) -> Response {
    let index = root.join("index.html");
    match tokio::fs::read(&index).await {
        Ok(bytes) => static_response(&index, bytes),
        Err(_) => {
            let mut response = Response::new(Body::from("panel not available\n"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            response
        }
    }
}

/// Join a URL path to the static root, rejecting traversal and absolute paths.
fn safe_join(root: &FsPath, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    let mut found = false;
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains('\\') || segment.contains('\0') {
            return None;
        }
        path.push(segment);
        found = true;
    }
    found.then_some(path)
}

fn static_response(path: &FsPath, bytes: Vec<u8>) -> Response {
    let content_type = content_type(path);
    let cache = if path_has_hashed_assets(path) {
        "public, max-age=31536000, immutable"
    } else if content_type.starts_with("text/html") {
        "no-store"
    } else {
        "no-cache"
    };
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

fn path_has_hashed_assets(path: &FsPath) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value == "assets")
    })
}

fn content_type(path: &FsPath) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn api_not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

/// Add hardening response headers to every dashboard response. The API only
/// returns JSON, so a deny-all CSP is safe here.
async fn security_headers(request: Request, next: Next) -> Response {
    let secure = is_https(request.headers());
    let mut response = next.run(request).await;
    apply_security_headers(response.headers_mut(), secure);
    response
}

fn apply_security_headers(headers: &mut HeaderMap, secure: bool) {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_html = content_type.starts_with("text/html");
    // JSON (or body-less) responses are API output: deny everything and never
    // cache. Static assets keep the cache policy set by the file handler.
    let is_api = content_type.is_empty() || content_type.starts_with("application/json");

    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(if is_html { PANEL_CSP } else { API_CSP }),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if is_api {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
    );
    if secure {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
}

async fn require_auth(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let user = match bearer_token(request.headers()) {
        Some(token) if is_service_token(&state.service_tokens, token) => Some(service_user()),
        Some(token) => auth::authenticate(&state.database, token).await,
        None => match session_token(request.headers()) {
            Some(token) => auth::authenticate(&state.database, token).await,
            None => None,
        },
    };

    match user {
        Some(user) => {
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        None => {
            let mut response = (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unauthorized" })),
            )
                .into_response();
            // Drop a session cookie that no longer authenticates (expired or
            // revoked) so the browser stops replaying it.
            if session_token(request.headers()).is_some() {
                set_cookie(
                    &mut response,
                    cleared_session_cookie(is_https(request.headers())),
                );
            }
            response
        }
    }
}

fn service_user() -> User {
    User {
        id: "service".to_string(),
        username: "service".to_string(),
        role: "service".to_string(),
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(name, value)| {
            let (name, value) = (name.trim(), value.trim());
            (name == SESSION_COOKIE && !value.is_empty()).then_some(value)
        })
}

/// Compare a candidate against every configured service token without exiting
/// early, so the comparison does not leak position information.
fn is_service_token(tokens: &[String], candidate: &str) -> bool {
    let mut matched = false;
    for token in tokens {
        matched |= constant_time_eq(token.as_bytes(), candidate.as_bytes());
    }
    matched
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = (left.len() ^ right.len()) as u32;
    for index in 0..left.len().min(right.len()) {
        difference |= u32::from(left[index] ^ right[index]);
    }
    difference == 0
}

fn session_cookie(token: &str, ttl_secs: u64, secure: bool) -> String {
    let mut cookie =
        format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={ttl_secs}");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn cleared_session_cookie(secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn set_cookie(response: &mut Response, cookie: String) {
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
}

/// A TLS-terminating reverse proxy is expected to set X-Forwarded-Proto.
fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .next()
                .unwrap_or(value)
                .trim()
                .eq_ignore_ascii_case("https")
        })
        .unwrap_or(false)
}

async fn require_admin(request: Request, next: Next) -> Response {
    match request.extensions().get::<User>() {
        Some(user) if auth::is_admin(user) => next.run(request).await,
        _ => (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "admin role required" })),
        )
            .into_response(),
    }
}

async fn require_certificate_writer(request: Request, next: Next) -> Response {
    match request.extensions().get::<User>() {
        Some(user) if auth::can_write_certificates(user) => next.run(request).await,
        _ => (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "certificate write permission required" })),
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
) -> Result<Response, ApiError> {
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
        state.session_ttl_secs,
    )
    .await
    .map_err(internal)?
    {
        auth::LoginOutcome::Success(success) => {
            // The JWT never reaches JavaScript: it is delivered as an HttpOnly,
            // SameSite=Strict cookie (SameSite blocks cross-site CSRF) and the
            // JSON body only reports the authenticated user.
            let mut response = Json(json!({
                "user": success.user,
                "expires_in": state.session_ttl_secs,
            }))
            .into_response();
            set_cookie(
                &mut response,
                session_cookie(&success.token, state.session_ttl_secs, is_https(&headers)),
            );
            Ok(response)
        }
        auth::LoginOutcome::Invalid => {
            crate::metrics::record_auth_failure();
            tokio::time::sleep(Duration::from_millis(500)).await;
            Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "invalid username or code" })),
            ))
        }
        auth::LoginOutcome::Throttled { retry_after_secs } => {
            crate::metrics::record_auth_throttled();
            Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "error": "too many login attempts",
                    "retry_after_secs": retry_after_secs,
                })),
            ))
        }
    }
}

async fn me(Extension(user): Extension<User>) -> Json<Value> {
    Json(json!({ "user": user }))
}

async fn logout(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if user.role != "service" {
        auth::revoke_tokens(&state.database, &user.id)
            .await
            .map_err(internal)?;
    }
    let mut response = Json(json!({ "status": "ok" })).into_response();
    set_cookie(&mut response, cleared_session_cookie(is_https(&headers)));
    Ok(response)
}

async fn list_users(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let users = auth::list_users(&state.database).await.map_err(internal)?;
    Ok(Json(json!({ "items": users })))
}

#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    username: String,
    role: String,
}

async fn create_user(
    State(state): State<AppState>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<Value>, ApiError> {
    let (user, totp_secret) = auth::create_user(&state.database, &payload.username, &payload.role)
        .await
        .map_err(auth_error)?;
    Ok(Json(json!({ "user": user, "totp_secret": totp_secret })))
}

#[derive(Debug, Deserialize)]
struct UpdateUserRequest {
    username: Option<String>,
    role: Option<String>,
}

async fn update_user(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateUserRequest>,
) -> Result<Json<Value>, ApiError> {
    if payload.username.is_none() && payload.role.is_none() {
        return Err(bad_request("nothing to update"));
    }
    match auth::update_user(
        &state.database,
        &id,
        payload.username.as_deref(),
        payload.role.as_deref(),
    )
    .await
    .map_err(auth_error)?
    {
        Some(user) => Ok(Json(json!({ "user": user }))),
        None => Err(not_found("user")),
    }
}

async fn delete_user(
    State(state): State<AppState>,
    Extension(current): Extension<User>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if id == current.id {
        return Err(bad_request("cannot delete the current user"));
    }
    let target = auth::get_user(&state.database, &id)
        .await
        .map_err(internal)?;
    let Some(target) = target else {
        return Err(not_found("user"));
    };
    if target.role == "admin"
        && auth::count_admins(&state.database)
            .await
            .map_err(internal)?
            <= 1
    {
        return Err(bad_request("cannot delete the last admin"));
    }
    match auth::delete_user(&state.database, &id)
        .await
        .map_err(internal)?
    {
        true => Ok(Json(json!({ "deleted": true }))),
        false => Err(not_found("user")),
    }
}

async fn reset_user_totp(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match auth::reset_totp(&state.database, &id)
        .await
        .map_err(internal)?
    {
        Some(secret) => Ok(Json(json!({ "totp_secret": secret }))),
        None => Err(not_found("user")),
    }
}

async fn revoke_user(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match auth::revoke_tokens(&state.database, &id)
        .await
        .map_err(internal)?
    {
        true => Ok(Json(json!({ "revoked": true }))),
        false => Err(not_found("user")),
    }
}

async fn create_website(
    State(state): State<AppState>,
    Json(input): Json<websites::WebsiteInput>,
) -> Result<Json<Value>, ApiError> {
    let id = websites::create(&state.database, input)
        .await
        .map_err(bad_request)?;
    Ok(Json(json!({ "id": id })))
}

async fn update_website(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<websites::WebsiteInput>,
) -> Result<Json<Value>, ApiError> {
    match websites::update(&state.database, &id, input)
        .await
        .map_err(bad_request)?
    {
        true => Ok(Json(json!({ "updated": true }))),
        false => Err(not_found("website")),
    }
}

async fn delete_website(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match websites::delete(&state.database, &id)
        .await
        .map_err(internal)?
    {
        true => Ok(Json(json!({ "deleted": true }))),
        false => Err(not_found("website")),
    }
}

#[derive(Debug, Deserialize)]
struct UpdateGatewayRequest {
    name: Option<String>,
    country: Option<String>,
    city: Option<String>,
    address: Option<String>,
}

async fn update_gateway(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateGatewayRequest>,
) -> Result<Json<Value>, ApiError> {
    let result = sqlx::query(
        "UPDATE gateways
            SET name = COALESCE($2, name),
                country = COALESCE($3, country),
                city = COALESCE($4, city),
                address = COALESCE($5, address)
          WHERE id = $1",
    )
    .bind(&id)
    .bind(payload.name)
    .bind(payload.country)
    .bind(payload.city)
    .bind(payload.address)
    .execute(state.database.pool())
    .await
    .map_err(internal)?;
    if result.rows_affected() == 0 {
        return Err(not_found("gateway"));
    }
    Ok(Json(json!({ "updated": true })))
}

async fn delete_gateway(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = sqlx::query("DELETE FROM gateways WHERE id = $1")
        .bind(&id)
        .execute(state.database.pool())
        .await
        .map_err(internal)?;
    if result.rows_affected() == 0 {
        return Err(not_found("gateway"));
    }
    Ok(Json(json!({ "deleted": true })))
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
        Err(error) => {
            // /health is unauthenticated; log the cause instead of returning
            // database/driver details to the caller.
            event!(Level::ERROR, error = %format!("{error:#}"), "database health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "error",
                    "database": "down",
                    "message": "database unavailable",
                    "schema_version": schema_version(),
                })),
            )
        }
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

fn not_found(resource: &str) -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("{resource} not found") })),
    )
}

fn bad_request(message: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.to_string() })),
    )
}

/// Validation failures keep their message (they are actionable for the
/// caller); database failures are logged and redacted.
fn auth_error(error: anyhow::Error) -> ApiError {
    if contains_sqlx(&error) {
        return internal(format!("{error:#}"));
    }
    bad_request(error.to_string())
}

/// ACME and certificate validation errors are returned to the operator, but
/// database failures are redacted like every other internal error.
fn certificate_error(error: anyhow::Error) -> ApiError {
    if contains_sqlx(&error) {
        return internal(format!("{error:#}"));
    }
    event!(Level::WARN, error = %format!("{error:#}"), "certificate API request failed");
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": error.to_string() })),
    )
}

type ApiError = (StatusCode, Json<Value>);

/// Log the full error, then return a generic response plus a correlation id.
fn internal(error: impl std::fmt::Display) -> ApiError {
    crate::metrics::record_api_error();
    let error_id = error_id();
    event!(
        Level::ERROR,
        error = %error,
        error_id = %error_id,
        "dashboard request failed"
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": "internal server error",
            "error_id": error_id,
        })),
    )
}

fn contains_sqlx(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<sqlx::Error>().is_some())
}

fn error_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return "unknown".to_string();
    }
    data_encoding::HEXLOWER.encode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens_in_constant_time() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn matches_any_rotating_service_token() {
        let tokens = vec!["old-token".to_string(), "new-token".to_string()];
        assert!(is_service_token(&tokens, "old-token"));
        assert!(is_service_token(&tokens, "new-token"));
        assert!(!is_service_token(&tokens, "other"));
        assert!(!is_service_token(&[], "old-token"));
    }

    #[test]
    fn reads_the_session_cookie() {
        let mut headers = HeaderMap::new();
        assert_eq!(session_token(&headers), None);
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; wg_session=abc.def.ghi"),
        );
        assert_eq!(session_token(&headers), Some("abc.def.ghi"));
        headers.insert(header::COOKIE, HeaderValue::from_static("theme=dark"));
        assert_eq!(session_token(&headers), None);
    }

    #[test]
    fn builds_secure_session_cookies_only_over_tls() {
        let cookie = session_cookie("jwt", 600, false);
        assert!(cookie.starts_with("wg_session=jwt;"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Max-Age=600"));
        assert!(!cookie.contains("Secure"));
        assert!(session_cookie("jwt", 600, true).ends_with("; Secure"));

        let cleared = cleared_session_cookie(true);
        assert!(cleared.contains("Max-Age=0"));
        assert!(cleared.ends_with("; Secure"));
    }

    #[test]
    fn detects_https_behind_a_proxy() {
        let mut headers = HeaderMap::new();
        assert!(!is_https(&headers));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(is_https(&headers));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(!is_https(&headers));
    }

    #[test]
    fn adds_hardening_headers() {
        let mut headers = HeaderMap::new();
        apply_security_headers(&mut headers, true);
        assert_eq!(
            headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        assert_eq!(
            headers.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            API_CSP
        );
        assert!(headers.get(header::STRICT_TRANSPORT_SECURITY).is_some());
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");

        let mut plain = HeaderMap::new();
        apply_security_headers(&mut plain, false);
        assert!(plain.get(header::STRICT_TRANSPORT_SECURITY).is_none());
    }

    #[test]
    fn html_gets_the_panel_csp_and_keeps_its_cache_policy() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        apply_security_headers(&mut headers, false);
        assert_eq!(
            headers.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            PANEL_CSP
        );
        assert!(PANEL_CSP.contains("script-src 'self'"));
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");

        let mut asset = HeaderMap::new();
        asset.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        );
        asset.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
        apply_security_headers(&mut asset, false);
        assert_eq!(
            asset.get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
    }

    #[test]
    fn rejects_static_path_traversal() {
        let root = FsPath::new("/srv/panel");
        assert_eq!(
            safe_join(root, "assets/app.js"),
            Some(PathBuf::from("/srv/panel/assets/app.js"))
        );
        assert_eq!(safe_join(root, "/"), None);
        assert_eq!(safe_join(root, ".."), None);
        assert_eq!(safe_join(root, "assets/../../etc/passwd"), None);
        assert_eq!(safe_join(root, "assets\\windows.js"), None);
    }

    #[test]
    fn maps_static_content_types() {
        assert_eq!(
            content_type(FsPath::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(FsPath::new("app.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type(FsPath::new("logo.svg")), "image/svg+xml");
        assert_eq!(content_type(FsPath::new("font.woff2")), "font/woff2");
        assert_eq!(
            content_type(FsPath::new("blob.bin")),
            "application/octet-stream"
        );
    }

    #[test]
    fn redacts_internal_errors() {
        let (status, body) = internal(sqlx::Error::RowNotFound);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let text = body.0.to_string();
        assert!(text.contains("internal server error"));
        assert!(text.contains("error_id"));
        assert!(!text.contains("RowNotFound"));
    }

    #[test]
    fn keeps_validation_errors_but_redacts_database_errors() {
        let (status, body) = auth_error(anyhow::anyhow!("username already exists"));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0.to_string().contains("username already exists"));

        let database =
            anyhow::Error::from(sqlx::Error::RowNotFound).context("failed to load the user");
        let (status, body) = auth_error(database);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!body.0.to_string().contains("failed to load"));
    }
}
