use shared::{
    database,
    database::access_v2,
    logger::{self, LoggerConfig},
};
use tokio::signal::ctrl_c;

use crate::config::get_config;

pub mod access;
pub mod config;
pub mod dns;
pub mod foundation;
pub mod migration;
// pub mod proxy;
pub mod state;
pub mod sync;
pub mod transport;
pub mod upstream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    logger::init(LoggerConfig::default());
    config::init_config()?;
    // 数据面默认只校验 schema、不执行任何 DDL（ISSUES.md P0-7）：
    // 两个进程并发重放 `CREATE TABLE/INDEX IF NOT EXISTS` 会互相冲突并可能启动失败。
    //   * DB_AUTO_MIGRATE=1 —— 本进程在 advisory lock 下补齐 schema，再进入服务；
    //   * --migrate          —— 只执行迁移然后退出（供独立迁移 Job 使用）。
    let mode = database::DbStartupMode::from_env_args();
    database::init_database_with_mode(&get_config().database, get_config().max_connections, mode)
        .await?;
    if mode == database::DbStartupMode::Migrate {
        println!("Migration finished, exiting");
        return Ok(());
    }

    // 一次性命令：同步跑完 v1→v2 的数据搬迁与切换（默认是在后台自动跑）。
    // 低峰期运维可用它把迁移做完再启动服务，`--migrate-v2` 结束后进程退出。
    if std::env::args().any(|a| a == "--migrate-v2") {
        migration::run_once().await?;
        println!("Access log v1→v2 migration finished, exiting");
        return Ok(());
    }

    // 一次性命令：**不做数据搬迁**，直接把 v2 分区表激活为读写对象，v1 原样保留为
    // `access_*_v1`（历史数据不再自动回收，可用 mnt 工具以后慢慢搬）。
    // 适用：历史访问日志可以不要 / 只想立刻拿到"新数据按周分区 + 整周回收"的能力。
    if std::env::args().any(|a| a == "--activate-v2-keep-v1") {
        migration::activate_keeping_v1().await?;
        println!("Access log v2 activated (v1 kept as *_v1), exiting");
        return Ok(());
    }

    // 部署开关：ACCESS_LOG_V2_ACTIVATE=1 时**启动即激活**（同样不搬数据、v1 保留）。
    // 这样只需改一次 .env 再 up，不必进容器执行一次性命令。
    if migration::activation_enabled() && !access_v2::is_switched(&database::get_database().pool).await?
    {
        migration::activate_keeping_v1().await?;
    }

    access::init_access_logs().await?;
    // 历史访问日志清理（保留期可在控制面板配置，默认 180 天，下限 90 天）。
    access::init_access_log_pruner().await;
    // v1 → v2 访问日志分区表的**后台**自动迁移（默认开启，零停机；`ACCESS_LOG_V2=off` 可关）。
    // 放在服务起来之后：搬迁与切换都不阻塞启动，也不会像 2026-10-02 的事故那样
    // 在启动路径上持锁做重活。
    migration::spawn();
    sync::main().await?;

    match ctrl_c().await {
        Ok(()) => println!("Ctrl-C received, shutting down..."),
        Err(err) => println!("Error waiting for ctrl-c: {:?}", err),
    }

    Ok(())
}
