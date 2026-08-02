//! Easy Pair: start mosh-server locally and print a QR code the sailor app
//! scans to connect — no SSH from the phone at all.
//!
//! The normal mosh flow bootstraps over SSH to run `mosh-server new` and
//! read its `MOSH CONNECT <port> <key>` line. Easy Pair runs that first leg
//! here, on the host, and hands the result to the phone as a
//! `sailor://mosh?host=..&port=..&key=..` URI (parsed by the app's
//! lib/easyPair.ts — keep the two in sync). The key is the session's
//! authentication: it lives only in this terminal and the scanning phone.

use std::process::Command;

pub async fn run(host: Option<String>, colors: u16) -> anyhow::Result<()> {
    // mosh-server needs a UTF-8 locale; a bare env may not have one.
    let output = Command::new("mosh-server")
        .args(["new", "-c", &colors.to_string()])
        .env("LC_ALL", "en_US.UTF-8")
        .output()
        .map_err(|e| anyhow::anyhow!("could not run mosh-server ({e}) — is mosh installed?"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (port, key) = parse_connect_line(&stdout).ok_or_else(|| {
        anyhow::anyhow!(
            "mosh-server did not print a MOSH CONNECT line.\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;

    let host = match host {
        Some(h) => h,
        None => local_ip().ok_or_else(|| {
            anyhow::anyhow!("could not determine this machine's IP — pass --host <ip-or-name>")
        })?,
    };
    let user = std::env::var("USER").ok();
    let name = hostname();

    let mut uri = format!(
        "sailor://mosh?host={}&port={port}&key={}",
        percent_encode(&host),
        percent_encode(&key)
    );
    if let Some(user) = &user {
        uri.push_str(&format!("&user={}", percent_encode(user)));
    }
    if let Some(name) = &name {
        uri.push_str(&format!("&name={}", percent_encode(name)));
    }

    let qr = qrcode::QrCode::new(uri.as_bytes())?;
    let art = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build();

    println!("{art}");
    println!("Scan with the sailor app (+ → Easy Pair), or paste:");
    println!("  {uri}");
    println!();
    println!("mosh-server is waiting on udp {host}:{port}; the code is single-use.");
    Ok(())
}

/// Parse `MOSH CONNECT <port> <key>` out of mosh-server's stdout.
fn parse_connect_line(stdout: &str) -> Option<(u16, String)> {
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("MOSH") && parts.next() == Some("CONNECT") {
            let port: u16 = parts.next()?.parse().ok()?;
            let key = parts.next()?.to_string();
            return Some((port, key));
        }
    }
    None
}

/// The primary outbound IPv4 of this machine: the address of a UDP socket
/// "connected" to a public IP (no packets are sent).
fn local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn hostname() -> Option<String> {
    let out = Command::new("hostname").arg("-s").output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Minimal percent-encoding for URI query values (matches what the app's
/// decodeURIComponent expects; non-ASCII goes byte-by-byte as UTF-8).
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connect_line() {
        let out = "\nMOSH CONNECT 60001 zr0v9zLwXqFsdxls3Wq2iA\n";
        assert_eq!(
            parse_connect_line(out),
            Some((60001, "zr0v9zLwXqFsdxls3Wq2iA".to_string()))
        );
        assert_eq!(parse_connect_line("no connect here"), None);
        assert_eq!(parse_connect_line("MOSH CONNECT notaport key"), None);
    }

    #[test]
    fn percent_encodes_query_values() {
        assert_eq!(percent_encode("plain-value_1.2~"), "plain-value_1.2~");
        assert_eq!(percent_encode("My Mac"), "My%20Mac");
        assert_eq!(percent_encode("a/b+c"), "a%2Fb%2Bc");
    }
}
