use std::sync::{Arc, LazyLock};

use rustls::ServerConfig;
use shared::database::get_database;
use tracing::{Level, event};

use crate::{
    sync::{
        cert::{AutoCertificate, sync_certificates},
        websites::sync_websites,
    },
    upstream::listen,
};

pub mod cert;
pub mod websites;

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
    // tokio::spawn(tokio_schedule::every(10).seconds().perform(|| async {
    //     if let Err(e) = sync_config().await {
    //         event!(Level::ERROR, "Failed to sync config: {e}");
    //     }
    // }));

    tokio::spawn(async move {
        match get_database()
            .listen_service_fn("websites", async |_| {
                event!(Level::INFO, "Recvied notification, syncing websites");
                match sync_websites().await {
                    Ok(ports) => {
                        for port in ports {
                            match listen(port).await {
                                Err(e) => event!(Level::ERROR, "Failed to listen port {port}: {e}"),
                                Ok(()) => {
                                    event!(Level::INFO, "Listen port {port}");
                                }
                            }
                        }
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
    for port in ports {
        listen(port).await?;
    }
    // maybe need clean LINKED_WEBSITES, maybe make a lat performance
    Ok(())
}
