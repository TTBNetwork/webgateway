//! 把 PostgreSQL 里的大表**流式**复制到另一个库（生产 → beta 演练用）。
//!
//! 为什么需要它：本机/容器里没有 `psql` / `pg_dump`，而我们要把生产的四张访问日志表
//! （约 2000 万行 / 6.3 GB）全量搬到 beta，好让 v1→v2 的自动迁移按**生产量级**演练。
//!
//! 实现用 PostgreSQL 的 **binary COPY**：`COPY (SELECT ...) TO STDOUT` 的输出**原样**喂给
//! 目标库的 `COPY ... FROM STDIN`，两端列类型一致时不会做任何解析 —— 全程流式，
//! 内存占用与表大小无关。
//!
//! ```bash
//! # 只看结构（列/类型/行数），不搬数据
//! cargo run -p shared --example pg_sync -- --from <SRC> --to <DST> --inspect
//!
//! # 全量搬迁（先清空目标表，再按外键顺序搬）
//! cargo run -p shared --example pg_sync -- --from <SRC> --to <DST> --truncate
//! ```
//!
//! 注意：本工具**只用于演练/运维**，不属于产品运行路径。它不做任何凭据管理，
//! 连接串一律从命令行传入（别写进仓库）。

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use futures::StreamExt;
use sqlx::postgres::{PgPoolCopyExt, PgPoolOptions};
use sqlx::{Pool, Postgres, Row};

/// 默认按**外键安全顺序**复制：先请求、再响应、最后两张 size 明细。
const DEFAULT_TABLES: &[&str] = &[
    "access_request_logs",
    "access_response_logs",
    "access_request_size_logs",
    "access_response_size_logs",
];

/// 复制的"根"表（其余表都要引用它）。
///
/// 源库是**在线**的，因此四张表共用**同一组** `since` / `before` 边界（在开始复制前
/// 一次性算好）：请求表按 `requested_at` 切、响应表按 `responsed_at` 切，子表的
/// `EXISTS` 复述父表的同一组条件。见 [`source_where`] 的说明。
const ROOT_TABLE: &str = "access_request_logs";

struct Args {
    from: String,
    to: String,
    tables: Vec<String>,
    truncate: bool,
    inspect: bool,
    /// `--before <RFC3339>`：子表只复制 `created_at < before` 的行（见 [`ROOT_TABLE`]）。
    before: Option<chrono::DateTime<chrono::Utc>>,
    /// `--since <RFC3339>`：只复制 `created_at >= since` 的行（四张表都适用）。
    since: Option<chrono::DateTime<chrono::Utc>>,
    /// `--days <N>`：只复制最近 N 天（等价于 `--since <源库 NOW() - N 天>`）。
    days: Option<i64>,
    /// `--sql`：在 `--on` 指定的库上执行（可多条语句，不返回行）。
    sql: Option<String>,
    /// `--query`：在 `--on` 指定的库上查询，每行以 JSON 打印（类型安全、无需猜列类型）。
    query: Option<String>,
    /// `--sql` / `--query` 的目标：`src` 或 `dst`（默认 dst）。
    on_src: bool,
    /// `--where '<SQL 谓词>'`：给单表复制追加一个自定义条件（默认对所有表生效）。
    ///
    /// 用途：把某一行单独搬过去做对照（排查"为什么会漏行"），而不是重搬整表。
    extra_where: Option<String>,
}

