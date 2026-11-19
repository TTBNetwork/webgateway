//! PostgreSQL LISTEN/NOTIFY bridge for configuration changes.

use std::time::Duration;

use shared::database::Database;
use sqlx::postgres::PgListener;
use tokio::sync::mpsc;
use tracing::{Level, event};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigEvent {
    /// The listener connected (or reconnected); compare watermarks.
    Connected,
    /// A configuration row was inserted, updated or deleted.
    Changed,
}

/// Keep a dedicated LISTEN connection open and forward configuration events.
///
/// The listener reconnects forever; after every reconnect it sends Connected so
/// the main loop can compare watermarks and catch anything missed while the
/// connection was down.
pub async fn listen(database: Database, sender: mpsc::Sender<ConfigEvent>) {
    loop {
        let mut listener = match PgListener::connect_with(database.pool()).await {
            Ok(listener) => listener,
            Err(error) => {
                event!(Level::WARN, %error, "failed to connect the PostgreSQL notification listener");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let subscribed = async {
            listener.listen("websites_updater").await?;
            listener.listen("certificates_updater").await?;
            Ok::<_, sqlx::Error>(())
        }
        .await;

        if let Err(error) = subscribed {
            event!(Level::WARN, %error, "failed to subscribe to configuration notifications");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        if sender.send(ConfigEvent::Connected).await.is_err() {
            return;
        }

        loop {
            match listener.recv().await {
                Ok(notification) => {
                    event!(
                        Level::DEBUG,
                        channel = notification.channel(),
                        payload = notification.payload(),
                        "configuration notification received"
                    );
                    if sender.send(ConfigEvent::Changed).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    event!(Level::WARN, %error, "PostgreSQL notification listener failed; reconnecting");
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
