//! `sailor-hook inbox [--watch]` — the inbox as NDJSON on stdout.
//!
//! This is how the phone reads the inbox. `TECH.md` §2.2 routes the app at
//! the HTTP gateway over an SSH local-forward, but NMSSH exposes no
//! `direct-tcpip` (same gap that blocks jump hosts on iOS), so there is no
//! forward to ride today. An SSH *exec* channel already works on both
//! platforms — so the app runs this and reads lines. The gateway keeps the
//! same data at `GET`/`WS /events` for Phase 4, when the diff WebView needs
//! a real HTTP origin and the forward has to exist anyway.
//!
//! One JSON row per line, so a reader can parse incrementally without
//! framing.

use crate::ipc::{socket_path, Client, ClientMessage, ServerMessage};

pub async fn run(watch: bool) -> anyhow::Result<()> {
    let mut client = Client::connect(&socket_path()?)
        .await?
        .ok_or_else(|| anyhow::anyhow!("sailor-hook daemon is not running"))?;

    client
        .send(&ClientMessage::Subscribe { once: !watch })
        .await?;

    while let Some(message) = client.recv().await? {
        if let ServerMessage::Row { row } = message {
            println!("{}", serde_json::to_string(&row)?);
        }
    }
    Ok(())
}
