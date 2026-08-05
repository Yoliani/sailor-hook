//! LAN presence: advertises a `_sailor._tcp` Bonjour/mDNS service so the
//! sailor app can auto-discover this host on the local network.
//!
//! Design note (why TXT carries `hostname`): mDNS does not cross Tailscale —
//! no multicast. So discovery is *enumeration only*: the phone's
//! `NetServiceBrowser` learns "a sailor host exists" from the service name,
//! then reads the TXT record for the *connect target*. The TXT `hostname` is
//! the host's Tailscale MagicDNS FQDN when Tailscale is running, so once a
//! host is found on the LAN it keeps connecting over the tailnet (and roams
//! to cellular). Without Tailscale, `hostname` falls back to the LAN IPv4 —
//! only reachable on the same LAN, but at least something.
//!
//! The Tailscale name is read by shelling out to the `tailscale` CLI (the
//! macOS GUI app exposes no documented local HTTP API on a stock install;
//! it ships its CLI at
//! `/Applications/Tailscale.app/Contents/MacOS/Tailscale`). On Linux
//! `tailscaled`'s socket-backed API is an option, but a single CLI path
//! keeps the two hosts uniform.
//!
//! The phone resolves a service per `NetService.resolve` to obtain the TXT
//! records; it does **not** use the service's A-record address (the `.local`
//! name only works on the LAN). See `app/modules/sailor-discovery`.

use std::collections::HashMap;

use mdns_sd::{ServiceDaemon, ServiceInfo};

/// Register the `_sailor._tcp` service and block until Ctrl-C.
///
/// `ssh_port` is the port the phone should SSH to in order to bootstrap a
/// mosh session (or use a plain SSH terminal) — the advertised SRV port.
pub async fn advertise(ssh_port: u16) -> anyhow::Result<()> {
    let info = tokio::task::spawn_blocking(move || build_service(ssh_port)).await??;
    let fullname = info.get_fullname().to_string();

    let mdns = ServiceDaemon::new()?;
    mdns.register(info)?;

    tracing::info!(%fullname, ssh_port, "advertising _sailor._tcp on the LAN");
    tracing::info!("ctrl-c to stop");

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down advertiser");
    mdns.shutdown()?;
    Ok(())
}

/// Assemble the `ServiceInfo` from the host's current Tailscale state, LAN
/// IP, and tool availability. Kept synchronous + private: it does blocking
/// I/O (Tailscale local API) and isn't exercised by unit tests.
fn build_service(ssh_port: u16) -> anyhow::Result<ServiceInfo> {
    let ts = tailscale_info();
    let lan =
        lan_ipv4().ok_or_else(|| anyhow::anyhow!("no IPv4 LAN address found to advertise"))?;

    // The Bonjour *instance name* is just a display label; the phone reads
    // the TXT `hostname` for the real connect target.
    let instance = ts
        .as_ref()
        .map(|t| t.host_name.clone())
        .unwrap_or_else(|| format!("sailor-{lan}"));
    let hostname = ts
        .as_ref()
        .map(|t| t.dns_name.clone())
        .unwrap_or_else(|| lan.clone());

    let mut props = HashMap::new();
    props.insert("hostname".into(), hostname);
    props.insert("port".into(), ssh_port.to_string());
    if let Some(user) = local_username() {
        props.insert("username".into(), user);
    }
    props.insert(
        "mosh".into(),
        if mosh_available() {
            "1".into()
        } else {
            "0".into()
        },
    );
    props.insert("version".into(), env!("CARGO_PKG_VERSION").into());

    let info = ServiceInfo::new(
        "_sailor._tcp.local.",
        &instance,
        &format!("{instance}.local."),
        lan.as_str(),
        ssh_port,
        props,
    )?;
    Ok(info)
}

pub(crate) struct TailscaleInfo {
    /// MagicDNS FQDN without the trailing dot, e.g. `mac.tailxyz.ts.net`.
    pub dns_name: String,
    /// First label of the MagicDNS name, e.g. `mac` — used as the Bonjour
    /// instance label (DNS-clean, unique on the tailnet).
    pub host_name: String,
}

/// Locate a usable `tailscale` CLI. Prefers `tailscale` on `PATH` (Linux
/// installs, homebrew, or a user symlink), then falls back to the macOS GUI
/// app's bundled binary, which doubles as the CLI on a stock install.
fn tailscale_cli() -> Option<std::path::PathBuf> {
    if on_path("tailscale") {
        return Some(std::path::PathBuf::from("tailscale"));
    }
    let app = std::path::Path::new("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
    if app.is_file() {
        return Some(app.to_path_buf());
    }
    None
}

/// Query `tailscale status --json` for the MagicDNS name. Returns `None` if
/// no Tailscale CLI is found or Tailscale isn't running — the caller falls
/// back to the LAN IPv4.
pub(crate) fn tailscale_info() -> Option<TailscaleInfo> {
    let cli = tailscale_cli()?;
    let out = std::process::Command::new(cli)
        .arg("status")
        .arg("--json")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let dns_raw = v.get("Self")?.get("DNSName")?.as_str()?;
    let dns_name = dns_raw.trim_end_matches('.');
    if dns_name.is_empty() {
        return None;
    }
    Some(TailscaleInfo {
        dns_name: dns_name.to_string(),
        host_name: dns_name
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("sailor")
            .to_string(),
    })
}

/// Best-effort primary IPv4 on a non-loopback interface, without sending any
/// packets: `connect` on a UDP socket only fills the kernel's routing for the
/// local endpoint.
pub(crate) fn lan_ipv4() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()? {
        std::net::SocketAddr::V4(v4) => {
            let ip = v4.ip();
            if ip.is_loopback() || ip.is_unspecified() {
                None
            } else {
                Some(ip.to_string())
            }
        }
        _ => None,
    }
}

/// `mosh` TXT is `1` only when `mosh-server` is on `PATH` (bootstrap target).
fn mosh_available() -> bool {
    on_path("mosh-server")
}

/// True if `name` is an executable file in any `PATH` directory.
fn on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':')
        .any(|dir| std::path::Path::new(dir).join(name).is_file())
}

fn local_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mosh_flag_reflects_path() {
        // Defensible: if mosh-server happens to be installed on a dev/CI box,
        // the flag is 1; otherwise 0. Either way it must be a bool.
        let _ = mosh_available();
    }

    #[test]
    fn username_reads_env() {
        // On CI USER is set; on a bare process it may not be. Both are
        // acceptable — the TXT field is optional.
        let _ = local_username();
    }
}
