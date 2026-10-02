use sqlx::types::Json;
use sqlx::{Postgres, Transaction};
use sqlx_pg_ext_uint::c_u16::U16;

use crate::{
    database::Database,
    models::websites::{CreateDatabaseWebsite, DatabaseWebsite, DatabaseWebsiteConfig},
    objectid::ObjectId,
};

#[async_trait::async_trait]
pub trait DatabaseWebsiteInitializer {
    async fn initialize_websites(&self, tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
impl DatabaseWebsiteInitializer for Database {
    async fn initialize_websites(&self, tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
        for sql in [
            r#"CREATE TABLE IF NOT EXISTS websites (
                id TEXT PRIMARY KEY,
                name TEXT,
                hosts TEXT[] NOT NULL DEFAULT '{}',
                ports uint2[] NOT NULL DEFAULT '{}',
                certificates TEXT[] NOT NULL DEFAULT '{}',
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                backends JSONB NOT NULL,
                config JSONB NOT NULL
            );"#,
            "CREATE INDEX IF NOT EXISTS idx_websites_hosts ON websites USING GIN (hosts);",
            "CREATE INDEX IF NOT EXISTS idx_websites_name ON websites USING GIN (name);",
            "CREATE INDEX IF NOT EXISTS idx_websites_created_at ON websites (created_at);",
        ] {
            sqlx::query(sql).execute(&mut **tx).await?;
        }
        self.create_trigger_notify_tx(tx, "websites").await?;
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait DatabaseWebsiteRepository {
    async fn get_websites(&self) -> anyhow::Result<Vec<DatabaseWebsite>>;
    async fn get_websites_before_updated_at(
        &self,
        updated_at: &chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<DatabaseWebsite>>;
    async fn get_website(&self, id: &ObjectId) -> anyhow::Result<DatabaseWebsite>;
}

#[async_trait::async_trait]
impl DatabaseWebsiteRepository for Database {
    async fn get_websites(&self) -> anyhow::Result<Vec<DatabaseWebsite>> {
        let rows = sqlx::query_as::<_, DatabaseWebsite>("SELECT * FROM websites;")
            .fetch_all(&self.pool)
            .await?;
        // println!("Got {rows:?} websites");
        Ok(rows)
    }

    async fn get_websites_before_updated_at(
        &self,
        updated_at: &chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<DatabaseWebsite>> {
        let rows =
            sqlx::query_as::<_, DatabaseWebsite>("SELECT * FROM websites WHERE updated_at > $1;")
                .bind(updated_at)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    async fn get_website(&self, id: &ObjectId) -> anyhow::Result<DatabaseWebsite> {
        let row = sqlx::query_as::<_, _>("SELECT * FROM websites WHERE id = $1;")
            .bind(id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(row)
    }
}

#[async_trait::async_trait]
pub trait DatabaseWebsiteModifyRepository {
    async fn create_website(
        &self,
        website: &CreateDatabaseWebsite,
    ) -> anyhow::Result<DatabaseWebsite>;

    /// 按 id **整条覆盖**站点配置。
    ///
    /// 语义是替换而不是合并：面板提交的是完整表单（域名/端口/证书/后端），
    /// 合并语义会让"删掉某个域名/后端"这类操作无法生效。
    /// 返回受影响行数 —— 0 表示 id 不存在，调用方应据此回 404 而不是假装成功。
    async fn update_website(
        &self,
        id: &ObjectId,
        website: &CreateDatabaseWebsite,
    ) -> anyhow::Result<u64>;

    /// 按 id 删除站点，返回受影响行数（0 = 不存在）。
    async fn delete_website(&self, id: &ObjectId) -> anyhow::Result<u64>;
}

#[async_trait::async_trait]
impl DatabaseWebsiteModifyRepository for Database {
    async fn create_website(
        &self,
        website: &CreateDatabaseWebsite,
    ) -> anyhow::Result<DatabaseWebsite> {
        let id = ObjectId::new();
        let row = sqlx::query_as::<_, _>("INSERT INTO websites (id, name, hosts, ports, certificates, backends, config) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *;")
            .bind(id)
            .bind(website.name.as_ref())
            .bind(website.hosts.to_vec())
            .bind(website.ports.to_vec().iter().map(|v| U16::from(*v)).collect::<Vec<U16>>())
            .bind(website.certificates.to_vec())
            .bind(Json(&website.backends.to_vec()))
            .bind(Json(website.config.as_ref().unwrap_or(&DatabaseWebsiteConfig::default())))
            .fetch_one(&self.pool)
            .await?;
        Ok(row)
    }

    async fn update_website(
        &self,
        id: &ObjectId,
        website: &CreateDatabaseWebsite,
    ) -> anyhow::Result<u64> {
        // `updated_at` 由触发器（`update_updated_at()`）维护，这里不手写，
        // 以免与触发器语义冲突；网关侧靠 NOTIFY + 10s 兜底全量同步感知变更。
        let result = sqlx::query(
            "UPDATE websites SET name = $2, hosts = $3, ports = $4, certificates = $5, \
             backends = $6, config = $7 WHERE id = $1",
        )
        .bind(id)
        .bind(website.name.as_ref())
        .bind(website.hosts.to_vec())
        .bind(
            website
                .ports
                .to_vec()
                .iter()
                .map(|v| U16::from(*v))
                .collect::<Vec<U16>>(),
        )
        .bind(website.certificates.to_vec())
        .bind(Json(&website.backends.to_vec()))
        .bind(Json(
            website
                .config
                .as_ref()
                .unwrap_or(&DatabaseWebsiteConfig::default()),
        ))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn delete_website(&self, id: &ObjectId) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM websites WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
