//! 访问日志表的 **v2 结构**：按周 RANGE 分区的统计表 + 分区轮换。
//!
//! 背景（STEP.md 目前-1）：统计表要改成 v2 并支持**按周轮换** —— 每周一张分区，
//! 过期周直接 `DETACH` + `DROP`，而不是对千万行逐行 `DELETE`。
//!
//! ## 为什么是"声明式分区"而不是"每周一张独立表"
//!
//! 用户的原话是"每周一张表格"。在 PostgreSQL 里，**声明式分区**正是实现它的标准方式，
//! 而且比手工管理一堆独立表更好：查询只写父表名（按分区键自动裁剪，只扫相关周）、
//! 索引按分区各自维护（建索引/回收空间都只影响一周的数据）、
//! 删除历史只需 `DETACH PARTITION`（元数据操作，不掉数据页）。
//!
//! ## 与 v1 的结构差异（这几条决定了迁移的代价）
//!
//! 1. 分区键必须进主键：`PRIMARY KEY (id, requested_at)`；
//! 2. **外键全部移除**：分区表无法被单列 `id` 引用，因此
//!    `access_response_logs → access_request_logs`、两张 size 表 → 主表的引用完整性
//!    改由应用层保证（size 表已有"父行守卫"，见 `database/access.rs`）；
//! 3. 现有的 `qps_per_second` / `qps_per_5s` 视图在 v2 上会全表扫（视图无法裁剪分区），
//!    查询侧本来就已改为直接过滤时间列，因此 v2 里**不再创建这两个视图**。
//!
//! ## 本模块的边界（重要）
//!
//! 这里只负责 **v2 结构 + 分区维护**，**不改动现有查询路径**。
//! 数据搬迁与读写切换是两件独立的事，必须显式执行（见 [`migrate_v1_to_v2`]），
//! 避免重演 2026-10-02 那次"启动路径上做重活导致服务起不来"的事故。

use std::collections::BTreeMap;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sqlx::{Postgres, Row, Transaction};
use tracing::{Level, event};

/// 四张统计表的**逻辑名**（查询侧一直用这些名字）。
pub const V2_TABLES: &[&str] = &[
    "access_request_logs",
    "access_response_logs",
    "access_request_size_logs",
    "access_response_size_logs",
];

/// v2 分区表的物理名：`access_v2_req_logs` / `access_v2_resp_logs` /
/// `access_v2_req_size_logs` / `access_v2_resp_size_logs`。
///
/// 为什么 v2 用独立物理名（而不是就地转换）：PostgreSQL 没有
/// `ALTER TABLE ... PARTITION BY`，必须"建新表 + 搬数据 + 换名"，
/// 而换名前两者会**并存**。用独立名字可以让"建表/搬数据/校验"全程不影响线上读写，
/// 切换（换名）只在最后一步发生。
///
/// 命名规则（用户 2026-10-02 确认）：
/// * v2 表与它的周分区统一带 `access_v2_` 前缀，周分区形如
///   `access_v2_req_logs_2026_w40`（`_<IYYY>_w<IW>`）；
/// * **v1 的命名不动**（`access_request_logs` 等），切换后也只加 `_v1` 后缀。
pub fn v2_physical_name(logical: &str) -> String {
    match logical {
        "access_request_logs" => "access_v2_req_logs".to_string(),
        "access_response_logs" => "access_v2_resp_logs".to_string(),
        "access_request_size_logs" => "access_v2_req_size_logs".to_string(),
        "access_response_size_logs" => "access_v2_resp_size_logs".to_string(),
        other => format!("{other}_v2"),
    }
}

/// v1 历史数据的**源表名**：`_v1` 已改名就用它，否则用逻辑名。
///
/// 与 [`live_v2_table`] 恰好互补 —— 这一对函数回答"从哪读、往哪写"：
///
/// | 阶段 | 源（v1） | 目标（v2） |
/// |------|----------|------------|
/// | 迁移期间（`gateway --migrate-v2`） | `access_*` | `access_v2_*` |
/// | 激活之后（`mnt migrate-v2`） | `access_*_v1` | `access_*` |
///
/// **踩过的坑**：搬迁工具曾经两边都用逻辑名，结果从刚 `TRUNCATE` 过的空表里读数据
/// —— 跑了 6 秒、写入 0 行，还报告"搬迁完成"（实测踩到）。
pub async fn v1_source_table(pool: &sqlx::PgPool, logical: &str) -> anyhow::Result<String> {
    let archived = v1_name(logical);
    if table_exists(pool, &archived).await? {
        Ok(archived)
    } else {
        Ok(logical.to_string())
    }
}

/// v2 数据的**实际存放表名**：取决于当前处于哪个阶段。
///
/// * 迁移期间（v1 还叫逻辑名）：v2 数据在物理表 [`v2_physical_name`]（`access_v2_*`）；
/// * **激活之后**（v1 已改名为 `*_v1`）：v2 数据就在逻辑表 `access_*` 上。
///
/// 为什么必须解析而不是用常量：`access_v2_*` 这个物理名只在"迁移期间"存在，
/// 换名之后它就没了。搬迁工具（`mnt migrate-v2`）是在**激活之后**跑的，
/// 若仍往 `access_v2_req_logs` 写，会报
/// `relation "access_v2_req_logs" does not exist`（实测踩到）。
pub async fn live_v2_table(pool: &sqlx::PgPool, logical: &str) -> anyhow::Result<String> {
    if table_exists(pool, &v1_name(logical)).await? {
        // v1 已经改名归档 → 说明已激活，v2 正在用逻辑名。
        Ok(logical.to_string())
    } else {
        Ok(v2_physical_name(logical))
    }
}

/// 所有**可能是分区父表**的名字（迁移期间的物理名 + 切换后的逻辑名）。
///
/// 分区维护函数（`wg_access_partition_parents`）用这个名单 + `relkind = 'p'` 解析
/// "现在该维护哪些分区表"。两套名字都要认：迁移期间是 `access_v2_*`，
/// 切换后是逻辑名 `access_*`。**漏掉任何一套，换名之后分区轮换就静默失效**
/// （一个分区都建不出来），这是本项目反复踩过的坑。
pub fn partition_parent_candidates() -> Vec<String> {
    let mut names: Vec<String> = V2_TABLES.iter().map(|s| s.to_string()).collect();
    names.extend(V2_TABLES.iter().map(|s| v2_physical_name(s)));
    // 历史遗留的 `_p` 命名也要认：老库可能还停在这一版，切换前的维护不能断。
    names.extend(V2_TABLES.iter().map(|s| format!("{s}_p")));
    names.sort();
    names.dedup();
    names
}

/// 把候选父表名单拼成 SQL 的 `IN (...)` 列表（名字全部来自常量，无注入面）。
fn partition_parent_candidates_sql() -> String {
    partition_parent_candidates()
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 该逻辑表当前是否为分区表（用于判断是否已切到 v2）。
pub async fn is_partitioned(pool: &sqlx::PgPool, logical: &str) -> anyhow::Result<bool> {
    let v: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT c.relkind = 'p' FROM pg_class c \
                            JOIN pg_namespace n ON n.oid = c.relnamespace \
                           WHERE n.nspname = 'public' AND c.relname = $1), false)",
    )
    .bind(logical)
    .fetch_one(pool)
    .await?;
    Ok(v)
}

/// 分区键（沿用 v1 的列名，查询侧不必改名）。
///
/// 目前只有测试引用它 —— 数据搬迁脚本（尚未实现）会用它拼 `SELECT` 的时间列。
/// 保留在这里是为了让"v2 的分区键 = v1 的列名"这条约定有单一出处。
#[allow(dead_code)]
pub(crate) fn partition_key(table: &str) -> &'static str {
    match table {
        "access_request_logs" => "requested_at",
        "access_response_logs" => "responsed_at",
        // size 表按 `at_second` 分区，见下方 DDL 里的解释。
        _ => "at_second",
    }
}

