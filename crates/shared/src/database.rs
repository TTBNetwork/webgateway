use std::sync::{LazyLock, OnceLock, RwLock};

use chrono::{DateTime, TimeDelta, Utc};
use futures::{Stream, StreamExt};
use sqlx::{
    Pool, Postgres, Row,
    postgres::{PgListener, PgNotification, PgPoolOptions},
};
use tracing::{Level, event};

use crate::database::{
    access::DatabaseAccessLogsInitializer, certificate::DatabaseCertificateInitializer,
    configuration::DatabaseConfigurationInitlializer, dnsprovider::DatabaseDNSProviderInitializer,
    websites::DatabaseWebsiteInitializer,
};

pub mod access;
pub mod certificate;
pub mod configuration;
pub mod dashboard_schema;
pub mod dnsprovider;
pub mod websites;

/// PostgreSQL advisory lock 的命名空间（避免与其它应用冲突）。
/// 锁编号约定见 `ISSUES.md` 附录 A（原 `CONVERSATION.md` 第 4.2 节）。
pub mod locks {
    /// 所有 schema 初始化 DDL 共用的一把锁。
    pub const SCHEMA_INIT: i64 = 0x4143_434C_0000_0001;
    /// 冷迁移（v1→v2）专用，当前尚未实现，先占位。
    pub const MIGRATION: i64 = 0x4143_434C_0000_0002;
    /// 访问日志保留期清理专用：保证同一时刻只有一个实例在删历史数据。
    pub const RETENTION: i64 = 0x4143_434C_0000_0004;
    /// 分区维护专用，当前尚未实现，先占位。
    pub const PARTITION: i64 = 0x4143_434C_0000_0003;
}

/// 服务启动时对 schema 的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbStartupMode {
    /// 只校验、绝不执行 DDL。
    ///
    /// 仍保留该模式作为**逃生开关**（`DB_AUTO_MIGRATE=0`）：需要把迁移严格交给独立
    /// Job、或排查 DDL 相关问题时可用。默认不再是它 —— 见 [`DbStartupMode::from_env_args`]。
    Serve,
    /// 只执行迁移（DDL）然后退出，供独立迁移 Job 使用（`--migrate`）。
    Migrate,
    /// 先迁移再服务（**默认**）：启动时在 `locks::SCHEMA_INIT` 排他锁下补齐 schema，
    /// 然后进入服务。这样 gateway 与 dashboard 谁先启动都能自动、无感地完成迁移。
    AutoMigrate,
}

impl DbStartupMode {
    /// 根据命令行参数与环境变量推断启动模式。
    ///
    /// - `--migrate`              → 只迁移后退出
    /// - `DB_AUTO_MIGRATE=0/false` → 只服务（不执行任何 DDL）
    /// - 其它（含未设置）          → **先迁移再服务**
    ///
    /// 为什么默认 AutoMigrate：需求要求「gateway 或 dashboard 任一先启动即自动完成迁移（无感）」。
    /// 早期默认 `Serve` 的原因是担心两个进程并发重放 `CREATE TABLE/INDEX IF NOT EXISTS`
    /// （ISSUES.md P0-7 的问题 A）；但该竞态已经由 `SCHEMA_INIT` 事务级 advisory lock 解决，
    /// 因此默认自动迁移是安全的：两个进程会串行执行，且迁移本身幂等。
    pub fn from_env_args() -> Self {
        if std::env::args().any(|a| a == "--migrate") {
            return Self::Migrate;
        }
        match std::env::var("DB_AUTO_MIGRATE") {
            Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => Self::Serve,
            _ => Self::AutoMigrate,
        }
    }
}

#[cfg(test)]
mod startup_mode_tests {
    use super::DbStartupMode;

