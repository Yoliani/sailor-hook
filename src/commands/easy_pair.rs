//! Easy Pair: start mosh-server locally and print a QR code the sailor app
//! scans to connect — no SSH from the phone at all.
//!
//! The normal mosh flow bootstraps over SSH to run `mosh-server new` and
//! read its `MOSH CONNECT <port> <key>` line. Easy Pair runs that first leg
//! here, on the host, and hands the result to the phone as a
//! `sailor://mosh?host=..&port=..&key=..` URI (parsed by the app's
//! lib/easyPair.ts — keep the two in sync). The key is the session's
//! authentication: it lives only in this terminal and the scanning phone.

use std::net::{IpAddr, SocketAddr, TcpListener, ToSocketAddrs};
use std::os::unix::process::CommandExt as _;
use std::process::Command;

pub async fn run(
    host: Option<String>,
    colors: u16,
    gateway_port: u16,
    serve: bool,
) -> anyhow::Result<()> {
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

    // A mosh-only session has no SSH leg to exec `sailor-hook inbox` on, so
    // the inbox needs its own channel: a second mosh-server running
    // `inbox --watch --stdin-approvals`. The phone's second mosh connection
    // streams rows and answers approvals over the pty (commands/inbox.rs).
    let inbox = spawn_inbox_session(colors)?;

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
    if let Some((inbox_port, inbox_key)) = &inbox {
        uri.push_str(&format!(
            "&inboxPort={inbox_port}&inboxKey={}",
            percent_encode(inbox_key)
        ));
    }
    // A mosh session is one channel and mosh only transmits the visible
    // screen, so anything the app wants to *read* from the host (session
    // listings) can't come back through the terminal once it's larger than a
    // screenful. When a daemon is reachable at this address, hand the phone
    // its port + token so it can ask over HTTP instead.
    let mut gateway = reachable_gateway(&host, gateway_port).await;
    let mut started_gateway = false;
    if gateway.is_none() && serve {
        // Only ever bind an address this machine actually holds. `--host` is
        // whatever the user typed — a tailnet IP, a MagicDNS name, a LAN
        // address — and the reliable test for "mine" is whether it can be
        // bound, which costs nothing and needs no interface enumeration.
        if let Some(ip) = own_address(&host) {
            match spawn_gateway(ip, gateway_port) {
                Ok(()) => {
                    gateway = await_gateway(&host, gateway_port).await;
                    started_gateway = gateway.is_some();
                }
                Err(e) => tracing::warn!("could not start the gateway: {e}"),
            }
        }
    }
    if let Some(token) = &gateway {
        uri.push_str(&format!(
            "&gw={gateway_port}&gwkey={}",
            percent_encode(token)
        ));
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
    if inbox.is_some() {
        println!("A second mosh-server carries the agent inbox on the same host.");
    } else {
        println!("No inbox channel: this binary can't locate itself.");
    }
    match (&gateway, started_gateway) {
        // Starting a network listener on the user's behalf is a side effect
        // that outlives this command, so say so plainly rather than leaving
        // them to discover a daemon they never launched.
        (Some(_), true) => {
            println!(
                "started the sailor-hook daemon on {host}:{gateway_port} (logs: {}).",
                gateway_log_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "n/a".into())
            );
            println!("session listing will use it. Stop it with `pkill -f 'sailor-hook serve'`.");
        }
        (Some(_), false) => {
            println!("session listing will use the daemon at {host}:{gateway_port}.")
        }
        (None, _) => println!(
            "no daemon reachable at {host}:{gateway_port} — session listing will fall back to \
             probing the terminal, which mosh truncates for large listings. Start one with \
             `sailor-hook serve --bind {host}`."
        ),
    }
    Ok(())
}

/// Start the inbox channel: a mosh-server whose command is
/// `sailor-hook inbox --watch --stdin-approvals`. Returns its
/// `(port, key)` so the URI can carry it. `None` when this binary can't
/// reach itself (moved after build) — the shell session still works, the
/// phone just gets no inbox for it.
fn spawn_inbox_session(colors: u16) -> anyhow::Result<Option<(u16, String)>> {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    let out = Command::new("mosh-server")
        .args([
            "new",
            "-c",
            &colors.to_string(),
            "--",
            exe.to_str().unwrap_or("sailor-hook"),
            "inbox",
            "--watch",
            "--stdin-approvals",
        ])
        .env("LC_ALL", "en_US.UTF-8")
        .output()
        .map_err(|e| anyhow::anyhow!("could not start the inbox mosh-server ({e})"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_connect_line(&stdout))
}

/// The address behind `--host`, if this machine holds it.
///
/// Binding is the test: a TCP bind to an address the host doesn't have fails
/// with EADDRNOTAVAIL. That covers a tailnet IP, a MagicDNS name, and a LAN
/// address without special-casing any of them — and refuses to auto-start a
/// daemon for a `--host` that points somewhere else entirely.
fn own_address(host: &str) -> Option<IpAddr> {
    for addr in (host, 0u16).to_socket_addrs().ok()? {
        // Port 0: the kernel picks an ephemeral one, so this never collides
        // with the port the daemon is about to take.
        if TcpListener::bind(SocketAddr::new(addr.ip(), 0)).is_ok() {
            return Some(addr.ip());
        }
    }
    None
}

/// Launch `sailor-hook serve --bind <ip>` detached, so it outlives this
/// command and the terminal that ran it.
fn spawn_gateway(bind: IpAddr, port: u16) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let log_path = gateway_log_path()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::File::create(&log_path)?;
    let errors = log.try_clone()?;
    Command::new(exe)
        .args([
            "serve",
            "--bind",
            &bind.to_string(),
            "--port",
            &port.to_string(),
            // Pairing already happened out of band (the QR); announcing this
            // host on the LAN is a separate decision the user hasn't made.
            "--no-advertise",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(errors)
        // Its own process group: closing the terminal that ran `easy-pair`
        // sends SIGHUP to the group, which would take the daemon with it.
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// Poll /health until the freshly spawned daemon answers. Binding a port and
/// starting to accept isn't instant, and the QR has to carry the token or
/// not — there is no revising it once printed.
async fn await_gateway(host: &str, port: u16) -> Option<String> {
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        if let Some(token) = reachable_gateway(host, port).await {
            return Some(token);
        }
    }
    None
}

fn gateway_log_path() -> anyhow::Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    Ok(home.join(".sailor").join("gateway.log"))
}

/// The gateway's token, when a daemon actually answers at `host:port`.
///
/// `/health` is unauthenticated precisely so this probe can tell "there is a
/// daemon here" from "there isn't" without holding a credential yet. The
/// token itself comes from local storage — the same value `serve` validates.
async fn reachable_gateway(host: &str, port: u16) -> Option<String> {
    let responded = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        reqwest::get(format!("http://{host}:{port}/health")),
    )
    .await
    .ok()?
    .ok()?
    .status()
    .is_success();
    if !responded {
        return None;
    }
    crate::secret::ensure_gateway_token().ok()
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
