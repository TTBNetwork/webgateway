//! Local-only operations channel (`mnt`).
//!
//! The socket is created with mode 0600 and lives on the dashboard host. It is
//! deliberately not an HTTP endpoint: the admin TOTP is the login factor, so a
//! TCP route would expose it to the network. Operators run the `webgateway-mnt`
//! client locally (or in the dashboard container) to fetch a current code or,
//! during enrollment, the secret/otpauth URI.

use std::{
    env,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::PathBuf,
    time::Duration,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use shared::database::Database;
use sqlx::Row;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{Level, event};

use crate::auth;

const DEFAULT_SOCKET: &str = "/run/webgateway/mnt.sock";

#[derive(Debug, Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    secret: bool,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(user: &str) -> Self {
        Self {
            ok: true,
            user: Some(user.to_string()),
            code: None,
            secret: None,
            uri: None,
            error: None,
        }
    }

    fn error(error: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            user: None,
            code: None,
            secret: None,
            uri: None,
            error: Some(error.to_string()),
        }
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn socket_path() -> PathBuf {
    env::var("MNT_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SOCKET))
}

pub async fn spawn(database: Database) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let path = socket_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "failed to create the mnt socket directory {}",
                parent.display()
            )
        })?;
    }
    match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) if metadata.file_type().is_socket() => {
            let _ = tokio::fs::remove_file(&path).await;
        }
        Ok(_) => anyhow::bail!(
            "mnt socket path {} exists and is not a socket",
            path.display()
        ),
        Err(_) => {}
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("failed to bind the mnt socket {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod the mnt socket {}", path.display()))?;
    event!(Level::INFO, path = %path.display(), "mnt socket listening");

    let guard = SocketGuard(path);
    let handle = tokio::spawn(async move {
        let _guard = guard;
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let database = database.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream, database).await {
                            event!(Level::WARN, %error, "mnt request failed");
                        }
                    });
                }
                Err(error) => {
                    event!(Level::WARN, %error, "mnt accept failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
    Ok(handle)
}

async fn handle_connection(stream: UnixStream, database: Database) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.len() > 4096 {
            break;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => match request.op.as_str() {
                "admin_totp" => match admin_totp(&database, request.secret).await {
                    Ok(response) => response,
                    Err(error) => Response::error(error),
                },
                other => Response::error(format!("unknown operation: {other}")),
            },
            Err(error) => Response::error(format!("invalid request: {error}")),
        };
        writer
            .write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}

async fn admin_totp(database: &Database, include_secret: bool) -> anyhow::Result<Response> {
    let row = sqlx::query(
        "SELECT username, totp_secret FROM users
          WHERE role = 'admin'
          ORDER BY created_at
          LIMIT 1",
    )
    .fetch_optional(database.pool())
    .await
    .context("failed to read the admin account")?;
    let Some(row) = row else {
        return Ok(Response::error("no admin user exists"));
    };
    let username: String = row.try_get("username")?;
    let sealed: String = row.try_get("totp_secret")?;
    let secret =
        shared::crypto::open(&sealed).context("failed to decrypt the admin TOTP secret")?;
    let code = auth::totp_code(&secret).context("the admin TOTP secret is invalid")?;

    let mut response = Response::ok(&username);
    response.code = Some(code);
    if include_secret {
        response.uri = Some(format!(
            "otpauth://totp/WebGateway:{username}?secret={secret}&issuer=WebGateway&algorithm=SHA1&digits=6&period=30"
        ));
        response.secret = Some(secret);
    }
    Ok(response)
}
