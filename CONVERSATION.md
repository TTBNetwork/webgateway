# 访问日志存储优化 + v1→v2 迁移方案

环境：PostgreSQL 18 + uint128/btree_gin 扩展；2kw 行/1~2 月；峰值 100~500 QPS；id 为 ObjectId(TEXT)；gateway 写、dashboard 读，多服务共享 schema。

## 0. 核心决策

| 项 | 决策 |
|---|---|
| 分区 | 按周 RANGE，分区键 requested_at / responsed_at / created_at |
| 主键 | PRIMARY KEY (id, 分区键) |
| 外键 | 全部移除（分区表无法被单列 id 引用） |
| 视图 | 废弃，改成参数化 SQL 函数（视图无法裁剪分区） |
| v1/v2 兼容 | 读侧 COALESCE(size_logs, 主表 body_length) + 迁移脚本回填 |
| DDL 权限 | 收敛到单一迁移入口（--migrate），服务启动只 verify |
| DDL 并发 | 全局 advisory lock（事务级 pg_advisory_xact_lock） |
| 分区维护 | try_lock + 单实例；DETACH 用 CONCURRENTLY |
| 明细表写者 | 单一写者：只 gateway 写 |

## 1. access_init.sql v2（完整）

CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- 请求日志
CREATE TABLE IF NOT EXISTS access_request_logs (
    id TEXT NOT NULL, host TEXT NOT NULL, method TEXT NOT NULL, path TEXT NOT NULL,
    headers JSONB NOT NULL DEFAULT '[]', http_version TEXT NOT NULL,
    remote_addr TEXT NOT NULL, body_length uint8 NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), website_id TEXT,
    PRIMARY KEY (id, requested_at)
) PARTITION BY RANGE (requested_at);
CREATE INDEX IF NOT EXISTS idx_req_requested_at ON access_request_logs (requested_at);
CREATE INDEX IF NOT EXISTS idx_req_website_time ON access_request_logs (website_id, requested_at);
CREATE INDEX IF NOT EXISTS idx_req_remote_time  ON access_request_logs (remote_addr, requested_at);
CREATE INDEX IF NOT EXISTS idx_req_id           ON access_request_logs (id);

-- 响应日志
CREATE TABLE IF NOT EXISTS access_response_logs (
    id TEXT NOT NULL, status UINT2 NOT NULL, headers JSONB NOT NULL DEFAULT '[]',
    body_length uint8, http_version TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    responsed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    backend_responsed_at TIMESTAMPTZ DEFAULT NOW(), website_id TEXT,
    PRIMARY KEY (id, responsed_at)
) PARTITION BY RANGE (responsed_at);
CREATE INDEX IF NOT EXISTS idx_resp_responsed_at ON access_response_logs (responsed_at);
CREATE INDEX IF NOT EXISTS idx_resp_status_time  ON access_response_logs (status, responsed_at);
CREATE INDEX IF NOT EXISTS idx_resp_website_time ON access_response_logs (website_id, responsed_at);
CREATE INDEX IF NOT EXISTS idx_resp_id           ON access_response_logs (id);