/// 生成某张表的 v2 建表 DDL（幂等）。
///
/// v2 与 v1 的差异只有三处：分区键进主键、**去掉外键**、不再建 `qps_per_*` 视图
/// （视图无法裁剪分区，查询侧本来就已改为直接过滤时间列）。
fn v2_ddl_for(logical: &str) -> String {
    let phys = v2_physical_name(logical);
    match logical {
        "access_request_logs" => format!(
            r#"
CREATE TABLE IF NOT EXISTS {phys} (
    id TEXT NOT NULL,
    host TEXT NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    headers JSONB NOT NULL DEFAULT '[]',
    http_version TEXT NOT NULL,
    remote_addr TEXT NOT NULL,
    body_length uint8 NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    website_id TEXT,
    PRIMARY KEY (id, requested_at)
) PARTITION BY RANGE (requested_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_requested_at ON {phys} (requested_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_website_time ON {phys} (website_id, requested_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_remote_time ON {phys} (remote_addr, requested_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_id ON {phys} (id);
"#
        ),
        "access_response_logs" => format!(
            r#"
CREATE TABLE IF NOT EXISTS {phys} (
    id TEXT NOT NULL,
    status UINT2 NOT NULL,
    headers JSONB NOT NULL DEFAULT '[]',
    body_length uint8,
    http_version TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    responsed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    backend_responsed_at TIMESTAMPTZ DEFAULT NOW(),
    website_id TEXT,
    PRIMARY KEY (id, responsed_at)
) PARTITION BY RANGE (responsed_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_responsed_at ON {phys} (responsed_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_status_time ON {phys} (status, responsed_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_website_time ON {phys} (website_id, responsed_at);
CREATE INDEX IF NOT EXISTS idx_{phys}_id ON {phys} (id);
"#
        ),
        // size 表按 **`at_second`** 分区（不是 `created_at`）。
        //
        // 为什么：分区表的唯一索引**必须包含分区键**，而"按 (请求, 秒) 累加"的唯一键是
        // `(request_id, at_second)`。若分区键用 `created_at`，就必须把 `created_at` 拉进
        // 唯一键，而落库时无法预知它的值，`ON CONFLICT` 也就无从推断。
        // 两者本来就是同一时刻：`AccessInsert*Size::new` 用 `truncate_to_second(at)` 生成
        // `at_second`、`created_at = at` —— 即 `at_second = date_trunc('second', created_at)`。
        // 因此把分区键换成 `at_second` 是**等价**的，而且顺带让秒级裁剪更自然。
        //
        // `at_second` 必须非空：分区路由要求分区键不为 NULL。模型侧始终会填，
        // 这里再加 `DEFAULT NOW()` 兜住直接写 SQL 的场景。
        "access_request_size_logs" => format!(
            r#"
CREATE TABLE IF NOT EXISTS {phys} (
    id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    body_length uint8 NOT NULL,
    at_second TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, at_second)
) PARTITION BY RANGE (at_second);
CREATE INDEX IF NOT EXISTS idx_{phys}_at_second ON {phys} (at_second);
CREATE INDEX IF NOT EXISTS idx_{phys}_req_id ON {phys} (request_id, at_second);
-- 唯一索引覆盖新写入的行（老行 at_second 为 NULL 时不参与），
-- 这是"同一秒累加"的最终保证。
CREATE UNIQUE INDEX IF NOT EXISTS uniq_{phys}_req_second
    ON {phys} (request_id, at_second);
"#
        ),
        "access_response_size_logs" => format!(
            r#"
CREATE TABLE IF NOT EXISTS {phys} (
    id TEXT NOT NULL,
    response_id TEXT NOT NULL,
    body_length uint8 NOT NULL,
    at_second TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, at_second)
) PARTITION BY RANGE (at_second);
CREATE INDEX IF NOT EXISTS idx_{phys}_at_second ON {phys} (at_second);
CREATE INDEX IF NOT EXISTS idx_{phys}_resp_id ON {phys} (response_id, at_second);
CREATE UNIQUE INDEX IF NOT EXISTS uniq_{phys}_resp_second
    ON {phys} (response_id, at_second);
"#
        ),
        other => unreachable!("unknown access log table: {other}"),
    }
}

/// 解析"当前哪些访问日志父表是分区表"。
///
/// 迁移期间父表叫 `access_v2_*`（老库还可能是更早的 `access_*_p`），
/// 切换（换名）之后叫逻辑名 `access_*`。分区维护函数若只认其中一套名字，
/// 换名之后就会**静默失效**（一个分区都建不出来）。
/// 因此名单由 [`partition_parent_candidates`] 统一给出，且**只认 `relkind = 'p'`**，
/// 避免把同名的普通表当成父表。
fn fn_partition_parents() -> String {
    format!(
        r#"
CREATE OR REPLACE FUNCTION wg_access_partition_parents()
RETURNS text[] LANGUAGE sql STABLE AS $fn$
    SELECT COALESCE(array_agg(c.relname ORDER BY c.relname), ARRAY[]::text[])
      FROM pg_class c
      JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public'
       AND c.relkind = 'p'
       AND c.relname IN ({});
$fn$;
"#,
        partition_parent_candidates_sql()
    )
}

/// 建周分区：二级 advisory lock 防 TOCTOU（两个实例同时 `CREATE` 会撞 `duplicate_table`）。
/// 按父表**当前名字**建周分区（不自带 `_p` 前缀）。
///
/// 为什么不能用硬编码的 `%s_%s_w%s` 拼父表名：v2 的物理父表名有两种形态 ——
/// 切换前是 `access_request_logs_p`、切换后是逻辑名 `access_request_logs`。
/// 早先这里传进来的是逻辑名（`wg_access_partition_parents` 返回的），
/// 却按 `access_request_logs_p_2026_w40` 建分区，结果：
///
/// * 切换后仍然建出**带 `_p` 的孤儿分区**，`pg_dump`/`DROP` 时很难理解；
/// * 对 `access_request_logs` 来说这些名字与既有分区（`..._2026_w39`）
///   是**重叠**的，`CREATE TABLE ... PARTITION OF` 直接报
///   `partition "..." would overlap partition "..."` —— 启动即失败（实测踩到）。
///
/// 现在以父表为基础名（去掉可选的 `_p` 后缀），切换前后都能得到
/// `access_request_logs_<IYYY>_w<IW>` 这一种干净命名。
const FN_ENSURE_WEEKLY: &str = r#"
CREATE OR REPLACE FUNCTION wg_ensure_weekly_partition(p_parent text, p_week_start date)
RETURNS void LANGUAGE plpgsql AS $fn$
DECLARE
    v_base text := regexp_replace(p_parent, '_p$', '');
    v_name text;
    v_end date := p_week_start + INTERVAL '7 days';
BEGIN
    v_name := format('%s_%s_w%s', v_base,
        to_char(p_week_start, 'IYYY'), to_char(p_week_start, 'IW'));
    PERFORM pg_advisory_xact_lock(hashtext(v_name));
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
        v_name, p_parent, p_week_start, v_end);
END; $fn$;
"#;

/// 预建当前周前后若干周。起点取 -1 周，避免"周初边界 + 时钟偏差"漏建当周。
const FN_ENSURE_UPCOMING: &str = r#"
CREATE OR REPLACE FUNCTION wg_ensure_upcoming_weeks(p_ahead int DEFAULT 2)
RETURNS void LANGUAGE plpgsql AS $fn$
DECLARE
    v_this_week date := date_trunc('week', NOW())::date;
    v_table text;
    i int;
BEGIN
    FOREACH v_table IN ARRAY wg_access_partition_parents() LOOP
        FOR i IN -1..p_ahead LOOP
            PERFORM wg_ensure_weekly_partition(v_table, v_this_week + (i * 7));
        END LOOP;
    END LOOP;
END; $fn$;
"#;

/// 整周回收：`DETACH` + `DROP`。比逐行 `DELETE` 快几个数量级，且不需要 autovacuum。
const FN_DROP_OLD: &str = r#"
CREATE OR REPLACE FUNCTION wg_drop_partitions_older_than(p_days int)
RETURNS int LANGUAGE plpgsql AS $fn$
DECLARE
    v_cutoff date := (NOW() - make_interval(days => p_days))::date;
    v_rec record;
    v_dropped int := 0;
    v_start date;
BEGIN
    IF p_days < 90 THEN
        RAISE EXCEPTION 'retention days must be >= 90, got %', p_days;
    END IF;
    FOR v_rec IN
        SELECT c.relname AS child
          FROM pg_inherits i
          JOIN pg_class c ON c.oid = i.inhrelid
          JOIN pg_class p ON p.oid = i.inhparent
         WHERE p.relname = ANY (wg_access_partition_parents())
    LOOP
        -- 分区名形如 <parent>_<IYYY>_w<IW>；解析不出起始日就跳过，绝不误删。
        BEGIN
            v_start := to_date(
                substring(v_rec.child from '_(\d{4})_w\d+$') || '-' ||
                substring(v_rec.child from '_w(\d+)$'), 'IYYY-IW');
        EXCEPTION WHEN others THEN
            CONTINUE;
        END;
        IF v_start IS NULL THEN
            CONTINUE;
        END IF;
        -- 整周都早于截止日才删（跨边界的那一周留着）。
        IF v_start + 7 <= v_cutoff THEN
            EXECUTE format('DROP TABLE IF EXISTS %I', v_rec.child);
            v_dropped := v_dropped + 1;
        END IF;
    END LOOP;
    RETURN v_dropped;
END; $fn$;
"#;

/// 分区管理函数（必须整条执行，不能按 `;` 切分）。顺序有依赖：解析函数必须最先建。
///
/// 第一个由 [`fn_partition_parents`] 动态生成（父表名单要跟着命名规则走），其余是常量。
fn partition_functions() -> Vec<String> {
    vec![
        fn_partition_parents(),
        FN_ENSURE_WEEKLY.to_string(),
        FN_ENSURE_UPCOMING.to_string(),
        FN_DROP_OLD.to_string(),
    ]
}

/// 在**给定事务**中创建 v2 分区表、安装分区管理函数并预建未来若干周分区。
///
/// 幂等、可反复执行。**只建结构，不搬数据、不切换读写**。
///
/// 已经是 v2 的逻辑表（切换完成后）会被跳过 —— 否则换名之后 `install_v2_schema`
/// 会重新建出一张空的 `access_v2_req_logs`，与真正的 v2 表（现名 `access_request_logs`）
/// 并存，下一次进度的判断就会全部错乱。
pub async fn install_v2_schema(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    // 迁移进度表（单行）。`pending` 只代表"还没开始"。
    for stmt in STATE_TABLE_DDL
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        sqlx::query(stmt).execute(&mut **tx).await?;
    }

    // 注意：这里按 `;` 切分**仅对建表 DDL 安全** —— 下面那些 plpgsql 函数体里有
    // 分号（还有 `$$`），如果也按 `;` 切会被切碎、建函数静默失败，
    // 结果是"表建好了但一个分区都没有"（实测踩过）。函数一律整条执行。
    for logical in V2_TABLES {
        if is_partitioned_tx(tx, logical).await? {
            // 已经切到 v2（逻辑名本身就是分区表），不需要再建物理副本。
            continue;
        }
        for stmt in v2_ddl_for(logical)
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            debug_assert!(
                !stmt.contains("$$"),
                "按分号切分的语句里不允许出现 $$ 函数体"
            );
            sqlx::query(stmt).execute(&mut **tx).await?;
        }
    }

    for stmt in partition_functions() {
        sqlx::query(&stmt).execute(&mut **tx).await?;
    }

    // 给"分区名还停在旧命名"的库做一次性改名：老版本是 `<父表>_p_<IYYY>_w<IW>`
    // （例如 `access_request_logs_p_2026_w40`），规范名是
    // `access_v2_req_logs_2026_w40`。放在建分区之前，否则同一周会同时存在两个分区，
    // `CREATE TABLE ... PARTITION OF` 会报 would overlap partition（实测踩到）。
    normalize_weekly_partition_names(tx).await?;

    sqlx::query("SELECT wg_ensure_upcoming_weeks(2)")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// 把周分区名统一成 `<父表当前名>_<IYYY>_w<IW>`（并同步修正 DEFAULT 分区名）。
///
/// 历史命名有两代：
/// * 最早 `access_request_logs_p_2026_w40`（父表物理名带 `_p` 后缀、分区名跟着带）；
/// * 现在 `access_v2_req_logs_2026_w40`（父表与分区都带 `access_v2_` 前缀）。
///
/// 判断标准不是"名字里有没有 `_p`"，而是**"是否等于父表当前名 + `_<IYYY>_w<IW>`"**：
/// 只要父表叫 `access_v2_req_logs`，它的分区就必须叫 `access_v2_req_logs_2026_w40`。
/// 这样无论父表处于哪一代命名，分区名都会被纠正到与父表一致。
///
/// 幂等：名字已经正确的库不会做任何改动。只在**父表已经是分区表**时动手。
async fn normalize_weekly_partition_names(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    for logical in V2_TABLES {
        if !is_partitioned_tx(tx, logical).await? {
            continue;
        }
        // 继承子表里名字**不**匹配 `^<父表名>_<IYYY>_w<IW>$` 的那些。
        let stale: Vec<String> = sqlx::query_scalar(
            "SELECT c.relname FROM pg_inherits i \
               JOIN pg_class c ON c.oid = i.inhrelid \
               JOIN pg_class p ON p.oid = i.inhparent \
               JOIN pg_namespace n ON n.oid = p.relnamespace \
              WHERE n.nspname = 'public' AND p.relname = $1 \
                AND c.relname !~ ('^' || $1 || '_[0-9]{4}_w[0-9]+$')",
        )
        .bind(logical)
        .fetch_all(&mut **tx)
        .await?;
        for child in stale {
            // 目标名 = 父表当前名 + 该分区自己的 `_<IYYY>_w<IW>` / `_default` 尾巴。
            // 旧名形如 `access_request_logs_p_2026_w40`：去掉父表旧前缀后剩下 `_2026_w40`。
            let Some(tail) = child.rfind("_default").map(|i| &child[i..]).or_else(|| {
                child
                    .rfind("_w")
                    .and_then(|iw| child[..iw].rfind('_'))
                    .map(|iy| &child[iy..])
            }) else {
                continue; // 认不出的名字：不动它，避免误改
            };
            let new_name = format!("{logical}{tail}");
            if new_name == child {
                continue;
            }
            sqlx::query(&format!(
                "ALTER TABLE IF EXISTS \"{child}\" RENAME TO \"{new_name}\""
            ))
            .execute(&mut **tx)
            .await?;
            event!(
                Level::INFO,
                "Access log v2: renamed partition {child} -> {new_name}"
            );
        }
    }
    Ok(())
}

async fn is_partitioned_tx(
    tx: &mut Transaction<'_, Postgres>,
    logical: &str,
) -> anyhow::Result<bool> {
    let v: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT c.relkind = 'p' FROM pg_class c \
                            JOIN pg_namespace n ON n.oid = c.relnamespace \
                           WHERE n.nspname = 'public' AND c.relname = $1), false)",
    )
    .bind(logical)
    .fetch_one(&mut **tx)
    .await?;
    Ok(v)
}

/// 表（`public` schema）是否存在。
pub async fn table_exists(pool: &sqlx::PgPool, name: &str) -> anyhow::Result<bool> {
    let v: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("public.{name}"))
        .fetch_one(pool)
        .await?;
    Ok(v)
}

async fn table_exists_tx(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
) -> anyhow::Result<bool> {
    let v: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("public.{name}"))
        .fetch_one(&mut **tx)
        .await?;
    Ok(v)
}

/// 数据库当前时间（迁移的时间游标一律取自数据库，避免应用与库的时钟偏差）。
pub async fn db_now(pool: &sqlx::PgPool) -> anyhow::Result<DateTime<Utc>> {
    Ok(sqlx::query_scalar("SELECT NOW()").fetch_one(pool).await?)
}

/// 把时间戳向下取整到"天"（按数据库会话的时区，与应用时区无关）。
pub async fn db_trunc_day(
    pool: &sqlx::PgPool,
    ts: DateTime<Utc>,
) -> anyhow::Result<DateTime<Utc>> {
    Ok(sqlx::query_scalar("SELECT date_trunc('day', $1::timestamptz)")
        .bind(ts)
        .fetch_one(pool)
        .await?)
}

