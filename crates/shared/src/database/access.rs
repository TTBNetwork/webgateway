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

const INIT_SQL: &str = include_str!("../../../../assets/sqls/access_init.sql");

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

/// 在调用方提供的（可由 advisory lock 保护的）事务中创建访问日志相关对象。
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
            // 注意：这里必须逐条执行，不能用 `sqlx::raw_sql`。
            // `RawSql` 会在 `Executor` 上引入 `'q` 借用参数，与 `Transaction`
            // 的 reborrow 组合后会触发 "implementation of `Executor` is not
            // general enough" 的编译错误。
            for statement in split_sql_statements(INIT_SQL) {
                sqlx::query(&statement).execute(&mut **tx).await?;
            }

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

/// 把 DDL 脚本拆成单条语句。
///
/// `access_init.sql` 中没有字符串字面量里的分号，也没有 `$$ ... $$` 函数体，
/// 因此按行累积、遇到以 `;` 结尾的行即切分即可；注释行与空行会被跳过。
fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    for line in sql.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        current.push_str(line);
        current.push('\n');
        if trimmed.ends_with(';') {
            statements.push(std::mem::take(&mut current));
        }
    }
    if !current.trim().is_empty() {
        statements.push(current);
    }
    statements
}

// 以下为占位的空实现，可根据后续需求填充方法
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
        // 直接在 requested_at 上过滤（sargable），使 idx_requested_at 可用。
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
        // 使用 LEFT JOIN 关联请求表和响应表，一次性获取所有统计指标
        let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, USize, USize)>(
            r#"        
            WITH
            req_size_agg AS (
                SELECT request_id, SUM(body_length) AS total_request_size
                FROM access_request_size_logs
                WHERE created_at > NOW() - INTERVAL '1 day' * $1
                GROUP BY request_id
            ),
            resp_size_agg AS (
                SELECT response_id, SUM(body_length) AS total_response_size
                FROM access_response_size_logs
                WHERE created_at > NOW() - INTERVAL '1 day' * $1
                GROUP BY response_id
            )
            SELECT
                COUNT(req.id) AS total_requests,
                COUNT(DISTINCT req.remote_addr) AS total_ips,
                COUNT(resp.id) FILTER (WHERE resp.status >= 400 AND resp.status <= 499) AS e4xx_requests,
                COUNT(resp.id) FILTER (WHERE resp.status >= 500 AND resp.status <= 599) AS e5xx_requests,
                COUNT(req.id) FILTER (WHERE resp.id IS NULL) AS backend_error_requests,
                COALESCE(SUM(req_agg.total_request_size), 0)::uint8 AS total_requests_size,
                COALESCE(SUM(resp_agg.total_response_size), 0)::uint8 AS total_response_size
            FROM access_request_logs req
            LEFT JOIN access_response_logs resp ON req.id = resp.id
            LEFT JOIN req_size_agg req_agg ON req.id = req_agg.request_id
            LEFT JOIN resp_size_agg resp_agg ON req.id = resp_agg.response_id
            WHERE req.requested_at > NOW() - INTERVAL '1 day' * $1
        "#,
        )
        .bind(in_days as i64)  // 绑定天数参数
        .fetch_one(&self.pool)
        .await?;

        // 将数据库返回的 i64 转换为 usize（注意溢出风险，通常天数范围内的请求数不会超过 usize 最大值）
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

    async fn get_today_metrics_info_of_websites(
        &self,
    ) -> anyhow::Result<Vec<TodayMetricsInfoOfWebsite>> {
        let rows = sqlx::query_as::<_, TodayMetricsInfoOfWebsite>
            (r#"
                WITH
                req_size_agg AS (
                    SELECT ars.request_id, SUM(ars.body_length) AS total_request_size
                    FROM access_request_size_logs ars
                    INNER JOIN access_request_logs ar ON ars.request_id = ar.id
                    WHERE ar.requested_at >= CURRENT_DATE AND ar.requested_at < CURRENT_DATE + INTERVAL '1 day'
                    GROUP BY ars.request_id
                ),
                resp_size_agg AS (
                    SELECT ars.response_id, SUM(ars.body_length) AS total_response_size
                    FROM access_response_size_logs ars
                    INNER JOIN access_response_logs ar ON ars.response_id = ar.id
                    WHERE ar.responsed_at >= CURRENT_DATE AND ar.responsed_at < CURRENT_DATE + INTERVAL '1 day'
                    GROUP BY ars.response_id
                )
                SELECT 
                    req.website_id as website_id,
                    COUNT(req.id) AS total_requests,
                    COUNT(DISTINCT req.remote_addr) AS total_ips,
                    COUNT(resp.id) AS total_responses,
                    COUNT(resp.id) FILTER (WHERE resp.status >= 400 AND resp.status <= 499) AS e4xx_requests,
                    COUNT(resp.id) FILTER (WHERE resp.status >= 500 AND resp.status <= 599) AS e5xx_requests,
                    COUNT(req.id) FILTER (WHERE resp.id IS NULL) AS backend_error_requests,
                    COALESCE(SUM(req_agg.total_request_size), 0)::uint8 AS total_requests_size,
                    COALESCE(SUM(resp_agg.total_response_size), 0)::uint8 AS total_response_size
                FROM access_request_logs req
                LEFT JOIN access_response_logs resp ON req.id = resp.id
                LEFT JOIN req_size_agg req_agg ON req.id = req_agg.request_id
                LEFT JOIN resp_size_agg resp_agg ON req.id = resp_agg.response_id
                WHERE req.requested_at >= CURRENT_DATE AND req.requested_at < CURRENT_DATE + INTERVAL '1 day'
                GROUP BY req.website_id
                "#
            ).fetch_all(&self.pool).await?;
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
    /// 撑爆 WAL，还会把删除的元组堆在表尾让表**更大**（直到 vacuum 回收）。调用方
    /// 应循环调用直到返回 0（见 `prune_access_logs`）。
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
            builder.push(" ON CONFLICT (id) DO NOTHING");
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
            builder.push(" ON CONFLICT (id) DO NOTHING");
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
        // 整个批次放进**一个事务**：这些是"累加"写入（ON CONFLICT DO UPDATE
        // body_length = body_length + EXCLUDED.body_length），不是幂等的。
        // 若像其它批量写那样分块提交，第一块成功、第二块失败后整批重试会把
        // 第一块的字节**重复累加**。单事务保证要么全做要么全不做，重试永远不会重复计数。
        // 批次上限见 gateway 的 MAX_ROWS_PER_FLUSH（1000 行），单事务足够小。
        // 先把批次内同键行合并（见 `merge_same_second` 的说明）。
        let responses = merge_same_second(
            responses,
            |r| (r.id, r.at_second),
            |acc, r| acc.body_length += r.body_length,
        );
        let mut tx = self.pool.begin().await?;
        let mut builder = QueryBuilder::new(
            "INSERT INTO access_response_size_logs (id, response_id, body_length, at_second, created_at)",
        );
        builder.push_values(responses.iter(), |mut b, resp| {
            b.push_bind(ObjectId::new())
                .push_bind(resp.id)
                .push_bind(USize::from(resp.body_length))
                .push_bind(resp.at_second)
                .push_bind(resp.created_at);
        });
        // 同一 (response_id, at_second) 已存在 → **累加**该秒的字节数（保留每秒颗粒度）。
        //
        // 只写一个 ON CONFLICT：PostgreSQL **不允许**多个 ON CONFLICT 子句
        // （`ON CONFLICT (a) ... ON CONFLICT (b) ...` 会报 syntax error at or near "ON"，
        // 已实测）。这里选唯一键 (response_id, at_second)；`id` 是新生成的 ObjectId，
        // 撞主键在数学上可忽略（12 字节含 5 字节随机 + 计数器）。
        // 注意 `DO UPDATE` 的 SET 只能引用目标表与 EXCLUDED 的列，因此这里显式写出
        // 表名（`response_id`/`at_second` 不参与计算）。
        builder.push(
            " ON CONFLICT (response_id, at_second) DO UPDATE SET \
               body_length = access_response_size_logs.body_length + EXCLUDED.body_length",
        );
        builder.build().execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn insert_batch_access_request_increase_size_logs(
        &self,
        requests: Vec<AccessInsertRequestSize>,
    ) -> anyhow::Result<()> {
        if requests.is_empty() {
            return Ok(());
        }
        // 同一批次内同键行必须先合并：单条 INSERT 不允许两次命中同一冲突键。
        let requests = merge_same_second(
            requests,
            |r| (r.id, r.at_second),
            |acc, r| acc.body_length += r.body_length,
        );
        // 同上：累加写入必须整批单事务，否则重试会重复累加。
        let mut tx = self.pool.begin().await?;
        let mut builder = QueryBuilder::new(
            "INSERT INTO access_request_size_logs (id, request_id, body_length, at_second, created_at)",
        );
        builder.push_values(requests.iter(), |mut b, req| {
            b.push_bind(ObjectId::new())
                .push_bind(req.id)
                .push_bind(USize::from(req.body_length))
                .push_bind(req.at_second)
                .push_bind(req.created_at);
        });
        // 同上：只允许一个 ON CONFLICT，选 (request_id, at_second) 做累加。
        builder.push(
            " ON CONFLICT (request_id, at_second) DO UPDATE SET \
               body_length = access_request_size_logs.body_length + EXCLUDED.body_length",
        );
        builder.build().execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
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

/// 把同一批次内 `(id, at_second)` 相同的行合并成一行（字节相加）。
///
/// 为什么必须做：PostgreSQL 的 `INSERT ... ON CONFLICT DO UPDATE` 在**同一条语句**
/// 里不允许两次命中同一个冲突键，会报
/// `ON CONFLICT DO UPDATE command cannot affect row a second time`。
/// gateway 侧的内存累加器已经保证正常情况下不会出现同键多行，但刷盘是**多轮**的、
/// 且接口是公开的，这里再兜一层，保证任何调用方都能安全使用。
fn merge_same_second<T, F, G>(
    rows: Vec<T>,
    key: F,
    merge: G,
) -> Vec<T>
where
    T: Clone,
    F: Fn(&T) -> (ObjectId, chrono::DateTime<chrono::Utc>),
    G: Fn(&mut T, &T),
{
    let mut out: Vec<T> = Vec::with_capacity(rows.len());
    let mut seen: HashMap<(ObjectId, chrono::DateTime<chrono::Utc>), usize> = HashMap::new();
    for row in rows {
        match seen.get(&key(&row)) {
            Some(&idx) => merge(&mut out[idx], &row),
            None => {
                seen.insert(key(&row), out.len());
                out.push(row);
            }
        }
    }
    out
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
    // 删除顺序由外键决定（见 assets/sqls/access_init.sql）：
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

    // 1) 响应大小明细
    deleted += sqlx::query(&format!(
        "WITH victims AS ( \
             SELECT id FROM access_response_size_logs \
              WHERE created_at < {window} LIMIT $2 \
         ) \
         DELETE FROM access_response_size_logs t USING victims v WHERE t.id = v.id"
    ))
    .bind(retention_days.to_string())
    .bind(max_rows)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if deleted >= max_rows as u64 {
        return Ok(deleted);
    }

    // 2) 请求大小明细
    deleted += sqlx::query(&format!(
        "WITH victims AS ( \
             SELECT id FROM access_request_size_logs \
              WHERE created_at < {window} LIMIT $2 \
         ) \
         DELETE FROM access_request_size_logs t USING victims v WHERE t.id = v.id"
    ))
    .bind(retention_days.to_string())
    .bind(max_rows - deleted as i64)
    .execute(&mut *conn)
    .await?
    .rows_affected();
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

/// size 明细表的升级迁移：补 `at_second` 列、回填、**收敛重复行**、建唯一索引。
///
/// 必须在 `SCHEMA_INIT` 事务里调用（调用方已持锁）。整个过程幂等：
/// * 补列用 `ADD COLUMN IF NOT EXISTS`；
/// * 回填只处理 `at_second IS NULL` 的行；
/// * 去重只删"同一 (xxx_id, 秒) 里多余的行"；
/// * 建索引用 `CREATE UNIQUE INDEX IF NOT EXISTS`。
///
/// **首次在老库上执行会较慢**（要为每行算 `at_second` 并合并重复行；生产库实测
/// 417 万行需要合并）。已迁移过的库只会走一次廉价的 `NOT EXISTS` 判断。
async fn migrate_size_logs_second_granularity(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    // 迁移期的长语句是预期的：临时放宽语句超时，避免被默认超时打断后整事务回滚。
    sqlx::query("SET LOCAL statement_timeout = 0")
        .execute(&mut **tx)
        .await?;

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
        // 1) 补列
        sqlx::query(&format!(
            "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS at_second TIMESTAMPTZ"
        ))
        .execute(&mut **tx)
        .await?;

        // 2) 回填：历史行的 `created_at` 就是它的时间轴，截断到整秒即可。
        let backfilled = sqlx::query(&format!(
            "UPDATE {table} SET at_second = date_trunc('second', created_at) \
              WHERE at_second IS NULL"
        ))
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if backfilled > 0 {
            event!(
                Level::INFO,
                "Retention migration: backfilled at_second for {backfilled} rows in {table}"
            );
        }

        // 3) 去重：同一 (id_col, at_second) 只保留一行，保留的字节数**合并**其余行，
        //    以免历史统计凭空变小。
        let has_dup: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {table} \
              GROUP BY {id_col}, at_second HAVING COUNT(*) > 1)"
        ))
        .fetch_one(&mut **tx)
        .await?;

        if has_dup {
            event!(
                Level::WARN,
                "Retention migration: {table} contains rows sharing the same ({id_col}, second); \
                 merging them into one row each. This is a one-time cost and may take a while"
            );
            // 分组谓词只写一次，DELETE 与 INSERT 共用，保证两次聚合结果完全一致。
            // 做法：**先删掉组内所有行、再插回一行合并值**。
            // 不能只 `SET body_length = 自己` —— 那样被删行的字节会被丢掉，
            // 历史统计会凭空变小；也不做多余的预 UPDATE（删完就没了）。
            let groups = format!(
                "SELECT {id_col}, at_second, MIN(id) AS keep_id, SUM(body_length)::uint8 AS total \
                   FROM {table} GROUP BY {id_col}, at_second HAVING COUNT(*) > 1"
            );
            let deleted = sqlx::query(&format!(
                "DELETE FROM {table} t USING ({groups}) g \
                  WHERE t.{id_col} = g.{id_col} AND t.at_second = g.at_second"
            ))
            .execute(&mut **tx)
            .await?
            .rows_affected();
            let merged_rows = sqlx::query(&format!(
                "INSERT INTO {table} (id, {id_col}, body_length, at_second, created_at) \
                 SELECT g.keep_id, g.{id_col}, g.total, g.at_second, NOW() FROM ({groups}) g"
            ))
            .execute(&mut **tx)
            .await?
            .rows_affected();
            event!(
                Level::INFO,
                "Retention migration: {table} merged {deleted} duplicate rows into \
                 {merged_rows} aggregated rows (byte totals preserved)"
            );
        }

        // 4) 约束与索引
        sqlx::query(&format!(
            "ALTER TABLE {table} ALTER COLUMN at_second SET NOT NULL"
        ))
        .execute(&mut **tx)
        .await?;
        sqlx::query(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {uniq_name} ON {table} ({id_col}, at_second)"
        ))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PostgreSQL 单条 `INSERT ... ON CONFLICT DO UPDATE` 不允许两次命中同一冲突键
    /// （实测报 `ON CONFLICT DO UPDATE command cannot affect row a second time`）。
    /// 因此批量写入前必须在内存里把同键行合并，否则同一秒的两行会让整批失败。
    #[test]
    fn merge_same_second_folds_duplicate_keys() {
        let id = ObjectId::new();
        let other = ObjectId::new();
        let base = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();

        let rows = vec![
            AccessInsertRequestSize::new(id, 10, base),
            AccessInsertRequestSize::new(id, 25, base + chrono::TimeDelta::milliseconds(400)),
            AccessInsertRequestSize::new(id, 5, base + chrono::TimeDelta::seconds(1)),
            AccessInsertRequestSize::new(other, 7, base),
        ];
        let merged = merge_same_second(
            rows,
            |r| (r.id, r.at_second),
            |acc, r| acc.body_length += r.body_length,
        );

        assert_eq!(merged.len(), 3, "同一 (id, 秒) 的两行必须合成一行，跨秒不合并");
        let same_second = merged
            .iter()
            .find(|r| r.id == id && r.at_second == base)
            .expect("应保留 (id, base) 这一行");
        assert_eq!(same_second.body_length, 10 + 25, "同一秒的字节必须相加");
        assert!(
            merged.iter().any(|r| r.id == id && r.at_second == base + chrono::TimeDelta::seconds(1)),
            "跨秒的行必须单独保留"
        );
        assert!(merged.iter().any(|r| r.id == other), "其它 id 不受影响");
    }
}
