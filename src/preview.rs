//! Local dev-server discovery for the Phase 4 browser preview.
//!
//! Scans for TCP listeners on `127.0.0.1` that are responding to HTTP. The
//! results are served at `GET /preview` and the app opens a per-session SSH
//! local-forward to the gateway so the phone can reach them without any
//! tunnel on the user's part.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

use anyhow::Context;

/// One discovered dev server.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DevServer {
    /// Port number.
    pub port: u16,
    /// Process name or PID that owns the listener.
    pub process: String,
    /// Bind address — should always be `127.0.0.1`.
    pub bind: String,
    /// Detected name from the HTTP response headers (X-App-Name, Server, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Collect all HTTP servers listening on localhost.
pub fn discover() -> anyhow::Result<Vec<DevServer>> {
    let listeners = listeners()?;
    let mut servers = Vec::new();
    for addr in listeners {
        let name = probe_http(&addr).ok().flatten();
        servers.push(DevServer {
            port: addr.port(),
            process: process_name(&addr).unwrap_or_else(|_| "unknown".to_owned()),
            bind: addr.ip().to_string(),
            name,
        });
    }
    Ok(servers)
}

// ---------------------------------------------------------------------------
// listing listeners
// ---------------------------------------------------------------------------

/// Parse `lsof -iTCP -sTCP:LISTEN -n -P` output on macOS.
///
/// We could use `ss` on Linux, but sailor-hook is primarily for macOS
/// developers (the app is iOS-first). If Linux is needed later, add a
/// `cfg(unix)` branch with `ss -tulnp`.
fn listeners() -> anyhow::Result<Vec<SocketAddr>> {
    let output = Command::new("lsof")
        .args([
            "-iTCP",
            "-sTCP:LISTEN",
            "-n", // numeric, no DNS
            "-P", // numeric ports
        ])
        .output()
        .context("failed to run lsof")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut addrs = Vec::new();
    for line in text.lines().skip(1) {
        // Format: COMMAND  PID USER  FD   TYPE DEVICE SIZE/OFF NODE NAME
        // NAME is the last column: e.g. 127.0.0.1:8080 (LISTEN)
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let name_field = fields[8];
        // Parse "127.0.0.1:8080" or "[::1]:8080"
        if let Some(addr) = parse_socket(name_field) {
            if addr.ip().is_loopback() {
                addrs.push(addr);
            }
        }
    }
    addrs.sort();
    addrs.dedup();
    Ok(addrs)
}

/// Parse a socket string like `127.0.0.1:8080` or `[::1]:8080`.
fn parse_socket(s: &str) -> Option<SocketAddr> {
    let s = s.trim_end_matches("(LISTEN)").trim();
    if let Some(stripped) = s.strip_prefix('[') {
        let end = stripped.find(']')?;
        let ip = stripped[0..end].parse().ok()?;
        let port = stripped[end + 1..].trim_start_matches(':').parse().ok()?;
        return Some(SocketAddr::new(ip, port));
    }
    let parts: Vec<&str> = s.rsplitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let port = parts[0].parse().ok()?;
    let ip = parts[1].parse().ok()?;
    Some(SocketAddr::new(ip, port))
}

// ---------------------------------------------------------------------------
// HTTP probing
// ---------------------------------------------------------------------------

/// Send a minimal HTTP/1.1 GET to the port and extract a name from headers.
/// Returns `None` if the port is not HTTP or times out.
fn probe_http(addr: &SocketAddr) -> anyhow::Result<Option<String>> {
    let mut stream = TcpStream::connect_timeout(addr, Duration::from_millis(300))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .ok();
    let _ =
        stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nConnection: close\r\n\r\n");
    let mut buf = Vec::new();
    let mut n = 0usize;
    loop {
        let mut tmp = [0u8; 4096];
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n_) => {
                n += n_;
                buf.extend_from_slice(&tmp[..n_]);
                if n > 32 * 1024 {
                    break; // safety cap
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    Ok(extract_name(&text))
}

fn extract_name(headers: &str) -> Option<String> {
    // Try custom headers first, then Server header.
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("x-app-name:") {
            return Some(line.split_once(':').unwrap().1.trim().to_owned());
        }
        if lower.starts_with("server:") {
            let val = line.split_once(':').unwrap().1.trim();
            // Skip generic servers; we want app-level names.
            if !val.contains("nginx") && !val.contains("apache") && !val.contains("gunicorn") {
                return Some(val.to_owned());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// process name
// ---------------------------------------------------------------------------

fn process_name(addr: &SocketAddr) -> anyhow::Result<String> {
    let output = Command::new("lsof")
        .args(["-i", &format!("{}:{}", addr.ip(), addr.port()), "-n", "-P"])
        .output()
        .context("failed to run lsof")?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 2 {
            return Ok(fields[0].to_owned());
        }
    }
    anyhow::bail!("no process found for {addr}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_socket_v4() {
        assert_eq!(
            parse_socket("127.0.0.1:8080"),
            Some(SocketAddr::new("127.0.0.1".parse().unwrap(), 8080))
        );
    }

    #[test]
    fn parse_socket_v6() {
        assert_eq!(
            parse_socket("[::1]:3000"),
            Some(SocketAddr::new("::1".parse().unwrap(), 3000))
        );
    }

    #[test]
    fn parse_socket_with_listen_suffix() {
        assert_eq!(
            parse_socket("127.0.0.1:8080 (LISTEN)"),
            Some(SocketAddr::new("127.0.0.1".parse().unwrap(), 8080))
        );
    }
}
