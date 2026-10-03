use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use anyhow::Result;
use clap::{Parser, Subcommand};
use simple_shared::mnt_protocols::{
    ClientRequest, ClientRequestContent, ServerResponse, ServerResponseContent, SyncZigZagVarint,
    MNT_PATH,
};
use simple_shared::objectid::ObjectId;

/// 简单的命令行客户端，用于与 WebGateway 的管理 Unix 套接字通信
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 可选的 Unix 套接字路径（默认使用协议中定义的 MNT_PATH）
    #[arg(short, long, default_value = MNT_PATH)]
    socket: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// 获取某个管理员的 TOTP 验证码
    AdminTotp,
    /// 把 `access_*_v1` 里的历史访问日志分批搬进 v2 分区表
    ///
    /// 服务端会持续推送进度，搬完打印汇总。**不会删除 v1** ——
    /// 核对无误后请手工 `DROP TABLE access_*_v1`（或先留着当备份）。
    MigrateV2 {
        /// 从零重搬：清空 v2 四表并重置进度（默认从上次游标续跑）
        #[arg(long)]
        reset: bool,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();

    // 1. 连接到 Unix 套接字
    let mut stream = UnixStream::connect(&args.socket)?;

    // 2. 根据命令行参数构造请求
    // `streaming` 要在请求被 move 之前算出来：搬迁是流式命令，其余是一问一答。
    let streaming = matches!(args.command, Command::MigrateV2 { .. });
    let request_content = match args.command {
        Command::AdminTotp => ClientRequestContent::AdminTOTP,
        Command::MigrateV2 { reset } => ClientRequestContent::MigrateV2 { reset },
    };

    let id = ObjectId::new();
    let request = ClientRequest {
        id,
        content: request_content,
    };

    // 3. 序列化并发送
    let buf = serde_json::to_vec(&request)?;
    stream.write_zigzag_varint::<usize>(buf.len())?;
    stream.write_all(&buf)?;

    // 4. 读响应。搬迁是长任务：服务端会先连推若干条进度，最后给 Done 或 error，
    //    因此这里循环读到"终态"为止（一问一答的命令读一条就结束）。
    loop {
        let size = stream.read_zigzag_varint::<usize>()?;
        let mut buf = vec![0; size];
        stream.read_exact(&mut buf)?;
        let response: ServerResponse = serde_json::from_slice(&buf)?;

        if let Some(err) = response.error {
            eprintln!("服务端返回错误: {err}");
            std::process::exit(1);
        }

        // `done` 必须在 move 之前判定（下面要把 content 取出来用）。
        let done = !streaming
            || matches!(
                response.content,
                Some(ServerResponseContent::MigrateV2Done { .. })
            );

        match response.content {
            Some(ServerResponseContent::AdminTOTP { user, totp }) => {
                println!("User: {user}");
                println!("TOTP: {totp}");
            }
            Some(ServerResponseContent::MigrateV2Progress { message, .. }) => {
                // 进度用 `\r` 覆盖同一行；非终端环境（日志/重定向）下退化成逐行输出。
                use std::io::IsTerminal;
                if std::io::stdout().is_terminal() {
                    print!("\r{message}\x1b[K");
                    let _ = std::io::stdout().flush();
                } else {
                    println!("{message}");
                }
            }
            Some(ServerResponseContent::MigrateV2Done {
                copied_rows,
                v1_rows,
                v2_rows,
                message,
            }) => {
                println!("\n{message}");
                println!(
                    "统计：本次写入 {copied_rows} 行；v1 共 {v1_rows} 行；v2 共 {v2_rows} 行"
                );
            }
            None => {
                eprintln!("服务端返回了空响应");
                std::process::exit(1);
            }
        }

        if done {
            break;
        }
    }

    Ok(())
}
