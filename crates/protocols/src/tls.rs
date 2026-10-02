use std::io::{BufRead, BufReader, Read};

#[derive(Debug, Clone)]
pub enum ProtocolTLSError {
    WantMoreData(Option<usize>),
    /// 客户端声明的手shake长度超过 [`MAX_TLS_HANDSHAKE_LENGTH`]，直接拒绝而不是继续分配内存。
    HandshakeTooLarge {
        length: usize,
        limit: usize,
    },
}

#[derive(Debug, Clone)]
pub struct ProtocolTLS {
    pub hostname: Option<String>,
    pub version: u16,
}

pub type ProtocolTLSResult<T> = Result<T, ProtocolTLSError>;

pub const TLS_HANDSHAKE_PREFIX_LENGTH: usize = 1;
pub const TLS_HANDSHAKE_START_LENGTH: usize = 3;
/// ClientHello 手shake 消息长度的硬上限。
///
/// 握手长度字段是 3 字节大端，理论最大 0xFFFFFF（约 16MB）。攻击者只要在
/// ClientHello 头部声明这个长度，预读路径就会为其分配并等待读取 16MB，
/// 并发发起即可耗尽内存与 fd（ISSUES.md P0-10）。
/// 正常浏览器的 ClientHello 远小于 16KB，这里取 64KB 作为宽松但安全的上限。
pub const MAX_TLS_HANDSHAKE_LENGTH: usize = 64 * 1024;
const TLS_HANDSHAKE: &[u8; TLS_HANDSHAKE_PREFIX_LENGTH] = b"\x16";

pub fn is_tls_handshake(data: &[u8]) -> bool {
    data.starts_with(TLS_HANDSHAKE)
}