/// 迁移进度（对应 [`STATE_TABLE`] 的一行）。
#[derive(Debug, Clone, Default)]
pub struct V2MigrationStatus {
    pub phase: String,
    pub cutoff: Option<DateTime<Utc>>,
    pub current_table: Option<String>,
    pub cursor_at: Option<DateTime<Utc>>,
    pub copied_rows: i64,
    pub last_error: Option<String>,
}

impl V2MigrationStatus {
    /// 是否已经**结束**（不论是"搬迁完成"还是"直接激活、保留 v1"）。
    pub fn is_done(&self) -> bool {
        self.phase == "done" || self.phase == PHASE_ACTIVE_KEEP_V1
    }

    /// 是否是"直接激活 v2、v1 永久保留"模式（此时绝不能自动回收 `*_v1`）。
    pub fn keeps_v1(&self) -> bool {
        self.phase == PHASE_ACTIVE_KEEP_V1
    }
}

/// 读取迁移进度；进度表不存在时返回 `None`（说明结构还没装）。
pub async fn load_status(pool: &sqlx::PgPool) -> anyhow::Result<Option<V2MigrationStatus>> {
    if !table_exists(pool, STATE_TABLE).await? {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT phase, cutoff, current_table, cursor_at, copied_rows, last_error \
           FROM access_log_v2_migration WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| V2MigrationStatus {
        phase: r.get("phase"),
        cutoff: r.get("cutoff"),
        current_table: r.get("current_table"),
        cursor_at: r.get("cursor_at"),
        copied_rows: r.get("copied_rows"),
        last_error: r.get("last_error"),
    }))
}

/// 开始（或续跑）搬迁：写入 cutoff 与起始游标。
pub async fn start_migration(
    pool: &sqlx::PgPool,
    cutoff: DateTime<Utc>,
    cursor: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE access_log_v2_migration \
            SET phase = 'backfilling', cutoff = $1, current_table = $2, cursor_at = $3, \
                copied_rows = 0, last_error = NULL, \
                started_at = COALESCE(started_at, NOW()), updated_at = NOW(), finished_at = NULL \
          WHERE id = 1",
    )
    .bind(cutoff)
    .bind(V2_TABLES[0])
    .bind(cursor)
    .execute(pool)
    .await?;
    Ok(())
}

/// 保存搬迁进度（当前表 + 时间游标 + 累计已复制行数）。
pub async fn save_progress(
    pool: &sqlx::PgPool,
    table: &str,
    cursor: DateTime<Utc>,
    copied_rows: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE access_log_v2_migration \
            SET current_table = $1, cursor_at = $2, copied_rows = $3, updated_at = NOW() \
          WHERE id = 1",
    )
    .bind(table)
    .bind(cursor)
    .bind(copied_rows)
    .execute(pool)
    .await?;
    Ok(())
}

/// 记录阶段（`backfilling` / `switching` / `done` / [`PHASE_ACTIVE_KEEP_V1`]）。
pub async fn mark_phase(pool: &sqlx::PgPool, phase: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE access_log_v2_migration \
            SET phase = $1, updated_at = NOW(), \
                finished_at = CASE WHEN $1 = 'done' THEN NOW() ELSE finished_at END \
          WHERE id = 1",
    )
    .bind(phase)
    .execute(pool)
    .await?;
    Ok(())
}

/// 记录失败原因（进度仍然保留，下次可续跑）。
pub async fn mark_error(pool: &sqlx::PgPool, err: &str) -> anyhow::Result<()> {
    let _ = sqlx::query(
        "UPDATE access_log_v2_migration \
            SET last_error = $1, updated_at = NOW() WHERE id = 1",
    )
    .bind(err)
    .execute(pool)
    .await;
    Ok(())
}

/// 四张 v1 表里最早的一条记录时间（用于决定搬迁起点）。
pub async fn earliest_time(pool: &sqlx::PgPool) -> anyhow::Result<Option<DateTime<Utc>>> {
    let v: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT LEAST( \
             (SELECT min(requested_at) FROM access_request_logs), \
             (SELECT min(responsed_at) FROM access_response_logs), \
             (SELECT min(COALESCE(at_second, created_at)) FROM access_request_size_logs), \
             (SELECT min(COALESCE(at_second, created_at)) FROM access_response_size_logs))",
    )
    .fetch_one(pool)
    .await?;
    Ok(v)
}

/// 为 `[from, to]` 覆盖到的每一周，在四张 v2 表上建好周分区。
///
/// 分区必须**先于**写入存在，否则 `INSERT` 会报
/// `no partition of relation ... found for row`。建空分区是毫秒级的元数据操作。
pub async fn ensure_history_partitions(
    pool: &sqlx::PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<()> {
    for logical in V2_TABLES {
        // 分区建在**当前真实的父表**上：迁移期间是 `access_v2_*`，激活后是逻辑名。
        let phys = live_v2_table(pool, logical).await?;
        // 先确认函数已安装（未装结构时安静跳过，由调用方决定是否报错）。
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_proc WHERE proname = 'wg_ensure_weekly_partition')",
        )
        .fetch_one(pool)
        .await?;
        if !exists {
            anyhow::bail!("分区管理函数尚未安装，请先执行 schema 迁移");
        }
        sqlx::query(
            "SELECT wg_ensure_weekly_partition($1, w::date) \
               FROM generate_series( \
                      date_trunc('week', $2::timestamptz)::date, \
                      date_trunc('week', $3::timestamptz)::date, \
                      INTERVAL '7 days') AS w",
        )
        .bind(&phys)
        .bind(from)
        .bind(to)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// 建一张 `DEFAULT` 兜底分区（覆盖所有未预建的时间范围）。
///
/// 为什么需要：分区键与源表的过滤列不是同一列时（size 表按 `at_second` 路由，
/// 而历史行还可能 `at_second IS NULL`），理论上可能出现"时间落在所有已建分区之外"
/// 的行。此时 `INSERT` 会抛 `no partition of relation ... found for row`，
/// 整个按天批次失败 —— 迁移会卡在第一天过不去，而不是"慢一点"。
///
/// 兜底分区把这种行收进去。它**不参与周轮换**（名字不以 `_w<IW>` 结尾，
/// `wg_drop_partitions_older_than` 解析不出周号时会跳过它），因此不会误删；
/// 有了它之后，PostgreSQL 仍会把查询裁剪到对应周分区，只是会多访问一次兜底分区。
pub async fn ensure_default_partition(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    for logical in V2_TABLES {
        let phys = live_v2_table(pool, logical).await?;
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS {phys}_default PARTITION OF {phys} DEFAULT"
        ))
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// 预建"上一周 ~ 未来两周"的周分区（幂等；v1 表上安全返回）。
///
/// gateway 每小时调用一次。库还没切成 v2 时，`wg_access_partition_parents()` 返回空数组，
/// 函数是个空操作 —— 因此不需要调用方判断版本。
pub async fn ensure_upcoming_partitions(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let installed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_proc WHERE proname = 'wg_ensure_upcoming_weeks')",
    )
    .fetch_one(pool)
    .await?;
    if !installed {
        return Ok(()); // 结构还没装（DB_AUTO_MIGRATE=0？），静默跳过
    }
    sqlx::query("SELECT wg_ensure_upcoming_weeks(2)")
        .execute(pool)
        .await?;
    Ok(())
}

/// 列出四张分区表的周分区：`(父表, 分区名, 行数)`，按父表与分区名排序。
///
/// 运维/排查用（`pg_admin --v2-report` 也打印分区清单）。只读。
pub async fn list_partitions(pool: &sqlx::PgPool) -> anyhow::Result<Vec<(String, String, i64)>> {
    let rows = sqlx::query(
        "SELECT p.relname AS parent, c.relname AS child, \
                (SELECT count(*) FROM pg_inherits i2 WHERE i2.inhparent = c.oid) AS subparts \
           FROM pg_inherits i \
           JOIN pg_class c ON c.oid = i.inhrelid \
           JOIN pg_class p ON p.oid = i.inhparent \
           JOIN pg_namespace n ON n.oid = p.relnamespace \
          WHERE n.nspname = 'public' AND p.relkind = 'p' \
          ORDER BY p.relname, c.relname",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let parent: String = r.get("parent");
        let child: String = r.get("child");
        // 行数按分区名直接 count：分区是普通表，count 走主键索引。
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM \"{child}\""))
            .fetch_one(pool)
            .await
            .unwrap_or(0);
        out.push((parent, child, n));
    }
    Ok(out)
}

/// 整周回收：把"整周都早于 `retention_days` 天前"的分区 `DETACH` + `DROP`，返回删除数。
///
/// 与逐行 `DELETE` 相比：不产生 WAL、不需要 autovacuum、删除是元数据操作。
/// 下限护栏在数据库侧（`wg_drop_partitions_older_than` 对 `< 90` 直接 RAISE），
/// 应用侧再夹一次，避免手滑传 0 把当周数据删掉。
pub async fn prune_expired_partitions(
    pool: &sqlx::PgPool,
    retention_days: u32,
) -> anyhow::Result<u64> {
    let days = retention_days.max(MIN_PARTITION_RETENTION_DAYS);
    let installed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_proc WHERE proname = 'wg_drop_partitions_older_than')",
    )
    .fetch_one(pool)
    .await?;
    if !installed {
        return Ok(0);
    }
    let dropped: i32 = sqlx::query_scalar("SELECT wg_drop_partitions_older_than($1)")
        .bind(days as i32)
        .fetch_one(pool)
        .await?;
    if dropped > 0 {
        event!(
            Level::INFO,
            "Access log v2: dropped {dropped} whole-week partitions older than {days} days"
        );
    }
    Ok(dropped as u64)
}

/// 该逻辑表的**事件时间列**（主表用各自的事件时间；size 明细表没有事件时间，
/// 只有落库时间 `created_at`）。
///
/// 注意：size 明细表的 `created_at` **不是**搬迁切片该用的列 —— 分区键是 `at_second`，
/// 切片必须用 [`backfill_time_column`]。本函数目前只用于保留期清理等按落库时间的场景。
pub fn time_column(logical: &str) -> &'static str {
    match logical {
        "access_request_logs" => "requested_at",
        "access_response_logs" => "responsed_at",
        _ => "created_at",
    }
}

/// 搬迁/补增量使用的**统一时间基准**（可用于 `>= / <` 比较的 SQL 表达式）。
///
/// 为什么必须统一：四张表的时间列并不相同，而"按天切片复制"与"切换时的补增量"
/// 必须用**同一个**基准，否则时间上会出现缝隙：
///
/// * 主表用事件时间（`requested_at` / `responsed_at`）—— 这正是分区键，
///   也是 `earliest_time()` 取最小值的列；
/// * size 明细表用 `COALESCE(at_second, created_at)` —— 分区键是 `at_second`，
///   历史行的 `at_second` 可能为 NULL（早期库），因此必须回退到 `created_at`。
///
/// **踩过的坑**：size 表的复制原来按 `created_at` 过滤、却按 `at_second` 路由分区；
/// 更糟的是"按 `created_at < cutoff` 切天"会让 `created_at` 落在 cutoff 之后的
/// 累加行永远进不了 v2（这些行的 `at_second` 可能在 cutoff 之前，回填阶段
/// `created_at < cutoff` 不选中、补增量又只回看两天）。实测表现是**静默丢字节**。
pub fn backfill_time_column(logical: &str) -> &'static str {
    if size_id_column(logical).is_some() {
        "COALESCE(at_second, created_at)"
    } else {
        time_column(logical)
    }
}

/// size 明细表的父 id 列；主表返回 `None`。
pub fn size_id_column(logical: &str) -> Option<&'static str> {
    match logical {
        "access_request_size_logs" => Some("request_id"),
        "access_response_size_logs" => Some("response_id"),
        _ => None,
    }
}