fn parse_args() -> Args {
    let mut from = None;
    let mut to = None;
    let mut tables = DEFAULT_TABLES.iter().map(|s| s.to_string()).collect();
    let mut truncate = false;
    let mut inspect = false;
    let mut before = None;
    let mut since = None;
    let mut days = None;
    let mut sql = None;
    let mut query = None;
    let mut on_src = false;
    let mut extra_where = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" => from = it.next(),
            "--to" => to = it.next(),
            "--tables" => {
                tables = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "--truncate" => truncate = true,
            "--inspect" => inspect = true,
            "--days" => {
                days = Some(it.next().unwrap_or_default().parse::<i64>().unwrap_or_else(|_| {
                    eprintln!("--days 需要整数");
                    std::process::exit(2);
                }))
            }
            "--since" => {
                let raw = it.next().unwrap_or_default();
                since = Some(parse_rfc3339("--since", &raw));
            }
            "--before" => {
                let raw = it.next().unwrap_or_default();
                before = Some(parse_rfc3339("--before", &raw));
            }
            "--sql" => sql = it.next(),
            "--query" => query = it.next(),
            "--on" => on_src = it.next().as_deref() == Some("src"),
            "--where" => extra_where = it.next(),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let from = from.or_else(|| std::env::var("SRC_DATABASE_URL").ok());
    let to = to.or_else(|| std::env::var("DST_DATABASE_URL").ok());
    let (Some(from), Some(to)) = (from, to) else {
        eprintln!(
            "usage: pg_sync --from <SRC_URL> --to <DST_URL> [--tables a,b,c] [--truncate] [--inspect]\n\
             \x20      pg_sync --from <SRC> --to <DST> [--before <RFC3339>] --truncate\n\
             \x20      pg_sync --from <SRC> --to <DST> --sql '<DDL/DML>' [--on src|dst]\n\
             \x20      pg_sync --from <SRC> --to <DST> --query '<SELECT>' [--on src|dst]"
        );
        std::process::exit(2);
    };
    Args {
        from,
        to,
        tables,
        truncate,
        inspect,
        before,
        since,
        days,
        sql,
        query,
        on_src,
        extra_where,
    }
}

fn parse_rfc3339(flag: &str, raw: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| {
            eprintln!("{flag} 必须是 RFC3339 时间（如 2026-10-02T09:50:00Z）: {e}");
            std::process::exit(2);
        })
        .with_timezone(&chrono::Utc)
}

/// 读取一张表的列名 → 类型（`format_type`，例如 `timestamp with time zone`）。
async fn columns_of(
    pool: &Pool<Postgres>,
    table: &str,
) -> anyhow::Result<BTreeMap<String, String>> {
    let rows = sqlx::query(
        "SELECT a.attname AS name, format_type(a.atttypid, a.atttypmod) AS ty \
           FROM pg_attribute a \
           JOIN pg_class c ON c.oid = a.attrelid \
           JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'public' AND c.relname = $1 \
            AND a.attnum > 0 AND NOT a.attisdropped \
          ORDER BY a.attnum",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("name"), r.get::<String, _>("ty")))
        .collect())
}

async fn approx_rows(pool: &Pool<Postgres>, table: &str) -> anyhow::Result<i64> {
    let n: Option<i64> = sqlx::query_scalar(
        "SELECT c.reltuples::bigint FROM pg_class c \
           JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'public' AND c.relname = $1",
    )
    .bind(table)
    .fetch_optional(pool)
    .await?;
    Ok(n.unwrap_or(-1))
}

/// 两端列的**交集**，且类型必须一致（binary COPY 不做类型转换）。
fn common_columns(
    src: &BTreeMap<String, String>,
    dst: &BTreeMap<String, String>,
    table: &str,
) -> anyhow::Result<Vec<String>> {
    let mut cols = Vec::new();
    for (name, src_ty) in src {
        match dst.get(name) {
            Some(dst_ty) if dst_ty == src_ty => cols.push(name.clone()),
            Some(dst_ty) => anyhow::bail!(
                "{table}: 列 `{name}` 类型不一致（src={src_ty}, dst={dst_ty}），\
                 binary COPY 无法直传，请先对齐 schema"
            ),
            None => eprintln!("  ! {table}: 目标库缺少列 `{name}`，跳过该列"),
        }
    }
    if cols.is_empty() {
        anyhow::bail!("{table}: 没有可复制的共同列");
    }
    Ok(cols)
}