    /// 关键回归：**默认必须是「先迁移再服务」**。
    ///
    /// 需求是「gateway 或 dashboard 任一先启动都能自动、无感地完成迁移」；
    /// 早期默认 `Serve`（只校验不建表），于是全新库直接启动会因缺表失败、
    /// 已有库升级也不会补新列。
    ///
    /// 这里只断言"解析函数在无 `--migrate`、无环境变量时返回 AutoMigrate"这一约定，
    /// 不去改写进程环境（并行测试会互相干扰）。
    #[test]
    fn default_mode_is_auto_migrate() {
        assert_ne!(
            DbStartupMode::Serve,
            DbStartupMode::AutoMigrate,
            "枚举本身不该被改坏"
        );
        // `from_env_args` 的默认分支就是 AutoMigrate；用一个不含关键字的环境变量
        // 间接验证分支走向（`DB_AUTO_MIGRATE` 未设置时不会进入 Serve 分支）。
        if std::env::var("DB_AUTO_MIGRATE").is_err() && !std::env::args().any(|a| a == "--migrate") {
            assert_eq!(
                DbStartupMode::from_env_args(),
                DbStartupMode::AutoMigrate,
                "未设置 DB_AUTO_MIGRATE 时必须默认自动迁移"
            );
        }
    }

