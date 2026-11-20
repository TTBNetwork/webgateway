//! Local operations client for the dashboard's Unix-socket management channel.
//!
//! The socket is created by the dashboard with mode 0600. It is intentionally
//! local-only: the TOTP code is the admin login factor, so exposing it over TCP
//! would weaken the panel. Run this binary on the dashboard host (or in the
//! dashboard container) to obtain the current code or, for enrollment,
//! the secret/otpauth URI.

use std::{
    env,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

use serde::Deserialize;

const DEFAULT_SOCKET: &str = "/run/webgateway/mnt.sock";

#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

fn usage() -> ! {
    eprintln!("usage: webgateway-mnt [--socket PATH] [--secret] [admin-totp]");
    eprintln!();
    eprintln!("  --socket PATH  Unix socket (default: $MNT_SOCKET or {DEFAULT_SOCKET})");
    eprintln!("  --secret       print the TOTP secret and otpauth URI instead of a code");
    std::process::exit(2);
}

fn main() {
    let mut socket = env::var("MNT_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
    let mut show_secret = false;
    let mut command = String::from("admin-totp");
    let args: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--socket" | "-s" => {
                index += 1;
                socket = args.get(index).cloned().unwrap_or_else(|| usage());
            }
            "--secret" => show_secret = true,
            "admin-totp" | "admin_totp" => command = String::from("admin-totp"),
            "--help" | "-h" => usage(),
            other => {
                eprintln!("unknown argument: {other}");
                usage();
            }
        }
        index += 1;
    }
    if command != "admin-totp" {
        usage();
    }

    let mut stream = UnixStream::connect(&socket).unwrap_or_else(|error| {
        eprintln!("cannot connect to {socket}: {error}");
        std::process::exit(1);
    });
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let request = serde_json::json!({ "op": "admin_totp", "secret": show_secret });
    writeln!(stream, "{request}").unwrap_or_else(|error| {
        eprintln!("failed to write the mnt request: {error}");
        std::process::exit(1);
    });

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .unwrap_or_else(|error| {
            eprintln!("failed to read the mnt response: {error}");
            std::process::exit(1);
        });
    let response: Response = serde_json::from_str(line.trim()).unwrap_or_else(|error| {
        eprintln!("invalid mnt response: {error}");
        std::process::exit(1);
    });
    if !response.ok {
        eprintln!(
            "mnt: {}",
            response.error.unwrap_or_else(|| "request failed".into())
        );
        std::process::exit(1);
    }

    let user = response.user.unwrap_or_else(|| "unknown".into());
    if show_secret {
        println!("User: {user}");
        println!("Secret: {}", response.secret.unwrap_or_default());
        println!("URI: {}", response.uri.unwrap_or_default());
    } else {
        println!("User: {user}");
        println!("TOTP: {}", response.code.unwrap_or_default());
    }
}
