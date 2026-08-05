//! Print daemon + hook status: pairing token, gateway liveness, and which
//! agents currently have sailor hooks installed.

use crate::{commands::install, secret};

const GATEWAY_PORT: u16 = 24543;

pub fn run(json: bool) -> anyhow::Result<()> {
    let paired = secret::load_pairing_token().is_ok();
    let running = gateway_running(GATEWAY_PORT);
    let agents = install::installed_agents();

    let state = crate::paths::state_dir()?;
    let config = crate::paths::config_dir()?;
    let socket = ipc_socket_path()?;
    let paths = serde_json::json!({
        "state": state,
        "config": config,
        "socket": socket,
        "lock": state.join("serve.lock"),
        "gateway_log": state.join("gateway.log"),
    });

    let status = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "paired": paired,
        "gateway": { "host": "127.0.0.1", "port": GATEWAY_PORT, "running": running },
        "agents_installed": agents,
        "paths": paths,
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("sailor-hook v{}", env!("CARGO_PKG_VERSION"));
        println!("  paired:     {}", if paired { "yes" } else { "no" });
        println!(
            "  gateway:    127.0.0.1:{GATEWAY_PORT} ({})",
            if running { "running" } else { "not running" }
        );
        println!(
            "  agents:     {}",
            if agents.is_empty() {
                "(none installed)".to_string()
            } else {
                agents.join(", ")
            }
        );
        println!("  state:      {}", state.display());
        println!("  config:     {}", config.display());
        println!("  socket:     {}", socket.display());
    }
    Ok(())
}

/// Resolve the hook socket path without connecting to it. Shared by the
/// status output and `serve` (which binds the same path).
fn ipc_socket_path() -> anyhow::Result<std::path::PathBuf> {
    crate::ipc::socket_path()
}

/// The gateway binds loopback only, so "can we connect to it" is the whole
/// liveness probe — no HTTP round trip needed.
fn gateway_running(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(250),
    )
    .is_ok()
}