async fn inspect(args: &Args, src: &Pool<Postgres>, dst: &Pool<Postgres>) -> anyhow::Result<()> {
    for table in &args.tables {
        println!("== {table} ==");
        let sc = columns_of(src, table).await?;
        let dc = columns_of(dst, table).await?;
        let sr = approx_rows(src, table).await?;
        let dr = approx_rows(dst, table).await?;
        println!("  src ~{sr} 行 / dst ~{dr} 行");
        for (name, src_ty) in &sc {
            match dc.get(name) {
                Some(dst_ty) if dst_ty == src_ty => println!("  {name:<22} {src_ty}"),
                Some(dst_ty) => println!("  {name:<22} {src_ty}   <== dst 是 {dst_ty}，类型不一致！"),
                None => println!("  {name:<22} {src_ty}   <== dst 缺少此列"),
            }
        }
        for name in dc.keys() {
            if !sc.contains_key(name) {
                println!("  {name:<22} (仅 dst 有)");
            }
        }
    }
    Ok(())
}

async fn copy_table(
    src: &Pool<Postgres>,
    dst: &Pool<Postgres>,
    table: &str,
    before: Option<chrono::DateTime<chrono::Utc>>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    extra_where: Option<&str>,
) -> anyhow::Result<(u64, Duration)> {
    let sc = columns_of(src, table).await?;
    let dc = columns_of(dst, table).await?;
    let cols = common_columns(&sc, &dc, table)?;
    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let mut where_clause = source_where(table, since, before);
    if let Some(extra) = extra_where {
        // 与自定义谓词合并（自定义谓词用 AND 接在后面，便于"再收窄一层"）。
        if where_clause.is_empty() {
            where_clause = format!(" WHERE ({extra})");
        } else {
            where_clause = format!("{where_clause} AND ({extra})");
        }
    }
    if !where_clause.is_empty() {
        println!("   （过滤条件：{}）", where_clause.trim_start_matches(" WHERE "));
    }
    let source_query = format!(
        "COPY (SELECT {col_list} FROM \"{table}\"{where_clause}) TO STDOUT (FORMAT binary)"
    );

    let total = approx_rows(src, table).await?;
    println!("== {table} ==  源表 ~{total} 行，复制 {} 列", cols.len());

    let started = Instant::now();
    let mut out = src.copy_out_raw(&source_query).await?;
    let mut inw = dst
        .copy_in_raw(&format!(
            "COPY \"{table}\" ({col_list}) FROM STDIN (FORMAT binary)"
        ))
        .await?;

    let mut bytes: u64 = 0;
    let mut last_report = Instant::now();
    while let Some(chunk) = out.next().await {
        let chunk = chunk?;
        bytes += chunk.len() as u64;
        inw.send(chunk).await.map_err(|e| db_error(table, &e))?;
        if last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            let secs = started.elapsed().as_secs_f64().max(0.001);
            println!(
                "   ... 已传输 {:.1} MiB（{:.1} MiB/s）",
                bytes as f64 / 1_048_576.0,
                bytes as f64 / 1_048_576.0 / secs
            );
        }
    }
    inw.finish().await.map_err(|e| db_error(table, &e))?;
    let elapsed = started.elapsed();
    println!(
        "   ✓ 完成：{:.1} MiB / 用时 {:.1}s（{:.1} MiB/s）",
        bytes as f64 / 1_048_576.0,
        elapsed.as_secs_f64(),
        bytes as f64 / 1_048_576.0 / elapsed.as_secs_f64().max(0.001)
    );
    Ok((bytes, elapsed))
}

