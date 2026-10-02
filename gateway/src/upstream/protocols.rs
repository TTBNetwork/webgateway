use std::time::Duration;

use protocols::{
    proxyprotocol::PRROXY_PROTOCOL_READ_BUF_SIZE,
    tls::{
        MAX_TLS_HANDSHAKE_LENGTH, ProtocolTLS, TLS_HANDSHAKE_START_LENGTH, get_tls_sni_from_buf,
        is_tls_handshake,
    },
};
use tokio::time::timeout;
// use tracing::event;

use shared::streams::BufferStream;

/// TLS / PROXY protocol 预读阶段的整体超时。
///
/// 预读发生在 hyper 层之前，`HTTP_BUILDER` 的 `header_read_timeout(30s)` 覆盖不到它。
/// 没有超时的话，慢速攻击者可以只发一个字节就长期占住一个连接与一份预读缓冲（ISSUES.md P0-10）。
const PRE_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub trait SimpleReadExt {
    fn pre_read_buf(&mut self, size: usize) -> impl Future<Output = tokio::io::Result<Vec<u8>>>;
}

impl SimpleReadExt for BufferStream {
    async fn pre_read_buf(&mut self, size: usize) -> tokio::io::Result<Vec<u8>> {
        // 上限保护：防止调用方（或解析器返回的 `WantMoreData`）申请超大缓冲。
        let size = size.min(MAX_TLS_HANDSHAKE_LENGTH);
        let mut buf = vec![0u8; size];
        let size = self.pre_read(&mut buf).await?;
        Ok(buf[..size].to_owned())
    }
}

#[allow(unused)]
pub async fn get_proxy_protocol(
    mut stream: BufferStream,
) -> anyhow::Result<(BufferStream, Option<()>)> {
    let buf = timeout(
        PRE_READ_TIMEOUT,
        stream.pre_read_buf(PRROXY_PROTOCOL_READ_BUF_SIZE),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out while reading PROXY protocol header"))??;
    // TODO: implement
    let _ = buf;
    Ok((stream.into_inner(), None))
}

pub async fn get_tls_sni(
    mut stream: BufferStream,
) -> anyhow::Result<(BufferStream, Option<ProtocolTLS>)> {
    let mut data = timeout(
        PRE_READ_TIMEOUT,
        stream.pre_read_buf(TLS_HANDSHAKE_START_LENGTH),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out while reading TLS record header"))??;
    if !is_tls_handshake(&data) {
        return Ok((stream.into_inner(), None));
    }
    data.extend(
        timeout(PRE_READ_TIMEOUT, stream.pre_read_buf(8192))
            .await
            .map_err(|_| anyhow::anyhow!("timed out while reading ClientHello"))??,
    );
    let mut current_length = data.len();
    let mut last_length = 0;
    loop {
        if last_length == current_length {
            break;
        }
        // 整个 ClientHello 的预读总量硬上限：`MAX_TLS_HANDSHAKE_LENGTH` 是握手长度字段的上限，
        // 再加上 5 字节记录头与 4 字节握手头，超出即说明数据异常，直接放弃解析。
        if data.len() > MAX_TLS_HANDSHAKE_LENGTH + 9 {
            return Ok((stream.into_inner(), None));
        }
        match get_tls_sni_from_buf(&data) {
            Ok(sni) => {
                // event!(tracing::Level::INFO, "TLS SNI: {:?}", &sni);
                return Ok((stream, sni));
            }
            Err(protocols::tls::ProtocolTLSError::WantMoreData(Some(n))) => {
                let want = n.min(MAX_TLS_HANDSHAKE_LENGTH);
                data.extend(
                    timeout(PRE_READ_TIMEOUT, stream.pre_read_buf(want))
                        .await
                        .map_err(|_| anyhow::anyhow!("timed out while reading ClientHello"))??,
                );
            }
            Err(protocols::tls::ProtocolTLSError::WantMoreData(None)) => {
                data.extend(
                    timeout(PRE_READ_TIMEOUT, stream.pre_read_buf(8192))
                        .await
                        .map_err(|_| anyhow::anyhow!("timed out while reading ClientHello"))??,
                );
            }
            Err(protocols::tls::ProtocolTLSError::HandshakeTooLarge { length, limit }) => {
                // 客户端声明了超长握手：不再读取数据，也不接受该连接。
                return Err(anyhow::anyhow!(
                    "rejecting ClientHello: declared handshake length {length} exceeds limit {limit}"
                ));
            }
        };
        last_length = current_length;
        current_length = data.len();
    }
    Ok((stream.into_inner(), None))
}