/// 从完整的 TLS 记录（包含记录头）中解析 ClientHello，返回 Option<ProtocolTLS>。
/// 如果数据不是 TLS 握手记录或不是 ClientHello，返回 Ok(None)；
/// 如果数据不足，返回 Err(ProtocolTLSError::WantMoreData(Some(n))) 或 None；
/// 成功解析则返回 Ok(Some(ProtocolTLS))。
pub fn parse_tls_client_hello(data: &[u8]) -> ProtocolTLSResult<Option<ProtocolTLS>> {
    // 至少需要 5 字节记录头 + 4 字节握手头
    if data.len() < 9 {
        return Err(ProtocolTLSError::WantMoreData(Some(9 - data.len())));
    }

    // 检查是否为握手记录 (0x16)
    if data[0] != 0x16 {
        // 不是 TLS 握手，直接返回 None
        return Ok(None);
    }

    // 提取 TLS 版本（记录层版本，仅用于非 ClientHello 情况）
    let _record_version = u16::from_be_bytes([data[1], data[2]]);

    // 记录长度（可选，用于验证）
    let _record_len = u16::from_be_bytes([data[3], data[4]]) as usize;

    // 检查握手消息类型是否为 ClientHello (0x01)
    if data[5] != 0x01 {
        return Ok(None);
    }

    // 握手消息长度（3 字节，大端）
    let handshake_len = u32::from_be_bytes([0, data[6], data[7], data[8]]) as usize;
    // 长度上限校验必须放在计算 `WantMoreData` 之前：否则单连接就能申请 16MB。
    if handshake_len > MAX_TLS_HANDSHAKE_LENGTH {
        return Err(ProtocolTLSError::HandshakeTooLarge {
            length: handshake_len,
            limit: MAX_TLS_HANDSHAKE_LENGTH,
        });
    }
    if data.len() < 9 + handshake_len {
        return Err(ProtocolTLSError::WantMoreData(Some(
            9 + handshake_len - data.len(),
        )));
    }

    // 现在解析 ClientHello 内部结构，起始位置为 9
    let mut pos = 9;

    // ---- Client version (2 字节) ----
    if data.len() < pos + 2 {
        return Err(ProtocolTLSError::WantMoreData(None)); // 不可能发生，因为已经确保有 handshake_len 字节
    }
    let client_version = u16::from_be_bytes([data[pos], data[pos + 1]]);
    pos += 2;

    // ---- Random (32 字节) ----
    if data.len() < pos + 32 {
        return Err(ProtocolTLSError::WantMoreData(None));
    }
    pos += 32;

    // ---- Session ID ----
    if data.len() < pos + 1 {
        return Err(ProtocolTLSError::WantMoreData(None));
    }
    let session_id_len = data[pos] as usize;
    pos += 1;
    if data.len() < pos + session_id_len {
        let needed = pos + session_id_len - data.len();
        return Err(ProtocolTLSError::WantMoreData(Some(needed)));
    }
    pos += session_id_len;

    // ---- Cipher Suites ----
    if data.len() < pos + 2 {
        return Err(ProtocolTLSError::WantMoreData(None));
    }
    let cipher_suites_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;
    if data.len() < pos + cipher_suites_len {
        let needed = pos + cipher_suites_len - data.len();
        return Err(ProtocolTLSError::WantMoreData(Some(needed)));
    }
    pos += cipher_suites_len;

    // ---- Compression Methods ----
    if data.len() < pos + 1 {
        return Err(ProtocolTLSError::WantMoreData(None));
    }
    let comp_methods_len = data[pos] as usize;
    pos += 1;
    if data.len() < pos + comp_methods_len {
        let needed = pos + comp_methods_len - data.len();
        return Err(ProtocolTLSError::WantMoreData(Some(needed)));
    }
    pos += comp_methods_len;

    // ---- Extensions ----
    // 可能没有扩展，这是允许的
    if data.len() < pos + 2 {
        return Ok(Some(ProtocolTLS {
            hostname: None,
            version: client_version,
        }));
    }
    let extensions_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;
    if data.len() < pos + extensions_len {
        let needed = pos + extensions_len - data.len();
        return Err(ProtocolTLSError::WantMoreData(Some(needed)));
    }
    let extensions_end = pos + extensions_len;

    let mut hostname = None;
    while pos + 4 <= extensions_end {
        let ext_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let ext_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        if pos + ext_len > extensions_end {
            // 扩展长度超出总扩展区域，数据异常，停止解析
            break;
        }

        if ext_type == 0x00 {
            // server_name 扩展
            let ext_data = &data[pos..pos + ext_len];
            // ext_data 格式：2 字节列表长度 + 列表项
            if ext_data.len() < 2 {
                break;
            }
            let list_len = u16::from_be_bytes([ext_data[0], ext_data[1]]) as usize;
            if ext_data.len() < 2 + list_len {
                break;
            }
            let mut name_pos = 2;
            let list_end = name_pos + list_len;
            while name_pos + 3 <= list_end {
                let name_type = ext_data[name_pos];
                let name_len =
                    u16::from_be_bytes([ext_data[name_pos + 1], ext_data[name_pos + 2]]) as usize;
                name_pos += 3;
                if name_pos + name_len > list_end {
                    break;
                }
                if name_type == 0x00 {
                    // host_name
                    let name_bytes = &ext_data[name_pos..name_pos + name_len];
                    hostname = Some(String::from_utf8_lossy(name_bytes).to_string());
                    break;
                }
                name_pos += name_len;
            }
            // 找到 SNI 后无需继续解析其他扩展
            break;
        }

        pos += ext_len;
    }

    Ok(Some(ProtocolTLS {
        hostname,
        version: client_version,
    }))
}

/// 仅提取 SNI 主机名（如果存在），否则返回空字符串。
/// 若数据不足，返回 WantMoreData。
pub fn get_tls_sni_from_buf(data: &[u8]) -> ProtocolTLSResult<Option<ProtocolTLS>> {
    parse_tls_client_hello(data)
}

pub trait SkipBufReader {
    fn skip(&mut self, n: usize) -> std::result::Result<(), std::io::Error>;
}

impl<T: BufRead + Read> SkipBufReader for BufReader<T> {
    fn skip(&mut self, n: usize) -> std::result::Result<(), std::io::Error> {
        let mut buf = vec![0u8; n];
        self.read_exact(&mut buf)
    }
}
