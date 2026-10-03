use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::objectid::ObjectId;
/// 默认 socket 路径；后端可用 `MNT_SOCKET` 覆盖（客户端用 `--socket` 指定）。
pub static MNT_PATH: &str = "/tmp/webgateway-mnt.sock";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", content = "payload")]
pub enum ClientRequestContent {
    AdminTOTP,
    /// 把 `access_log_v2_migration` 里记录的 v1 历史数据**分批搬进 v2 分区表**。
    ///
    /// 这条命令是长任务（千万行级别、分钟到小时），因此**不是一问一答**：
    /// 服务端会先连续推送若干 [`ServerResponseContent::MigrateV2Progress`]，
    /// 最后推送一个 [`ServerResponseContent::MigrateV2Done`]。
    /// 客户端读到 `Done`（或收到 `error`）才算结束。
    ///
    /// `reset = true` 表示从零重搬（丢弃已有进度与 v2 中已搬入的行）；
    /// 否则从进度表的游标**续跑**。
    MigrateV2 { reset: bool },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClientRequest {
    pub id: ObjectId,
    #[serde(flatten)]
    pub content: ClientRequestContent,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", content = "payload")]
pub enum ServerResponseContent {
    AdminTOTP {
        user: String,
        totp: String,
    },
    /// 搬迁进度（可多次推送）。
    MigrateV2Progress {
        phase: String,
        table: String,
        copied_rows: i64,
        total_rows: i64,
        message: String,
    },
    /// 搬迁结束（成功）。
    MigrateV2Done {
        copied_rows: i64,
        v1_rows: i64,
        v2_rows: i64,
        message: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerResponse {
    pub id: ObjectId,
    #[serde(flatten)]
    pub content: Option<ServerResponseContent>,
    pub error: Option<String>,
}

pub trait AsyncZigZagVarint<'a>:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync
{
    fn read_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &'a mut self,
    ) -> impl Future<Output = Result<T, std::io::Error>>;
    fn write_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &'a mut self,
        value: T,
    ) -> impl Future<Output = Result<(), std::io::Error>>;
}

// UnixStream
impl<'a> AsyncZigZagVarint<'a> for tokio::net::UnixStream {
    async fn read_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &'a mut self,
    ) -> Result<T, std::io::Error> {
        let size = size_of::<T>() * 8;
        let mut result = T::zero();
        let mut shift = 0;

        loop {
            let mut buf = [0u8; 1];
            if self.read_exact(&mut buf).await? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading varint",
                ));
            }
            let byte = buf[0];
            let value = T::from(byte & 0x7F).unwrap(); // low 7 bits
            result = result | (value << shift);
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "varint exceeds maximum size for type",
                ));
            }
        }

        // ZigZag decoding: for signed T, (n >> 1) ^ -(n & 1)
        // But to avoid negation on generic T, we use the standard formula:
        // (result >> 1) ^ (-(result & 1))
        // However, bitwise negation on signed integers is tricky. Instead, we do:
        let decoded = (result >> 1) ^ (result & T::one());
        Ok(decoded)
    }

    async fn write_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &'a mut self,
        value: T,
    ) -> Result<(), std::io::Error> {
        let bits = std::mem::size_of::<T>() * 8;
        let shift = bits - 1;
        let mut n = (value << 1) ^ (value >> shift); // ZigZag encoding, still type T, but non-negative
        loop {
            let mut byte = (n & T::from(0x7F).unwrap()).to_u8().unwrap(); // need to convert to u8
            n = n >> 7;
            if n != T::zero() {
                byte |= 0x80;
            }
            self.write_all(&[byte]).await?;
            if n == T::zero() {
                break;
            }
        }
        Ok(())
    }
}

pub trait SyncZigZagVarint: std::io::Read + std::io::Write {
    /// 从流中读取一个 ZigZag 编码的 varint，并解码为无符号整数 T
    fn read_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &mut self,
    ) -> Result<T, std::io::Error>;

    /// 将无符号整数 T 进行 ZigZag 编码后写入流
    fn write_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &mut self,
        value: T,
    ) -> Result<(), std::io::Error>;
}

impl SyncZigZagVarint for UnixStream {
    fn read_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &mut self,
    ) -> Result<T, std::io::Error> {
        let size = std::mem::size_of::<T>() * 8;
        let mut result = T::zero();
        let mut shift = 0;

        loop {
            let mut buf = [0u8; 1];
            if self.read_exact(&mut buf).is_err() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading varint",
                ));
            }
            let byte = buf[0];
            let value = T::from(byte & 0x7F).unwrap(); // 取低 7 位
            result = result | (value << shift);
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "varint exceeds maximum size for type",
                ));
            }
        }

        // ZigZag 解码：对于有符号整数编码为无符号整数的还原
        // 公式：(result >> 1) ^ (-(result & 1))，但这里用无符号运算等价形式
        let decoded = (result >> 1) ^ (result & T::one());
        Ok(decoded)
    }

    fn write_zigzag_varint<T: num_traits::PrimInt + num_traits::Unsigned>(
        &mut self,
        value: T,
    ) -> Result<(), std::io::Error> {
        let bits = std::mem::size_of::<T>() * 8;
        let shift = bits - 1;
        // ZigZag 编码：对于有符号整数， (value << 1) ^ (value >> (bits-1))
        // 这里 value 已经是无符号类型，但原值可能是有符号的，通过调用者保证
        let mut n = (value << 1) ^ (value >> shift);
        loop {
            let mut byte = (n & T::from(0x7F).unwrap()).to_u8().unwrap(); // 取低 7 位
            n = n >> 7;
            if n != T::zero() {
                byte |= 0x80; // 设置延续位
            }
            self.write_all(&[byte])?;
            if n == T::zero() {
                break;
            }
        }
        Ok(())
    }
}
