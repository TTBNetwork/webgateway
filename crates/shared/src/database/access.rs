use async_trait::async_trait;
use std::collections::HashMap;

use crate::{
    database::{Database, get_database},
    models::access::{
        AccessCreateRequest, AccessCreateResponse, AccessInfo, AccessInsertRequestSize,
        AccessInsertResponseSize, AccessUpdateRequestSize, AccessUpdateResponseSize, DatabaseQPS,
        ResponseQPS, TodayMetricsInfoOfWebsite,
    },
    models::retention::AccessLogRetention,
};
use futures::future::BoxFuture;
use simple_shared::objectid::ObjectId;
use sqlx::{Postgres, QueryBuilder, Transaction, types::Json};
use sqlx_pg_ext_uint::{c_u16::U16, c_usize::USize};
use tracing::{Level, event};

/// 单条语句的最大绑定参数个数上限（PostgreSQL 扩展查询协议用 Int16 表示参数个数）。
const PG_MAX_BIND_PARAMS: usize = 65_535;
/// 批量写入的目标分块行数：在语句数与参数数之间取平衡。
const BATCH_ROWS: usize = 1_000;

/// 按每条记录消耗的绑定参数个数计算安全的分块行数。
///
/// 单条 INSERT/UPDATE 的参数总数一旦超过 65535，PostgreSQL 会整体报错；
/// 原先未分块的实现会在流量高峰（也就是最需要日志时）必然失败并丢数据。
const fn chunk_rows(binds_per_row: usize) -> usize {
    let rows = PG_MAX_BIND_PARAMS / binds_per_row;
    if rows < BATCH_ROWS { rows } else { BATCH_ROWS }
}

/// 在调用方提供的（可由 advisory lock 保护的）事务中初始化访问日志相关对象。
///
/// **不再创建 v1 表结构**（2026-10-02 决策）：访问日志只有 v2 一种形态。
/// 表结构的创建在 `access_v2::install_v2_schema`，新库会紧接着
/// `access_v2::activate_fresh_v2` 把逻辑名交给 v2。
pub trait DatabaseAccessLogsInitializer {
    fn initialize_access_logs<'a>(
        &'a self,
        tx: &'a mut Transaction<'_, Postgres>,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
}

impl DatabaseAccessLogsInitializer for Database {
    fn initialize_access_logs<'a>(
        &'a self,
        tx: &'a mut Transaction<'_, Postgres>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            // **这里不再创建 v1 表结构**（2026-10-02 决策）：访问日志只有 v2 一种形态
            // ——按周 RANGE 分区的 `access_v2_*`，切换后接管逻辑名 `access_*`。
            //
            // 原先这里执行 `assets/sqls/access_init.sql`（四张普通表 + 索引）。
            // 在"新库直接是 v2"的目标下那份 DDL 只会造成两种 bad case：
            // * 新库：建出四张空的 v1 表，随后启动路径又把它们改名成 `*_v1`，白建；
            // * 已切换的库：逻辑名已经是 v2 分区表，这份 DDL 会再建出四张空的普通表
            //   （相当于"v3"），既占元数据又容易让后来的查询指错对象。
            //
            // 因此建表职责完全交给 `access_v2::install_v2_schema`（建 `access_v2_*`
            // 分区表 + 分区管理函数），并在其后立刻激活（`activate_fresh_v2`）。

            // 保留期配置的默认值（180 天）。`ON CONFLICT DO NOTHING` 保证
            // 不会覆盖运维已经调整过的值，因此每次迁移重跑都是安全的。
            let config = crate::models::configuration::Configuration::new(
                AccessLogRetention::CONFIG_KEY,
                AccessLogRetention::default(),
            );
            sqlx::query(
                "INSERT INTO configurations (key, value) VALUES (LOWER($1), $2) \
                 ON CONFLICT (key) DO NOTHING",
            )
            .bind(config.key())
            .bind(Json(config.get_helper_value()))
            .execute(&mut **tx)
            .await?;

            // size 明细表的 `at_second` 与唯一键（升级路径）。
            //
            // 背景：这两张表要按秒统计请求/响应体大小（颗粒度到秒），而历史上
            // `StatisticsIncoming` 每读一个 body chunk 就插一行，生产库因此有
            // 1600 万行 / 3.6 GB（请求主表的 7 倍，单条 response_id 最多 262791 行）。
            // 新写入改为「同一 (xxx_id, 秒) 用 ON CONFLICT 累加」，这需要唯一约束。
            //
            // 代价说明：若老库里已存在同一 (xxx_id, 秒) 的重复行，必须先收敛重复行
            // 才能建唯一索引，因此**首次**迁移会较慢（生产库实测约 417 万行需要合并）。
            // 已迁移过的库再跑是几毫秒（NOT EXISTS 判断 + 幂等 DDL）。
            migrate_size_logs_second_granularity(tx).await?;
            Ok(())
        })
    }
}

