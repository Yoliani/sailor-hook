//! `sailor-hook approve <pending-action-id> --allow|--deny` — answer an
//! approval the agent is parked on.
//!
//! This is what the phone runs over its SSH exec channel. It exits non-zero
//! when nothing was waiting on that id, so the app can tell "answered" from
//! "too late" rather than showing a success it didn't get.

use crate::ipc::socket_path;
use crate::ipc::{Client, ClientMessage, ServerMessage};

pub async fn run(pending_action_id: String, allow: bool) -> anyhow::Result<()> {
    let id = uuid::Uuid::parse_str(&pending_action_id)
        .map_err(|_| anyhow::anyhow!("`{pending_action_id}` is not a pending-action id"))?;

    let mut client = Client::connect(&socket_path()?)
        .await?
        .ok_or_else(|| anyhow::anyhow!("sailor-hook daemon is not running"))?;

    client
        .send(&ClientMessage::Decision {
            pending_action_id: id,
            allow,
        })
        .await?;

    match client.recv().await? {
        Some(ServerMessage::Ack { ok: true, .. }) => {
            println!("{}", if allow { "allowed" } else { "denied" });
            Ok(())
        }
        Some(ServerMessage::Ack { error, .. }) => {
            anyhow::bail!(error.unwrap_or_else(|| "approval could not be answered".into()))
        }
        _ => anyhow::bail!("unexpected reply from the daemon"),
    }
}
