use shared::{
    database,
    logger::{self, LoggerConfig},
};
use tokio::signal::ctrl_c;

use crate::config::get_config;

pub mod access;
pub mod config;
pub mod dns;
pub mod foundation;
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

    access::init_access_logs().await?;
    // 历史访问日志清理（保留期可在控制面板配置，默认 180 天，下限 90 天）。
    access::init_access_log_pruner().await;
    sync::main().await?;

    match ctrl_c().await {
        Ok(()) => println!("Ctrl-C received, shutting down..."),
        Err(err) => println!("Error waiting for ctrl-c: {:?}", err),
    }

    Ok(())
}
