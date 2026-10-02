use async_trait::async_trait;
use sqlx::types::Json;
use sqlx::{Postgres, Transaction};

use crate::{
    database::Database,
    models::dnsprovider::{CreateDatabaseDNSProvider, DatabaseDNSProvider},
    objectid::ObjectId,
};
use anyhow::Result;

#[async_trait]
pub trait DatabaseDNSProviderInitializer {
    async fn initialize_dns_provider(&self, tx: &mut Transaction<'_, Postgres>) -> Result<()>;
}

#[async_trait]
impl DatabaseDNSProviderInitializer for Database {
    async fn initialize_dns_provider(&self, tx: &mut Transaction<'_, Postgres>) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS dns_providers (
                id TEXT PRIMARY KEY,
                name TEXT,
                provider JSONB NOT NULL,
                domains TEXT[] NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
        "#,
        )
        .execute(&mut **tx)
        .await?;
        self.create_trigger_notify_tx(tx, "dns_providers").await?;
        Ok(())
    }
}

#[async_trait]
pub trait DatabaseDNSProviderRepository {
    async fn get_dns_providers(&self) -> Result<Vec<DatabaseDNSProvider>>;
    async fn get_dns_provider_by_id(&self, id: &ObjectId) -> Result<DatabaseDNSProvider>;
    async fn get_total_of_dns_providers(&self) -> Result<usize>;
    async fn get_dns_providers_by_page(
        &self,
        page: usize,
        limit: usize,
    ) -> Result<Vec<DatabaseDNSProvider>>;
}

#[async_trait]
impl DatabaseDNSProviderRepository for Database {
    async fn get_dns_providers(&self) -> Result<Vec<DatabaseDNSProvider>> {
        let rows = sqlx::query_as::<_, DatabaseDNSProvider>(
            r#"
            SELECT * FROM dns_providers
        "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn get_dns_provider_by_id(&self, id: &ObjectId) -> Result<DatabaseDNSProvider> {
        let row = sqlx::query_as::<_, DatabaseDNSProvider>(
            r#"
            SELECT * FROM dns_providers WHERE id = $1
        "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn get_total_of_dns_providers(&self) -> Result<usize> {
        let count: i64 = sqlx::query_scalar(r#"SELECT COUNT(id) FROM dns_providers"#)
            .fetch_one(&self.pool)
            .await?;
        Ok(count as usize)
    }

    async fn get_dns_providers_by_page(
        &self,
        page: usize,
        limit: usize,
    ) -> Result<Vec<DatabaseDNSProvider>> {
        let dnsproviders: Vec<DatabaseDNSProvider> = sqlx::query_as(
            r#"SELECT * FROM dns_providers ORDER BY created_at DESC LIMIT $1 OFFSET $2"#,
        )
        .bind(limit as i64)
        .bind((std::cmp::max(0, page) * limit) as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(dnsproviders)
    }
}

#[async_trait]
pub trait DatabaseDNSProviderModifyRepository {
    async fn create_dns_provider(
        &self,
        dnsprovider: &CreateDatabaseDNSProvider,
    ) -> Result<DatabaseDNSProvider>;
}

#[async_trait]
impl DatabaseDNSProviderModifyRepository for Database {
    async fn create_dns_provider(
        &self,
        dnsprovider: &CreateDatabaseDNSProvider,
    ) -> Result<DatabaseDNSProvider> {
        // create, means insert
        let row = sqlx::query_as::<_, DatabaseDNSProvider>
            (r#"INSERT INTO dns_providers (id, name, provider, domains) VALUES ($1, $2, $3, $4) RETURNING *"#)
            .bind(ObjectId::default())
            .bind(&dnsprovider.name)
            .bind(Json(&dnsprovider.provider))
            .bind(&dnsprovider.domains)
            .fetch_one(&self.pool)
            .await?;
        Ok(row)
    }
}
