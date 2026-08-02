//! The daemon's side of the hook socket: one task per connection, handling
//! the three `ipc::ClientMessage` kinds against the shared inbox and pending
//! registry.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::inbox::{Inbox, Row};
use crate::ipc::{ClientMessage, ServerMessage};
use crate::pending::Pending;

/// Everything a connection needs. `on_row` is the daemon's side effect for a
/// newly applied row (logging, push) — kept as a callback so this module
/// stays about the protocol.
pub struct Context {
    pub inbox: Arc<Inbox>,
    pub pending: Arc<Pending>,
    pub on_row: Box<dyn Fn(Row) + Send + Sync>,
}

pub async fn accept_loop(listener: UnixListener, ctx: Arc<Context>) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let ctx = Arc::clone(&ctx);
        // One task per connection: an approval parks its connection for
        // minutes, and must not hold up the next agent event.
        tokio::spawn(async move {
            if let Err(e) = handle(stream, ctx).await {
                tracing::debug!("hook connection ended: {e}");
            }
        });
    }
}

async fn handle(stream: UnixStream, ctx: Arc<Context>) -> anyhow::Result<()> {
    let (read, mut writer) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: ClientMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("dropping malformed hook message: {e}");
                continue;
            }
        };

        match message {
            ClientMessage::Event {
                event,
                await_decision,
            } => {
                let pending_action_id = event.pending_action_id;
                let row = ctx.inbox.apply(*event);
                (ctx.on_row)(row);

                if await_decision {
                    let Some(id) = pending_action_id else {
                        send(&mut writer, &ServerMessage::NoDecision).await?;
                        continue;
                    };
                    let rx = ctx.pending.register(id);
                    // The hook enforces its own deadline and drops the
                    // connection; that closes this task, which is what
                    // eventually clears the entry.
                    let reply = match rx.await {
                        Ok(allow) => ServerMessage::Decision { allow },
                        Err(_) => ServerMessage::NoDecision,
                    };
                    if let ServerMessage::Decision { allow } = &reply {
                        ctx.inbox.resolve(id, *allow);
                    }
                    send(&mut writer, &reply).await?;
                }
            }

            ClientMessage::Decision {
                pending_action_id,
                allow,
            } => {
                let ok = ctx.pending.resolve(pending_action_id, allow);
                let error = (!ok).then(|| {
                    "no approval is waiting on that id (already answered, or it timed out)"
                        .to_string()
                });
                send(&mut writer, &ServerMessage::Ack { ok, error }).await?;
            }

            ClientMessage::Subscribe { once } => {
                // Subscribe before snapshotting so a row landing in between
                // arrives late rather than not at all.
                let mut rx = ctx.inbox.subscribe();
                for row in ctx.inbox.rows() {
                    send(&mut writer, &ServerMessage::Row { row: Box::new(row) }).await?;
                }
                if once {
                    return Ok(());
                }
                while let Ok(row) = rx.recv().await {
                    send(&mut writer, &ServerMessage::Row { row: Box::new(row) }).await?;
                }
                return Ok(());
            }
        }
    }
    Ok(())
}

async fn send(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    message: &ServerMessage,
) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}
