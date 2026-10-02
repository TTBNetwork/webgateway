use async_trait::async_trait;
use std::collections::HashMap;

use crate::{
    database::Database,
    models::access::{
        AccessCreateRequest, AccessCreateResponse, AccessInfo, AccessInsertRequestSize,
        AccessInsertResponseSize, AccessUpdateRequestSize, AccessUpdateResponseSize, DatabaseQPS,
        ResponseQPS, TodayMetricsInfoOfWebsite,
    },
};
use futures::future::BoxFuture;
use simple_shared::objectid::ObjectId;
use sqlx::{Postgres, QueryBuilder, Transaction, types::Json};
use sqlx_pg_ext_uint::{c_u16::U16, c_usize::USize};

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
        // 4 个绑定/行。
        for chunk in responses.chunks(chunk_rows(4)) {
            let mut builder = QueryBuilder::new(
                "INSERT INTO access_response_size_logs (id, response_id, body_length, created_at)",
            );
            builder.push_values(chunk.iter(), |mut b, resp| {
                b.push_bind(ObjectId::new())
                    .push_bind(resp.id)
                    .push_bind(USize::from(resp.body_length))
                    .push_bind(resp.created_at);
            });
            builder.build().execute(&self.pool).await?;
        }
        Ok(())
    }

    async fn insert_batch_access_request_increase_size_logs(
        &self,
        requests: Vec<AccessInsertRequestSize>,
    ) -> anyhow::Result<()> {
        if requests.is_empty() {
            return Ok(());
        }
        for chunk in requests.chunks(chunk_rows(4)) {
            let mut builder = QueryBuilder::new(
                "INSERT INTO access_request_size_logs (id, request_id, body_length, created_at)",
            );
            builder.push_values(chunk.iter(), |mut b, req| {
                b.push_bind(ObjectId::new())
                    .push_bind(req.id)
                    .push_bind(USize::from(req.body_length))
                    .push_bind(req.created_at);
            });
            builder.build().execute(&self.pool).await?;
        }
        Ok(())
    }
}
