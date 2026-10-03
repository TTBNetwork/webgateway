//! PostgreSQL **库级运维**小工具（本机没有 `psql` 时的替代品）。
//!
//! 与 [`pg_sync`](pg_sync.rs) 分工：`pg_sync` 管**表数据**的搬运，本工具管**库**本身 ——
//! 列库、看大小/连接数、`DROP DATABASE` + `CREATE DATABASE`（beta 冷启动重建），
//! 以及在指定库上执行任意 SQL / 查询。
//!
//! ```bash
//! # 列出实例上的库（大小 + 连接数）
//! cargo run -p shared --example pg_admin -- --admin-url 'postgres://u:p@h:5432/postgres' --list
//!
//! # 冷启动重建：先踢掉旧连接，再 DROP + CREATE（**不可逆**，目标库全部数据消失）
//! cargo run -p shared --example pg_admin -- --admin-url '...' --drop webgateway_beta --create webgateway_beta
//!
//! # 在任意库上跑 SQL / 查询（查询结果按 JSON 逐行打印）
//! cargo run -p shared --example pg_admin -- --url '...' --sql 'CREATE EXTENSION IF NOT EXISTS btree_gin'
//! cargo run -p shared --example pg_admin -- --url '...' --query 'SELECT count(*) FROM access_request_logs'
//! ```
//!
//! 注意：本工具**只用于演练/运维**，不属于产品运行路径。连接串一律从命令行传入。

use sqlx::postgres::PgPoolOptions;
use sqlx::{Pool, Postgres, Row};

