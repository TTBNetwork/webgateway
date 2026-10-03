use anyhow::Result;
use shared::{database::Database, objectid::ObjectId};
use sqlx::types::Json;


use crate::models::log::{Log, LogAddr, LogContent};

/*
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shared::objectid::ObjectId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Log {
    pub id: ObjectId,
    pub user_id: ObjectId,
    pub content: LogContent,
    pub created_at: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", content = "content")]
pub enum LogContent {
    Raw(String),
    Data(LogContentData),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogContentParams {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogContentData {
    pub content: String,
    pub params: Vec<LogContentParams>,
} */

#[async_trait::async_trait]
pub trait WebLogManager {
    async fn add_web_log(
        &self,
        user_id: &ObjectId,
        content: &LogContent,
        address: &LogAddr,
    ) -> Result<()>;

    async fn get_total_of_web_logs(&self) -> Result<usize>;
    async fn get_web_logs_by_page(&self, page: usize, size: usize) -> Result<Vec<Log>>;
}

#[async_trait::async_trait]
impl WebLogManager for Database {
    async fn add_web_log(
        &self,
        user_id: &ObjectId,
        content: &LogContent,
        address: &LogAddr,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO web_log (id, user_id, content, address)
            VALUES ($1, $2, $3, $4)
        "#,
        )
        .bind(ObjectId::new())
        .bind(user_id)
        .bind(Json(content))
        .bind(&address.0)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_total_of_web_logs(&self) -> Result<usize> {
        let count: i64 = sqlx::query_scalar(r#"SELECT COUNT(content) FROM web_log"#)
            .fetch_one(&self.pool)
            .await?;
        Ok(count as usize)
    }

    async fn get_web_logs_by_page(&self, page: usize, size: usize) -> Result<Vec<Log>> {
        let logs: Vec<Log> =
            sqlx::query_as(r#"SELECT * FROM web_log ORDER BY created_at DESC LIMIT $1 OFFSET $2"#)
                .bind(size as i64)
                .bind((std::cmp::max(0, page) * size) as i64)
                .fetch_all(&self.pool)
                .await?;
        Ok(logs)
    }
}
