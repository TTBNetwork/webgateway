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
}
