//! Print daemon + hook status: pairing token, gateway liveness, and which
//! agents currently have sailor hooks installed.

use crate::{commands::install, secret};

const GATEWAY_PORT: u16 = 24543;

pub fn run(json: bool) -> anyhow::Result<()> {
    let paired = secret::load_pairing_token().is_ok();
    let running = gateway_running(GATEWAY_PORT);
    let agents = install::installed_agents();

    let status = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "paired": paired,
        "gateway": { "host": "127.0.0.1", "port": GATEWAY_PORT, "running": running },
        "agents_installed": agents,
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
    }
    Ok(())
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
