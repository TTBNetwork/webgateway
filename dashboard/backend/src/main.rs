use crate::{
    config::{get_config, init_config},
    database::auth::Authentication,
    foundation::{CListener, RemoteAddr},
    response::wrapper_router,
};
use shared::{
    database::{DbStartupMode, get_database, init_database_with_mode},
    listener::CustomDualStackTcpListener,
    logger::LoggerConfig,
};
use tokio::signal::ctrl_c;
use tracing::{Level, event};

pub mod auth;
pub mod certificate;
pub mod config;
mod database;
mod foundation;
pub mod ip;
pub mod mnt;
pub mod models;
pub mod response;
pub mod router;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    LoggerConfig::default().init();
    init_config()?;

    // 启动模式（ISSUES.md P0-7）：
    //   * 默认 Serve      —— 只校验 schema，不执行 DDL；
    //   * DB_AUTO_MIGRATE=1 —— 本进程执行迁移（全程持 advisory lock），再进入服务；
    //   * --migrate        —— 只执行迁移然后退出。
    // 控制面自己的 users / web_log 表也必须在同一把锁下迁移，否则会和数据面抢锁。
    // 控制面的表（users / users_client_secrets / web_log）已经并入共享迁移入口
    // （`shared::database::dashboard_schema`），因此这里不再单独迁移一次 ——
    // 这样 gateway 与 dashboard **谁先启动都会得到同一份完整 schema**。
    let mode = DbStartupMode::from_env_args();
    init_database_with_mode(&get_config().database, get_config().max_connections, mode).await?;
    if mode == DbStartupMode::Migrate {
        event!(Level::INFO, "Migration finished, exiting");
        return Ok(());
    }
    // 表结构就绪后引导默认管理员账号。
    get_database().ensure_default_admin().await?;

    event!(
        Level::INFO,
        "Dashboard API listening on port {}",
        get_config().port
    );
    let listener = CustomDualStackTcpListener::new_by_port(get_config().port).await?;
    let router = axum::Router::new()
        .nest("/auth", auth::get_router())
        .merge(router::get_router());

    let web = tokio::spawn(async move {
        let r = axum::serve(
            CListener::from(listener),
            wrapper_router(router).into_make_service_with_connect_info::<RemoteAddr>(),
        )
        .await;
        if let Err(e) = r {
            event!(Level::ERROR, "Error while serving: {}", e);
        }
    });

    let mnt = tokio::spawn(async move {
        // unix for mgt
        let res = mnt::init().await;
        if let Err(e) = res {
            event!(Level::ERROR, "Error while serving: {}", e);
        }
    });

    // 保存真正的调度器句柄：`certificate::init()` 内部 spawn 后立即返回，
    // 原先拿到的 JoinHandle 指向的是已经结束的 init() 任务，abort() 停不掉调度器（P0-14 问题 C）。
    let auto_cert = match certificate::init().await {
        Ok(handle) => handle,
        Err(e) => {
            event!(Level::ERROR, "Failed to start certificate scheduler: {e}");
            return Err(e);
        }
    };

    match ctrl_c().await {
        Ok(()) => {
            web.abort();
            mnt.abort();
            auto_cert.abort();
            event!(Level::INFO, "Dashboard API shutting down")
        }
        Err(e) => event!(Level::ERROR, "Dashboard API failed to shut down: {}", e),
    };

    Ok(())
}