/// 面板读路径（只读查询）。
///
/// **`get_access_info` / `get_today_metrics_info_of_websites` 走日汇总表**
/// （`access_stats_daily`，见 `access_v2::install_stats_schema` / `refresh_stats_days`）：
/// 把"近 N 天汇总"从"扫百万行 + 关联主表"压到"读几十行"（实测 3.68 秒 → 1.1 毫秒）。
/// QPS 与按 IP 聚合仍是实时查询 —— 它们要的是**秒级**数据，汇总表（按天）给不了。
#[async_trait]
pub trait DatabaseAccessLogsRepository {
    async fn get_qps_per_second(&self, count: usize) -> anyhow::Result<ResponseQPS>;
    async fn get_qps_per_5s(&self, count: usize) -> anyhow::Result<ResponseQPS>;
    async fn get_access_info(&self, in_days: usize) -> anyhow::Result<AccessInfo>;
    async fn get_today_metrics_info_of_websites(
        &self,
    ) -> anyhow::Result<Vec<TodayMetricsInfoOfWebsite>>;
    async fn get_requests_of_ips(&self, in_days: usize) -> anyhow::Result<HashMap<String, usize>>;
}

#[async_trait]
impl DatabaseAccessLogsRepository for Database {
    async fn get_qps_per_second(&self, count: usize) -> anyhow::Result<ResponseQPS> {
        let max_limit = count;
        // 直接在 requested_at 上过滤（sargable），使分区的 requested_at 索引可用。
        // 旧写法 `WHERE time >= ...` 中的 time 是 date_trunc(...) 的别名，
        // 无法走索引，每次都退化为全表聚合 + 排序（见 ISSUES.md P0-3）。
        let rows = sqlx::query_as::<_, DatabaseQPS>(
            "SELECT date_trunc('second', requested_at) AS time, COUNT(id) AS total_requests \
             FROM access_request_logs \
             WHERE requested_at >= NOW() - INTERVAL '1 second' * $1 \
             GROUP BY 1 ORDER BY 1 DESC LIMIT $1",
        )
        .bind(max_limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(ResponseQPS {
            interval: 1,
            data: rows,
            current_time: self.get_database_time()?,
        })
    }
    async fn get_qps_per_5s(&self, count: usize) -> anyhow::Result<ResponseQPS> {
        let max_limit = count * 5;
        // 同上：谓词落在 requested_at 原始列上。
        let rows = sqlx::query_as::<_, DatabaseQPS>(
            "SELECT to_timestamp(floor(extract(epoch FROM requested_at) / 5) * 5) AS time, \
                    COUNT(id) AS total_requests \
             FROM access_request_logs \
             WHERE requested_at >= NOW() - INTERVAL '1 second' * $1 \
             GROUP BY 1 ORDER BY 1 DESC LIMIT $1",
        )
        .bind(max_limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(ResponseQPS {
            interval: 5,
            data: rows,
            current_time: self.get_database_time()?,
        })
    }
    async fn get_access_info(&self, in_days: usize) -> anyhow::Result<AccessInfo> {
        // 走**日汇总表**（`access_stats_daily`）：把"近 N 天汇总"从"扫百万行 +
        // 关联主表"变成"读几十行"。
        //
        // 背景（实测）：原来每次都在扫原始日志，30 天窗口 3.7 秒，其中最贵的一段是
        // 为了拿 `website_id` 而把 size 明细关联回主表（原始形态 **44 秒**）。
        // 汇总表按 `(day, website_id)` 预计算，面板只需 SUM 几十行。
        //
        // 口径：汇总只统计 `website_id` 非空的日志；`total_ips` 是**每日独立 IP 求和**
        // （同一 IP 跨天会重复计入），与"整窗口精确去重"不同 —— 汇总表的意义是
        // 面板的快速概览，精确去重仍可用 `get_requests_of_ips` 那条实时路径。
        let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, USize, USize)>(
            r#"
            SELECT
                COALESCE(SUM(total_requests), 0)::bigint,
                COALESCE(SUM(total_ips), 0)::bigint,
                COALESCE(SUM(e4xx_requests), 0)::bigint,
                COALESCE(SUM(e5xx_requests), 0)::bigint,
                COALESCE(SUM(backend_error_requests), 0)::bigint,
                COALESCE(SUM(total_requests_size), 0)::uint8,
                COALESCE(SUM(total_response_size), 0)::uint8
              FROM access_stats_daily
             WHERE day > CURRENT_DATE - ($1::int)
        "#,
        )
        .bind(in_days as i64)
        .fetch_one(&self.pool)
        .await?;

        Ok(AccessInfo {
            total_requests: row.0 as usize,
            total_ips: row.1 as usize,
            e4xx_requests: row.2 as usize,
            e5xx_requests: row.3 as usize,
            backend_error_requests: row.4 as usize,
            total_request_size: row.5.into(),
            total_response_size: row.6.into(),
        })
    }

    async fn get_requests_of_ips(&self, in_days: usize) -> anyhow::Result<HashMap<String, usize>> {
        // 这是"按 IP 聚合"的实时路径（访问地图用）。它**不**走日汇总表：
        // 汇总表的粒度是 (日, 站点)，无法还原 IP 明细。
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT remote_addr, COUNT(id) FROM access_request_logs
             WHERE requested_at > NOW() - INTERVAL '1 day' * $1
             GROUP BY remote_addr",
        )
        .bind(in_days as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(addr, count)| (addr, count as usize))
            .collect())
    }

    /// 今日按站点的指标：走日汇总表 —— 站点卡片"今日"就是汇总表里
    /// `day = CURRENT_DATE` 的那些行（原来是实时聚合三张表 + 关联，每张卡片都要等它）。
    async fn get_today_metrics_info_of_websites(
        &self,
    ) -> anyhow::Result<Vec<TodayMetricsInfoOfWebsite>> {
        let rows = sqlx::query_as::<_, TodayMetricsInfoOfWebsite>(
            r#"
            SELECT website_id,
                   total_requests::bigint         AS total_requests,
                   total_responses::bigint        AS total_responses,
                   total_ips::bigint              AS total_ips,
                   backend_error_requests::bigint AS backend_error_requests,
                   e4xx_requests::bigint          AS e4xx_requests,
                   e5xx_requests::bigint          AS e5xx_requests,
                   total_requests_size            AS total_requests_size,
                   total_response_size            AS total_response_size
              FROM access_stats_daily
             WHERE day = CURRENT_DATE
               AND website_id <> '__unknown__'
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

#[async_trait]
pub trait DatabaseAccessLogsModifyRepository {
    async fn insert_batch_access_requests(
        &self,
        requests: Vec<AccessCreateRequest>,
    ) -> anyhow::Result<()>;
    async fn insert_batch_access_responses(
        &self,
        responses: Vec<AccessCreateResponse>,
    ) -> anyhow::Result<()>;
    async fn update_batch_access_request_size_logs(
        &self,
        requests: Vec<AccessUpdateRequestSize>,
    ) -> anyhow::Result<()>;
    async fn update_batch_access_response_size_logs(
        &self,
        responses: Vec<AccessUpdateResponseSize>,
    ) -> anyhow::Result<()>;
    async fn insert_batch_access_response_increase_size_logs(
        &self,
        responses: Vec<AccessInsertResponseSize>,
    ) -> anyhow::Result<()>;
    async fn insert_batch_access_request_increase_size_logs(
        &self,
        requests: Vec<AccessInsertRequestSize>,
    ) -> anyhow::Result<()>;

    /// 删除早于 `retention_days` 天的访问日志，最多删除 `max_rows` 行，返回实际删除数。
    ///
    /// 单轮限量是为了用**小事务**推进清理：一次性 `DELETE` 上千万行会长时间持锁、
    /// 撑爆 WAL，还会把删除的元组堆在表尾让表**更大**（直到 vacuum 回收）。
    async fn delete_access_logs_before(
        &self,
        retention_days: u32,
        max_rows: i64,
    ) -> anyhow::Result<u64>;
}

#[async_trait]
impl DatabaseAccessLogsModifyRepository for Database {
    async fn insert_batch_access_requests(
        &self,
        requests: Vec<AccessCreateRequest>,
    ) -> anyhow::Result<()> {
        if requests.is_empty() {
            return Ok(());
        }
        // 10 个绑定/行。
        for chunk in requests.chunks(chunk_rows(10)) {
            let mut builder = QueryBuilder::new(
                "INSERT INTO access_request_logs (id, host, method, path, headers, http_version, remote_addr, body_length, requested_at, website_id)",
            );
            builder.push_values(chunk.iter(), |mut b, req| {
                b.push_bind(req.id)
                    .push_bind(&req.host)
                    .push_bind(&req.method)
                    .push_bind(&req.path)
                    .push_bind(Json(&req.headers))
                    .push_bind(req.http_version.to_string())
                    .push_bind(&req.remote_addr)
                    .push_bind(USize::from(req.body_length))
                    .push_bind(req.requested_at)
                    .push_bind(req.website_id);
            });
            // 幂等写入：刷盘是「批量取出 → 写库 → 成功才清空内存」，写库失败时整批
            // 留在内存下一轮重试（gateway/src/access.rs 的 PendingBuffer）。而这里的
            // 分块执行**不是**原子的：前几块提交成功、最后一块失败（连接池超时、
            // PG 重启、语句超时都会）时，下一轮重试会把已提交的行再写一次，
            // 命中主键冲突 —— 没有 ON CONFLICT 的话该批次会**永远**失败，
            // 刷盘队列被永久毒化：新日志只进不出，内存无限增长直至 OOM。
            //
            // **冲突目标故意省略**（不写 `(id)`）：v2 是分区表，主键是
            // `(id, requested_at)` —— 分区表的唯一索引必须包含分区键，
            // 因此 `ON CONFLICT (id)` 在 v2 上推断不出索引，切换之后刷盘会整批失败。
            // 不带目标时 v1（`PK(id)`）与 v2（`PK(id, requested_at)`）都成立。
            builder.push(" ON CONFLICT DO NOTHING");
            builder.build().execute(&self.pool).await?;
        }
        Ok(())
    }
    async fn insert_batch_access_responses(
        &self,
        responses: Vec<AccessCreateResponse>,
    ) -> anyhow::Result<()> {
        if responses.is_empty() {
            return Ok(());
        }
        // 8 个绑定/行。
        for chunk in responses.chunks(chunk_rows(8)) {
            let mut builder = QueryBuilder::new(
                "INSERT INTO access_response_logs (id, status, headers, body_length, http_version, backend_responsed_at, responsed_at, website_id)",
            );
            builder.push_values(chunk.iter(), |mut b, resp| {
                b.push_bind(resp.id)
                    .push_bind(U16::from(resp.status))
                    .push_bind(Json(&resp.headers))
                    .push_bind(USize::from(resp.body_length))
                    .push_bind(resp.http_version.to_string())
                    .push_bind(resp.backend_responsed_at)
                    .push_bind(resp.responsed_at)
                    .push_bind(resp.website_id);
            });
            // 与请求日志同理：必须幂等，否则部分成功的批次会在重试时永久失败。
            // 冲突目标同样省略，以同时适配 v1 的 `PK(id)` 与 v2 的分区主键
            // `(id, responsed_at)`（原因见上）。
            builder.push(" ON CONFLICT DO NOTHING");
            builder.build().execute(&self.pool).await?;
        }
        Ok(())
    }

    async fn update_batch_access_request_size_logs(
        &self,
        requests: Vec<AccessUpdateRequestSize>,
    ) -> anyhow::Result<()> {
        if requests.is_empty() {
            return Ok(());
        }
        // 3 个绑定/行：`SET (id, body_length) = (VALUES ...)` 比原来的
        // `CASE WHEN id = ? THEN ? ...` 少一半绑定参数，且避免重复绑定 id。
        for chunk in requests.chunks(chunk_rows(3)) {
            let mut builder = QueryBuilder::new(
                "UPDATE access_request_logs AS ar SET body_length = v.body_length \
                 FROM (VALUES ",
            );
            {
                // 缩小 `separated` 的作用域：它借用 `builder`，出块后自动释放。
                let mut separated = builder.separated(", ");
                for req in chunk {
                    separated.push("(");
                    separated.push_bind_unseparated(req.id);
                    separated.push_unseparated(", ");
                    separated.push_bind_unseparated(USize::from(req.body_length));
                    separated.push_unseparated(")");
                }
                separated.push_unseparated(") AS v(id, body_length) WHERE ar.id = v.id");
            }
            builder.build().execute(&self.pool).await?;
        }
        Ok(())
    }
    async fn update_batch_access_response_size_logs(
        &self,
        responses: Vec<AccessUpdateResponseSize>,
    ) -> anyhow::Result<()> {
        if responses.is_empty() {
            return Ok(());
        }
        for chunk in responses.chunks(chunk_rows(3)) {
            let mut builder = QueryBuilder::new(
                "UPDATE access_response_logs AS ar SET body_length = v.body_length \
                 FROM (VALUES ",
            );
            {
                let mut separated = builder.separated(", ");
                for resp in chunk {
                    separated.push("(");
                    separated.push_bind_unseparated(resp.id);
                    separated.push_unseparated(", ");
                    separated.push_bind_unseparated(USize::from(resp.body_length));
                    separated.push_unseparated(")");
                }
                separated.push_unseparated(") AS v(id, body_length) WHERE ar.id = v.id");
            }
            builder.build().execute(&self.pool).await?;
        }
        Ok(())
    }

    async fn insert_batch_access_response_increase_size_logs(
        &self,
        responses: Vec<AccessInsertResponseSize>,
    ) -> anyhow::Result<()> {
        if responses.is_empty() {
            return Ok(());
        }
        accumulate_size_rows(
            &self.pool,
            "access_response_size_logs",
            "access_response_logs",
            "response_id",
            responses
                .into_iter()
                .map(|r| (r.id, r.body_length, r.at_second, r.created_at))
                .collect(),
        )
        .await
    }

    async fn insert_batch_access_request_increase_size_logs(
        &self,
        requests: Vec<AccessInsertRequestSize>,
    ) -> anyhow::Result<()> {
        if requests.is_empty() {
            return Ok(());
        }
        accumulate_size_rows(
            &self.pool,
            "access_request_size_logs",
            "access_request_logs",
            "request_id",
            requests
                .into_iter()
                .map(|r| (r.id, r.body_length, r.at_second, r.created_at))
                .collect(),
        )
        .await
    }

    async fn delete_access_logs_before(
        &self,
        retention_days: u32,
        max_rows: i64,
    ) -> anyhow::Result<u64> {
        let mut conn = self.pool.acquire().await?;
        delete_access_logs_before_on_impl(&mut conn, retention_days, max_rows).await
    }

}

// ==================== 访问日志保留期 ====================

/// 单轮删除的行数上限。小批量推进，避免长事务持锁、WAL 暴涨。
const PRUNE_ROUND_ROWS: i64 = 50_000;
/// 单次调用最多推进多少轮，防止在一次 tick 里无限循环占住连接。
const PRUNE_MAX_ROUNDS: usize = 20;

/// `access_log_retention` 的默认值（180 天），在迁移时写入 `configurations` 表。
///
/// 使用 `ON CONFLICT DO NOTHING`，因此**不会**覆盖运维已经改过的值。
pub async fn ensure_default_retention_config() -> anyhow::Result<()> {
    let config = crate::models::configuration::Configuration::new(
        AccessLogRetention::CONFIG_KEY,
        AccessLogRetention::default(),
    );
    sqlx::query(
        "INSERT INTO configurations (key, value) VALUES (LOWER($1), $2) ON CONFLICT (key) DO NOTHING",
    )
    .bind(config.key())
    .bind(Json(config.get_helper_value()))
    .execute(&get_database().pool)
    .await?;
    Ok(())
}

/// 读取保留期配置；未配置或值非法时回落到默认值（180 天），并夹紧到 [90, 3650]。
pub async fn get_retention_config() -> anyhow::Result<AccessLogRetention> {
    use crate::database::configuration::DatabaseConfigurationRepository;
    let stored: Option<AccessLogRetention> = get_database()
        .get_configuration(AccessLogRetention::CONFIG_KEY)
        .await
        .unwrap_or_else(|e| {
            event!(Level::WARN, "Failed to read retention config, using default: {e}");
            None
        });
    Ok(match stored {
        Some(cfg) => AccessLogRetention::sanitized(cfg.retention_days, cfg.enabled),
        None => AccessLogRetention::default(),
    })
}

/// 写入保留期配置（先夹紧到合法区间再落库）。
pub async fn set_retention_config(config: &AccessLogRetention) -> anyhow::Result<AccessLogRetention> {
    use crate::database::configuration::DatabaseConfigurationModifyRepository;
    let sanitized = AccessLogRetention::sanitized(config.retention_days, config.enabled);
    get_database()
        .set_configuration(AccessLogRetention::CONFIG_KEY, sanitized.clone())
        .await?;
    Ok(sanitized)
}

/// 按保留期清理历史访问日志，返回本次实际删除的行数。
///
/// 用 `pg_try_advisory_lock` 保证同一时刻只有一个实例在删（拿不到锁直接返回 0，
/// 不是错误 —— 另一个实例正在做同样的事）。
///
/// **注意：必须全程占用同一条连接。** `pg_try_advisory_lock` 是**会话级**锁，
/// 如果像早期实现那样在池上 `fetch_one` 取锁、再在池上 `execute` 解锁，两次
/// 很可能落在不同连接上 —— 解锁语句释放的是"那条连接自己的锁"（它根本没持有），
/// 而真正持有的锁要等那条连接被复用/关闭才释放，于是后续每一轮都拿不到锁，
/// 清理**静默停止**。这里显式 `acquire` 一条连接并在 `finally` 语义下解锁。
pub async fn prune_access_logs() -> anyhow::Result<u64> {
    prune_access_logs_with(PRUNE_ROUND_ROWS, PRUNE_MAX_ROUNDS).await
}

/// 可调参数版本，便于测试与按需小批量清理。
pub async fn prune_access_logs_with(round_rows: i64, max_rounds: usize) -> anyhow::Result<u64> {
    let config = get_retention_config().await?;
    if !config.should_prune() {
        event!(
            Level::DEBUG,
            "Access log retention disabled or set to keep everything (days = {}, enabled = {})",
            config.retention_days,
            config.enabled
        );
        return Ok(0);
    }

    let db = get_database();
    // 独占一条连接：取锁、删数据、解锁都在这条连接上完成。
    let mut conn = db.pool.acquire().await?;

    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(crate::database::locks::RETENTION)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        event!(
            Level::DEBUG,
            "Another instance is already pruning access logs, skipping this round"
        );
        return Ok(0);
    }

    let mut total = 0u64;
    let mut first_err: Option<anyhow::Error> = None;
    for round in 0..max_rounds {
        match delete_access_logs_before_on_impl(&mut conn, config.retention_days, round_rows).await {
            Ok(deleted) => {
                total += deleted;
                if (deleted as i64) < round_rows {
                    // 本轮没删满，说明已经追平保留期边界。
                    if total > 0 {
                        event!(
                            Level::INFO,
                            "Access log pruning finished: removed {total} rows older than {} days",
                            config.retention_days
                        );
                    }
                    break;
                }
                event!(
                    Level::DEBUG,
                    "Access log pruning round {} removed {deleted} rows (total {total})",
                    round + 1
                );
            }
            Err(e) => {
                first_err = Some(e);
                break;
            }
        }
    }

    // 无论成功失败都要在同一条连接上解锁，否则该实例后续轮次永远拿不到锁。
    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(crate::database::locks::RETENTION)
        .execute(&mut *conn)
        .await
    {
        event!(Level::ERROR, "Failed to release retention advisory lock: {e}");
    }
    drop(conn);

    match first_err {
        Some(e) => Err(e),
        None => Ok(total),
    }
}

/// 在**指定连接**上执行一轮分批删除，返回删除行数。
///
/// 之所以要求传入连接：调用方需要在同一条连接上持有会话级 advisory lock
/// （见 [`prune_access_logs`]）；若这里改用连接池，取锁与删数据会落在不同
/// 连接上，锁的语义就失效了。
async fn delete_access_logs_before_on_impl(
    conn: &mut sqlx::PgConnection,
    retention_days: u32,
    max_rows: i64,
) -> anyhow::Result<u64> {
    // 删除顺序按"子表 → 主表"（v1 时代由外键决定；v2 去掉了外键，顺序不再致命，
    // 但保持这个顺序对两种结构都安全）：
    //   access_response_size_logs.response_id -> access_response_logs.id
    //   access_request_size_logs.request_id   -> access_request_logs.id
    //   access_response_logs.id               -> access_request_logs.id
    // 因此必须先删两张 size 明细、再删响应、最后删请求，否则会撞外键。
    //
    // 时间列各不相同（响应表用 `responsed_at`，请求表用 `requested_at`，
    // 明细表用 `created_at`），窗口一律用 `NOW() - ($1 || ' days')::INTERVAL`，
    // 注意**不能**用 `NOW() - INTERVAL '1 day' * $1`：参数在 `INTERVAL` 里
    // 只按整型解析，传浮点或字符串会报错。
    let window = format!("NOW() - ($1 || ' days')::INTERVAL");
    let mut deleted = 0u64;

    // 1) 响应大小明细。
    //
    // 两条谓词各删一轮，而不是合成一条 `OR`：
    // * `at_second < window` —— **v2 的分区键就是 at_second**，这样能直接裁剪分区
    //   （生产库 v1 也已是 `at_second NOT NULL`，同样正确）；
    // * `at_second IS NULL AND created_at < window` —— 兼容还没按秒归一的历史行
    //   （早期/beta 库才有；v2 上该分支恒为空）。
    // 合并成 `OR` 会让优化器无法裁剪分区，退化成每次扫全部周分区。
    for predicate in [
        format!("at_second < {window}"),
        format!("at_second IS NULL AND created_at < {window}"),
    ] {
        if deleted >= max_rows as u64 {
            break;
        }
        deleted += sqlx::query(&format!(
            "WITH victims AS ( \
                 SELECT id FROM access_response_size_logs \
                  WHERE {predicate} LIMIT $2 \
             ) \
             DELETE FROM access_response_size_logs t USING victims v WHERE t.id = v.id"
        ))
        .bind(retention_days.to_string())
        .bind(max_rows - deleted as i64)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    }
    if deleted >= max_rows as u64 {
        return Ok(deleted);
    }

    // 2) 请求大小明细（口径同上）。
    for predicate in [
        format!("at_second < {window}"),
        format!("at_second IS NULL AND created_at < {window}"),
    ] {
        if deleted >= max_rows as u64 {
            break;
        }
        deleted += sqlx::query(&format!(
            "WITH victims AS ( \
                 SELECT id FROM access_request_size_logs \
                  WHERE {predicate} LIMIT $2 \
             ) \
             DELETE FROM access_request_size_logs t USING victims v WHERE t.id = v.id"
        ))
        .bind(retention_days.to_string())
        .bind(max_rows - deleted as i64)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    }
    if deleted >= max_rows as u64 {
        return Ok(deleted);
    }

    // 3) 响应主表
    deleted += sqlx::query(&format!(
        "WITH victims AS ( \
             SELECT id FROM access_response_logs \
              WHERE responsed_at < {window} LIMIT $2 \
         ) \
         DELETE FROM access_response_logs t USING victims v WHERE t.id = v.id"
    ))
    .bind(retention_days.to_string())
    .bind(max_rows - deleted as i64)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if deleted >= max_rows as u64 {
        return Ok(deleted);
    }

    // 4) 请求主表（此时引用它的行都已删掉）
    deleted += sqlx::query(&format!(
        "WITH victims AS ( \
             SELECT id FROM access_request_logs \
              WHERE requested_at < {window} LIMIT $2 \
         ) \
         DELETE FROM access_request_logs t USING victims v WHERE t.id = v.id"
    ))
    .bind(retention_days.to_string())
    .bind(max_rows - deleted as i64)
    .execute(&mut *conn)
    .await?
    .rows_affected();

    Ok(deleted)
}

/// size 明细表的升级迁移：补 `at_second` 列 + **部分唯一索引**（不触碰历史行）。
///
/// ## 为什么不做回填与去重（重要教训）
///
/// 早期实现是「补列 → 全表回填 → 合并同秒重复行 → 建全量唯一索引」，全部放在**启动路径**的
/// 一个事务里。在 1600 万行的生产表上这是灾难：
/// * 全表 `UPDATE` + 删 417 万行 + 建 1600 万行索引需要数分钟，且全程持有 `SCHEMA_INIT`
///   排他锁 —— 另一个服务只能干等，容器被健康检查/重启打断后事务整体回滚，**重来一遍**，
///   形成"永远起不来"的循环（2026-10-02 生产事故）。
///
/// 现在的做法把代价降到**与表大小无关**：
/// * `at_second` 允许为 NULL：PostgreSQL 11+ 加一个可空列是元数据操作，**不重写表**；
/// * 唯一索引是**部分索引** `WHERE at_second IS NOT NULL` —— 只覆盖新写入的行，
///   建索引时老行直接被谓词排除，因此不需要回填、不需要去重、不用扫全表；
/// * 写入侧用 `ON CONFLICT (xxx_id, at_second) WHERE at_second IS NOT NULL` 与索引谓词精确匹配
///   （已实测：部分索引必须带同样的 WHERE 才能被推断）。
///
/// 结果：**新数据按秒聚合并累加**，历史行原样保留（`at_second` 为 NULL）。
/// 历史行的秒级信息本来就在 `created_at` 上（一行一个 chunk），不需要也无法可靠还原。
/// 判断 `public` 下的某张表是不是 **分区表**（`relkind = 'p'`）。
///
/// 抽成常量是为了让"v2 表要跳过 v1 升级 DDL"这条约定有单一出处并被单测钉住：
/// v2 里 `at_second` 是分区键兼主键的一部分，对它执行
/// `ALTER COLUMN at_second DROP NOT NULL` 会报 `column "at_second" is in a primary key`，
/// 整个迁移事务回滚（实测：把已切到 v2 的库回滚成 v1 名字后，`--migrate` 直接失败）。
const IS_PARTITIONED_SQL: &str = "SELECT COALESCE((SELECT c.relkind = 'p' FROM pg_class c \
                                    JOIN pg_namespace n ON n.oid = c.relnamespace \
                                   WHERE n.nspname = 'public' AND c.relname = $1), false)";

async fn migrate_size_logs_second_granularity(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    for (table, id_col, uniq_name) in [
        (
            "access_request_size_logs",
            "request_id",
            "uniq_access_request_size_logs_req_second",
        ),
        (
            "access_response_size_logs",
            "response_id",
            "uniq_access_response_size_logs_resp_second",
        ),
    ] {
        // 0) 表不存在就跳过。
        //
        //    全新库只创建 v2 分区表（逻辑名 `access_*`），压根没有 v1 表；
        //    本函数出现在 v2 建表**之前**，不加守卫会直接报
        //    `relation "access_request_size_logs" does not exist`（实测踩到）。
        //    老库（有 v1 普通表）会正常走到下面的升级步骤。
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("public.{table}"))
            .fetch_one(&mut **tx)
            .await?;
        if !exists {
            continue;
        }

        // 1) 可空列（不重写表；历史行为 NULL，表示"未按秒归一"）
        sqlx::query(&format!(
            "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS at_second TIMESTAMPTZ"
        ))
        .execute(&mut **tx)
        .await?;

        // 2) 已经是 **v2 分区表** 的表必须原样跳过。
        //
        //    v2 里 `at_second` 是**分区键**、也是主键的一部分，因此：
        //    * `DROP NOT NULL` 会直接报 `column "at_second" is in a primary key`，
        //      整个迁移事务回滚 —— 表现为"回滚过 v2 的库再也起不来"（实测踩到）；
        //    * v2 的唯一索引是**整表**唯一索引（分区表的唯一索引必须含分区键），
        //      不需要也不应该再建 v1 那个 `WHERE at_second IS NOT NULL` 的部分索引。
        //    这个判断让 `--migrate` 在切到 v2 之后依然幂等。
        let partitioned: bool = sqlx::query_scalar(IS_PARTITIONED_SQL)
            .bind(table)
            .fetch_one(&mut **tx)
            .await?;
        if partitioned {
            continue;
        }

        // 3) 若曾被早期版本加上 NOT NULL，这里解除（否则新写入的历史兼容路径会失败）。
        sqlx::query(&format!(
            "ALTER TABLE {table} ALTER COLUMN at_second DROP NOT NULL"
        ))
        .execute(&mut **tx)
        .await?;

        // 4) 部分唯一索引：只约束新写入的行。老行不在索引里 → 无需回填、无需去重。
        //    注意写入侧必须带**相同的谓词**
        //    （`ON CONFLICT (xxx_id, at_second) WHERE at_second IS NOT NULL`），
        //    否则 PostgreSQL 无法推断该索引，会报
        //    "there is no unique or exclusion constraint matching the ON CONFLICT specification"
        //    （已实测）。
        sqlx::query(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {uniq_name} \
               ON {table} ({id_col}, at_second) WHERE at_second IS NOT NULL"
        ))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// 把「(请求, 秒) → 字节数」的增量**累加**进 size 明细表。
///
/// 每一行按 `DELETE 该键已有行（RETURNING 取回旧值） → INSERT 旧值+增量` 执行，整批一个事务。
///
/// ## 为什么不用 `INSERT ... ON CONFLICT DO UPDATE`（重要）
///
/// 累加语义要求冲突目标必须能解析到唯一索引，而**两种索引的语法不兼容**：
/// * 全量唯一索引 `(xxx_id, at_second)`：`ON CONFLICT (xxx_id, at_second)` 可以，
///   但带谓词的 `ON CONFLICT (...) WHERE at_second IS NOT NULL` **推断不出来**；
/// * 部分唯一索引 `WHERE at_second IS NOT NULL`：反过来，必须带同样的谓词才行。
///
/// 生产库在 2026-10-02 的事故中已经建成了**全量**唯一索引（当时那版迁移跑完了），
/// 而仓库新方案建的是**部分**索引 —— 若代码只认其中一种，换一次部署就会让刷盘整批失败。
/// `DELETE + INSERT` 不依赖冲突推断，对两种索引都成立（已实测）。
///
/// 并发安全：`DELETE` 会锁住该键的已有行；并发事务要么先看到我们插入的最终值（在其
/// `DELETE` 中取回并继续累加），要么被行锁阻塞到我们提交为止。因此不会丢增量。
async fn accumulate_size_rows(
    pool: &sqlx::PgPool,
    table: &str,
    parent: &str,
    id_col: &str,
    rows: Vec<(ObjectId, usize, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>,
) -> anyhow::Result<()> {
    // 同一批次内同键必须先合并：否则后面的行会覆盖前面的累加结果。
    let mut merged: HashMap<(ObjectId, chrono::DateTime<chrono::Utc>), (ObjectId, usize, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> =
        HashMap::new();
    for (id, size, at_second, created_at) in rows {
        merged
            .entry((id, at_second))
            .and_modify(|v| v.1 += size)
            .or_insert((id, size, at_second, created_at));
    }

    // 整批单事务：累加写不是幂等的，分块提交后重试会重复累加。
    let mut tx = pool.begin().await?;
    for ((id, at_second), (_id, size, _at, created_at)) in merged {
        let prev: Option<USize> = sqlx::query_scalar(&format!(
            "DELETE FROM {table} WHERE {id_col} = $1 AND at_second = $2 RETURNING body_length"
        ))
        .bind(id)
        .bind(at_second)
        .fetch_optional(&mut *tx)
        .await?;

        let total = usize::from(prev.unwrap_or_else(|| USize::from(0))) + size;
        // **父行守卫**：size 表对主表有外键（`response_id → access_response_logs.id`）。
        // 若父行尚未落库（或已被清理/迁移删除），这条 INSERT 会抛外键错误，
        // 而累加批次是"失败就整批留内存重试"的语义 —— 于是**一个坏行会把整批永久卡住**，
        // 响应大小统计从此停止写入（2026-10-02 生产实测，日志里的
        // `violates foreign key constraint "access_response_size_logs_response_id_fkey"`）。
        // 用 `INSERT ... SELECT ... WHERE EXISTS` 把这种行变为**静默跳过**：
        // 该 id 的大小统计本就依赖主表，主表没有它就没有统计对象。
        sqlx::query(&format!(
            "INSERT INTO {table} (id, {id_col}, body_length, at_second, created_at) \
             SELECT $1, $2, $3, $4, $5 \
              WHERE EXISTS (SELECT 1 FROM {parent} p WHERE p.id = $2)"
        ))
        .bind(ObjectId::new())
        .bind(id)
        .bind(USize::from(total))
        .bind(at_second)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v2 分区表必须被 v1 的 size 升级 DDL 跳过。
    ///
    /// v2 里 `at_second` 是分区键兼主键的一部分，对它跑
    /// `ALTER COLUMN at_second DROP NOT NULL` 会报
    /// `column "at_second" is in a primary key` 并把整个迁移事务回滚 ——
    /// 实测发生在"把已切到 v2 的库回滚成 v1 名字后再启动"的场景，
    /// 表现为服务再也起不来。守卫 SQL 必须只认 `relkind = 'p'`。
    #[test]
    fn size_ddl_guard_only_skips_partitioned_tables() {
        assert!(
            IS_PARTITIONED_SQL.contains("c.relkind = 'p'"),
            "守卫必须只认分区表：{IS_PARTITIONED_SQL}"
        );
        assert!(
            IS_PARTITIONED_SQL.contains("relname = $1"),
            "守卫必须按表名判断，不能写死"
        );
        // 兜底 false：表不存在时不能当成"已分区"而跳过升级。
        assert!(
            IS_PARTITIONED_SQL.contains("COALESCE"),
            "表不存在时必须回退 false（否则新库会被误跳过建索引）"
        );
    }
}
