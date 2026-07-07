//! Print daemon + hook status. Phase 0: partial — reports whether the
//! pairing token exists and the gateway port. Full liveness probe lands
//! with the real daemon in Phase 3.

use crate::secret;

pub fn run(json: bool) -> anyhow::Result<()> {
    let paired = secret::load_pairing_token().is_ok();
    let status = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "paired": paired,
        "gateway": { "host": "127.0.0.1", "port": 24543, "running": false },
        "agents_installed": [],
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("sailor-hook v{}", env!("CARGO_PKG_VERSION"));
        println!("  paired:     {}", if paired { "yes" } else { "no" });
        println!("  gateway:    127.0.0.1:24543 (not running)");
        println!("  agents:     (none installed)");
    }
    Ok(())
}
