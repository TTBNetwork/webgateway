use std::sync::{LazyLock, OnceLock, RwLock};

use chrono::{DateTime, TimeDelta, Utc};
use futures::{Stream, StreamExt};
use sqlx::{
    Pool, Postgres, Row,
    postgres::{PgListener, PgNotification, PgPoolOptions},
};
use tracing::event;

use crate::database::{
    access::DatabaseAccessLogsInitializer, certificate::DatabaseCertificateInitializer,
    dnsprovider::DatabaseDNSProviderInitializer, websites::DatabaseWebsiteInitializer,
};

pub mod access;
pub mod certificate;
pub mod configuration;
pub mod dnsprovider;
pub mod websites;

static PG_EXTENSION: &[&str; 2] = &["uint128", "btree_gin"];
static DATABASE_OFFSET_TIME: LazyLock<RwLock<TimeDelta>> =
    LazyLock::new(|| RwLock::new(TimeDelta::zero()));

#[derive(Debug)]
pub struct Database {
    pub pool: Pool<Postgres>,
    url: String,
}

impl Database {
    pub async fn new(url: &str, max_connections: u32) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await?;

        Ok(Self {
            pool,
            url: url.to_string(),
        })
    }

    async fn init_extensions(&self) -> anyhow::Result<()> {
        for ext in PG_EXTENSION {
            sqlx::query(&format!("CREATE EXTENSION IF NOT EXISTS {}", ext))
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    async fn init_nofity_trigger_function(&self) -> anyhow::Result<()> {
        for sql in [
            r#"CREATE OR REPLACE FUNCTION notify_change()
                RETURNS TRIGGER AS $$
                BEGIN
                    PERFORM pg_notify(
                        TG_TABLE_NAME || '_updater',
                        json_build_object(
                            'id', NEW.id,
                            'updated_at', NEW.updated_at
                        )::text
                    );
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
            "#,
            r#"CREATE OR REPLACE FUNCTION update_updated_at()
                RETURNS TRIGGER AS $$
                BEGIN
                    NEW.updated_at = NOW();
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
            "#,
        ] {
            sqlx::query(sql).execute(&self.pool).await?;
        }
        Ok(())
    }

    pub async fn listen(
        &self,
        channel: impl Into<String>,
    ) -> anyhow::Result<impl Stream<Item = Result<sqlx::postgres::PgNotification, sqlx::Error>>>
    {
        let mut listener = PgListener::connect(&self.url).await?;
        listener
            .listen(&format!("{}_updater", channel.into().as_str()))
            .await?;
        Ok(listener.into_stream())
    }

    pub async fn listen_service_fn<H, Fut>(
        &self,
        channel: impl Into<String>,
        mut handler: H,
    ) -> anyhow::Result<()>
    where
        H: FnMut(PgNotification) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let channel = channel.into();
        // 由于监听需要独立连接，建议在循环内重新连接以应对断开情况
        loop {
            let listener_result = self.listen(&channel).await;
            let mut stream = match listener_result {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("监听频道 {} 失败: {:?}，5秒后重试", channel, e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            event!(tracing::Level::INFO, "Start listening channel {}", channel);

            while let Some(notification_result) = stream.next().await {
                match notification_result {
                    Ok(notification) => {
                        println!("收到通知: {:?}", notification);
                        // 调用用户提供的处理函数
                        handler(notification).await;
                    }
                    Err(e) => {
                        eprintln!("接收通知时出错: {:?}，继续等待下一个", e);
                        // 这里不退出循环，继续接收后续通知（如果流仍然有效）
                    }
                }
            }
            // 流结束（通常因为连接断开），稍后重连
            eprintln!("通知流已结束，重新连接频道 {}", channel);
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    pub async fn create_trigger_notify(&self, table_name: impl Into<String>) -> anyhow::Result<()> {
        let table_name = table_name.into();
        for sql in [
            format!("DROP TRIGGER IF EXISTS {table_name}_notify ON {table_name};"),
            format!(
                r#"CREATE TRIGGER {table_name}_notify
                AFTER INSERT OR UPDATE ON {table_name}
                FOR EACH ROW
                EXECUTE FUNCTION notify_change();
            "#
            ),
            format!(r"DROP TRIGGER IF EXISTS {table_name}_updated_at ON {table_name};"),
            format!(
                r#"CREATE TRIGGER {table_name}_updated_at
                BEFORE UPDATE ON {table_name}
                FOR EACH ROW
                EXECUTE FUNCTION update_updated_at();
            "#
            ),
        ] {
            sqlx::query(&sql).execute(&self.pool).await?;
        }
        Ok(())
    }

    #[inline]
    pub fn get_database_time(&self) -> anyhow::Result<DateTime<Utc>> {
        Ok(Utc::now()
            + *(DATABASE_OFFSET_TIME
                .read()
                .map_err(|e| anyhow::anyhow!(format!("{e}")))?))
    }

    #[inline]
    pub async fn get_real_database_time(&self) -> anyhow::Result<DateTime<Utc>> {
        let r = sqlx::query("SELECT NOW() as current_time;")
            .fetch_one(&self.pool)
            .await?;
        Ok(r.try_get::<DateTime<Utc>, _>("current_time")?)
    }
}

pub static DATABASE: OnceLock<Database> = OnceLock::new();

#[inline]
pub async fn init_database(url: &str, max_connections: u32) -> anyhow::Result<()> {
    let database = match Database::new(url, max_connections).await {
        Ok(res) => res,
        Err(e) => {
            event!(
                tracing::Level::ERROR,
                "Failed to connect to database: {:?}",
                e
            );
            eprintln!("Failed to connect to database url: {:?}", url);
            return Err(e.into());
        }
    };
    DATABASE.set(database).unwrap();

    tokio::spawn(async move {
        loop {
            let r = inner_sync_offset_time().await;
            if let Err(e) = r {
                event!(
                    tracing::Level::ERROR,
                    "Failed to sync database time: {:?}",
                    e
                );
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        }
    });

    inner_init_database().await?;

    Ok(())
}

#[inline]
async fn inner_sync_offset_time() -> anyhow::Result<()> {
    let req_current_time = Utc::now();
    let r = sqlx::query("SELECT NOW() as current_time;")
        .fetch_one(&get_database().pool)
        .await?;
    let resp_current_time = Utc::now();
    let db_time = r.try_get::<DateTime<Utc>, _>("current_time")?;
    let offset = { (db_time - req_current_time) + (db_time - resp_current_time) } / 2;
    let mut write = DATABASE_OFFSET_TIME
        .write()
        .map_err(|e| anyhow::anyhow!(format!("{e}")))?;
    if write.is_zero() {
        event!(
            tracing::Level::INFO,
            "Database time offset is zero, set to {:?}",
            offset
        );
    }
    *write = offset;
    event!(
        tracing::Level::DEBUG,
        "Database time offset set to {:?}",
        offset
    );
    event!(tracing::Level::DEBUG, "Sync database time is {}", db_time);
    event!(tracing::Level::DEBUG, "Sync time is {}", resp_current_time);
    Ok(())
}

pub fn get_database() -> &'static Database {
    DATABASE.get().unwrap()
}

async fn inner_init_database() -> anyhow::Result<()> {
    get_database().init_extensions().await?;
    get_database().init_nofity_trigger_function().await?;
    get_database().initialize_dns_provider().await?;
    get_database().initialize_certificates().await?;
    get_database().initialize_websites().await?;
    get_database().initialize_access_logs().await?;
    Ok(())
}
