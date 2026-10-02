use std::{net::IpAddr, time::Instant};

use anyhow::Result;
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use serde::Serialize;
// use serde_json::{json, Value};
use client_ip::{
    cf_connecting_ip, cloudfront_viewer_address, fly_client_ip, rightmost_x_forwarded_for,
    true_client_ip, x_real_ip,
};
use tower_http::catch_panic::CatchPanicLayer;
use tracing::{Level, event};
use units_formatter::time::format_duration;

use crate::foundation::RemoteAddr;

#[derive(Debug, Clone, Serialize)]
struct InnerAPIResponse<T: Serialize = ()> {
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

// ---------- 统一的 API 响应结构 ----------
#[derive(Debug, Clone)]
pub struct APIResponse<T: Serialize = ()> {
    inner: InnerAPIResponse<T>,
    pub headers: HeaderMap,
}

impl<T: Serialize> APIResponse<T> {
    pub fn status(&self) -> u16 {
        self.inner.status
    }

    pub fn message(&self) -> Option<&str> {
        self.inner.message.as_deref()
    }

    pub fn data(&self) -> Option<&T> {
        self.inner.data.as_ref()
    }

    pub fn new(data: Option<T>, status: u16, message: Option<&str>) -> Self {
        Self {
            inner: InnerAPIResponse {
                status,
                data,
                message: message.map(|s| s.to_string()),
            },
            headers: HeaderMap::new(),
        }
    }

    pub fn ok(data: T) -> Self {
        Self::new(Some(data), 200, None)
    }

    pub fn error(data: Option<T>, status: u16, message: impl Into<String>) -> Self {
        Self::new(data, status, Some(message.into().as_str()))
    }

    pub fn result(data: Result<T>) -> Self {
        match data {
            Ok(data) => Self::ok(data),
            Err(e) => Self::error(None, 500, e.to_string()),
        }
    }
}

impl<T: Serialize> IntoResponse for APIResponse<T> {
    fn into_response(self) -> Response<Body> {
        let status =
            StatusCode::from_u16(self.inner.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_string(&self.inner).unwrap();
        let mut headers = self.headers.clone();
        headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        let mut builder = Response::builder().status(status);

        let response_headers = builder.headers_mut();
        if let Some(response_headers) = response_headers {
            *response_headers = headers;
        }
        builder.body(Body::new(body)).unwrap()
    }
}

// ---------- 全局错误类型（自动转为 APIResponse）----------
#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    NotFound(String),
    Internal(anyhow::Error), // 包裹任意底层错误
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response<Body> {
        let (status, message) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "Forbidden".to_string()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::Internal(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Internal server error: {}", e),
            ),
        };
        APIResponse::<()>::error(None, status.as_u16(), message).into_response()
    }
}

/// 把 [`AppError`] 转成任意 `APIResponse<T>`。
///
/// 授权检查（`require_write` / `require_admin`）会返回 `AppError`，
/// 而 handler 的返回类型是 `APIResponse<Something>`，因此需要一个泛型的转换入口，
/// 让 `return auth.require_write().map_err(Into::into)?;` 这类写法可用。
impl<T: Serialize> From<AppError> for APIResponse<T> {
    fn from(err: AppError) -> Self {
        let status = err.status_code();
        APIResponse::error(None, status.as_u16(), err.message())
    }
}

impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            AppError::BadRequest(msg) => msg.clone(),
            AppError::Unauthorized => "Unauthorized".to_string(),
            AppError::Forbidden => "Forbidden".to_string(),
            AppError::NotFound(msg) => msg.clone(),
            AppError::Internal(e) => format!("Internal server error: {e}"),
        }
    }
}

// 将 anyhow::Error 转换为 AppError::Internal
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        AppError::Internal(err.into())
    }
}

// AppError into APIResponse
// ---------- 请求日志中间件 ----------
#[derive(Debug, Clone)]
#[allow(unused)]
pub struct ReqRecord {
    method: String,
    raw_path: String,
    user_agent: Option<String>,
    host: Option<String>,
    remote_addr: IpAddr, // 可以从扩展中获取真实 IP
}

pub async fn logging_middleware(req: Request<Body>, next: Next) -> Response<Body> {
    let remote_addr = req.extensions().get::<RemoteAddr>().copied();
    let ip = IpAddr::from(remote_addr.unwrap());
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let user_agent = req
        .headers()
        .get("user-agent")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let method = req.method().to_string();
    let raw_path = req.uri().to_string();

    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed();

    // 固定列宽（根据实际需要调整）
    const IPV4_W: usize = 16; // IPv6 最大长度 39
    const IPV6_W: usize = 39;
    const METHOD_W: usize = 6;
    const UA_W: usize = 40;
    const TIME_W: usize = 14;

    event!(Level::INFO, "{}", {
        let host_str = host.as_deref().unwrap_or("-");
        let ip_str = ip.to_string();
        let ua_str = user_agent.as_deref().unwrap_or("-");

        let ip_fmt = format!(
            "{:<width$}",
            ip_str,
            width = match ip {
                IpAddr::V4(_) => IPV4_W,
                IpAddr::V6(_) => IPV6_W,
            }
        );
        // 中间部分：方法、路径、用户代理，固定宽度并用 " - " 连接
        let time_str = format_duration(elapsed, Some(4));
        let time_fmt = format!("{:>width$}", time_str, width = TIME_W);
        let status_str = response.status().as_u16().to_string();
        let middle = format!(
            "{:<mw$} {status_str} | {time_fmt} | {} - {:<uw$}",
            method,
            raw_path,
            ua_str,
            mw = METHOD_W,
            uw = UA_W
        );

        format!("{} | {} | {}", host_str, ip_fmt, middle)
    });

    response
}

// 按优先级尝试从不同代理头提取 IP
fn extract_ip_from_headers(headers: &HeaderMap) -> Option<IpAddr> {
    cf_connecting_ip(headers)
        .or_else(|_| cloudfront_viewer_address(headers))
        .or_else(|_| fly_client_ip(headers))
        .or_else(|_| true_client_ip(headers))
        .or_else(|_| x_real_ip(headers))
        .or_else(|_| rightmost_x_forwarded_for(headers))
        .ok()
}

pub async fn client_ip_middleware(
    ConnectInfo(RemoteAddr(addr)): ConnectInfo<RemoteAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let ip = extract_ip_from_headers(headers).unwrap_or(addr);

    request.extensions_mut().insert(RemoteAddr(ip));
    // println!("client_ip_middleware: {:?}", ip);
    next.run(request).await
}

pub fn wrapper_router(router: Router) -> Router {
    router
        .layer(middleware::from_fn(logging_middleware))
        .layer(middleware::from_fn(client_ip_middleware))
        .layer(CatchPanicLayer::new())
}
