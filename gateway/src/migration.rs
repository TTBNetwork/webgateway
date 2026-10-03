//! v1 → v2 访问日志分区表的**后台自动迁移**（零停机）。
//!
//! 背景：`access_*` 四张表要改成按周 RANGE 分区的 v2（`access_v2` 模块已完成结构层），
//! 生产上是约 2000 万行 / 6.3 GB 的数据搬迁 + 读写切换。**绝不能在启动路径上做**
//! —— 2026-10-02 的生产事故就是"启动路径里的单事务大迁移"把两个服务全卡死了。
//!
//! 因此这里是一个**服务起来之后**的后台任务：
//!
//! 1. 结构已在启动迁移里建好（空的分区表，与表大小无关）；
//! 2. 本任务按**天**分批把 v1 复制进 v2，进度写进 `access_log_v2_migration`，
//!    控制台打印进度条；进程被杀/重启后从游标续跑（不会整体回滚重来）；
//! 3. 全部搬完后，**短暂持有刷盘闸门**（`access::acquire_flush_gate`）：
//!    这段时间网关照常代理请求，日志只是先在内存里排队 —— 然后补齐增量、换名；
//! 4. 换名后 v2 就是逻辑表本身，读写自动走 v2；核对行数后回收 `*_v1` 旧表。
//!
//! ## 环境变量
//!
//! | 变量 | 默认 | 作用 |
//! |------|------|------|
//! | `ACCESS_LOG_V2` | `auto` | `off` / `0` / `false` = 只建结构，**不**自动搬数据与切换 |
//! | `ACCESS_LOG_V2_KEEP_V1` | 未设置 | 设为 `1` 时切换后**保留** `*_v1` 旧表不回收 |
//! | `ACCESS_LOG_V2_DROP_V1_AFTER_SECS` | `300` | 切换后等待多久再核对并回收旧表（留出反悔窗口） |
//!
//! ## 多实例部署的限制
//!
//! 迁移用 `locks::MIGRATION` 保证只有一个实例在搬。但**换名**要求"没有其它进程正在
//! 写 v1"：另一个 gateway 实例的刷盘循环不受本进程的刷盘闸门约束。当前生产是单
//! gateway 容器，因此安全；多副本部署时需要先停掉其它实例，或改成数据库侧写屏障。

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use shared::database::{
    Database, access_v2, get_database, locks, try_lock_session, unlock_session,
};
use tracing::{Level, event};

/// 进度条宽度（字符数）。
const BAR_WIDTH: usize = 28;
/// 终端上刷新进度条的间隔。
const TTY_REFRESH: Duration = Duration::from_secs(1);
/// 非终端（docker logs）下打印一行的间隔 —— 避免每分钟刷出几十行。
const LOG_REFRESH: Duration = Duration::from_secs(15);
/// 失败重试次数与退避。
const MAX_ATTEMPTS: usize = 5;
const RETRY_BACKOFF: Duration = Duration::from_secs(30);

/// 是否启用数据搬迁与自动切换（结构由启动迁移无条件安装）。
fn enabled() -> bool {
    !matches!(
        std::env::var("ACCESS_LOG_V2").as_deref(),
        Ok("off") | Ok("0") | Ok("false") | Ok("OFF") | Ok("False")
    )
}

