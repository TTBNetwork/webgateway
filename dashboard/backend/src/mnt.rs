use std::{os::unix::net::SocketAddr as UnixSocketAddr, path::PathBuf};

use anyhow::Result;
use shared::{
    database::get_database,
    mnt_protocols::{
        AsyncZigZagVarint, ClientRequest, ClientRequestContent, MNT_PATH, ServerResponse,
        ServerResponseContent,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};
use tracing::event;

use shared::objectid::ObjectId;

use crate::{
    auth::get_totp_code,
    database::{auth::Authentication, log::WebLogManager},
    models::log::LogAddr,
};

pub struct AutoCleanUnixListener(PathBuf);

impl Drop for AutoCleanUnixListener {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// mnt 管理通道的 socket 路径。
///
/// 默认 [`MNT_PATH`]（`/tmp/webgateway-mnt.sock`），可用 `MNT_SOCKET` 覆盖。
/// 覆盖是必要的运维能力：容器/沙箱各自有私有 `/tmp` 时，把 socket 放到
/// 双方共享的目录（例如部署目录下的 `mnt.sock`）才能让 `mnt` 客户端连上。
pub fn socket_path() -> PathBuf {
    std::env::var("MNT_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(MNT_PATH))
}

pub async fn init() -> anyhow::Result<()> {
    // unix
    let path = socket_path();
    let _ = AutoCleanUnixListener(path.clone());
    event!(tracing::Level::INFO, "starting unix listener");
    let listener = UnixListener::bind(&path)?;
    event!(tracing::Level::INFO, "unix listener started");
    loop {
        let (stream, addr) = listener.accept().await?;
        event!(tracing::Level::INFO, "new unix connection: {addr:?}");
        tokio::spawn(async move {
            let res = handle(stream, UnixSocketAddr::from(addr)).await;
            if let Err(e) = res {
                event!(tracing::Level::DEBUG, "handle error: {e:?}");
            }
        });
    }
}

async fn handle(mut stream: UnixStream, addr: UnixSocketAddr) -> Result<()> {
    loop {
        let size = stream.read_zigzag_varint::<usize>().await?;
        let mut buf = vec![0; size];
        stream.read_exact(&mut buf).await?;
        let data = serde_json::from_slice::<ClientRequest>(&buf)?;
        let id = data.id;

        // 长任务（搬迁）需要**多次推送**：进度一条条发，最后发 Done/error。
        // 其余命令仍是一问一答，用同一个发送函数写回。
        match data.content {
            ClientRequestContent::MigrateV2 { reset } => {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerResponse>(32);
                let task = tokio::spawn(async move { migrate_v2_task(reset, tx).await });
                while let Some(resp) = rx.recv().await {
                    send(&mut stream, &resp).await?;
                }
                // 任务自己负责把最后一条（Done 或 error）发出来；这里只兜底打印异常。
                if let Err(e) = task.await {
                    event!(tracing::Level::ERROR, "migrate_v2 task panicked: {e}");
                }
            }
            other => {
                let response = match handle_req(other, &addr).await {
                    Ok(res) => ServerResponse {
                        id,
                        content: Some(res),
                        error: None,
                    },
                    Err(e) => ServerResponse {
                        id,
                        content: None,
                        error: Some(e.to_string()),
                    },
                };
                send(&mut stream, &response).await?;
            }
        }
    }
}

/// 把一条响应写回客户端（`长度 + JSON`，与请求同格式）。
async fn send(stream: &mut UnixStream, response: &ServerResponse) -> Result<()> {
    let buf = serde_json::to_vec(response)?;
    stream.write_zigzag_varint::<usize>(buf.len()).await?;
    stream.write_all(&buf).await?;
    Ok(())
}

/// 长任务：把 `*_v1` 历史数据分批搬进 v2，边搬边通过 `tx` 推送进度。
///
/// 为什么放在后端而不是 mnt 客户端：数据库连接与凭据都在后端（`get_database()`），
/// mnt 只是控制通道 —— 这也是 `mnt` 现有的设计（AdminTOTP 就是这么拿的）。
///
/// 注意：**搬完不删 v1**。核对无误后由人手工 `DROP`，避免自动化误删历史数据。
async fn migrate_v2_task(
    reset: bool,
    tx: tokio::sync::mpsc::Sender<ServerResponse>,
) -> Result<()> {
    use shared::database::access_v2;

    let id = ObjectId::new();
    let pool = &get_database().pool;
    let started = std::time::Instant::now();

    // 进度是"边搬边推"，但 `copy_v1_into_v2` 的回调是同步 FnMut，而发送是异步的。
    // 用一个无界通道把回调里的进度转交给异步发送循环（进度条丢一条不影响正确性）。
    let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<access_v2::BatchProgress>();
    let pump = {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut last_sent = std::time::Instant::now() - std::time::Duration::from_secs(60);
            while let Some(p) = prx.recv().await {
                // 按时间节流：每天一批、1284 天，不节流会刷出上千条消息。
                let pct = if p.total_rows > 0 {
                    (p.copied_rows as f64 / p.total_rows as f64 * 100.0).min(100.0)
                } else {
                    0.0
                };
                let finished_all = p.copied_rows >= p.total_rows;
                if !finished_all && last_sent.elapsed() < std::time::Duration::from_secs(3) {
                    continue;
                }
                last_sent = std::time::Instant::now();
                // 无界通道：这里不做背压（进度条丢几条不影响正确性）。
                let _ = tx.send(ServerResponse {
                    id,
                    content: Some(ServerResponseContent::MigrateV2Progress {
                        phase: "backfilling".to_string(),
                        table: p.table.clone(),
                        copied_rows: p.copied_rows,
                        total_rows: p.total_rows,
                        message: format!(
                            "{:>5.1}%  当前表 {}  已写入 {} 行  游标 {}",
                            pct,
                            p.table,
                            p.copied_rows,
                            p.cursor.format("%Y-%m-%d %H:%M")
                        ),
                    }),
                    error: None,
                });
            }
        })
    };

    let result = access_v2::copy_v1_into_v2(pool, reset, |p| {
        let _ = ptx.send(p);
    })
    .await;
    drop(ptx);
    let _ = pump.await;

    let response = match result {
        Ok(copied) => {
            let v1_rows = access_v2::v1_total_rows(pool).await.unwrap_or(-1);
            let v2_rows = access_v2::v2_total_rows(pool).await.unwrap_or(-1);
            let elapsed = started.elapsed().as_secs();
            ServerResponse {
                id,
                content: Some(ServerResponseContent::MigrateV2Done {
                    copied_rows: copied,
                    v1_rows,
                    v2_rows,
                    message: format!(
                        "搬迁完成：本次写入 {copied} 行，用时 {elapsed}s；\
                         v1 剩余 {v1_rows} 行、v2 现有 {v2_rows} 行。\
                         **v1 未删除** —— 核对无误后请手工 DROP"
                    ),
                }),
                error: None,
            }
        }
        Err(e) => {
            let _ = access_v2::mark_error(pool, &format!("{e:#}")).await;
            ServerResponse {
                id,
                content: None,
                error: Some(format!("{e:#}")),
            }
        }
    };
    let _ = tx.send(response).await;
    Ok(())
}

async fn handle_req(
    req: ClientRequestContent,
    addr: &UnixSocketAddr,
) -> Result<ServerResponseContent> {
    match req {
        // 长任务在 `handle` 里就地处理（要流式推送进度），不会走到这里。
        ClientRequestContent::MigrateV2 { .. } => {
            anyhow::bail!("MigrateV2 必须由流式通道处理")
        }
        ClientRequestContent::AdminTOTP => {
            let user = get_database().get_first_user().await?;
            let code = get_totp_code(&user.username, user.totp_secret)?;
            get_database()
                .add_web_log(
                    &user.id,
                    &crate::models::log::LogContent::Raw("mnt.get_totp".to_string()),
                    &LogAddr::from(addr.clone()),
                )
                .await?;
            Ok(ServerResponseContent::AdminTOTP {
                user: user.username.to_string(),
                totp: code,
            })
        }
    }
}