/// 该表用于**按时间切窗口**的列。
///
/// 为什么不能统一用 `created_at`（实测踩到的缺陷）：`created_at` 是**落库时间**，
/// 而根表（请求表）与子表的落库时间并不对应 —— 一个慢响应可以在请求落库很久之后
/// 才落响应行。于是"请求表用 `created_at < before` 切窗口"与"响应表用
/// `created_at < before` 切窗口"用的是**两次不同的快照**，中间到达的请求既不在
/// 请求表的窗口里、又被响应表的 `EXISTS` 认为"父行会被复制" → 提交时撞外键
/// （实测：`Key (id)=(6abf8f89…) is not present in table "access_request_logs"`，
/// 该请求的 `created_at` 恰好落在请求表快照之后 105 秒）。
///
/// 正确做法：**父行一律按自己的时间列切窗口**（请求按 `requested_at`、响应按
/// `responsed_at`），子表的 `EXISTS` 复述同一个窗口 —— 只要 `EXISTS` 成立，
/// 父行就必然在目标库里。size 明细表没有事件时间，仍用 `created_at`。
fn time_column(table: &str) -> &'static str {
    match table {
        "access_request_logs" => "requested_at",
        "access_response_logs" => "responsed_at",
        _ => "created_at",
    }
}

/// 生成一张表的源端 `WHERE`。
///
/// 两层保证：
/// 1. 表自身按 [`time_column`] 切窗口（父表与子表用同一组 `since`/`before` 边界）；
/// 2. 子表再用 `EXISTS` 复述父表的**同一组**条件 —— 只有父行确实在目标库里，
///    子行才允许复制。这样提交时不会撞外键。
fn source_where(
    table: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    if since.is_none() && before.is_none() {
        return String::new();
    }
    let mut conds: Vec<String> = Vec::new();
    let col = time_column(table);
    if let Some(ts) = since {
        conds.push(format!("{col} >= '{}'", ts.to_rfc3339()));
    }
    if let Some(ts) = before {
        conds.push(format!("{col} < '{}'", ts.to_rfc3339()));
    }
    match table {
        ROOT_TABLE => {}
        "access_response_logs" => conds.push(request_exists("id", since, before)),
        "access_request_size_logs" => conds.push(request_exists("request_id", since, before)),
        "access_response_size_logs" => conds.push(response_exists(since, before)),
        _ => {}
    }
    format!(" WHERE {}", conds.join(" AND "))
}

/// `EXISTS (父请求行会被复制)`；`child_col` 是外层表里指向 `access_request_logs.id` 的列。
fn request_exists(
    child_col: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    let mut conds = vec![format!("q.id = {child_col}")];
    if let Some(ts) = since {
        conds.push(format!("q.requested_at >= '{}'", ts.to_rfc3339()));
    }
    if let Some(ts) = before {
        conds.push(format!("q.requested_at < '{}'", ts.to_rfc3339()));
    }
    format!(
        "EXISTS (SELECT 1 FROM access_request_logs q WHERE {})",
        conds.join(" AND ")
    )
}

/// `EXISTS (父响应行会被复制)`；外层表的列固定是 `response_id`。
fn response_exists(
    since: Option<chrono::DateTime<chrono::Utc>>,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    let mut conds = vec!["r.id = response_id".to_string()];
    if let Some(ts) = since {
        conds.push(format!("r.responsed_at >= '{}'", ts.to_rfc3339()));
    }
    if let Some(ts) = before {
        conds.push(format!("r.responsed_at < '{}'", ts.to_rfc3339()));
    }
    // 响应行本身还依赖请求行，逐级复述。
    conds.push(request_exists("r.id", since, before));
    format!(
        "EXISTS (SELECT 1 FROM access_response_logs r WHERE {})",
        conds.join(" AND ")
    )
}