-- size_logs
CREATE TABLE IF NOT EXISTS access_request_size_logs (
    id TEXT NOT NULL, request_id TEXT NOT NULL, body_length uint8 NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE INDEX IF NOT EXISTS idx_req_size_created ON access_request_size_logs (created_at);
CREATE INDEX IF NOT EXISTS idx_req_size_req_id  ON access_request_size_logs (request_id, created_at);

CREATE TABLE IF NOT EXISTS access_response_size_logs (
    id TEXT NOT NULL, response_id TEXT NOT NULL, body_length uint8 NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE INDEX IF NOT EXISTS idx_resp_size_created ON access_response_size_logs (created_at);
CREATE INDEX IF NOT EXISTS idx_resp_size_resp_id ON access_response_size_logs (response_id, created_at);

-- 分区管理函数（含二级 advisory lock 防 TOCTOU）
CREATE OR REPLACE FUNCTION ensure_weekly_partition(p_parent text, p_week_start date)
RETURNS void LANGUAGE plpgsql AS $fn$
DECLARE v_name text; v_end date := p_week_start + INTERVAL '7 days'; v_exists int;
BEGIN
    v_name := format('%s_%sw%s', p_parent,
        to_char(p_week_start,'IYYY'), to_char(p_week_start,'IW'));
    PERFORM pg_advisory_xact_lock(hashtext(v_name));
    SELECT 1 INTO v_exists FROM pg_class WHERE relname = v_name;
    IF v_exists IS NOT NULL THEN RETURN; END IF;
    BEGIN
        EXECUTE format('CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
            v_name, p_parent, p_week_start, v_end);
    EXCEPTION WHEN duplicate_table THEN NULL;
    END;
END; $fn$;

CREATE OR REPLACE FUNCTION ensure_upcoming_weeks(p_ahead int DEFAULT 3)
RETURNS void LANGUAGE plpgsql AS $fn$
DECLARE v_this_week date := date_trunc('week', NOW())::date;
        v_table text; i int; w date;
        v_tables text[] := ARRAY['access_request_logs','access_response_logs',
                                 'access_request_size_logs','access_response_size_logs'];
BEGIN
    FOREACH v_table IN ARRAY v_tables LOOP
        FOR i IN 0..p_ahead LOOP
            w := v_this_week + (i * 7);
            PERFORM ensure_weekly_partition(v_table, w);
        END LOOP;
    END LOOP;
END; $fn$;

-- 查询函数（替代视图）
CREATE OR REPLACE FUNCTION get_qps_per_second(p_count int)
RETURNS TABLE(time timestamptz, total_requests bigint, qps float8)
LANGUAGE sql STABLE AS $fn$
    SELECT date_trunc('second', requested_at), COUNT(*), COUNT(*)::float8
    FROM access_request_logs
    WHERE requested_at >= NOW() - make_interval(secs => p_count)
    GROUP BY 1 ORDER BY 1 DESC LIMIT p_count;
$fn$;

CREATE OR REPLACE FUNCTION get_qps_per_5s(p_count int)
RETURNS TABLE(time timestamptz, total_requests bigint, qps float8)
LANGUAGE sql STABLE AS $fn$
    SELECT to_timestamp(floor(extract(epoch FROM requested_at)/5)*5),
           COUNT(*), COUNT(*)::float8 / 5.0
    FROM access_request_logs
    WHERE requested_at >= NOW() - make_interval(secs => p_count * 5)
    GROUP BY 1 ORDER BY 1 DESC LIMIT p_count;
$fn$;

CREATE OR REPLACE FUNCTION get_access_info(p_days int)
RETURNS TABLE(total_requests bigint, total_ips bigint, e4xx_requests bigint,
              e5xx_requests bigint, backend_error_requests bigint,
              total_requests_size numeric, total_response_size numeric)
LANGUAGE sql STABLE AS $fn$
    WITH req_size_agg AS (
        SELECT request_id, SUM(body_length) AS sz FROM access_request_size_logs
        WHERE created_at > NOW() - make_interval(days => p_days) GROUP BY request_id),
    resp_size_agg AS (
        SELECT response_id, SUM(body_length) AS sz FROM access_response_size_logs
        WHERE created_at > NOW() - make_interval(days => p_days) GROUP BY response_id)
    SELECT COUNT(req.id), COUNT(DISTINCT req.remote_addr),
        COUNT(resp.id) FILTER (WHERE resp.status BETWEEN 400 AND 499),
        COUNT(resp.id) FILTER (WHERE resp.status BETWEEN 500 AND 599),
        COUNT(req.id) FILTER (WHERE resp.id IS NULL),
        COALESCE(SUM(COALESCE(req_agg.sz,  req.body_length)), 0),
        COALESCE(SUM(COALESCE(resp_agg.sz, resp.body_length, 0)), 0)
    FROM access_request_logs req
    LEFT JOIN access_response_logs resp ON req.id = resp.id
    LEFT JOIN req_size_agg req_agg ON req.id = req_agg.request_id
    LEFT JOIN resp_size_agg resp_agg ON req.id = resp_agg.response_id
    WHERE req.requested_at > NOW() - make_interval(days => p_days);
$fn$;

CREATE OR REPLACE FUNCTION get_today_metrics_info_of_websites()
RETURNS TABLE(website_id text, total_requests bigint, total_ips bigint,
              total_responses bigint, e4xx_requests bigint, e5xx_requests bigint,
              backend_error_requests bigint, total_requests_size numeric,
              total_response_size numeric)
LANGUAGE sql STABLE AS $fn$
    WITH req_size_agg AS (
        SELECT request_id, SUM(body_length) AS sz FROM access_request_size_logs
        WHERE created_at >= CURRENT_DATE AND created_at < CURRENT_DATE + INTERVAL '1 day'
        GROUP BY request_id),
    resp_size_agg AS (
        SELECT response_id, SUM(body_length) AS sz FROM access_response_size_logs
        WHERE created_at >= CURRENT_DATE AND created_at < CURRENT_DATE + INTERVAL '1 day'
        GROUP BY response_id)
    SELECT req.website_id, COUNT(req.id), COUNT(DISTINCT req.remote_addr),
        COUNT(resp.id),
        COUNT(resp.id) FILTER (WHERE resp.status BETWEEN 400 AND 499),
        COUNT(resp.id) FILTER (WHERE resp.status BETWEEN 500 AND 599),
        COUNT(req.id) FILTER (WHERE resp.id IS NULL),
        COALESCE(SUM(COALESCE(req_agg.sz, req.body_length)), 0),
        COALESCE(SUM(COALESCE(resp_agg.sz, resp.body_length, 0)), 0)
    FROM access_request_logs req
    LEFT JOIN access_response_logs resp ON req.id = resp.id
    LEFT JOIN req_size_agg req_agg ON req.id = req_agg.request_id
    LEFT JOIN resp_size_agg resp_agg ON req.id = resp_agg.response_id
    WHERE req.requested_at >= CURRENT_DATE
      AND req.requested_at < CURRENT_DATE + INTERVAL '1 day'
    GROUP BY req.website_id;
$fn$;

CREATE OR REPLACE FUNCTION get_requests_of_ips(p_days int)
RETURNS TABLE(remote_addr text, req_count bigint)
LANGUAGE sql STABLE AS $fn$
    SELECT remote_addr, COUNT(*) FROM access_request_logs
    WHERE requested_at > NOW() - make_interval(days => p_days)
    GROUP BY remote_addr;
$fn$;

SELECT ensure_upcoming_weeks(3);

## 2. Rust 仓储层改造要点

### 2.1 调用改查函数
旧：`SELECT ... FROM qps_per_second WHERE ... LIMIT $1`
新：`SELECT time, total_requests, qps FROM get_qps_per_second($1)`，bind(count as i32)

字段名对齐：5s 函数里 `avg_qps AS qps`，Rust `DatabaseQPS.qps`。

### 2.2 批量 UPDATE 用 FROM (VALUES ...) 替代 CASE WHEN
```rust
let mut b = QueryBuilder::new(
    "UPDATE access_request_logs AS ar SET body_length = v.body_length FROM (VALUES ");
let mut sep = b.separated(", ");
for req in &requests {
    sep.push("(");
    sep.push_bind_unseparated(&req.id);
    sep.push_unseparated(", ");
    sep.push_bind_unseparated(USize::from(req.body_length));
    sep.push_unseparated(")");
}
b.push(") AS v(id, body_length) WHERE ar.id = v.id");


3. 冷启动迁移（停机 5~15 分钟）
步骤：

停写，记基线行数 N_req/N_resp/N_reqsz/N_respsz

DROP 三个跨表 FK

老表改名 *_v1，DROP 老表普通索引（保留主键索引）

建新分区父表（第 1 节 SQL）+ ensure_upcoming_weeks(3)

按老数据覆盖的周逐周 ensure_weekly_partition

并发回填（直插子分区，WHERE 严格对齐分区边界）

回填完再在父表建索引（自动级联）

校验行数 + 抽样

恢复写入

一周后 DROP access_*_v1

DROP FK：

sql
ALTER TABLE access_response_logs        DROP CONSTRAINT access_response_logs_id_fkey;
ALTER TABLE access_request_size_logs    DROP CONSTRAINT access_request_size_logs_request_id_fkey;
ALTER TABLE access_response_size_logs   DROP CONSTRAINT access_response_size_logs_response_id_fkey;
回填示例（直插子分区、多会话并发）：

sql
INSERT INTO access_request_logs_2025w40
SELECT * FROM access_request_logs_v1
WHERE requested_at >= '2025-09-29' AND requested_at < '2025-10-06';
回滚：DROP 新表，ALTER TABLE access_request_logs_v1 RENAME TO access_request_logs; 即可。

4. 多服务并发处理
4.1 三条铁律
DDL 只在迁移入口，其他服务只读 schema_version

所有 DDL 走 advisory lock

明细表单写者

4.2 database/locks.rs
rust
pub const SCHEMA_INIT: i64 = 0x4143_434C_0000_0001;
pub const MIGRATION:   i64 = 0x4143_434C_0000_0002;
pub const PARTITION:   i64 = 0x4143_434C_0000_0003;
4.3 schema_version 表
sql
CREATE TABLE IF NOT EXISTS schema_version (
    component TEXT PRIMARY KEY,
    version INT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
4.4 写者约定
表	写者	读者
access_request_logs	gateway	dashboard
access_response_logs	gateway	dashboard
*_size_logs	gateway	dashboard
schema_version	迁移 CLI	所有人
5. database/mod.rs 改造
5.1 Advisory lock 辅助
rust
impl Database {
    pub async fn with_ddl_lock<F, Fut, T>(&self, lock_id: i64, f: F) -> anyhow::Result<T>
    where F: FnOnce(&mut Transaction<'_, Postgres>) -> Fut,
          Fut: std::future::Future<Output = anyhow::Result<T>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(lock_id)
            .execute(&mut *tx).await?;
        let out = f(&mut tx).await?;
        tx.commit().await?;
        Ok(out)
    }
    pub async fn try_with_ddl_lock<F, Fut, T>(&self, lock_id: i64, f: F)
        -> anyhow::Result<Option<T>>
    where F: FnOnce(&mut Transaction<'_, Postgres>) -> Fut,
          Fut: std::future::Future<Output = anyhow::Result<T>> {
        let mut tx = self.pool.begin().await?;
        let (got,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
            .bind(lock_id).fetch_one(&mut *tx).await?;
        if !got { tx.rollback().await?; return Ok(None); }
        let out = f(&mut tx).await?;
        tx.commit().await?;
        Ok(Some(out))
    }
}
5.2 启动模式
rust
pub enum DbStartupMode { Serve, Migrate, AutoMigrate }

pub async fn init_database(url: &str, max_conn: u32, mode: DbStartupMode) -> anyhow::Result<()> {
    // ... DATABASE.set(...)
    match mode {
        DbStartupMode::Migrate     => migrate_database_schema().await?,
        DbStartupMode::AutoMigrate => { migrate_database_schema().await?;
                                        verify_database_schema().await?; }
        DbStartupMode::Serve       => verify_database_schema().await?,
    }
    Ok(())
}
main.rs：

rust
let mode = if std::env::args().any(|a| a == "--migrate") { DbStartupMode::Migrate }
    else if std::env::var("DB_AUTO_MIGRATE").is_ok() { DbStartupMode::AutoMigrate }
    else { DbStartupMode::Serve };
init_database(&url, 20, mode).await?;
5.3 各模块 trait 拆分
rust
#[async_trait]
pub trait DatabaseAccessLogsInitializer {
    async fn migrate_access_logs(&self) -> anyhow::Result<()>;
    async fn verify_access_logs_schema(&self) -> anyhow::Result<()>;
}
const ACCESS_LOGS_SCHEMA_VERSION: i32 = 2;

// migrate_access_logs 内：
// with_ddl_lock(SCHEMA_INIT, |tx| async move {
//   sqlx::raw_sql(INIT_SQL).execute(&mut **tx).await?;
//   sqlx::query("SELECT ensure_upcoming_weeks($1)").bind(3_i32)
//       .execute(&mut **tx).await?;
//   sqlx::query("INSERT INTO schema_version(component,version) VALUES('access_logs',$1)
//                ON CONFLICT (component) DO UPDATE SET version=EXCLUDED.version,
//                applied_at=NOW()").bind(ACCESS_LOGS_SCHEMA_VERSION)
//       .execute(&mut **tx).await?;
//   Ok(())
// }).await
certificate / dnsprovider / websites 同样拆。

5.4 分区维护 worker
rust
pub async fn maintain_access_partitions(&self) -> anyhow::Result<()> {
    let r = self.try_with_ddl_lock(locks::PARTITION, |tx| async move {
        sqlx::query("SELECT ensure_upcoming_weeks($1)").bind(4_i32)
            .execute(&mut **tx).await?;
        Ok(())
    }).await?;
    if r.is_none() { tracing::debug!("partition maintain held by other instance"); }
    Ok(())
}
DETACH CONCURRENTLY 不能进事务，需独立连接 + session 级 pg_advisory_lock，单条 DDL 自动提交。

6. 已知坑
6.1 create_trigger_notify 性能坑
access_*_logs 是 append-only，无 updated_at 列，触发器挂不上

FOR EACH ROW 在 500 QPS 下每秒 500 次 pg_notify，会拖慢事务

只给低频表挂（websites / dns_providers / certificates），加白名单

rust
const ALLOWED: &[&str] = &["websites", "dns_providers", "certificates"];
if !ALLOWED.contains(&table_name.as_str()) { anyhow::bail!("not in whitelist"); }
6.2 DETACH 要 CONCURRENTLY
默认 DETACH 拿父表 ACCESS EXCLUSIVE，阻塞读写。PG 14+ 支持 DETACH PARTITION ... CONCURRENTLY，但不能在事务块里。

6.3 分区裁剪验证
sql
EXPLAIN (ANALYZE, COSTS OFF) SELECT * FROM get_qps_per_second(60);
-- 应只列 1~2 个周分区
6.4 ObjectId 定位
可用于游标分页 WHERE id > $last_id ORDER BY id LIMIT N

不要用它替代 requested_at 做时间过滤（时钟漂移 / TEXT 比较慢 / 分区键必须是 timestamptz）

迁移分批回填可用 ORDER BY id LIMIT 50000（hex 字典序 == 时间序）

6.5 listen_service_fn
现每次循环新建 PgListener，多实例正确，不用改。建议 handler 重 IO 时 spawn 出去。

7. 部署
进程	启动方式	职责
迁移 Job	myapp --migrate	全部 DDL，跑完退出
gateway	myapp（Serve）	verify + 写日志
dashboard	myapp（Serve）	verify + 读统计
分区 worker	定时任务	ensure_upcoming_weeks + DETACH
开发	DB_AUTO_MIGRATE=1 myapp	先迁移再服务
定时任务：每周一凌晨 3 点 ensure_upcoming_weeks(4)；每月 detach_partitions_before(parent, now - 26 weeks)。

8. 实施清单（按优先级）
P0：

□ database/locks.rs 注册 SCHEMA_INIT / MIGRATION / PARTITION
□ Database::with_ddl_lock / try_with_ddl_lock
□ init_extensions / init_nofity_trigger_function 走锁
□ schema_version 表 + ensure_schema_version_table
□ init_database 拆 Serve / Migrate / AutoMigrate
P1：

□ 各模块 trait 拆 migrate_xxx + verify_xxx_schema
□ access_init.sql v2 落地
□ 仓储层改查函数 + 批量 UPDATE 改 FROM (VALUES ...)
□ create_trigger_notify 走锁 + 白名单
P2：

□ 冷启动迁移脚本 migrate_v1_to_v2.sql
□ 分区维护 worker
□ 部署脚本分离迁移 Job
P3：

□ （可选）access_qps_rollup 预聚合表 + 5s worker
9. 验证
sql
SELECT relname FROM pg_class WHERE relname LIKE 'access_request_logs\_%' ORDER BY relname;
EXPLAIN (ANALYZE, COSTS OFF) SELECT * FROM get_qps_per_second(60);
SELECT * FROM get_access_info(1);
SELECT * FROM get_today_metrics_info_of_websites();
SELECT * FROM schema_version;