struct Args {
    /// 运维连接串（连到 `postgres` 这类维护库），`--list` / `--drop` / `--create` / `--terminate` 用。
    admin_url: Option<String>,
    /// 普通连接串，`--sql` / `--query` 用。
    url: Option<String>,
    list: bool,
    drop: Option<String>,
    create: Option<String>,
    terminate: Option<String>,
    sql: Vec<String>,
    query: Vec<String>,
    /// `--explain '<SELECT>'`：打印 `EXPLAIN (ANALYZE, BUFFERS, VERBOSE off)` 计划。
    ///
    /// 迁移 v1→v2 前后各跑一次同一批查询，就能用真实执行时间判断"面板变快了多少"，
    /// 而不是靠猜。
    explain: Vec<String>,
    /// `--rebuild-stats <DAYS>`：重算最近 N 天的日汇总表（`access_stats_daily`）。
    ///
    /// 两个用途：
    /// 1. 启用汇总表后回填历史（面板的"近 N 天"才有数据）；
    /// 2. 与原始日志对账，验证汇总值是否正确。
    rebuild_stats_days: Option<i64>,
    /// `--v2-report`：打印 v1→v2 迁移的现状报告（分区表/行数/逐周字节对比）。
    ///
    /// 迁移演练的核心验收物：一次命令给出"切没切、搬了多少、逐周字节对不对"。
    v2_report: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        admin_url: std::env::var("ADMIN_DATABASE_URL").ok(),
        url: std::env::var("DATABASE_URL").ok(),
        list: false,
        drop: None,
        create: None,
        terminate: None,
        sql: Vec::new(),
        query: Vec::new(),
        explain: Vec::new(),
        v2_report: false,
        rebuild_stats_days: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--admin-url" => a.admin_url = it.next(),
            "--url" => a.url = it.next(),
            "--list" => a.list = true,
            "--v2-report" => a.v2_report = true,
            "--rebuild-stats" => {
                a.rebuild_stats_days = Some(it.next().unwrap_or_default().parse::<i64>().unwrap_or_else(|_| {
                    eprintln!("--rebuild-stats 需要整数（天数）");
                    std::process::exit(2);
                }))
            }
            "--drop" => a.drop = it.next(),
            "--create" => a.create = it.next(),
            "--terminate" => a.terminate = it.next(),
            "--sql" => {
                if let Some(s) = it.next() {
                    a.sql.push(s)
                }
            }
            "--query" => {
                if let Some(s) = it.next() {
                    a.query.push(s)
                }
            }
            "--explain" => {
                if let Some(s) = it.next() {
                    a.explain.push(s)
                }
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

/// 标识符安全拼接（库名不接受参数绑定，只能拼字符串 —— 因此必须严格校验）。
fn ident(name: &str) -> String {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        eprintln!("非法的标识符：{name:?}（只允许字母数字、下划线与连字符）");
        std::process::exit(2);
    }
    format!("\"{}\"", name)
}

/// 关掉目标库上的所有其它连接 —— `DROP DATABASE` 有活动连接就会失败。
async fn terminate(pool: &Pool<Postgres>, db: &str) -> anyhow::Result<u64> {
    let res = sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
          WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind(db)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

async fn list(pool: &Pool<Postgres>) -> anyhow::Result<()> {
    let rows = sqlx::query(
        "SELECT d.datname AS name, pg_get_userbyid(d.datdba) AS owner, \
                pg_database_size(d.datname) AS bytes, \
                (SELECT count(*) FROM pg_stat_activity a WHERE a.datname = d.datname) AS conns, \
                d.datallowconn AS allowconn \
           FROM pg_database d \
          WHERE d.datistemplate = false \
          ORDER BY pg_database_size(d.datname) DESC",
    )
    .fetch_all(pool)
    .await?;
    println!(
        "{:<28} {:<16} {:>14} {:>6}",
        "database", "owner", "size", "conns"
    );
    for r in rows {
        let name: String = r.get("name");
        let owner: String = r.get("owner");
        let bytes: i64 = r.get("bytes");
        let conns: i64 = r.get("conns");
        println!(
            "{:<28} {:<16} {:>14} {:>6}",
            name,
            owner,
            human(bytes as u64),
            conns
        );
    }
    // 扩展可用性：v2 依赖 btree_gin / uint128（缺了迁移会直接失败）。
    let avail = sqlx::query(
        "SELECT name, default_version, installed_version FROM pg_available_extensions \
          WHERE name IN ('btree_gin','uint128','plpgsql') ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    print!("可用扩展：");
    for r in avail {
        let name: String = r.get("name");
        let installed: Option<String> = r.get("installed_version");
        print!(" {name}={}", installed.unwrap_or_else(|| "-".into()));
    }
    println!();
    Ok(())
}

/// v1→v2 迁移的现状报告：一次命令回答"切没切、搬了多少、逐周字节对不对"。
///
/// 与产品代码里的 `access_v2::verify_after_switch` 同样的口径（主表比行数、
/// size 表比**逐周字节**），额外打印分区清单与进度表，便于迁移演练取证。
async fn v2_report(pool: &Pool<Postgres>) -> anyhow::Result<()> {
    const LOGICAL: [&str; 4] = [
        "access_request_logs",
        "access_response_logs",
        "access_request_size_logs",
        "access_response_size_logs",
    ];
    const SIZE_ID_COL: [&str; 4] = ["", "", "request_id", "response_id"];

    let progress = sqlx::query(
        "SELECT phase, cutoff, current_table, cursor_at, copied_rows, last_error, started_at, \
                finished_at \
           FROM access_log_v2_migration WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;
    println!("== 迁移进度（access_log_v2_migration）==");
    match progress {
        Some(r) => {
            for key in [
                "phase",
                "cutoff",
                "current_table",
                "cursor_at",
                "copied_rows",
                "last_error",
                "started_at",
                "finished_at",
            ] {
                let v: Option<String> = r.try_get::<Option<String>, _>(key).unwrap_or(None);
                println!("  {key:<14} {}", v.unwrap_or_else(|| "-".into()));
            }
        }
        None => println!("  （进度表不存在）"),
    }

    println!("\n== 表状态 ==");
    println!(
        "{:<30} {:>9} {:>14} {:>14}",
        "表", "分区表?", "行数", "大小"
    );
    for (i, name) in LOGICAL.iter().enumerate() {
        let kind: String = sqlx::query_scalar(
            "SELECT COALESCE((SELECT c.relkind::text FROM pg_class c \
                                JOIN pg_namespace n ON n.oid = c.relnamespace \
                               WHERE n.nspname='public' AND c.relname = $1), '-')",
        )
        .bind(name)
        .fetch_one(pool)
        .await?;
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("public.{name}"))
            .fetch_one(pool)
            .await?;
        if !exists {
            println!("{name:<30} {:>9} {:>14} {:>14}", "-", "-", "-");
            continue;
        }
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {name}"))
            .fetch_one(pool)
            .await?;
        let size: String = sqlx::query_scalar(&format!(
            "SELECT pg_size_pretty(pg_total_relation_size($1))"
        ))
        .bind(format!("public.{name}"))
        .fetch_one(pool)
        .await?;
        println!(
            "{name:<30} {:>9} {rows:>14} {size:>14}",
            if kind == "p" { "是" } else { "否(v1)" }
        );
        let _ = i;
    }

    // 换名后的旧表仍在时，逐周对比 size 表字节（这是"搬得对不对"的硬指标）。
    let v1_names: Vec<String> = LOGICAL
        .iter()
        .map(|n| format!("{n}_v1"))
        .filter(|n| n.starts_with("access_"))
        .collect();
    for v1 in &v1_names {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("public.{v1}"))
            .fetch_one(pool)
            .await?;
        if !exists {
            continue;
        }
        let logical = v1.trim_end_matches("_v1");
        let Some(idx) = LOGICAL.iter().position(|n| *n == logical) else {
            continue;
        };
        let id_col = SIZE_ID_COL[idx];
        println!("\n== {logical} vs {v1} ==");
        if id_col.is_empty() {
            let a: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {v1}"))
                .fetch_one(pool)
                .await?;
            let b: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {logical}"))
                .fetch_one(pool)
                .await?;
            println!("  行数 v1={a} v2={b}  {}", if b >= a { "OK" } else { "偏少!" });
        } else {
            let by_week = |table: &str| {
                format!(
                    "SELECT to_char(date_trunc('week', COALESCE(at_second, created_at)), 'YYYY-MM-DD') wk, \
                            count(*)::bigint rows, COALESCE(SUM(body_length),0)::bigint bytes \
                       FROM {table} GROUP BY 1 ORDER BY 1"
                )
            };
            let mut v1w: std::collections::BTreeMap<String, (i64, i64)> = Default::default();
            for r in sqlx::query(&by_week(v1)).fetch_all(pool).await? {
                v1w.insert(r.get("wk"), (r.get("rows"), r.get("bytes")));
            }
            let mut v2w: std::collections::BTreeMap<String, (i64, i64)> = Default::default();
            for r in sqlx::query(&by_week(logical)).fetch_all(pool).await? {
                v2w.insert(r.get("wk"), (r.get("rows"), r.get("bytes")));
            }
            println!("  {:<12} {:>12} {:>16} {:>12} {:>16}", "周", "v1 行", "v1 字节", "v2 行", "v2 字节");
            let mut bad = 0;
            for (wk, (r1, b1)) in &v1w {
                let (r2, b2) = v2w.get(wk).copied().unwrap_or((0, 0));
                if b2 < *b1 {
                    bad += 1;
                }
                println!("  {wk:<12} {r1:>12} {b1:>16} {r2:>12} {b2:>16}");
            }
            println!(
                "  逐周字节：{}",
                if bad == 0 {
                    "全部 OK".to_string()
                } else {
                    format!("有 {bad} 周偏少 —— 不要删 v1")
                }
            );
        }
    }

    println!("\n== 分区清单 ==");
    let parts = sqlx::query(
        "SELECT p.relname AS parent, c.relname AS child, \
                pg_get_expr(c.relpartbound, c.oid) AS bound, \
                pg_size_pretty(pg_total_relation_size(c.oid)) AS size \
           FROM pg_inherits i \
           JOIN pg_class c ON c.oid = i.inhrelid \
           JOIN pg_class p ON p.oid = i.inhparent \
           JOIN pg_namespace n ON n.oid = p.relnamespace \
          WHERE n.nspname = 'public' AND p.relkind = 'p' \
          ORDER BY p.relname, c.relname",
    )
    .fetch_all(pool)
    .await?;
    let mut current = String::new();
    for r in parts {
        let parent: String = r.get("parent");
        if parent != current {
            println!("  {parent}:");
            current = parent.clone();
        }
        println!(
            "    {:<44} {:<28} {}",
            r.get::<String, _>("child"),
            r.get::<String, _>("bound"),
            r.get::<String, _>("size")
        );
    }
    Ok(())
}

fn human(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        if args.list || args.drop.is_some() || args.create.is_some() || args.terminate.is_some() {
            let url = args
                .admin_url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--list/--drop/--create/--terminate 需要 --admin-url"))?;
            let pool = PgPoolOptions::new()
                .max_connections(2)
                .connect(url)
                .await?;
            if args.list {
                list(&pool).await?;
            }
            if let Some(db) = &args.terminate {
                let n = terminate(&pool, db).await?;
                println!("已断开 {db} 上的 {n} 个连接");
            }
            if let Some(db) = &args.drop {
                // 先踢连接：生产/共享实例上常有别的东西连着（beta 上可能还有本地服务）。
                terminate(&pool, db).await?;
                sqlx::query(&format!("DROP DATABASE IF EXISTS {}", ident(db)))
                    .execute(&pool)
                    .await?;
                println!("已 DROP DATABASE {db}");
            }
            if let Some(db) = &args.create {
                sqlx::query(&format!("CREATE DATABASE {}", ident(db)))
                    .execute(&pool)
                    .await?;
                println!("已 CREATE DATABASE {db}");
            }
        }

        if let Some(days) = args.rebuild_stats_days {
            let url = args
                .url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--rebuild-stats 需要 --url"))?;
            let pool = PgPoolOptions::new()
                .max_connections(2)
                .connect(url)
                .await?;
            let today = shared::database::access_v2::db_current_date(&pool).await?;
            let from = today - chrono::Duration::days(days.max(1) - 1);
            println!("重算日汇总：{from} ~ {today}（共 {days} 天）");
            let started = std::time::Instant::now();
            let rows = shared::database::access_v2::refresh_stats_days(&pool, from, today).await?;
            println!(
                "完成：写入 {rows} 行，用时 {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }

        if args.v2_report {
            let url = args
                .url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--v2-report 需要 --url"))?;
            let pool = PgPoolOptions::new()
                .max_connections(2)
                .connect(url)
                .await?;
            v2_report(&pool).await?;
        }

        if !args.sql.is_empty() || !args.query.is_empty() || !args.explain.is_empty() {
            // 允许复用 `--admin-url`：库级运维常常需要在维护库上顺带跑一句 SQL。
            let url = args
                .url
                .as_deref()
                .or(args.admin_url.as_deref())
                .ok_or_else(|| anyhow::anyhow!("--sql/--query 需要 --url"))?;
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(url)
                .await?;
            for sql in &args.sql {
                let res = sqlx::raw_sql(sql).execute(&pool).await?;
                println!("OK: rows_affected={}", res.rows_affected());
            }
            for q in &args.explain {
                println!("-- EXPLAIN ANALYZE: {q}");
                let wrapped =
                    format!("EXPLAIN (ANALYZE, BUFFERS, TIMING ON, SUMMARY ON) {q}");
                let rows = sqlx::query(&wrapped).fetch_all(&pool).await?;
                for row in rows {
                    // EXPLAIN 的每一行都是单列文本，列名固定是 "QUERY PLAN"。
                    let line: String = row.try_get(0)?;
                    println!("{line}");
                }
                println!();
            }
            for q in &args.query {
                let wrapped = format!("SELECT to_jsonb(_q)::text AS row FROM ({q}) _q");
                let rows = sqlx::query(&wrapped).fetch_all(&pool).await?;
                let count = rows.len();
                for row in rows {
                    let text: String = row.try_get("row")?;
                    let value: serde_json::Value = serde_json::from_str(&text)?;
                    println!("{}", serde_json::to_string(&value)?);
                }
                println!("({count} rows)");
            }
        }
        Ok(())
    })
}