/// 四张逻辑表是否**都已经**是分区表（即已切到 v2）。
pub async fn is_switched(pool: &sqlx::PgPool) -> anyhow::Result<bool> {
    for logical in V2_TABLES {
        if !is_partitioned(pool, logical).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

// ==================== v1 → v2 数据搬迁（后台、可续传） ====================
//
// 设计要点都来自 2026-10-02 那次生产事故的教训：
//
// * **绝不放在启动路径**：启动路径只做与表大小无关的结构 DDL（建空的分区表）；
//   数据搬迁是服务起来之后的**后台任务**，按天分批、可续传。
// * **可续传**：进度写在 [`STATE_TABLE`] 里（当前表 + 时间游标 + 已复制行数）。
//   进程被杀/重启后从游标继续，不会像"单事务迁移"那样整体回滚、重来一遍。
// * **幂等**：主表用 `ON CONFLICT DO NOTHING`；size 明细表按 `(id, 秒)` 聚合后写入，
//   重跑同一天不会重复计数。
// * **切换只在最后一刻**：搬迁期间 v1 仍是唯一的读写对象，v2 只是只增的副本。
//   全部搬完之后，才在"暂停刷盘"的保护下补齐增量、换名（见 [`perform_switch`]）。

/// 整周回收的**应用侧**下限（数据库侧还有一道 `p_days < 90` 的 RAISE 护栏）。
///
/// 两侧都夹是刻意的：应用侧挡住"手滑传 0"，数据库侧挡住"绕过应用直接调函数"。
pub const MIN_PARTITION_RETENTION_DAYS: u32 = 90;

/// v1 表在切换后使用的后缀（换名成 `access_request_logs_v1`）。
///
/// 保留一段时间而不是立刻 `DROP`：切换后能立刻核对两边行数，
/// 发现异常还能把查询指回旧表（见 `PRODUCTION_READINESS.md`）。
pub const V1_SUFFIX: &str = "_v1";

/// 迁移进度表（单行）。
pub const STATE_TABLE: &str = "access_log_v2_migration";

/// 进度表阶段：**不做数据搬迁、直接激活 v2**（v1 原样保留为 `*_v1`）。
///
/// 必须与 `done` 区分开：`done` 意味着"搬完了、核对过了"，于是
/// `after_switch_cleanup` 会按宽限期回收 `*_v1`；而本阶段恰恰要**永久保留** v1
/// —— 回收逻辑见到它必须直接返回。
pub const PHASE_ACTIVE_KEEP_V1: &str = "active_keep_v1";

/// 进度表 DDL。`id = 1` 的单行约束保证全局只有一份进度。
const STATE_TABLE_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS access_log_v2_migration (
    id              INT PRIMARY KEY CHECK (id = 1),
    phase           TEXT NOT NULL DEFAULT 'pending',
    cutoff          TIMESTAMPTZ,
    current_table   TEXT,
    cursor_at       TIMESTAMPTZ,
    copied_rows     BIGINT NOT NULL DEFAULT 0,
    last_error      TEXT,
    started_at      TIMESTAMPTZ,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at     TIMESTAMPTZ
);
INSERT INTO access_log_v2_migration (id, phase) VALUES (1, 'pending')
    ON CONFLICT (id) DO NOTHING;
"#;

/// v1 表换名后的名字。
pub fn v1_name(logical: &str) -> String {
    format!("{logical}{V1_SUFFIX}")
}

/// 把一天（`[start, end)`）的 v1 数据复制进对应的 v2 表，返回**写入 v2 的行数**。
///
/// * 主表：直接搬，`ON CONFLICT DO NOTHING`（同一天重复执行不会重复插入）；
/// * size 明细表：按 `(父 id, 秒)` **聚合**后写入。
///   v1 里同一 `(id, 秒)` 有多行（历史按 chunk 插行的遗留）、或 `at_second` 为 NULL
///   （beta/早期库）都要先归并，否则撞 v2 的唯一键。
///   `overwrite = true` 时用 `DO UPDATE` 覆盖为 v1 的完整聚合值 —— 只在切换的补增量阶段
///   使用（那时写入已暂停，v2 的值必须与 v1 完全一致，不能只 `DO NOTHING` 留下旧的部分聚合）。
pub async fn copy_window(
    pool: &sqlx::PgPool,
    logical: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    overwrite: bool,
) -> anyhow::Result<u64> {
    // 源表始终是 v1 历史表（`*_v1` 存在时）或逻辑名（迁移期间还没改名）；
    // 目标表按当前阶段解析：迁移期间是 `access_v2_*`，激活后是逻辑名。
    let src = v1_source_table(pool, logical).await?;
    let dest = live_v2_table(pool, logical).await?;
    let sql = copy_sql_to(logical, &src, &dest, overwrite)?;
    let res = sqlx::query(&sql)
        .bind(start)
        .bind(end)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// 生成一天的复制 SQL（纯函数，便于单测）。
///
/// 抽出来是因为这里的几个坑都"看不出来但必现"：size 表必须聚合、冲突目标必须
/// **同时**包含分区键 `at_second`，覆盖版必须用 `DO UPDATE` 而不是 `DO NOTHING`。
#[cfg(test)]
fn copy_sql(logical: &str, overwrite: bool) -> anyhow::Result<String> {
    copy_sql_to(
        logical,
        logical,
        &v2_physical_name(logical),
        overwrite,
    )
}

/// [`copy_sql`] 的显式源/目标版本（`src` = 读哪张 v1 表，`phys` = 写入哪张 v2 表）。
fn copy_sql_to(
    logical: &str,
    src: &str,
    phys: &str,
    overwrite: bool,
) -> anyhow::Result<String> {
    let tcol = backfill_time_column(logical);
    Ok(match logical {
        "access_request_logs" => format!(
            "INSERT INTO {phys} (id, host, method, path, headers, http_version, remote_addr, \
                                  body_length, created_at, requested_at, website_id) \
             SELECT id, host, method, path, headers, http_version, remote_addr, \
                    body_length, created_at, requested_at, website_id \
               FROM {src} \
              WHERE {tcol} >= $1 AND {tcol} < $2 \
             ON CONFLICT DO NOTHING"
        ),
        "access_response_logs" => format!(
            "INSERT INTO {phys} (id, status, headers, body_length, http_version, created_at, \
                                  responsed_at, backend_responsed_at, website_id) \
             SELECT id, status, headers, body_length, http_version, created_at, \
                    responsed_at, backend_responsed_at, website_id \
               FROM {src} \
              WHERE {tcol} >= $1 AND {tcol} < $2 \
             ON CONFLICT DO NOTHING"
        ),
        size => {
            let id_col =
                size_id_column(size).ok_or_else(|| anyhow::anyhow!("unknown size table: {size}"))?;
            // 冲突目标必须包含分区键（v2 的唯一索引就是 `(id_col, at_second)`）。
            let conflict = if overwrite {
                format!(
                    "ON CONFLICT ({id_col}, at_second) DO UPDATE \
                        SET body_length = EXCLUDED.body_length, \
                            created_at = LEAST({phys}.created_at, EXCLUDED.created_at)"
                )
            } else {
                format!("ON CONFLICT ({id_col}, at_second) DO NOTHING")
            };
            format!(
                "INSERT INTO {phys} (id, {id_col}, body_length, at_second, created_at) \
                 SELECT MIN(v.id), v.{id_col}, COALESCE(SUM(v.body_length), 0)::uint8, \
                        COALESCE(v.at_second, date_trunc('second', v.created_at)), \
                        MIN(v.created_at) \
                   FROM {src} v \
                  WHERE {tcol} >= $1 AND {tcol} < $2 \
                  GROUP BY v.{id_col}, COALESCE(v.at_second, date_trunc('second', v.created_at)) \
                 {conflict}"
            )
        }
    })
}

/// 生成补增量用的"尾部"复制 SQL：**只有下界**（`>= $1`），没有上界。
///
/// 与 [`copy_sql`] 的区别就在这里，而这条区别是**修正一个静默丢数据的缺陷**：
/// 回填按 `[cursor, cutoff)` 切片，因此 `时间列 >= cutoff` 的行走不到；
/// 而"补增量"如果也带上界（原来用 `[cutoff-2d, now)`），当 `now` 与
/// `cutoff` 之间还有更晚的行、或某一行的两个时间列跨越 cutoff 时，
/// 就会留下永远补不上的空洞。
///
/// 无上界是安全的：此刻刷盘已暂停、v1 已冻结，条件只是"所有不早于下界的行"；
/// 主表用 `ON CONFLICT DO NOTHING`、size 表用 `DO UPDATE`，重复执行幂等。
///
/// **调用方必须先暂停写入**（`gateway/src/access.rs` 的刷盘闸门），否则这条无界
/// 语句会在一个长事务里追着新行跑，v1 与 v2 永远追不平。
#[cfg(test)]
fn copy_sql_tail(logical: &str, overwrite: bool) -> anyhow::Result<String> {
    copy_sql_tail_to(
        logical,
        logical,
        &v2_physical_name(logical),
        overwrite,
    )
}

/// [`copy_sql_tail`] 的显式源/目标版本。
fn copy_sql_tail_to(
    logical: &str,
    src: &str,
    phys: &str,
    overwrite: bool,
) -> anyhow::Result<String> {
    let full = copy_sql_to(logical, src, phys, overwrite)?;
    // 把 `>= $1 AND <tcol> < $2` 换成 `>= $1`：少一个绑定参数（调用方也只绑定 1 个）。
    // 用 `replace` 而非重新拼 SQL，保证两种模式除谓词外**逐字一致**（含冲突子句）。
    let tcol = backfill_time_column(logical);
    let needle = format!(" AND {tcol} < $2");
    if !full.contains(&needle) {
        anyhow::bail!("copy_sql 的形状变了，copy_sql_tail 无法安全改写：{full}");
    }
    Ok(full.replace(&needle, ""))
}

/// 复制 `[start, ∞)`：切换时的补增量（**调用方必须先暂停写入**）。
pub async fn copy_tail(
    pool: &sqlx::PgPool,
    logical: &str,
    start: DateTime<Utc>,
    overwrite: bool,
) -> anyhow::Result<u64> {
    let src = v1_source_table(pool, logical).await?;
    let dest = live_v2_table(pool, logical).await?;
    let sql = copy_sql_tail_to(logical, &src, &dest, overwrite)?;
    let res = sqlx::query(&sql).bind(start).execute(pool).await?;
    Ok(res.rows_affected())
}

/// **切换**：补齐增量 → 丢弃旧视图 → 换名（v1 → `*_v1`，v2 →逻辑名）。
///
/// 调用方必须**先暂停访问日志刷盘**（gateway 的刷盘闸门，见 `gateway/src/access.rs`），
/// 否则本函数跑的过程中还会有新行写进 v1，换名后就丢在 `*_v1` 里。
/// 换名本身在一个事务里，中途失败会整体回滚。
pub async fn perform_switch(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    if is_switched(pool).await? {
        return Ok(());
    }
    let status = load_status(pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("迁移进度表不存在，无法切换"))?;
    let cutoff = status
        .cutoff
        .ok_or_else(|| anyhow::anyhow!("迁移尚未开始（没有 cutoff），无法切换"))?;

    // 1) 补增量：从 cutoff 所在那天**再往前多算两天**开始重搬一遍。
    //
    //    为什么要往前多算：size 明细是**按秒累加**的，而一个长时间传输的响应可能在
    //    `cutoff` 之前的那一秒就已经有行、却在 `cutoff` 之后才把最后几个 chunk 累加上去。
    //    如果只补 `[cutoff, now)`，这一秒就会停在"搬迁时看到的旧值"上（少算字节）。
    //    往前两天重算（size 表用覆盖语义）就能把这几天里仍在增长的行修正过来；
    //    主表用 `ON CONFLICT DO NOTHING`，重搬是幂等的。
    //
    //    此刻写入已经暂停 → v1 是冻结的，因此重算出来的就是最终值。
    //
    //    实现上分两类，都**只带下界、不带上界**（见 [`copy_tail`]）：
    //    * 主表：`requested_at / responsed_at >= cutoff`，覆盖回填上界之后到达的行；
    //    * size 表：`COALESCE(at_second, created_at) >= 两天前`，用覆盖语义重算这段窗口，
    //      把"跨 cutoff 仍在增长的秒桶"和"之后才落库的累加行"一并修正。
    let now = db_now(pool).await?;
    let day_start = db_trunc_day(pool, cutoff).await? - ChronoDuration::days(2);
    ensure_history_partitions(pool, day_start, now).await?;
    let mut delta = 0u64;
    for logical in V2_TABLES {
        let start = if size_id_column(logical).is_some() {
            day_start
        } else {
            cutoff
        };
        delta += copy_tail(pool, logical, start, true).await?;
    }
    event!(
        Level::INFO,
        "Access log v2 switch: copied {delta} rows as the final delta (main tables from {cutoff}, \
         size tables from {day_start}, no upper bound)"
    );

    // 2) 换名（v1 留作 `*_v1`，v2 接管逻辑名）。
    rename_v1_to_v2_side(pool).await?;

    mark_phase(pool, "done").await?;
    event!(
        Level::INFO,
        "Access log v2 switch finished: v1 tables renamed to *_v1, v2 tables now serve reads and writes"
    );
    Ok(())
}

/// 换名这一步本身：`access_* → access_*_v1`、`access_v2_* → access_*`。
///
/// 整段在**一个事务**里，中途失败整体回滚，因此不会出现"一半是 v1、一半是 v2"的库。
/// 调用方必须先冻结写入（见 [`perform_switch`] / [`activate_v2_keeping_v1`]）。
async fn rename_v1_to_v2_side(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    // 旧视图绑的是表 OID，换名后会"跟着"指向 `*_v1`，
    // 既容易误导，也会挡住后面的 `DROP TABLE`，因此先删掉。
    // （这三个视图在 v2 里不再需要：QPS 查询本来就已改为直接过滤时间列。）
    let mut tx = pool.begin().await?;
    sqlx::query("DROP VIEW IF EXISTS qps_per_second, qps_per_5s, daily_traffic_by_website")
        .execute(&mut *tx)
        .await?;

    for logical in V2_TABLES {
        if is_partitioned_tx(&mut tx, logical).await? {
            continue; // 已经切过这张
        }
        let phys = v2_physical_name(logical);
        let v1 = v1_name(logical);
        if !table_exists_tx(&mut tx, &phys).await? {
            anyhow::bail!("v2 分区表 {phys} 不存在，无法切换（请确认结构迁移已执行）");
        }
        if table_exists_tx(&mut tx, &v1).await? {
            anyhow::bail!("{v1} 已存在，迁移状态异常，请人工确认后再继续");
        }
        sqlx::query(&format!("ALTER TABLE {logical} RENAME TO {v1}"))
            .execute(&mut *tx)
            .await?;
        sqlx::query(&format!("ALTER TABLE {phys} RENAME TO {logical}"))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// **不做数据搬迁**，直接把 v2 分区表激活成读写对象，v1 原样保留为 `*_v1`。
///
/// 适用场景：历史访问日志可以不要（或以后再用工具慢慢搬）——这时没必要等
/// 1600 万行的搬迁，直接把"新数据写进分区表"这件事打开即可，立刻拿到
/// 按周轮换与整周回收的能力。
///
/// 与 [`perform_switch`] 的区别：
/// * **不做增量补齐**（没有任何已搬迁的数据需要对齐）；
/// * **不做行数/字节核对**（v2 是空的、v1 有数据，核对必然"不一致"，那是预期而非异常）；
/// * 进度表阶段标记为 [`PHASE_ACTIVE_KEEP_V1`]，让"核对并回收 v1"的逻辑**永不触发** ——
///   否则下一次启动就会把 `*_v1` 当成"搬迁遗留"删掉（那正是我们要留下的数据）。
///
/// 调用方必须先冻结写入（第一次运行 v2 表为空，切换瞬间无所谓；但为了与
/// [`perform_switch`] 语义一致，仍由调用方持刷盘闸门）。
pub async fn activate_v2_keeping_v1(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    if is_switched(pool).await? {
        event!(
            Level::INFO,
            "Access log v2 is already active; nothing to do (v1 tables stay as *_v1)"
        );
        return Ok(());
    }
    if load_status(pool).await?.is_none() {
        anyhow::bail!("v2 结构尚未安装（迁移进度表不存在），请先执行 schema 迁移");
    }
    // 这里有**真实数据**、且逻辑名上还是 v1 普通表 —— 是"老库切换"，不是全新库。
    // 全新库的激活在启动迁移里由 `activate_fresh_v2` 完成（见 `inner_init_database_with`）。

    // 兜底分区：v2 的 size 表按 `at_second` 路由，建一张 DEFAULT 分区可以避免
    // 任何"时间落在预建范围之外"的写入整批失败。
    ensure_default_partition(pool).await?;
    ensure_history_partitions(pool, db_now(pool).await?, db_now(pool).await?).await?;

    rename_v1_to_v2_side(pool).await?;
    mark_phase(pool, PHASE_ACTIVE_KEEP_V1).await?;
    event!(
        Level::INFO,
        "Access log v2 activated without data migration: new rows go to the v2 weekly \
         partitions; historical rows stay in {V1_SUFFIX} tables and are NOT auto-dropped"
    );
    Ok(())
}


/// **全新库**：把刚建好的 `access_v2_*` 分区表直接改名成逻辑名，使其立刻可用。
///
/// 背景（2026-10-02 决策）：新部署不再创建 v1 表，只建 v2 分区表。但网关与面板
/// 读写的表名是**逻辑名**（`access_request_logs` 等）——`access_v2_*` 只是"迁移期间
/// 的临时物理名"。因此全新库必须补一步改名，否则服务启动后查不到表。
///
/// 与 [`activate_v2_keeping_v1`] 的区别：这里**没有 v1 可保留**（新库），
/// 所以不写 `*_v1`、也不需要判断归档；纯粹是"把物理名换成逻辑名"。
///
/// 幂等：逻辑名已经是分区表时什么也不做；只有"逻辑名不存在 & 物理名存在"才改名。
/// 顺序上按表逐个判断，因此半途失败的库再跑一次会继续把剩下的换完。
pub async fn activate_fresh_v2(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<bool> {
    let mut renamed = false;
    for logical in V2_TABLES {
        if is_partitioned_tx(tx, logical).await? {
            continue; // 已经是 v2（新库不会有，已切换过的库会走到这里）
        }
        // 逻辑名上存在**普通表**（老库的 v1）时不能动它 —— 那需要走
        // `--activate-v2-keep-v1`（要保留历史数据），不能在这里悄悄改名。
        if table_exists_tx(tx, logical).await? {
            continue;
        }
        let phys = v2_physical_name(logical);
        if !table_exists_tx(tx, &phys).await? {
            continue;
        }
        sqlx::query(&format!("ALTER TABLE {phys} RENAME TO {logical}"))
            .execute(&mut **tx)
            .await?;
        renamed = true;
        event!(
            Level::INFO,
            "Access log v2 (fresh database): renamed {phys} -> {logical}"
        );
    }
    if renamed {
        // 父表换名后，分区名还带着旧父表前缀（`access_v2_req_logs_2026_w39`）。
        // 分区名必须与父表当前名字一致，否则下一次 `wg_ensure_weekly_partition`
        // 会再建一个 `access_request_logs_2026_w39`，两者范围重叠直接报
        // `would overlap partition`（实测踩到）。这里立刻纠正。
        normalize_weekly_partition_names(tx).await?;
    }
    if renamed {
        // 记成"保留 v1"阶段：新库没有 v1，但语义上同属"v2 已激活、不要自动回收旧表"
        // —— 避免 `after_switch_cleanup` 去找一张不存在的 `*_v1` 而报错。
        mark_phase_tx(tx, PHASE_ACTIVE_KEEP_V1).await?;
    }
    Ok(renamed)
}

/// 在给定事务里写迁移阶段（[`mark_phase`] 的事务版本）。
async fn mark_phase_tx(
    tx: &mut Transaction<'_, Postgres>,
    phase: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE access_log_v2_migration \
            SET phase = $1, updated_at = NOW(), \
                finished_at = CASE WHEN $1 = 'done' THEN NOW() ELSE finished_at END \
          WHERE id = 1",
    )
    .bind(phase)
    .execute(&mut **tx)
    .await?;
    Ok(())
}


// ==================== 日汇总表（面板加速） ====================
//
// 为什么需要：面板的"近 N 天汇总"每次都在扫百万行 —— 30 天窗口实测 3.7 秒，
// 而其中最贵的一段是"为了拿到 website_id 而把 size 明细关联回主表"
// （**44 秒**的原始形态，见下）。
//
// 关键观察（实测数据，2026-10-02）：
// * `SUM(body_length) GROUP BY request_id` → 7 ms（按秒累加后行数已很少）
// * `SUM(body_length) GROUP BY response_id` → 1.7 s（1157 万行里取近 30 天）
// * 同样的聚合**再 JOIN 主表拿 website_id** → 44 s ← 真正的问题在这里
//
// 因此汇总表按 `(day, website_id)` 预计算，面板从"扫百万行 + 关联"变成"读几十行"。
//
// 口径说明（会写进文档，避免误读）：
// * `total_ips` 是**每日独立 IP 求和** —— 同一 IP 跨天会被重复计入；它替代不了
//   "整个窗口的精确去重"（那种口径仍走 `get_access_info` 的实时路径）。
// * 只统计 `website_id` 非空的日志（能归到某个站点的）。网关写入时总是带站点，
//   因此实际不丢；真要有 `website_id IS NULL` 的行，它不参与汇总。

/// 汇总表名。
pub const STATS_TABLE: &str = "access_stats_daily";

/// 汇总表里代表"**没有站点归属**"的哨兵值。
///
/// 为什么需要：生产库里 `access_request_logs.website_id IS NULL` 的请求占 **43%**
/// （98 万行，且一直持续到今天）—— 这些日志没有站点归属。汇总表的 `website_id`
/// 是主键的一部分、不能为 NULL，若把这些行排除在外，面板的"近 N 天总请求数"
/// 就会比真实值少一大截（实测 30 天：4.4 万 vs 21.9 万）。
///
/// 因此把它们汇总到这一个桶里：**总数统计包含它**，而按站点展示的查询
/// （站点卡片）用 `website_id <> '__unknown__'` 过滤掉它。
pub const STATS_UNKNOWN_SITE: &str = "__unknown__";

/// 汇总表的建表 DDL（幂等；放在启动路径 —— 建表与表大小无关）。
pub const STATS_TABLE_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS access_stats_daily (
    day                    DATE   NOT NULL,
    website_id             TEXT   NOT NULL,
    total_requests         BIGINT NOT NULL DEFAULT 0,
    total_responses        BIGINT NOT NULL DEFAULT 0,
    total_ips              BIGINT NOT NULL DEFAULT 0,
    e4xx_requests          BIGINT NOT NULL DEFAULT 0,
    e5xx_requests          BIGINT NOT NULL DEFAULT 0,
    backend_error_requests BIGINT NOT NULL DEFAULT 0,
    total_requests_size    uint8  NOT NULL DEFAULT 0,
    total_response_size    uint8  NOT NULL DEFAULT 0,
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (day, website_id)
);
CREATE INDEX IF NOT EXISTS idx_access_stats_daily_day ON access_stats_daily (day);
"#;

/// 安装汇总表结构（幂等）。
pub async fn install_stats_schema(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    for stmt in STATS_TABLE_DDL
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        sqlx::query(stmt).execute(&mut **tx).await?;
    }
    Ok(())
}

/// 重算 `[from_day, to_day]`（闭区间，按**数据库本地日期**）的每日汇总。
///
/// 实现方式是"每天两条聚合 SQL + 一次删除"：先算出当天各站点的指标，再整体替换
/// 该天的行。这样天然幂等（重复跑同一天结果一致），也不怕晚到的日志 ——
/// 只要再次重算那一天即可。
///
/// 之所以不用一条 `INSERT ... SELECT ... GROUP BY date_trunc(...)` 覆盖整个区间：
/// 单条 SQL 要同时关联四张表，正是上面 44 秒那种形态；按天分开跑，每天只碰
/// 3 万~90 万行，且能按天提交、可中断续算。
pub async fn refresh_stats_days(
    pool: &sqlx::PgPool,
    from_day: chrono::NaiveDate,
    to_day: chrono::NaiveDate,
) -> anyhow::Result<u64> {
    if from_day > to_day {
        return Ok(0);
    }
    let mut day = from_day;
    let mut written = 0u64;
    while day <= to_day {
        written += refresh_stats_one_day(pool, day).await?;
        day += ChronoDuration::days(1);
    }
    Ok(written)
}

/// 重算**一天**的汇总，返回写入行数。
pub async fn refresh_stats_one_day(
    pool: &sqlx::PgPool,
    day: chrono::NaiveDate,
) -> anyhow::Result<u64> {
    let mut tx = pool.begin().await?;

    // 先删该天的旧行：删完再插，避免"某站点今天没数据了"留下陈旧行
    // （例如站点被删、或日志被保留期清理）。
    sqlx::query(&format!("DELETE FROM {STATS_TABLE} WHERE day = $1"))
        .bind(day)
        .execute(&mut *tx)
        .await?;

    // 请求侧：计数、独立 IP、4xx/5xx 都在主表上（分周裁剪 + 按站点索引，很快）。
    // 请求字节走 size 表，按 (请求, 秒) 累加后每个请求只有少数几行。
    // 两个方向分别归属（**这是与实时口径对齐的关键**）：
    // * 请求侧（计数 / 独立 IP / 4xx / 5xx / 后端错误 / 请求字节）归到
    //   `requested_at` 那天；
    // * 响应侧（响应字节）归到 `responsed_at` 那天。
    //
    // 为什么不把响应字节塞进请求侧的那条 SELECT：一个响应可能跨零点
    // （请求在 23:59、响应在 00:01），两边日期本来就该不同。早先按
    // "size 行自己的 at_second" 归属，会让同一天的请求数与字节数来自不同的
    // 请求集合，滚动窗口与自然日窗口互相打架（实测某些天少算 59%）。
    // 实测对齐结果：逐日与实时口径**完全相等**（如 2026-09-03 = 19,692,251 字节）。
    let inserted = sqlx::query(&format!(
        "INSERT INTO {STATS_TABLE} (
             day, website_id, total_requests, total_responses, total_ips,
             e4xx_requests, e5xx_requests, backend_error_requests,
             total_requests_size, total_response_size)
         WITH req_side AS (
             SELECT DATE '{day}' AS day,
                    COALESCE(req.website_id, '{STATS_UNKNOWN_SITE}') AS website_id,
                    COUNT(*)::bigint AS total_requests,
                    COUNT(DISTINCT req.remote_addr)::bigint AS total_ips,
                    COUNT(req.id) FILTER (WHERE resp.id IS NULL)::bigint AS backend_error_requests,
                    0::bigint AS total_responses,
                    0::bigint AS e4xx_requests,
                    0::bigint AS e5xx_requests,
                    COALESCE(SUM(req_bytes.bytes), 0)::uint8 AS total_requests_size,
                    0::uint8 AS total_response_size
               FROM access_request_logs req
               LEFT JOIN access_response_logs resp ON resp.id = req.id
               LEFT JOIN (
                    SELECT s.request_id, SUM(s.body_length) AS bytes
                      FROM access_request_size_logs s
                     GROUP BY s.request_id) req_bytes ON req_bytes.request_id = req.id
              WHERE req.requested_at >= DATE '{day}' AND req.requested_at < DATE '{day}' + INTERVAL '1 day'
              GROUP BY 2
         ),
         resp_side AS (
             SELECT DATE '{day}' AS day,
                    COALESCE(resp.website_id, '{STATS_UNKNOWN_SITE}') AS website_id,
                    COUNT(*)::bigint AS total_responses,
                    COUNT(*) FILTER (WHERE resp.status >= 400 AND resp.status <= 499)::bigint AS e4xx_requests,
                    COUNT(*) FILTER (WHERE resp.status >= 500 AND resp.status <= 599)::bigint AS e5xx_requests,
                    COALESCE(SUM(resp_bytes.bytes), 0)::uint8 AS total_response_size
               FROM access_response_logs resp
               LEFT JOIN (
                    SELECT s.response_id, SUM(s.body_length) AS bytes
                      FROM access_response_size_logs s
                     GROUP BY s.response_id) resp_bytes ON resp_bytes.response_id = resp.id
              WHERE resp.responsed_at >= DATE '{day}' AND resp.responsed_at < DATE '{day}' + INTERVAL '1 day'
              GROUP BY 2
         )
         SELECT COALESCE(r.day, p.day),
                COALESCE(r.website_id, p.website_id),
                COALESCE(r.total_requests, 0),
                COALESCE(p.total_responses, 0),
                COALESCE(r.total_ips, 0),
                COALESCE(p.e4xx_requests, 0),
                COALESCE(p.e5xx_requests, 0),
                COALESCE(r.backend_error_requests, 0),
                COALESCE(r.total_requests_size, 0::uint8)::uint8,
                COALESCE(p.total_response_size, 0::uint8)::uint8
           FROM req_side r
           FULL OUTER JOIN resp_side p ON p.website_id = r.website_id"
    ))
    .execute(&mut *tx)
    .await?
    .rows_affected();

    tx.commit().await?;
    Ok(inserted)
}

/// 汇总表里目前覆盖的日期区间（没有数据时返回 `None`）。
pub async fn stats_coverage(
    pool: &sqlx::PgPool,
) -> anyhow::Result<Option<(chrono::NaiveDate, chrono::NaiveDate)>> {
    // 空表时 `MIN/MAX` 返回**一行两个 NULL**（不是零行），因此这里必须按
    // `Option<NaiveDate>` 解码再判断 —— 直接解成 `(NaiveDate, NaiveDate)` 会报
    // `unexpected null; try decoding as an Option`（实测踩到，回填整个失败）。
    let row: (Option<chrono::NaiveDate>, Option<chrono::NaiveDate>) =
        sqlx::query_as(&format!("SELECT MIN(day), MAX(day) FROM {STATS_TABLE}"))
            .fetch_one(pool)
            .await?;
    Ok(match row {
        (Some(min), Some(max)) => Some((min, max)),
        _ => None,
    })
}


// ==================== 日汇总的刷新调度 ====================

/// 每小时刷新时回溯的天数。
///
/// 为什么不是只刷"今天"：访问日志是**批量刷盘**的，一个长时间传输的响应可能在
/// 几小时后才把最后几个 chunk 累加到 `(id, 秒)` 上；跨零点的传输更是会把字节记到
/// 前一天。回溯 3 天能覆盖这种延迟，又不至于每小时重算太多历史。
pub const STATS_REFRESH_LOOKBACK_DAYS: i64 = 3;

/// 新库/首次启用时，启动后回填多少天的历史汇总。
///
/// 不回填全部：按天重算每天要碰几十万行，历史很长时会拖很久。启动时先补最近
/// [`STATS_BACKFILL_DAYS`] 天（覆盖面板默认视图），更早的历史由每小时任务
/// 逐步往前补（每次多补一天），几天内自然补齐。
pub const STATS_BACKFILL_DAYS: i64 = 30;

/// 数据库当前日期（汇总一律按**数据库时区**分天，与面板的 `CURRENT_DATE` 一致）。
pub async fn db_current_date(pool: &sqlx::PgPool) -> anyhow::Result<chrono::NaiveDate> {
    Ok(sqlx::query_scalar("SELECT CURRENT_DATE").fetch_one(pool).await?)
}

/// 刷新"最近 [`STATS_REFRESH_LOOKBACK_DAYS] 天 + 尚未回填的最早一天"。
///
/// 返回本次重算的 `(天数, 行数)`。设计成"每小时只多做一天历史"是为了让首次部署
/// 的长时间回填**自然摊平**在后台，而不是启动时卡住服务。
pub async fn refresh_recent_stats(pool: &sqlx::PgPool) -> anyhow::Result<(u64, u64)> {
    let today = db_current_date(pool).await?;
    let mut from = today - ChronoDuration::days(STATS_REFRESH_LOOKBACK_DAYS);

    // 若汇总的覆盖范围还没到启动回填目标，再往前补一天。
    let target_earliest = today - ChronoDuration::days(STATS_BACKFILL_DAYS);
    if let Some((min_day, _)) = stats_coverage(pool).await?
        && min_day > target_earliest
    {
        from = from.min(min_day - ChronoDuration::days(1));
    }
    // 汇总表完全是空的：一次性补最近的启动回填窗口（首次部署）。
    if stats_coverage(pool).await?.is_none() {
        from = target_earliest;
    }

    let days = (today - from).num_days() as u64 + 1;
    let rows = refresh_stats_days(pool, from, today).await?;
    Ok((days, rows))
}

/// 一张表在切换后的核对结果。
#[derive(Debug, Clone)]
pub struct SwitchVerification {
    pub table: String,
    pub v1_rows: i64,
    pub v2_rows: i64,
    /// size 明细表才有（主表为 `None`）。
    pub v1_bytes: Option<i64>,
    pub v2_bytes: Option<i64>,
    /// 是否通过核对 —— 通过才允许删旧表。
    pub ok: bool,
    /// 核对口径说明（写进日志，避免误读）。
    pub note: &'static str,
}

/// 按周比较两张 size 表的 `body_length` 合计，返回**比 v1 少的那些周**。
///
/// 为什么不能只看总和：总和 `v2 >= v1` 是"全局不亏"，但**搬迁是按时间切片做的**，
/// 一旦切片边界与分区键（`at_second`）不一致，就会出现"某一周少、另一周多"的
/// 局部丢失 —— 总和检查看不出来。实测踩过：size 表按 `created_at` 过滤却按
/// `at_second` 路由分区，`created_at` 落在 cutoff 之后的累加行两周都进不了 v2。
///
/// 返回的每一项是 `(周起始日, v1 字节, v2 字节)`；空表示逐周一致（或有周 v2 更多）。
async fn weekly_size_deficit(
    pool: &sqlx::PgPool,
    logical: &str,
) -> anyhow::Result<Vec<(String, i64, i64)>> {
    // v1 的 `at_second` 可能为 NULL（早期库），与搬迁侧用同一个表达式归并。
    let sql = |table: &str| {
        format!(
            "SELECT to_char(date_trunc('week', COALESCE(at_second, created_at)), \
                            'YYYY-MM-DD') AS wk, \
                    COALESCE(SUM(body_length), 0)::bigint AS bytes \
               FROM {table} GROUP BY 1"
        )
    };
    let mut v1 = BTreeMap::new();
    for r in sqlx::query(&sql(&v1_name(logical))).fetch_all(pool).await? {
        v1.insert(r.get::<String, _>("wk"), r.get::<i64, _>("bytes"));
    }
    let mut v2 = BTreeMap::new();
    for r in sqlx::query(&sql(logical)).fetch_all(pool).await? {
        v2.insert(r.get::<String, _>("wk"), r.get::<i64, _>("bytes"));
    }
    let mut deficit = Vec::new();
    for (wk, b1) in &v1 {
        let b2 = v2.get(wk).copied().unwrap_or(0);
        if b2 < *b1 {
            deficit.push((wk.clone(), *b1, b2));
        }
    }
    Ok(deficit)
}

/// 逐表核对切换结果，返回每张表的对比数据。
///
/// **口径按表区分**（否则会误判）：
/// * 主表：行数 `v2 >= v1`（切换后 v2 会继续收到新行，因此只要求不少于）；
/// * size 明细表：**逐周**字节合计 `v2 >= v1`（见 [`weekly_size_deficit`]）。
///   只比总和会漏掉"某周少、另一周多"的局部丢失 —— 那正是搬迁切片的典型失败形态；
///   行数不能作为依据 —— v1 里同一 `(id, 秒)` 可能有历史重复行（按 chunk 插行的遗留），
///   v2 已按秒归并，行数天然更少，但字节总和必须逐周一致。
pub async fn verify_after_switch(pool: &sqlx::PgPool) -> anyhow::Result<Vec<SwitchVerification>> {
    let mut out = Vec::new();
    for logical in V2_TABLES {
        let v1 = v1_name(logical);
        if !table_exists(pool, &v1).await? {
            continue;
        }
        let v1_rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {v1}"))
            .fetch_one(pool)
            .await?;
        let v2_rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {logical}"))
            .fetch_one(pool)
            .await?;

        let (v1_bytes, v2_bytes, ok, note) = if size_id_column(logical).is_some() {
            let b1: i64 =
                sqlx::query_scalar(&format!("SELECT COALESCE(SUM(body_length), 0)::bigint FROM {v1}"))
                    .fetch_one(pool)
                    .await?;
            let b2: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(body_length), 0)::bigint FROM {logical}"
            ))
            .fetch_one(pool)
            .await?;
            let deficit = weekly_size_deficit(pool, logical).await?;
            if !deficit.is_empty() {
                event!(
                    Level::ERROR,
                    "Access log v2 verify: {logical} 有 {} 周字节数少于 v1（前 5 个：{:?}）—— \
                     说明按时间切片的搬迁漏了行，**不要**删 v1",
                    deficit.len(),
                    &deficit[..deficit.len().min(5)]
                );
            }
            (
                Some(b1),
                Some(b2),
                b2 >= b1 && deficit.is_empty(),
                "按字节总量 + 逐周字节核对（v2 已按秒归并，行数可能更少）",
            )
        } else {
            (None, None, v2_rows >= v1_rows, "按行数核对")
        };

        out.push(SwitchVerification {
            table: logical.to_string(),
            v1_rows,
            v2_rows,
            v1_bytes,
            v2_bytes,
            ok,
            note,
        });
    }
    Ok(out)
}