/// 把 PostgreSQL 的错误细节（constraint / detail / hint）带进错误信息 ——
/// 只看 `error returned from database: violates foreign key constraint` 无法定位是哪一行、
/// 缺的是哪个父键（实测踩过）。
fn db_error(table: &str, e: &sqlx::Error) -> anyhow::Error {
    match e.as_database_error() {
        Some(db) => {
            // `detail` / `hint` 不在 `DatabaseError` trait 上，需要 downcast 到 PG 实现。
            let pg = db
                .as_error()
                .downcast_ref::<sqlx::postgres::PgDatabaseError>();
            anyhow::anyhow!(
                "{table} 复制失败：{}（constraint={:?}, detail={:?}, hint={:?}）",
                db.message(),
                db.constraint(),
                pg.and_then(|p| p.detail()),
                pg.and_then(|p| p.hint()),
            )
        }
        None => anyhow::anyhow!("{table} 复制失败：{e}"),
    }
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    // shared 的 tokio 没开 macros/rt-multi-thread，手工建当前线程运行时。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let src = PgPoolOptions::new()
            .max_connections(2)
            .connect(&args.from)
            .await?;
        let dst = PgPoolOptions::new()
            .max_connections(2)
            .connect(&args.to)
            .await?;

        if args.inspect {
            inspect(&args, &src, &dst).await?;
            return Ok(());
        }

        // `--sql` / `--query`：临时 SQL 控制台（本工具的主要用途之一是不用装 psql 也能查库）。
        if args.sql.is_some() || args.query.is_some() {
            let target = if args.on_src { &src } else { &dst };
            if let Some(sql) = &args.sql {
                let res = sqlx::raw_sql(sql).execute(target).await?;
                println!(
                    "OK: rows_affected={}",
                    res.rows_affected()
                );
            }
            if let Some(q) = &args.query {
                // 包一层 `to_jsonb`，这样任何 SELECT 都能以 JSON 文本打印，
                // 不必为每种列类型写解码分支。
                let wrapped = format!("SELECT to_jsonb(_q)::text AS row FROM ({q}) _q");
                let rows = sqlx::query(&wrapped).fetch_all(target).await?;
                let count = rows.len();
                for row in rows {
                    let text: String = row.try_get("row")?;
                    let value: serde_json::Value = serde_json::from_str(&text)?;
                    println!("{}", serde_json::to_string(&value)?);
                }
                println!("({count} rows)");
            }
            return Ok(());
        }

        if args.truncate {
            // 一条 TRUNCATE 覆盖全部表即可满足外键顺序（CASCADE 会一并清掉引用方）。
            let list = args
                .tables
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ");
            println!("TRUNCATE {list}");
            sqlx::query(&format!("TRUNCATE {list}"))
                .execute(&dst)
                .await?;
        }

        // `--days N` → 以**源库**的当前时间为基准算出窗口（一次算好，四张表共用，
        // 避免各表复制时刻不同导致窗口边界不一致）。
        //
        // 关键：用 `--days` 时自动把子表上界 `before` 也设成同一个"现在"，
        // 否则子表没有上界，又会撞上"复制期间新来的响应引用还没搬过来的请求"这个外键坑。
        let mut before = args.before;
        let since = match (args.since, args.days) {
            (Some(ts), _) => Some(ts),
            (None, Some(days)) => {
                let now: chrono::DateTime<chrono::Utc> =
                    sqlx::query_scalar("SELECT NOW()").fetch_one(&src).await?;
                if before.is_none() {
                    before = Some(now);
                }
                Some(now - chrono::Duration::days(days))
            }
            _ => None,
        };
        if let Some(ts) = since {
            println!("只同步 created_at >= {ts} 的数据");
        }

        let mut total_bytes = 0u64;
        let total_started = Instant::now();
        for table in &args.tables {
            let (bytes, _) =
                copy_table(&src, &dst, table, before, since, args.extra_where.as_deref()).await?;
            total_bytes += bytes;
        }
        println!(
            "全部完成：{:.2} GiB / 用时 {:.1}s",
            total_bytes as f64 / 1_073_741_824.0,
            total_started.elapsed().as_secs_f64()
        );
        Ok(())
    })
}
