//! Run the daemon: Unix socket (agent events) + HTTP gateway + WebSocket
//! (approvals/status) + push delivery.
//!
//! Phase 0: stub — prints the intended port and exits. Phase 3 wires the
//! real socket + gateway (see `gateway.rs`).

pub async fn run(port: u16) -> anyhow::Result<()> {
    tracing::info!("sailor-hook serve starting (gateway 127.0.0.1:{port})");
    tracing::info!("unix socket, websocket, push: not yet implemented (Phase 3).");
    anyhow::bail!("serve: not yet implemented (Phase 3)");
}