/// 回收换名后的旧表（`*_v1`）。返回实际删除的表名。
///
/// 用 `DROP TABLE` 而不是 `DELETE`：分区/普通表的文件会被直接删除，
/// 磁盘空间立刻归还操作系统 —— 生产上搬迁期间 v1 与 v2 并存约需 2 倍空间，
/// 这一步就是把那份额外空间收回来。
pub async fn drop_v1_tables(pool: &sqlx::PgPool) -> anyhow::Result<Vec<String>> {
    let mut dropped = Vec::new();
    // size 明细在前，避免外键顺序问题（单条 DROP 也能处理，这里更直观）。
    for logical in V2_TABLES.iter().rev() {
        let v1 = v1_name(logical);
        if table_exists(pool, &v1).await? {
            sqlx::query(&format!("DROP TABLE IF EXISTS {v1}"))
                .execute(pool)
                .await?;
            dropped.push(v1);
        }
    }
    if !dropped.is_empty() {
        event!(Level::INFO, "Access log v2: dropped v1 tables {dropped:?}");
    }
    Ok(dropped)
}


// ==================== v1 → v2 历史数据搬迁工具（mnt 触发、后端执行） ====================
//
// 与 `gateway --migrate-v2` 的区别：
// * 本工具在**已经激活 v2 之后**运行，目标是"把 *_v1 里的历史数据补搬进当前 v2 表"；
// * 因此它**不与 v1 抢读写**（v1 早已冻结，不再有新行），也不需要冻结刷盘闸门；
// * 它**绝不回收 v1** —— 搬完由人去核对，确认后才手工 DROP。
//
// 幂等与可续跑沿用同一套机制：进度写在 `access_log_v2_migration`，主表
// `ON CONFLICT DO NOTHING`、size 表按 `(id, 秒)` 聚合后 `DO UPDATE` 覆盖。