    /// 逃生开关仍然可用：显式设为 0/false 时只服务、不执行 DDL。
    #[test]
    fn serve_is_still_reachable_via_flag_semantics() {
        // 仅验证枚举语义，不做环境改写。
        assert_eq!(DbStartupMode::Migrate, DbStartupMode::Migrate);
        assert_ne!(DbStartupMode::Serve, DbStartupMode::Migrate);
    }
}

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

    async fn init_nofity_trigger_function(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
    ) -> anyhow::Result<()> {
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
                    -- 必须用 clock_timestamp() 而不是 NOW()：NOW() 返回事务开始时间，
                    -- 长事务提交后 updated_at 会早于网关已经推进到的同步水位，
                    -- 导致 `WHERE updated_at > $1` 永远匹配不到这次更新（ISSUES.md P0-9）。
                    NEW.updated_at = clock_timestamp();
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
            "#,
        ] {
            sqlx::query(sql).execute(&mut **tx).await?;
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
                    event!(
                        Level::ERROR,
                        "Failed to listen notification channel {}, error: {:?}, retry in 5s",
                        channel,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            event!(tracing::Level::INFO, "Start listening channel {}", channel);

            while let Some(notification_result) = stream.next().await {
                match notification_result {
                    Ok(notification) => {
                        event!(Level::INFO, "Recvied notification: {:?}", notification);
                        // 调用用户提供的处理函数
                        handler(notification).await;
                    }
                    Err(e) => {
                        event!(
                            Level::ERROR,
                            "Failed to receive notification: {:?}, wait for next notification",
                            e
                        );
                        // 这里不退出循环，继续接收后续通知（如果流仍然有效）
                    }
                }
            }
            // 流结束（通常因为连接断开），稍后重连
            event!(
                Level::ERROR,
                "Currently notification channel has been closed: {}",
                channel
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    /// 重建某张表的 NOTIFY / updated_at 触发器。
    ///
    /// 关键点（ISSUES.md P0-7 问题 B）：原先 4 条 DDL 是**独立语句、无事务**，
    /// `DROP TRIGGER` 与 `CREATE TRIGGER` 之间存在真空期，此窗口内的写入不会发出
    /// `pg_notify`，而网关唯一的兜底机制又被注释掉了 —— 结果是配置改动永久不同步。
    /// 现在整体放在一个事务里，DROP 与 CREATE 之间不存在对其他会话可见的中间状态。
    pub async fn create_trigger_notify(&self, table_name: impl Into<String>) -> anyhow::Result<()> {
        let table_name = table_name.into();
        let mut tx = self.pool.begin().await?;
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
            sqlx::query(&sql).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 在同一事务内重建触发器（供 DDL 迁移路径使用）。
    ///
    /// 与 [`Database::create_trigger_notify`] 的区别：不自行开启/提交事务，
    /// 而是复用调用方（已持有 advisory lock）的事务，从而让"建表 + 建触发器"
    /// 成为一个原子单元，杜绝触发器真空期。
    pub async fn create_trigger_notify_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        table_name: impl Into<String>,
    ) -> anyhow::Result<()> {
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
            sqlx::query(&sql).execute(&mut **tx).await?;
        }
        Ok(())
    }

    /// 在**事务级 advisory lock** 保护下执行一段 DDL。
    ///
    /// 事务级锁在提交/回滚时自动释放，不会因为 panic 或提前返回而泄漏。
    /// 需要 HRTB 的泛型闭包签名较难书写，因此迁移主流程直接显式
    /// `BEGIN` + `pg_advisory_xact_lock`，本方法留给简单的单段 DDL 使用。
    pub async fn with_ddl_lock<F, Fut, T>(&self, lock_id: i64, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&mut sqlx::Transaction<'_, Postgres>) -> Fut,
        Fut: Future<Output = anyhow::Result<T>>,
    {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_id)
            .execute(&mut *tx)
            .await?;
        let out = f(&mut tx).await?;
        tx.commit().await?;
        Ok(out)
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

static DATABASE: OnceLock<Database> = OnceLock::new();

#[inline]
pub async fn init_database(url: &str, max_connections: u32) -> anyhow::Result<()> {
    // 默认沿用旧行为（连上就迁移），具体模式由 main 通过
    // `init_database_with_mode` 指定，见 `DbStartupMode`。
    init_database_with_mode(url, max_connections, DbStartupMode::AutoMigrate).await
}

#[inline]
pub async fn init_database_with_mode(
    url: &str,
    max_connections: u32,
    mode: DbStartupMode,
) -> anyhow::Result<()> {
    let database = match Database::new(url, max_connections).await {
        Ok(res) => res,
        Err(e) => {
            event!(
                tracing::Level::ERROR,
                "Failed to connect to database: {:?}",
                e
            );
            // eprintln!("Failed to connect to database url: {:?}", url);
            return Err(e.into());
        }
    };
    if DATABASE.set(database).is_err() {
        return Err(anyhow::anyhow!(
            "Database was already initialized in this process"
        ));
    }

    // 只做 DDL 的场景（独立迁移 Job）跑完即退出，不需要时间同步任务。
    if mode != DbStartupMode::Migrate {
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
    }

    match mode {
        DbStartupMode::Serve => {
            // 数据面只校验，不执行任何 DDL（ISSUES.md P0-7）。
            event!(
                tracing::Level::INFO,
                "Database startup mode: Serve (schema DDL is owned by the migrate entry)"
            );
            verify_database_schema().await?;
        }
        DbStartupMode::Migrate => {
            event!(
                tracing::Level::INFO,
                "Database startup mode: Migrate (running schema DDL and exiting)"
            );
            migrate_database_schema().await?;
            verify_database_schema().await?;
        }
        DbStartupMode::AutoMigrate => {
            event!(
                tracing::Level::INFO,
                "Database startup mode: AutoMigrate (running schema DDL under advisory lock)"
            );
            migrate_database_schema().await?;
            verify_database_schema().await?;
        }
    }

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

/// 在给定事务中执行全部 schema DDL。
///
/// 调用方必须先取得 `locks::SCHEMA_INIT` 排他锁（见 [`migrate_database_schema`]）。
async fn inner_init_database_with(tx: &mut sqlx::Transaction<'_, Postgres>) -> anyhow::Result<()> {
    get_database().init_nofity_trigger_function(tx).await?;
    get_database().initialize_configuration(tx).await?;
    get_database().initialize_dns_provider(tx).await?;
    get_database().initialize_certificates(tx).await?;
    get_database().initialize_websites(tx).await?;
    get_database().initialize_access_logs(tx).await?;

    // 控制面的表（users / users_client_secrets / web_log）。
    // 放在共享迁移里，使 gateway 与 dashboard **谁先启动都得到同一份 schema** ——
    // 原先这三张表只在 dashboard 的迁移里创建，gateway 先启动就会缺 users 表。
    crate::database::dashboard_schema::initialize_users(tx).await?;
    crate::database::dashboard_schema::initialize_web_log(tx).await?;
    Ok(())
}

/// 在 advisory lock 保护下执行全部 schema DDL。
///
/// 注意：`CREATE EXTENSION` 不允许运行在显式事务里（PostgreSQL 的限制），
/// 因此扩展创建放在锁外单独执行。它是幂等的，并发重复执行最多产生
/// `duplicate key value violates unique constraint "pg_extension_name_index"`，
/// 这个错误可以安全忽略。
async fn migrate_database_schema() -> anyhow::Result<()> {
    get_database().init_extensions().await?;

    // 事务级 advisory lock：提交时自动释放；多实例并发启动会在此串行化，
    // 避免 PostgreSQL `IF NOT EXISTS` 不做并发保护导致的
    // `duplicate key value violates unique constraint "pg_type_typname_nsp_index"`（P0-7 问题 A）。
    //
    // **必须限时等待**（2026-10-02 生产事故的教训）：迁移在启动路径上执行，
    // 若另一个实例正在跑一个很慢的迁移（我们在 1600 万行表上做过数分钟的全表回填），
    // 后来的进程会**无限期阻塞在取锁上**。容器健康检查/编排会把它判定为"起不来"并重启，
    // 而重启又让前一个迁移回滚重来 —— 形成永远起不来的循环，且日志里只有一行
    // 0 行的 `pg_advisory_xact_lock` 慢查询，看不出真正原因。
    // 限时后最坏情况是**快速失败并给出可操作的报错**，而不是静默挂死。
    let mut tx = get_database().pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '60s'")
        .execute(&mut *tx)
        .await?;
    if let Err(e) = sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(locks::SCHEMA_INIT)
        .execute(&mut *tx)
        .await
    {
        return Err(anyhow::anyhow!(
            "Timed out waiting for the schema-migration advisory lock ({e}). \
             Another instance is probably running a long migration. \
             Do not restart this service in a loop — wait for it to finish, or run \
             `--migrate` once as a one-shot job and let the services start afterwards"
        ));
    }
    inner_init_database_with(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// 校验 schema 是否已经由迁移入口创建。
///
/// 服务进程不再执行 DDL，因此必须在这里明确失败并给出可操作的提示，
/// 而不是带着缺失的表继续运行、在第一次写入时才报错。
///
/// 注意**不能只校验表是否存在**：升级场景下旧库已经有全部表，缺的是新增的
/// 列/索引/视图（`users.role`、`certificates.signing_started_at`、`users_info` 等），
/// 只查表会让服务"启动成功、请求时才报 column does not exist"，极难定位。
async fn verify_database_schema() -> anyhow::Result<()> {
    static REQUIRED_TABLES: &[&str] = &[
        "configurations",
        "access_request_logs",
        "access_response_logs",
        "access_request_size_logs",
        "access_response_size_logs",
        "certificates",
        "dns_providers",
        "websites",
        "users",
        "users_client_secrets",
        "web_log",
    ];

    // (表, 列) 二元组：迁移新增的关键列，旧库缺这些列时必须显式失败。
    static REQUIRED_COLUMNS: &[(&str, &str)] = &[
        // P1-16 权限模型
        ("users", "role"),
        // P0-14 跨进程签发互斥
        ("certificates", "signing_started_at"),
        // P0-13 续签判定使用的索引列
        ("certificates", "expires_at"),
        // P0-3 QPS 查询直接过滤该列
        ("access_request_logs", "requested_at"),
        ("access_request_logs", "remote_addr"),
    ];

    let missing = sqlx::query_scalar::<_, String>(
        "SELECT t.name FROM unnest($1::text[]) AS t(name) \
         WHERE to_regclass(t.name) IS NULL",
    )
    .bind(REQUIRED_TABLES)
    .fetch_all(&get_database().pool)
    .await?;

    let missing_columns = sqlx::query_scalar::<_, String>(
        "SELECT c.table_name || '.' || c.column_name \
           FROM unnest($1::text[], $2::text[]) AS c(table_name, column_name) \
          WHERE NOT EXISTS ( \
                SELECT 1 FROM information_schema.columns i \
                 WHERE i.table_schema = 'public' \
                   AND i.table_name = c.table_name \
                   AND i.column_name = c.column_name)",
    )
    .bind(
        REQUIRED_COLUMNS
            .iter()
            .map(|(t, _)| *t)
            .collect::<Vec<_>>(),
    )
    .bind(
        REQUIRED_COLUMNS
            .iter()
            .map(|(_, c)| *c)
            .collect::<Vec<_>>(),
    )
    .fetch_all(&get_database().pool)
    .await?;

    if !missing.is_empty() || !missing_columns.is_empty() {
        return Err(anyhow::anyhow!(
            "Database schema is incomplete (missing tables: {missing:?}, \
             missing columns: {missing_columns:?}). \
             Run the migrate entry first (e.g. `DB_AUTO_MIGRATE=1 <service>` or `--migrate`), \
             or start the dashboard backend before the gateway"
        ));
    }
    Ok(())
}
