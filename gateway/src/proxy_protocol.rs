//! PROXY protocol v1/v2 parsing (haproxy spec).
//!
//! The header arrives before TLS/HTTP, so it is consumed from the raw TCP
//! stream. Whether the parsed source address is trusted is decided per website
//! via `get_request_ip = { type: "proxyProtocol" }`.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use tokio::{io::AsyncReadExt, net::TcpStream};

/// v2 signature (12 bytes): \r\n\r\n\0\r\nQUIT\n
const V2_SIGNATURE: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProxyInfo {
    pub present: bool,
    pub source: Option<SocketAddr>,
}

/// Peek at the connection start and consume a PROXY header when present.
pub async fn read_header(stream: &mut TcpStream) -> io::Result<ProxyInfo> {
    let mut prefix = [0u8; 12];
    let read = stream.peek(&mut prefix).await?;
    if read == 0 {
        return Ok(ProxyInfo::default());
    }

    if read >= 12 && prefix == V2_SIGNATURE {
        return read_v2(stream).await;
    }
    if read >= 6 && prefix[..6] == *b"PROXY " {
        return read_v1(stream).await;
    }

    Ok(ProxyInfo::default())
}

async fn read_v1(stream: &mut TcpStream) -> io::Result<ProxyInfo> {
    let mut line = Vec::with_capacity(108);
    loop {
        let byte = stream.read_u8().await?;
        line.push(byte);
        if byte == b'\n' {
            break;
        }
        if line.len() > 108 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "PROXY v1 header is too long",
            ));
        }
    }

    Ok(ProxyInfo {
        present: true,
        source: parse_v1(&line),
    })
}

async fn read_v2(stream: &mut TcpStream) -> io::Result<ProxyInfo> {
    let mut header = [0u8; 16];
    stream.read_exact(&mut header).await?;

    if header[..12] != V2_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid PROXY v2 signature",
        ));
    }
    if header[12] >> 4 != 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported PROXY protocol version",
        ));
    }

    let command = header[12] & 0x0F;
    let family = header[13] >> 4;
    let transport = header[13] & 0x0F;
    let length = u16::from_be_bytes([header[14], header[15]]) as usize;

    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await?;

    // LOCAL: health checks etc.; the header carries no address.
    if command == 0 {
        return Ok(ProxyInfo {
            present: true,
            source: None,
        });
    }

    if transport != 1 {
        return Ok(ProxyInfo {
            present: true,
            source: None,
        });
    }

    let source = match family {
        // AF_INET
        1 if body.len() >= 12 => {
            let address = Ipv4Addr::new(body[0], body[1], body[2], body[3]);
            let port = u16::from_be_bytes([body[8], body[9]]);
            Some(SocketAddr::new(IpAddr::V4(address), port))
        }
        // AF_INET6
        2 if body.len() >= 36 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&body[..16]);
            let port = u16::from_be_bytes([body[32], body[33]]);
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port))
        }
        _ => None,
    };

    Ok(ProxyInfo {
        present: true,
        source,
    })
}

/// Parse a v1 header line, e.g. `PROXY TCP4 192.0.2.1 198.51.100.2 56324 443\r\n`.
pub fn parse_v1(line: &[u8]) -> Option<SocketAddr> {
    let text = std::str::from_utf8(line)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    let mut parts = text.split(' ');

    if parts.next()? != "PROXY" {
        return None;
    }

    let family = parts.next()?;
    if family == "UNKNOWN" {
        return None;
    }

    let source = parts.next()?;
    let _destination = parts.next()?;
    let source_port: u16 = parts.next()?.parse().ok()?;
    let _destination_port = parts.next()?;

    Some(SocketAddr::new(source.parse().ok()?, source_port))
}

/// Build a v2 header for tests.
#[cfg(test)]
pub fn build_v2(source: SocketAddr) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(&V2_SIGNATURE);
    packet.push(0x21); // version 2, PROXY
    let mut body = Vec::new();

    match source {
        SocketAddr::V4(address) => {
            packet.push(0x11); // AF_INET + STREAM
            body.extend_from_slice(&address.ip().octets());
            body.extend_from_slice(&[127, 0, 0, 1]); // destination
            body.extend_from_slice(&address.port().to_be_bytes());
            body.extend_from_slice(&0u16.to_be_bytes());
        }
        SocketAddr::V6(address) => {
            packet.push(0x21); // AF_INET6 + STREAM
            body.extend_from_slice(&address.ip().octets());
            body.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
            body.extend_from_slice(&address.port().to_be_bytes());
            body.extend_from_slice(&0u16.to_be_bytes());
        }
    }

    packet.extend_from_slice(&(body.len() as u16).to_be_bytes());
    packet.extend_from_slice(&body);
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v1_tcp4() {
        let line = b"PROXY TCP4 192.0.2.7 198.51.100.2 56324 443\r\n";
        assert_eq!(parse_v1(line), Some("192.0.2.7:56324".parse().unwrap()));
    }

    #[test]
    fn parses_v1_tcp6() {
        let line = b"PROXY TCP6 2001:db8::1 2001:db8::2 4000 443\r\n";
        assert_eq!(parse_v1(line), Some("[2001:db8::1]:4000".parse().unwrap()));
    }

    #[test]
    fn ignores_v1_unknown() {
        assert_eq!(parse_v1(b"PROXY UNKNOWN\r\n"), None);
    }

    #[test]
    fn v2_packet_round_trips() {
        let source: SocketAddr = "192.0.2.7:56324".parse().unwrap();
        let packet = build_v2(source);

        assert!(packet.starts_with(&V2_SIGNATURE));
        let family = packet[13] >> 4;
        let transport = packet[13] & 0x0F;
        assert_eq!(family, 1);
        assert_eq!(transport, 1);

        let length = u16::from_be_bytes([packet[14], packet[15]]) as usize;
        let body = &packet[16..16 + length];
        assert_eq!(
            SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(body[0], body[1], body[2], body[3])),
                u16::from_be_bytes([body[8], body[9]])
            ),
            source
        );
    }
}