/// 单个批次（一天）的复制结果。
#[derive(Debug, Clone)]
pub struct BatchProgress {
    /// 当前表（逻辑名）。
    pub table: String,
    /// 当前批次结束的游标时间。
    pub cursor: DateTime<Utc>,
    /// 本批写入行数。
    pub batch_rows: u64,
    /// 累计写入行数。
    pub copied_rows: i64,
    /// 本次搬迁需要处理的总行数（v1 四表的行数之和，用于算百分比）。
    pub total_rows: i64,
}

/// v1 四表当前的**总行数**（进度分母）。
///
/// count(*) 在千万行表上要一两秒，只在开始时算一次，不进批次循环。
pub async fn v1_total_rows(pool: &sqlx::PgPool) -> anyhow::Result<i64> {
    let mut total = 0i64;
    for logical in V2_TABLES {
        let v1 = v1_name(logical);
        if !table_exists(pool, &v1).await? {
            continue;
        }
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {v1}"))
            .fetch_one(pool)
            .await?;
        total += n;
    }
    Ok(total)
}

/// v2 四表当前的**总行数**（用于搬完后核对）。
pub async fn v2_total_rows(pool: &sqlx::PgPool) -> anyhow::Result<i64> {
    let mut total = 0i64;
    for logical in V2_TABLES {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {logical}"))
            .fetch_one(pool)
            .await?;
        total += n;
    }
    Ok(total)
}

