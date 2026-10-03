use std::collections::HashMap;

use axum::{Router, extract::Query, middleware, routing::get};
use shared::{
    database::{access::DatabaseAccessLogsRepository, get_database},
    models::access::{
        AccessInfo, QueryAccessInfo, QueryAccessMap, QueryQPS, QueryQPSType, ResponseQPS,
        TodayMetricsInfoOfWebsite,
    },
};

use crate::{auth::middle_refresh_token, ip, response::APIResponse};

/// QPS 查询允许的最大点数。
///
/// 修复（ISSUES.md P1-9）：`count` 原先直接透传，`inline?count=100000000`
/// 会先做全表聚合再 `LIMIT 100000000`，单个请求即可放大数据库负载。
/// 60 个点已经足够前端图形（QPS.vue 只画 ≤60 个）。
const MAX_QPS_COUNT: usize = 300;

pub async fn qps(Query(query): Query<QueryQPS>) -> APIResponse<ResponseQPS> {
    if query.count == 0 || query.count > MAX_QPS_COUNT {
        return APIResponse::error(
            None,
            400,
            format!("count must be between 1 and {MAX_QPS_COUNT}"),
        );
    }
    APIResponse::result(match query.interval {
        QueryQPSType::Second => get_database().get_qps_per_second(query.count).await,
        QueryQPSType::FiveSeconds => get_database().get_qps_per_5s(query.count).await,
    })
}

pub async fn access_info(Query(query): Query<QueryAccessInfo>) -> APIResponse<AccessInfo> {
    APIResponse::result(get_database().get_access_info(query.in_days.into()).await)
}

pub async fn website_metrics_info() -> APIResponse<Vec<TodayMetricsInfoOfWebsite>> {
    APIResponse::result(get_database().get_today_metrics_info_of_websites().await)
}

pub async fn access_map(
    Query(query): Query<QueryAccessMap>,
) -> APIResponse<HashMap<String, usize>> {
    // 修复（P1-1）：原先这里在 `#[cfg(not(debug_assertions))]` 下直接返回空表，
    // 导致 release/生产构建的访问地图**永远没有数据**（疑似临时熔断，已确认是缺陷）。
    // 代价问题改由查询侧解决：`remote_addr` 上有索引（v2 的每个周分区都带
    // `(remote_addr, requested_at)` 复合索引，见 `access_v2::v2_ddl_for`）。
    let res = match get_database()
        .get_requests_of_ips(query.in_days.into())
        .await
    {
        Ok(res) => res,
        Err(e) => {
            return APIResponse::error(None, 500, e.to_string());
        }
    };

    let mut result: HashMap<String, usize> = HashMap::new();
    for (ip, count) in res {
        match ip.parse() {
            Ok(ip) => {
                let info = match ip::lookup(ip) {
                    Ok(info) => info,
                    Err(_) => {
                        continue;
                    }
                };
                // println!("{}: {:?}", ip, info);
                match query.map_type {
                    shared::models::access::QueryAccessMapType::Global => match info.country {
                        Some(country) => {
                            *result.entry(country).or_insert(0) += count;
                        }
                        None => {
                            *result.entry("Unknown".to_string()).or_insert(0) += count;
                        }
                    },
                    shared::models::access::QueryAccessMapType::China => {
                        if let Some(country) = info.country
                            && country == "CN"
                        {
                            match info.city {
                                Some(city) => {
                                    *result.entry(city).or_insert(0) += count;
                                }
                                None => {
                                    *result.entry("Unknown".to_string()).or_insert(0) += count;
                                }
                            }
                        }
                    }
                }
            }
            Err(_) => {
                continue;
            }
        }
    }

    APIResponse::ok(result)
}

pub fn router() -> Router {
    Router::new()
        .route("/qps", get(qps))
        .route("/info", get(access_info))
        .route("/metrics/websites", get(website_metrics_info))
        .route("/access_map", get(access_map))
        .layer(middleware::from_fn(middle_refresh_token))
}
