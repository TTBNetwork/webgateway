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

pub async fn qps(Query(query): Query<QueryQPS>) -> APIResponse<ResponseQPS> {
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
    #[cfg(not(debug_assertions))]
    {
        return APIResponse::ok(HashMap::new());
    }
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