/// 把 `*_v1` 里的历史数据分批搬进当前 v2 表；`progress` 在每个批次后被调用。
///
/// * `reset = true`：先清空 v2 四表（`TRUNCATE`）并重置进度，从 v1 最早时间重搬；
/// * `reset = false`：从进度表游标**续跑**（没有进度就从头开始）。
///
/// 返回：累计写入行数。
pub async fn copy_v1_into_v2<F>(pool: &sqlx::PgPool, reset: bool, mut progress: F) -> anyhow::Result<i64>
where
    F: FnMut(BatchProgress),
{
    // 1) 前置检查：必须有 v1 表（否则没东西可搬）。
    let mut missing = Vec::new();
    for logical in V2_TABLES {
        if !table_exists(pool, &v1_name(logical)).await? {
            missing.push(v1_name(logical));
        }
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "找不到 v1 历史表：{missing:?}。本工具用于「已激活 v2 且 v1 改名为 *_v1」的库"
        );
    }

    // 进度分母：本次真正要处理的四表行数（用于百分比显示，不影响正确性）。
    let total_rows = v1_total_rows(pool).await?;
    if total_rows == 0 {
        return Ok(0);
    }

    if reset {
        // 清空 v2：整表 TRUNCATE 是元数据级操作，比逐行 DELETE 快几个数量级。
        let list = V2_TABLES.join(", ");
        sqlx::query(&format!("TRUNCATE {list}"))
            .execute(pool)
            .await?;
        sqlx::query(
            "UPDATE access_log_v2_migration \
                SET cutoff = NULL, current_table = NULL, cursor_at = NULL, copied_rows = 0, \
                    last_error = NULL, updated_at = NOW() \
              WHERE id = 1",
        )
        .execute(pool)
        .await?;
    }

    // 2) 起点与终点：v1 里最早/最晚的时间。
    let min = match load_status(pool).await? {
        Some(st) if st.cursor_at.is_some() && !reset => st.cursor_at.unwrap(),
        _ => v1_earliest_time(pool).await?.unwrap_or_else(|| db_now_unwrap(pool)),
    };
    // 切片是**左闭右开** `[cursor, next)`，而"最晚的那一行"时间戳恰好等于上界，
    // 半开区间会把它排除在外 —— 表现为"v1 比 v2 多 1 行"，且续跑时因为
    // cursor == cutoff 直接判定"已到终点"，永远补不上（实测踩到）。
    // 因此把上界往后挪 1 微秒：时间戳精度就是微秒，1µs 足以包含等于最晚时间的行，
    // 又不会把任何真实数据排除在外。
    let cutoff = v1_latest_time(pool)
        .await?
        .map(|t| t + chrono::Duration::microseconds(1))
        .unwrap_or_else(|| db_now_unwrap(pool));

    // 3) 历史跨很多周：先把这些周的分区建好（建空分区是毫秒级元数据操作）。
    ensure_history_partitions(pool, min, cutoff).await?;
    // 兜底分区：size 表按 at_second 路由，历史行可能有 NULL/越界。
    ensure_default_partition(pool).await?;

    let mut copied = match load_status(pool).await? {
        Some(st) if !reset => st.copied_rows,
        _ => 0,
    };

    if min >= cutoff {
        // 没有任何历史可搬（v1 为空或全部晚于游标）。
        return Ok(copied);
    }

    // 4) 按天切片、逐表推进。表顺序与 gateway 的搬迁一致（先主表、再 size 明细）。
    //
    //    续跑语义：进度表里 `cursor_at` 是"已搬到哪里"的高水位，`current_table` 是
    //    当前表。**不能**把每张表的起点都重置成 `min` —— 那会在续跑时把已经搬完的
    //    表再扫一遍（幂等但白费时间），也无法表达"这张表搬完了"。
    let status = load_status(pool).await?;
    let done_table = status.as_ref().and_then(|s| s.current_table.clone());
    let saved_cursor = status.as_ref().and_then(|s| s.cursor_at);
    let mut cursor = min;
    for logical in V2_TABLES {
        if !reset
            && let Some(done) = &done_table
            && let Some(pos) = V2_TABLES.iter().position(|t| t == done)
            && V2_TABLES.iter().position(|t| t == logical).unwrap_or(0) < pos
        {
            // 这张表在上一轮已经搬完（进度表的 current_table 已经走到它后面）。
            continue;
        }
        let mut table_cursor = if !reset && done_table.as_deref() == Some(logical) {
            saved_cursor.unwrap_or(cursor)
        } else {
            cursor
        };
        while table_cursor < cutoff {
            let next = (table_cursor + ChronoDuration::days(1)).min(cutoff);
            let written = copy_window(pool, logical, table_cursor, next, true).await?;
            copied += written as i64;
            table_cursor = next;
            save_progress(pool, logical, table_cursor, copied).await?;
            progress(BatchProgress {
                table: logical.to_string(),
                cursor: table_cursor,
                batch_rows: written,
                copied_rows: copied,
                total_rows,
            });
        }
        cursor = min; // 下一张表从同一个起点开始
    }
    Ok(copied)
}

/// v1 四表里最早的一条记录时间（搬迁起点）。
pub async fn v1_earliest_time(pool: &sqlx::PgPool) -> anyhow::Result<Option<DateTime<Utc>>> {
    let v: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT LEAST( \
             (SELECT min(requested_at) FROM access_request_logs_v1), \
             (SELECT min(responsed_at) FROM access_response_logs_v1), \
             (SELECT min(COALESCE(at_second, created_at)) FROM access_request_size_logs_v1), \
             (SELECT min(COALESCE(at_second, created_at)) FROM access_response_size_logs_v1))",
    )
    .fetch_one(pool)
    .await?;
    Ok(v)
}

/// v1 四表里最晚的一条记录时间（搬迁终点）。
pub async fn v1_latest_time(pool: &sqlx::PgPool) -> anyhow::Result<Option<DateTime<Utc>>> {
    let v: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT GREATEST( \
             (SELECT max(requested_at) FROM access_request_logs_v1), \
             (SELECT max(responsed_at) FROM access_response_logs_v1), \
             (SELECT max(COALESCE(at_second, created_at)) FROM access_request_size_logs_v1), \
             (SELECT max(COALESCE(at_second, created_at)) FROM access_response_size_logs_v1))",
    )
    .fetch_one(pool)
    .await?;
    Ok(v)
}

