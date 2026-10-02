use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use rustls::ServerConfig;
use shared::database::get_database;
use tracing::{Level, event};

use crate::{
    sync::{
        cert::{AutoCertificate, sync_certificates},
        websites::sync_websites,
    },
    upstream::sync_listeners,
};

pub mod cert;
pub mod websites;

/// 周期性兜底全量同步的间隔。
///
/// 恢复该机制的原因（ISSUES.md P0-7 问题 B）：NOTIFY 依赖数据库触发器，
/// 而触发器在重建窗口内、或通知在连接重连期间丢失时，网关会**永久**使用旧配置，
/// 直到进程重启。原先的兜底轮询被注释掉了，于是「真空期修改站点配置 → 网关永久不同步」。
/// 对账式全量同步本身很便宜（表规模小），每 10 秒跑一次即可作为最终一致性保障。
const FALLBACK_SYNC_INTERVAL: Duration = Duration::from_secs(10);

pub static SERVER_CONFIG: LazyLock<Arc<ServerConfig>> = LazyLock::new(|| {
    Arc::new({
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(AutoCertificate));
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        config
    })
});

pub async fn main() -> anyhow::Result<()> {
    let first_result = sync_config().await;
    if let Err(e) = first_result {
        event!(Level::ERROR, "Failed to sync first config: {e}");
        return Err(e);
    }

    // 周期兜底同步：即使 NOTIFY 丢失（触发器真空期、连接抖动）也能自愈。
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(FALLBACK_SYNC_INTERVAL).await;
            if let Err(e) = sync_config().await {
                event!(Level::ERROR, "Failed to run fallback config sync: {e}");
            }
        }
    });

    tokio::spawn(async move {
        match get_database()
            .listen_service_fn("websites", async |_| {
                event!(Level::INFO, "Recvied notification, syncing websites");
                match sync_websites().await {
                    Ok(ports) => {
                        sync_listeners(ports).await;
                    }
                    Err(e) => {
                        event!(Level::ERROR, "Failed to sync websites: {e}");
                    }
                }
            })
            .await
        {
            Ok(()) => {}
            Err(e) => event!(Level::ERROR, "Failed to listen websites: {e}"),
        };
    });

    tokio::spawn(async move {
        match get_database()
            .listen_service_fn("certificates", async |_| {
                event!(Level::INFO, "Recvied notification, syncing certificates");
                match sync_certificates().await {
                    Ok(()) => {}
                    Err(e) => {
                        event!(Level::ERROR, "Failed to sync certificates: {e}");
                    }
                }
            })
            .await
        {
            Ok(()) => {}
            Err(e) => event!(Level::ERROR, "Failed to listen certificates: {e}"),
        };
    });
    Ok(())
}

pub async fn sync_config() -> anyhow::Result<()> {
    event!(Level::DEBUG, "Syncing config at {}", chrono::Local::now());
    event!(Level::DEBUG, "Syncing certificates");
    sync_certificates().await?;
    event!(Level::DEBUG, "Syncing websites");
    let ports = sync_websites().await?;
    // 对账式监听：新端口开始监听，已移除的端口关闭监听（P0-8）。
    sync_listeners(ports).await;
    Ok(())
}
