use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use sqlx::{Postgres, Transaction};

use crate::{database::Database, models::configuration::Configuration};

#[async_trait]
pub trait DatabaseConfigurationInitlializer {
    /// 在调用方提供的（由 advisory lock 保护的）事务中创建 `configurations` 表。
    ///
    /// 原先接受 `&self` 并在连接池上直接执行 DDL，既绕过迁移事务、也不受
    /// `SCHEMA_INIT` 锁保护，而且**从未被任何地方调用** —— 结果这张表在真实库里
    /// 根本不存在，配置的读写一上线就会失败。现改为与其他 initializer 一致的
    /// 事务签名，并接入迁移入口。
    async fn initialize_configuration(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> anyhow::Result<()>;
}

#[async_trait]
impl DatabaseConfigurationInitlializer for Database {
    async fn initialize_configuration(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> anyhow::Result<()> {
        sqlx::query(r#"CREATE TABLE IF NOT EXISTS configurations (key TEXT PRIMARY KEY NOT NULL, value jsonb NOT NULL DEFAULT '{}')"#).execute(&mut **tx).await?;
        Ok(())
    }
}

#[async_trait]
pub trait DatabaseConfigurationRepository {
    async fn get_configuration<T: for<'de> Deserialize<'de> + Serialize + Clone>(
        &self,
        key: &str,
    ) -> anyhow::Result<Option<T>> {
        let result = self.get_raw_configuration::<T>(key).await?;
        Ok(result.map(|v| v.into_value()))
    }
    async fn get_raw_configuration<T: for<'de> Deserialize<'de> + Serialize + Clone>(
        &self,
        key: &str,
    ) -> anyhow::Result<Option<Configuration<T>>>;
}

#[async_trait]
impl DatabaseConfigurationRepository for Database {
    async fn get_raw_configuration<T: for<'de> Deserialize<'de> + Serialize + Clone>(
        &self,
        key: &str,
    ) -> anyhow::Result<Option<Configuration<T>>> {
        let row: Option<Configuration<T>> = sqlx::query_as::<_, Configuration<T>>(
            "SELECT key, value FROM configurations WHERE key = LOWER($1)",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }
}

#[async_trait]
pub trait DatabaseConfigurationModifyRepository {
    async fn set_configuration<T: for<'de> Deserialize<'de> + Serialize + Clone + Send>(
        &self,
        key: impl Into<String> + Send,
        value: T,
    ) -> anyhow::Result<()> {
        let config = Configuration::new(key.into(), value);
        self.set_raw_configuration(&config).await
    }
    async fn set_raw_configuration<T: for<'de> Deserialize<'de> + Serialize + Clone>(
        &self,
        config: &Configuration<T>,
    ) -> anyhow::Result<()>;
}

#[async_trait]
impl DatabaseConfigurationModifyRepository for Database {
    async fn set_raw_configuration<T: for<'de> Deserialize<'de> + Serialize + Clone>(
        &self,
        config: &Configuration<T>,
    ) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO configurations (key, value) VALUES (LOWER($1), $2) ON CONFLICT (key) DO UPDATE SET value = $2")
            .bind(config.key())
            .bind(Json(config.get_helper_value()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