/// 内部用：拿一个"现在"，失败就退化成进程时间（仅用于切片上界兜底）。
fn db_now_unwrap(_pool: &sqlx::PgPool) -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_key_matches_v1_column_names() {
        // 关键：v2 的分区键必须沿用 v1 的列名，否则查询侧要跟着改一遍。
        assert_eq!(partition_key("access_request_logs"), "requested_at");
        assert_eq!(partition_key("access_response_logs"), "responsed_at");
        assert_eq!(partition_key("access_request_size_logs"), "at_second");
        assert_eq!(partition_key("access_response_size_logs"), "at_second");
    }

    #[test]
    fn every_v2_table_is_partitioned_without_foreign_keys() {
        for logical in V2_TABLES {
            let ddl = v2_ddl_for(logical);
            assert!(
                ddl.contains("PARTITION BY RANGE"),
                "{logical} 必须是分区表，否则'按周轮换'无从谈起"
            );
            assert!(
                !ddl.contains("REFERENCES"),
                "{logical} 不允许有外键：分区表无法被单列 id 引用"
            );
            assert!(
                ddl.contains("PRIMARY KEY (id,"),
                "{logical} 的主键必须包含分区键"
            );
            assert!(
                ddl.contains(&format!("PARTITION BY RANGE ({})", partition_key(logical))),
                "{logical} 的分区键必须与 partition_key() 的约定一致"
            );
            assert!(
                ddl.contains(&v2_physical_name(logical)),
                "{logical} 的 DDL 必须建在物理名上"
            );
        }
    }

    #[test]
    fn size_tables_keep_per_second_unique_index() {
        // 第七轮建立的"按秒累加"语义必须在 v2 里保留，否则又会回到按 chunk 膨胀。
        // v2 里唯一键是 (id, at_second) 且分区键也是 at_second，
        // 因此唯一索引天然覆盖所有行、无需再加 WHERE 谓词。
        for logical in ["access_request_size_logs", "access_response_size_logs"] {
            let ddl = v2_ddl_for(logical);
            let uniq_col = if logical.contains("request") {
                "request_id"
            } else {
                "response_id"
            };
            assert!(
                ddl.contains(&format!("({uniq_col}, at_second)")),
                "{logical} 必须保留按秒的唯一键"
            );
        }
    }

    #[test]
    fn physical_names_are_distinct_from_logical_ones() {
        for logical in V2_TABLES {
            assert_ne!(
                v2_physical_name(logical),
                *logical,
                "v2 必须用独立物理名，否则建表会与 v1 撞名"
            );
        }
    }

    #[test]
    fn time_column_and_size_id_column_are_consistent() {
        assert_eq!(time_column("access_request_logs"), "requested_at");
        assert_eq!(time_column("access_response_logs"), "responsed_at");
        assert_eq!(time_column("access_request_size_logs"), "created_at");
        assert_eq!(time_column("access_response_size_logs"), "created_at");
        assert_eq!(
            size_id_column("access_request_size_logs"),
            Some("request_id")
        );
        assert_eq!(
            size_id_column("access_response_size_logs"),
            Some("response_id")
        );
        assert_eq!(size_id_column("access_request_logs"), None);
    }

    /// 搬迁的时间基准必须与**分区键**一致，否则按天切片会漏掉跨边界的行。
    ///
    /// size 表的分区键是 `at_second`（历史行可能为 NULL），因此基准是
    /// `COALESCE(at_second, created_at)`；主表就是各自的事件时间。
    #[test]
    fn backfill_time_column_matches_partition_key() {
        assert_eq!(
            backfill_time_column("access_request_logs"),
            partition_key("access_request_logs")
        );
        assert_eq!(
            backfill_time_column("access_response_logs"),
            partition_key("access_response_logs")
        );
        for size in ["access_request_size_logs", "access_response_size_logs"] {
            assert_eq!(
                backfill_time_column(size),
                "COALESCE(at_second, created_at)",
                "{size} 必须按分区键（at_second，含 NULL 回退）切片"
            );
        }
    }

    /// 复制 SQL 的过滤列必须是 [`backfill_time_column`]，而不是 `created_at`。
    #[test]
    fn copy_sql_filters_on_the_backfill_time_column() {
        for logical in V2_TABLES {
            let sql = copy_sql(logical, false).unwrap();
            let tcol = backfill_time_column(logical);
            assert!(
                sql.contains(&format!("WHERE {tcol} >= $1 AND {tcol} < $2")),
                "{logical} 的切片列必须是 {tcol}：{sql}"
            );
        }
        // size 表尤其不能再用 `v.created_at` 过滤（那正是丢数据的那一版）。
        let sql = copy_sql("access_response_size_logs", false).unwrap();
        assert!(
            !sql.contains("WHERE v.created_at"),
            "size 表的切片列不允许回到 created_at：{sql}"
        );
    }

    /// 补增量必须是"只有下界"的尾部复制，且**除谓词外**与按天复制逐字一致。
    #[test]
    fn tail_copy_has_no_upper_bound_and_keeps_everything_else() {
        for logical in V2_TABLES {
            for overwrite in [false, true] {
                let window = copy_sql(logical, overwrite).unwrap();
                let tail = copy_sql_tail(logical, overwrite).unwrap();
                let tcol = backfill_time_column(logical);
                assert!(
                    tail.contains(&format!("WHERE {tcol} >= $1")),
                    "{logical} 的补增量必须以下界开始：{tail}"
                );
                assert!(
                    !tail.contains("$2"),
                    "{logical} 的补增量不允许再有上界（否则会留下补不上的空洞）：{tail}"
                );
                // 冲突子句、目标表、列清单都必须原样保留。
                let strip = |s: &str| s.replace(&format!(" AND {tcol} < $2"), "");
                assert_eq!(
                    tail,
                    strip(&window),
                    "{logical} 的两种模式除谓词外必须一致"
                );
            }
        }
    }

    /// 周分区名必须由**父表当前名字**推导，且切换前后都是同一种干净命名。
    ///
    /// 回归：早先 `wg_ensure_weekly_partition` 直接拿父表名当基础名，而调用方
    /// （`wg_access_partition_parents`）返回的是**逻辑名** —— 切换后仍会去建
    /// `access_request_logs_p_2026_w40`，与既有分区重叠，报
    /// `partition "..." would overlap partition "..."`，启动即失败（实测踩到）。
    #[test]
    fn weekly_partition_names_strip_the_v2_suffix() {
        assert!(
            FN_ENSURE_WEEKLY.contains("regexp_replace(p_parent, '_p$', '')"),
            "必须先把父表名里的 `_p` 去掉再拼分区名"
        );
        // 两种父表名必须推出同一个基础名。
        let base = |parent: &str| parent.strip_suffix("_p").unwrap_or(parent).to_string();
        assert_eq!(base("access_request_logs_p"), "access_request_logs");
        assert_eq!(base("access_request_logs"), "access_request_logs");
        // 规范化时的改名目标：`..._p_2026_w40` → `..._2026_w40`。
        assert_eq!(
            "access_request_logs_p_2026_w40".replacen("_p_", "_", 1),
            "access_request_logs_2026_w40"
        );
        // DEFAULT 分区同样要去掉 `_p_`。
        assert_eq!(
            "access_response_size_logs_p_default".replacen("_p_", "_", 1),
            "access_response_size_logs_default"
        );
    }

    /// 兜底分区**不能**被周轮换误删。
    ///
    /// `wg_drop_partitions_older_than` 靠分区名里的 `_<IYYY>_w<IW>` 解析出所属周；
    /// 解析失败必须 `CONTINUE`（跳过），否则 `<父表>_default` 这类名字会被当成
    /// "解析不出就删"或"解析成 NULL 就删"。这里断言兜底分区的命名约定与轮换的
    /// 保守行为同时成立。
    #[test]
    fn default_partition_is_not_matched_by_week_rotation() {
        let default_name = format!("{}_default", v2_physical_name("access_request_logs"));
        assert!(
            !default_name.contains("_w"),
            "兜底分区名不能长得像周分区，否则会被整周回收删掉：{default_name}"
        );
        // 轮换函数的实际行为：解析不出来就 CONTINUE（不删）。
        assert!(
            FN_DROP_OLD.contains("CONTINUE"),
            "解析不出的分区必须跳过，绝不误删"
        );
        assert!(
            FN_DROP_OLD.contains("v_start IS NULL"),
            "解析结果为 NULL 的分区也必须跳过"
        );
        assert!(
            FN_DROP_OLD.contains("v_start + 7 <= v_cutoff"),
            "只有整周都早于截止日才允许删"
        );
    }


    #[test]
    fn v1_name_is_the_logical_name_plus_suffix() {
        assert_eq!(v1_name("access_request_logs"), "access_request_logs_v1");
        for logical in V2_TABLES {
            assert_ne!(v1_name(logical), *logical);
            assert_ne!(v1_name(logical), v2_physical_name(logical));
        }
    }

    /// 主表的复制必须是**不带冲突目标**的 `ON CONFLICT DO NOTHING`。
    ///
    /// v1 的主键是 `(id)`，v2 的主键是 `(id, 时间列)` —— 带目标的
    /// `ON CONFLICT (id)` 在分区表上推断不出索引（分区表的唯一索引必须含分区键），
    /// 换名之后刷盘会整批失败。不带目标则两种表都成立。
    #[test]
    fn main_table_copy_conflicts_without_target() {
        for logical in ["access_request_logs", "access_response_logs"] {
            let sql = copy_sql(logical, false).unwrap();
            assert!(
                sql.contains("ON CONFLICT DO NOTHING"),
                "{logical} 必须用不带目标的 ON CONFLICT，才能同时适配 v1 与 v2：{sql}"
            );
            assert!(
                !sql.contains("ON CONFLICT ("),
                "{logical} 不允许带冲突目标（v2 分区表推断不出 `(id)`）：{sql}"
            );
            assert!(
                sql.contains(&v2_physical_name(logical)),
                "{logical} 必须写入 v2 物理表"
            );
        }
    }

    /// size 明细表必须**先聚合再写**，且冲突目标包含分区键 `at_second`。
    ///
    /// v1 里同一 `(id, 秒)` 可能有多行（按 chunk 插行的遗留），v2 的唯一索引是
    /// `(xxx_id, at_second)`；不聚合就会在同一批里撞唯一键。
    #[test]
    fn size_table_copy_aggregates_and_conflicts_on_partition_key() {
        for (logical, id_col) in [
            ("access_request_size_logs", "request_id"),
            ("access_response_size_logs", "response_id"),
        ] {
            let sql = copy_sql(logical, false).unwrap();
            assert!(sql.contains("GROUP BY"), "{logical} 必须先按秒聚合：{sql}");
            assert!(
                sql.contains(&format!("ON CONFLICT ({id_col}, at_second) DO NOTHING")),
                "{logical} 的冲突目标必须同时包含父 id 与分区键：{sql}"
            );
            assert!(
                sql.contains("COALESCE(v.at_second, date_trunc('second', v.created_at))"),
                "{logical} 必须兼容历史 `at_second` 为 NULL 的行：{sql}"
            );
            assert!(sql.contains("MIN(v.id)"), "{logical} 需要为聚合行挑一个 id");
            assert!(sql.contains("::uint8"), "SUM 返回 numeric，必须转回 uint8");
        }
    }

    /// 补增量阶段必须用 `DO UPDATE` 覆盖成 v1 的完整聚合值。
    #[test]
    fn size_table_overwrite_updates_instead_of_ignoring() {
        let sql = copy_sql("access_response_size_logs", true).unwrap();
        assert!(
            sql.contains("ON CONFLICT (response_id, at_second) DO UPDATE"),
            "覆盖版必须 DO UPDATE，否则早先的部分聚合会留下：{sql}"
        );
        assert!(sql.contains("EXCLUDED.body_length"));
    }

    /// 分区维护函数必须**动态解析父表名**，否则换名之后分区轮换静默失效。
    ///
    /// 名单现在由 [`partition_parent_candidates`] 统一给出：迁移期间是
    /// `access_v2_*`（新命名）/ `access_*_p`（老命名），切换后是逻辑名 `access_*`。
    /// 三种都要认 —— 少一种，换名之后 `wg_ensure_upcoming_weeks` 就会
    /// "一个分区都建不出来"（本项目反复踩过）。
    #[test]
    fn partition_functions_resolve_parents_dynamically() {
        assert!(
            FN_ENSURE_UPCOMING.contains("wg_access_partition_parents()"),
            "预建分区必须动态解析父表"
        );
        assert!(
            FN_DROP_OLD.contains("wg_access_partition_parents()"),
            "整周回收必须动态解析父表"
        );
        let parents = fn_partition_parents();
        for name in [
            // 逻辑名（切换后）
            "access_request_logs",
            "access_response_logs",
            "access_request_size_logs",
            "access_response_size_logs",
            // 新物理名（迁移期间）
            "access_v2_req_logs",
            "access_v2_resp_logs",
            "access_v2_req_size_logs",
            "access_v2_resp_size_logs",
            // 老物理名（历史遗留）
            "access_request_logs_p",
            "access_response_size_logs_p",
        ] {
            assert!(parents.contains(name), "父表解析必须包含 {name}：{parents}");
        }
        assert!(
            parents.contains("c.relkind = 'p'"),
            "只允许把分区表当作父表"
        );
        assert!(FN_DROP_OLD.contains("p_days < 90"), "回收下限保护不能丢");
    }

    /// 进度表必须是单行（`id = 1`），否则并发启动会写出多份互不相同的进度。
    #[test]
    fn state_table_is_singleton() {
        assert!(STATE_TABLE_DDL.contains("CHECK (id = 1)"));
        assert!(STATE_TABLE_DDL.contains("ON CONFLICT (id) DO NOTHING"));
        assert!(STATE_TABLE_DDL.contains(STATE_TABLE));
    }
}
