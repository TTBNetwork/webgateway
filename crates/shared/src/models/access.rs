use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, de};
use simple_shared::objectid::ObjectId;
use sqlx::{
    FromRow, Row,
    postgres::PgRow,
    types::{Json, Text},
};
use sqlx_pg_ext_uint::{c_u16::U16, c_usize::USize};

#[derive(Debug, Clone)]
pub enum AccessVersion {
    HTTP09,
    HTTP10,
    HTTP11,
    HTTP2,
    HTTP3,
}

impl<'de> Deserialize<'de> for AccessVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?.to_lowercase();
        match s.as_str() {
            "http/1.1" => Ok(AccessVersion::HTTP11),
            "http/1.0" => Ok(AccessVersion::HTTP10),
            "http/2.0" => Ok(AccessVersion::HTTP2),
            "http/0.9" => Ok(AccessVersion::HTTP09),
            "http/3.0" => Ok(AccessVersion::HTTP3),
            _ => Err(serde::de::Error::custom("Invalid HTTP version")),
        }
    }
}

impl Serialize for AccessVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            AccessVersion::HTTP10 => serializer.serialize_str("HTTP/1.0"),
            AccessVersion::HTTP11 => serializer.serialize_str("HTTP/1.1"),
            AccessVersion::HTTP2 => serializer.serialize_str("HTTP/2.0"),
            AccessVersion::HTTP3 => serializer.serialize_str("HTTP/3.0"),
            AccessVersion::HTTP09 => serializer.serialize_str("HTTP/0.9"),
        }
    }
}

impl FromStr for AccessVersion {
    type Err = std::fmt::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "http/1.1" => Ok(AccessVersion::HTTP11),
            "http/1.0" => Ok(AccessVersion::HTTP10),
            "http/2.0" => Ok(AccessVersion::HTTP2),
            "http/0.9" => Ok(AccessVersion::HTTP09),
            "http/3.0" => Ok(AccessVersion::HTTP3),
            _ => Err(std::fmt::Error),
        }
    }
}