fn keep_v1() -> bool {
    matches!(
        std::env::var("ACCESS_LOG_V2_KEEP_V1").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn drop_grace() -> Duration {
    std::env::var("ACCESS_LOG_V2_DROP_V1_AFTER_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(300))
}

/// 同步执行一次迁移（供 `--migrate-v2` 一次性命令使用）。
///
/// 与后台任务走**同一份实现**：先搬迁（打印进度条），再切换、核对、回收旧表。
/// 区别只是这里不启动刷盘循环，因此切换时闸门无人争用。
pub async fn run_once() -> anyhow::Result<()> {
    run().await
}

/// 是否在**启动时**激活 v2（`ACCESS_LOG_V2_ACTIVATE=1`）。
///
/// 与一次性命令 `--activate-v2-keep-v1` 等价，只是把动作挪到启动路径，
/// 便于"改一次 .env 再 up"的部署方式。默认关闭：切换是**不可逆的结构变更**，
/// 默认行为应当保守 —— 全新库不需要它（启动迁移会自动激活）。
pub fn activation_enabled() -> bool {
    matches!(
        std::env::var("ACCESS_LOG_V2_ACTIVATE").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("True") | Ok("yes")
    )
}

/// **不做数据搬迁**地激活 v2：v1 原样保留为 `access_*_v1`，新数据从此刻起写进分区表。
///
/// 供 `--activate-v2-keep-v1` 一次性命令使用。适用场景：历史访问日志可以不要，
/// 或者打算以后用 mnt 工具慢慢搬 —— 这时没必要先等 1600 万行的搬迁。
pub async fn activate_keeping_v1() -> anyhow::Result<()> {
    let db = get_database();
    let _gate = crate::access::acquire_flush_gate().await;
    access_v2::activate_v2_keeping_v1(&db.pool).await?;
    println!(
        "[access-v2] 已激活 v2（未搬数据）：新日志写入按周分区；\
         历史数据保留在 access_*_v1，**不会**被自动回收"
    );
    Ok(())
}

/// 启动后台迁移任务（由 `main` 在数据库初始化之后调用）。
pub fn spawn() {
    if !enabled() {
        event!(
            Level::INFO,
            "Access log v2 migration is disabled (ACCESS_LOG_V2=off); \
             v2 structure stays installed but no data will be copied"
        );
        return;
    }
    tokio::spawn(async move {
        for attempt in 1..=MAX_ATTEMPTS {
            match run().await {
                Ok(()) => return,
                Err(e) => {
                    event!(
                        Level::ERROR,
                        "v1→v2 access log migration failed (attempt {attempt}/{MAX_ATTEMPTS}): {e:#}"
                    );
                    let _ = access_v2::mark_error(&get_database().pool, &format!("{e:#}")).await;
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(RETRY_BACKOFF).await;
                    }
                }
            }
        }
        event!(
            Level::ERROR,
            "v1→v2 access log migration gave up after {MAX_ATTEMPTS} attempts; \
             progress is persisted and the next restart will resume where it stopped"
        );
    });
}

async fn run() -> anyhow::Result<()> {
    let db = get_database();

    // 已经切到 v2：只需按需回收上一次遗留的旧表。
    if access_v2::is_switched(&db.pool).await? {
        if let Some(st) = access_v2::load_status(&db.pool).await?
            && st.keeps_v1()
        {
            // **直接激活 v2、保留 v1** 模式：没有任何搬迁进度要修正，
            // 也绝不能走"核对并回收 v1"。直接返回。
            return Ok(());
        }
        if let Some(st) = access_v2::load_status(&db.pool).await?
            && matches!(st.phase.as_str(), "pending" | "backfilling" | "switching")
        {
            // 上次可能在"换名提交成功、阶段还没写成 done"之间退出（例如被强杀）。
            let _ = access_v2::mark_phase(&db.pool, "done").await;
        }
        return after_switch_cleanup(db).await;
    }

    let Some(status) = access_v2::load_status(&db.pool).await? else {
        event!(
            Level::WARN,
            "v2 structure is not installed (DB_AUTO_MIGRATE=0?); skipping v1→v2 data migration"
        );
        return Ok(());
    };
    if status.keeps_v1() {
        // 阶段说"已激活并保留 v1"，但逻辑表还不是分区表：有人手工动过库，交给人工确认。
        event!(
            Level::WARN,
            "migration state says active_keep_v1 but tables are not partitioned; \
             skipping automatic migration (manual check required)"
        );
        return Ok(());
    }
    if status.is_done() {
        // phase=done 但表还不是分区表：说明有人手工动过库，交给人工确认。
        event!(
            Level::WARN,
            "migration state says done but tables are not partitioned; skipping automatic migration"
        );
        return Ok(());
    }

    // 单实例：拿到会话级锁的实例负责搬迁与切换，其余实例只旁观。
    let Some(mut guard) = try_lock_session(&db.pool, locks::MIGRATION).await? else {
        event!(
            Level::INFO,
            "another instance is already migrating access logs to v2; standing by"
        );
        return Ok(());
    };

    let result = run_locked(db, status).await;

    if let Err(e) = unlock_session(&mut guard, locks::MIGRATION).await {
        event!(Level::WARN, "failed to release migration advisory lock: {e}");
    }
    result
}

async fn run_locked(
    db: &Database,
    status: access_v2::V2MigrationStatus,
) -> anyhow::Result<()> {
    let pool = &db.pool;
    // 没有 cutoff 说明是首次运行：记录"开始时刻"作为搬迁上界。
    let status = if status.cutoff.is_none() {
        let cutoff = access_v2::db_now(pool).await?;
        let min = access_v2::earliest_time(pool).await?.unwrap_or(cutoff);
        access_v2::start_migration(pool, cutoff, min).await?;
        access_v2::load_status(pool).await?.unwrap_or_default()
    } else {
        status
    };

    if status.phase != "switching" {
        backfill(db, &status).await?;
    }

    // ---- 切换 ----
    access_v2::mark_phase(pool, "switching").await?;
    println!(
        "\n[access-v2] 历史数据搬迁完成，开始切换读写（暂停访问日志刷盘，网关继续代理请求）..."
    );
    let switch_result = {
        // 闸门只在这段里持有：补增量 + 换名，秒级到分钟级。
        let _gate = crate::access::acquire_flush_gate().await;
        access_v2::perform_switch(pool).await
    };
    switch_result?;
    println!("[access-v2] 切换完成：v2 分区表已接管读写。");
    event!(
        Level::INFO,
        "Access log tables switched to v2 (weekly RANGE partitions)"
    );

    after_switch_cleanup(db).await
}

/// 按天分批把 v1 复制进 v2，并在控制台打印进度条。
async fn backfill(
    db: &Database,
    status: &access_v2::V2MigrationStatus,
) -> anyhow::Result<()> {
    let pool = &db.pool;
    let cutoff = status
        .cutoff
        .ok_or_else(|| anyhow::anyhow!("migration cutoff is missing"))?;
    let min = access_v2::earliest_time(pool).await?.unwrap_or(cutoff);
    if min >= cutoff {
        println!("[access-v2] 没有需要搬迁的历史数据（表为空或全部晚于 cutoff）");
        return Ok(());
    }

    // 历史数据跨很多周，先把这些周的分区都建出来（建空分区是毫秒级）。
    access_v2::ensure_history_partitions(pool, min, cutoff).await?;
    // 再加一张 DEFAULT 兜底分区：size 表按 `at_second` 路由，而历史行可能
    // `at_second IS NULL`/落在预建范围之外 —— 没有兜底就会整批失败
    // （`no partition of relation ... found for row`），迁移卡在第一天。
    access_v2::ensure_default_partition(pool).await?;

    let current = status
        .current_table
        .clone()
        .unwrap_or_else(|| access_v2::V2_TABLES[0].to_string());
    let start_idx = access_v2::V2_TABLES
        .iter()
        .position(|t| *t == current)
        .unwrap_or(0);
    let mut cursor = status.cursor_at.unwrap_or(min);
    let mut copied = status.copied_rows;

    let mut reporter = Reporter::new(min, cutoff, start_idx, access_v2::V2_TABLES[start_idx]);
    reporter.copied = copied;
    println!(
        "[access-v2] 开始搬迁访问日志到 v2 分区表：{} 张表，时间范围 {} ~ {}（后台进行，网关照常服务）",
        access_v2::V2_TABLES.len(),
        min.format("%Y-%m-%d"),
        cutoff.format("%Y-%m-%d %H:%M"),
    );

    for idx in start_idx..access_v2::V2_TABLES.len() {
        let logical = access_v2::V2_TABLES[idx];
        if idx > start_idx {
            cursor = min;
        }
        reporter.set(idx, logical);
        while cursor < cutoff {
            let next = (cursor + TimeDelta::days(1)).min(cutoff);
            let written = access_v2::copy_window(pool, logical, cursor, next, false).await?;
            copied += written as i64;
            cursor = next;
            access_v2::save_progress(pool, logical, cursor, copied).await?;
            reporter.copied = copied;
            reporter.print(cursor, false);
        }
        reporter.print(cursor, true);
    }
    reporter.finish();
    println!(
        "[access-v2] 历史数据搬迁完成，共写入 v2 {} 行（进度表：access_log_v2_migration）",
        copied
    );
    Ok(())
}

/// 核对切换后的行数，并在宽限期之后回收 `*_v1` 旧表。
async fn after_switch_cleanup(db: &Database) -> anyhow::Result<()> {
    let pool = &db.pool;
    // 旧表不在了 → 已经清理过，无事了。
    if !access_v2::table_exists(pool, &access_v2::v1_name("access_request_logs")).await? {
        return Ok(());
    }
    // **直接激活 v2（保留 v1）模式：`*_v1` 是要长期留着的历史数据，永不自动回收。**
    if let Some(st) = access_v2::load_status(pool).await?
        && st.keeps_v1()
    {
        event!(
            Level::INFO,
            "Access log v2 was activated without data migration: keeping the *_v1 tables \
             (historical rows) — they are NOT auto-dropped"
        );
        return Ok(());
    }
    // 上一次核对**不一致**：绝不自动重试（每次核对都要扫两遍千万行），等人工确认。
    if let Some(st) = access_v2::load_status(pool).await?
        && st.phase == "verify_failed"
    {
        event!(
            Level::WARN,
            "skipping v1-table cleanup: the previous v1/v2 verification failed; \
             inspect the tables and either drop *_v1 manually or reset \
             access_log_v2_migration.phase"
        );
        return Ok(());
    }
    if keep_v1() {
        event!(
            Level::INFO,
            "ACCESS_LOG_V2_KEEP_V1 is set: keeping the *_v1 tables (drop them manually when sure)"
        );
        return Ok(());
    }

    let grace = drop_grace();
    println!(
        "[access-v2] v1/v2 行数核对与旧表回收将在 {} 后进行（`ACCESS_LOG_V2_KEEP_V1=1` 可保留旧表）",
        fmt_duration(grace)
    );
    if !grace.is_zero() {
        tokio::time::sleep(grace).await;
    }

    let report = access_v2::verify_after_switch(pool).await?;
    let mut all_ok = true;
    for v in &report {
        let bytes = match (v.v1_bytes, v.v2_bytes) {
            (Some(a), Some(b)) => format!(" 字节 v1={a} v2={b}"),
            _ => String::new(),
        };
        println!(
            "[access-v2] 核对 {}: v1={} 行 v2={} 行{}  {}  ({})",
            v.table,
            v.v1_rows,
            v.v2_rows,
            bytes,
            if v.ok { "OK" } else { "不一致!" },
            v.note,
        );
        event!(
            Level::INFO,
            "Access log v2 verify: {} v1_rows={} v2_rows={} v1_bytes={:?} v2_bytes={:?} ok={} ({})",
            v.table,
            v.v1_rows,
            v.v2_rows,
            v.v1_bytes,
            v.v2_bytes,
            v.ok,
            v.note
        );
        all_ok &= v.ok;
    }

    if all_ok {
        let dropped = access_v2::drop_v1_tables(pool).await?;
        println!("[access-v2] 已回收旧表：{dropped:?}（磁盘空间立即归还）");
    } else {
        access_v2::mark_phase(pool, "verify_failed").await?;
        println!(
            "[access-v2] v1/v2 核对不一致，**已保留**旧表；请人工核对后再删除 \
             （或把 access_log_v2_migration.phase 重置后重跑）"
        );
        event!(
            Level::ERROR,
            "v1/v2 verification failed — NOT dropping the *_v1 tables; please inspect manually"
        );
    }
    Ok(())
}

/// 控制台进度条。
struct Reporter {
    min: DateTime<Utc>,
    cutoff: DateTime<Utc>,
    started: Instant,
    table_idx: usize,
    table: &'static str,
    copied: i64,
    tty: bool,
    last_print: Instant,
}

impl Reporter {
    fn new(
        min: DateTime<Utc>,
        cutoff: DateTime<Utc>,
        table_idx: usize,
        table: &'static str,
    ) -> Self {
        Self {
            min,
            cutoff,
            started: Instant::now(),
            table_idx,
            table,
            copied: 0,
            tty: std::io::stdout().is_terminal(),
            last_print: Instant::now() - LOG_REFRESH,
        }
    }

    fn set(&mut self, table_idx: usize, table: &'static str) {
        self.table_idx = table_idx;
        self.table = table;
    }

    /// 打印进度（按间隔节流；`force` 用于阶段结束时的收尾打印）。
    fn print(&mut self, cursor: DateTime<Utc>, force: bool) {
        let interval = if self.tty { TTY_REFRESH } else { LOG_REFRESH };
        if !force && self.last_print.elapsed() < interval {
            return;
        }
        self.last_print = Instant::now();
        let line = self.render(cursor);
        if self.tty {
            print!("\r{line}\x1b[K");
            let _ = std::io::stdout().flush();
        } else {
            println!("{line}");
        }
    }

    /// 结束进度条（在终端里补一个换行，避免后续输出接在进度条后面）。
    fn finish(&mut self) {
        if self.tty {
            println!();
        }
    }

    fn render(&self, cursor: DateTime<Utc>) -> String {
        let total = access_v2::V2_TABLES.len() as f64;
        let span = (self.cutoff - self.min).num_seconds().max(1) as f64;
        let done = (cursor - self.min).num_seconds().clamp(0, span as i64) as f64;
        let table_fraction = (done / span).clamp(0.0, 1.0);
        let overall = ((self.table_idx as f64) + table_fraction) / total;
        let filled = (overall * BAR_WIDTH as f64).round() as usize;
        let bar = format!(
            "{}{}",
            "█".repeat(filled.min(BAR_WIDTH)),
            "░".repeat(BAR_WIDTH.saturating_sub(filled))
        );
        let elapsed = self.started.elapsed();
        let eta = if overall > 1e-3 {
            elapsed.mul_f64((1.0 - overall) / overall)
        } else {
            Duration::ZERO
        };
        format!(
            "[access-v2] [{bar}] {:>5.1}%  {}/{} {:<28} 已写入 {:>10} 行  用时 {}  剩余 ~{}",
            overall * 100.0,
            self.table_idx + 1,
            access_v2::V2_TABLES.len(),
            self.table,
            self.copied,
            fmt_duration(elapsed),
            fmt_duration(eta),
        )
    }
}

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formatting_is_compact() {
        assert_eq!(fmt_duration(Duration::from_secs(42)), "42s");
        assert_eq!(fmt_duration(Duration::from_secs(125)), "2m05s");
        assert_eq!(fmt_duration(Duration::from_secs(3725)), "1h02m");
    }

    #[test]
    fn reporter_renders_full_bar_at_the_end() {
        let min = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let cutoff = min + TimeDelta::days(30);
        let reporter = Reporter::new(min, cutoff, access_v2::V2_TABLES.len() - 1, "t");
        let line = reporter.render(cutoff);
        assert!(line.contains("100.0%"), "{line}");
        assert_eq!(
            line.matches('█').count(),
            BAR_WIDTH,
            "到达末尾时进度条应当填满：{line}"
        );
        assert_eq!(line.matches('░').count(), 0, "{line}");
    }

    #[test]
    fn reporter_starts_at_zero_percent() {
        let min = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let cutoff = min + TimeDelta::days(10);
        let reporter = Reporter::new(min, cutoff, 0, "t");
        let line = reporter.render(min);
        assert!(line.contains("0.0%"), "{line}");
        assert!(line.contains("1/4"), "{line}");
    }
}
