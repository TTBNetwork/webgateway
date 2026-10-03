use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Error, FromRow, Row, postgres::PgRow, types::Json};
use sqlx_pg_ext_uint::c_u16::U16;
use url::Url;

use crate::objectid::ObjectId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseWebsite {
    pub id: ObjectId,
    pub name: Option<String>,
    pub hosts: Vec<String>,
    pub ports: Vec<u16>,
    pub certificates: Vec<ObjectId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub backends: Vec<DatabaseWebsiteBackend>,
    pub config: DatabaseWebsiteConfig,
}

impl<'r> FromRow<'r, PgRow> for DatabaseWebsite {
    fn from_row(row: &PgRow) -> Result<Self, Error> {
        Ok(DatabaseWebsite {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            hosts: row.try_get("hosts")?,
            ports: row
                .try_get::<Vec<U16>, _>("ports")?
                .iter()
                .map(|v| u16::from(*v))
                .collect::<Vec<u16>>(),
            certificates: row.try_get("certificates")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            backends: row
                .try_get::<Json<Vec<DatabaseWebsiteBackend>>, _>("backends")?
                .0,
            config: row.try_get::<Json<DatabaseWebsiteConfig>, _>("config")?.0,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseWebsiteBackend {
    pub match_path: Option<String>, // default /
    pub url: Url,
    pub balance: usize,
    pub main: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DatabaseWebsiteConfig {
    /// 取客户端 IP 的方式。
    ///
    /// **必须带 `#[serde(default)]`**：数据库里 `config` 可能是一个空对象
    /// （`{}` —— 旧版本写入的行，或前端提交了不完整的配置）。没有默认值时
    /// 反序列化会报 `missing field get_request_ip`，而网关启动时的
    /// `sync_first_config()` 会把这个错误直接抛出 → **整个网关起不来**
    /// （实测踩到：一行脏数据 = 一次停机）。字段本身已经有 `#[default] Raw`，
    /// 加上 default 后空对象退化成"按最朴素的方式取 IP"，而不是崩溃。
    #[serde(default)]
    pub get_request_ip: DatabaseWebsiteRequestIp,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", tag = "type", content = "data")]
pub enum DatabaseWebsiteRequestIp {
    #[default]
    Raw,
    ProxyProtocol,
    XForwardedFor(Option<usize>),
    XRealIP,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDatabaseWebsite {
    pub name: Option<String>,
    pub hosts: Vec<String>,
    pub ports: Vec<u16>,
    pub certificates: Vec<ObjectId>,
    pub backends: Vec<DatabaseWebsiteBackend>,
    pub config: Option<DatabaseWebsiteConfig>,
}