// to str
impl std::fmt::Display for AccessVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessVersion::HTTP10 => write!(f, "HTTP/1.0"),
            AccessVersion::HTTP11 => write!(f, "HTTP/1.1"),
            AccessVersion::HTTP2 => write!(f, "HTTP/2.0"),
            AccessVersion::HTTP3 => write!(f, "HTTP/3.0"),
            AccessVersion::HTTP09 => write!(f, "HTTP/0.9"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRequest {
    pub id: ObjectId,
    pub host: String,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub http_version: AccessVersion,
    pub remote_addr: String,
    pub body_length: usize,
    pub created_at: DateTime<Utc>,
    pub requested_at: DateTime<Utc>,
    pub website_id: Option<ObjectId>,
}

impl<'r> FromRow<'r, PgRow> for AccessRequest {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            method: row.try_get("method")?,
            path: row.try_get("path")?,
            headers: row.try_get::<Json<Vec<_>>, _>("headers")?.0,
            http_version: row.try_get::<Text<AccessVersion>, _>("http_version")?.0,
            remote_addr: row.try_get("remote_addr")?,
            created_at: row.try_get("created_at")?,
            requested_at: row.try_get("requested_at")?,
            body_length: row.try_get::<USize, _>("body_length")?.into(),
            host: row.try_get("host")?,
            website_id: row.try_get("website_id")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessCreateRequest {
    pub id: ObjectId,
    pub host: String,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub http_version: AccessVersion,
    pub remote_addr: String,
    pub body_length: usize,
    pub requested_at: DateTime<Utc>,
    pub website_id: Option<ObjectId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessResponse {
    pub id: ObjectId,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_length: Option<usize>,
    pub http_version: AccessVersion,
    pub created_at: DateTime<Utc>,
    pub responsed_at: DateTime<Utc>,
    pub backend_responsed_at: Option<DateTime<Utc>>,
    pub website_id: Option<ObjectId>,
}

impl<'r> FromRow<'r, PgRow> for AccessResponse {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            status: row.try_get::<U16, _>("status")?.into(),
            headers: row.try_get::<Json<Vec<_>>, _>("headers")?.0,
            created_at: row.try_get("created_at")?,
            responsed_at: row.try_get("responsed_at")?,
            body_length: row
                .try_get::<Option<USize>, _>("body_length")?
                .map(|v| v.into()),
            http_version: row.try_get::<Text<AccessVersion>, _>("http_version")?.0,
            backend_responsed_at: row.try_get("backend_response_at")?,
            website_id: row.try_get("website_id")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessCreateResponse {
    pub id: ObjectId,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_length: usize,
    pub http_version: AccessVersion,
    pub responsed_at: DateTime<Utc>,
    pub backend_responsed_at: Option<DateTime<Utc>>,
    pub website_id: Option<ObjectId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseQPS {
    pub count: usize,
    pub time: DateTime<Utc>,
}

impl<'r> FromRow<'r, PgRow> for DatabaseQPS {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            count: row.try_get::<i64, _>("total_requests")? as usize,
            time: row.try_get("time")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseQPS {
    pub data: Vec<DatabaseQPS>,
    pub interval: usize,
    pub current_time: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessInfo {
    pub total_requests: usize,
    pub total_ips: usize,
    pub backend_error_requests: usize,
    pub e4xx_requests: usize,
    pub e5xx_requests: usize,
    pub total_request_size: usize,
    pub total_response_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(try_from = "usize")]
pub enum QueryQPSType {
    Second = 1,
    #[default]
    FiveSeconds = 5,
}

impl TryFrom<usize> for QueryQPSType {
    type Error = &'static str;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(QueryQPSType::Second),
            5 => Ok(QueryQPSType::FiveSeconds),
            _ => Err("invalid value for QueryQPSType, expected 1 or 5"),
        }
    }
}

impl From<QueryQPSType> for usize {
    fn from(value: QueryQPSType) -> Self {
        match value {
            QueryQPSType::Second => 1,
            QueryQPSType::FiveSeconds => 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryQPS {
    pub interval: QueryQPSType,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(try_from = "usize")]
pub enum QueryAccessInfoDays {
    #[default]
    Day = 1,
    SevenDays = 7,
    ThirtyDays = 30,
}

impl TryFrom<usize> for QueryAccessInfoDays {
    type Error = &'static str;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(QueryAccessInfoDays::Day),
            7 => Ok(QueryAccessInfoDays::SevenDays),
            30 => Ok(QueryAccessInfoDays::ThirtyDays),
            _ => Err("invalid value for QueryAccessInfoDays, expected 1, 7, or 30"),
        }
    }
}

impl From<QueryAccessInfoDays> for usize {
    fn from(value: QueryAccessInfoDays) -> Self {
        match value {
            QueryAccessInfoDays::Day => 1,
            QueryAccessInfoDays::SevenDays => 7,
            QueryAccessInfoDays::ThirtyDays => 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryAccessInfo {
    pub in_days: QueryAccessInfoDays,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessUpdateResponseSize {
    pub id: ObjectId,
    pub body_length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessUpdateRequestSize {
    pub id: ObjectId,
    pub body_length: usize,
}

// from
impl From<(ObjectId, usize)> for AccessUpdateRequestSize {
    fn from((id, body_length): (ObjectId, usize)) -> Self {
        Self { id, body_length }
    }
}

impl From<(ObjectId, usize)> for AccessUpdateResponseSize {
    fn from((id, body_length): (ObjectId, usize)) -> Self {
        Self { id, body_length }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessInsertResponseSize {
    pub id: ObjectId,
    pub body_length: usize,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessInsertRequestSize {
    pub id: ObjectId,
    pub body_length: usize,
    pub created_at: DateTime<Utc>,
}

// from
impl AccessInsertRequestSize {
    pub fn new(id: ObjectId, body_length: usize, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            body_length,
            created_at,
        }
    }
}

impl AccessInsertResponseSize {
    pub fn new(id: ObjectId, body_length: usize, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            body_length,
            created_at,
        }
    }
}

// Website Access Info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebsiteAccessInfo {
    pub total_requests: usize,
    pub total_responses: usize,
    pub total_ips: usize,
    pub backend_error_requests: usize,
    pub e4xx_requests: usize,
    pub e5xx_requests: usize,
    pub total_requests_size: usize,
    pub total_response_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodayMetricsInfoOfWebsite {
    pub website_id: Option<ObjectId>,
    pub total_requests: usize,
    pub total_responses: usize,
    pub total_ips: usize,
    pub backend_error_requests: usize,
    pub e4xx_requests: usize,
    pub e5xx_requests: usize,
    pub total_requests_size: usize,
    pub total_response_size: usize,
}

impl<'r> FromRow<'r, PgRow> for TodayMetricsInfoOfWebsite {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            website_id: row.try_get("website_id")?,
            total_requests: row.try_get::<i64, _>("total_requests")? as usize,
            total_responses: row.try_get::<i64, _>("total_responses")? as usize,
            total_ips: row.try_get::<i64, _>("total_ips")? as usize,
            backend_error_requests: row.try_get::<i64, _>("backend_error_requests")? as usize,
            e4xx_requests: row.try_get::<i64, _>("e4xx_requests")? as usize,
            e5xx_requests: row.try_get::<i64, _>("e5xx_requests")? as usize,
            total_requests_size: row.try_get::<USize, _>("total_requests_size")?.into(),
            total_response_size: row.try_get::<USize, _>("total_response_size")?.into(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub enum QueryAccessMapType {
    #[default]
    Global,
    China,
}

impl<'de> Deserialize<'de> for QueryAccessMapType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

impl std::fmt::Display for QueryAccessMapType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryAccessMapType::Global => write!(f, "global"),
            QueryAccessMapType::China => write!(f, "china"),
        }
    }
}

impl FromStr for QueryAccessMapType {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "global" => Ok(QueryAccessMapType::Global),
            "china" => Ok(QueryAccessMapType::China),
            _ => Err("invalid value for QueryAccessMapType, expected 'global' or 'china'"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryAccessMap {
    pub in_days: QueryAccessInfoDays,
    #[serde(rename = "type")]
    pub map_type: QueryAccessMapType,
}
