//! Unix socket between agent hooks, the CLI, and the daemon (`TECH.md` §2.2).
//!
//! Three kinds of client talk over it, one tagged JSON line each:
//!
//! - `event` — an agent hook reporting something. Fire-and-forget, except
//!   for a decidable approval, where the hook holds the connection open and
//!   reads one `decision` line back (see `pending.rs`).
//! - `decision` — the phone answering a pending approval, relayed by
//!   `sailor-hook approve`.
//! - `subscribe` — the app tailing the inbox as NDJSON, via
//!   `sailor-hook inbox --watch` over an SSH exec channel.
//!
//! The transport stays deliberately small: one JSON line per message. The
//! event path in particular is on the critical path of somebody's coding
//! agent and must never block it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::events::Event;
use crate::inbox::Row;

/// What a client sends the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// An agent hook event. `await_decision` asks the daemon to hold the
    /// connection open until the phone answers.
    Event {
        event: Box<Event>,
        #[serde(default)]
        await_decision: bool,
    },
    /// Resolve a pending approval.
    Decision {
        pending_action_id: uuid::Uuid,
        allow: bool,
    },
    /// Stream inbox rows: a snapshot, then every update.
    Subscribe {
        /// Send the snapshot and stop, rather than following.
        #[serde(default)]
        once: bool,
    },
}

/// What the daemon sends back.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The answer to a decidable approval.
    Decision { allow: bool },
    /// The approval was never answered (the phone didn't, or nobody was
    /// listening) — the agent falls back to asking in the terminal.
    NoDecision,
    /// One inbox row, for `subscribe`.
    Row { row: Box<Row> },
    /// The result of a `decision` message.
    Ack { ok: bool, error: Option<String> },
}

/// `$SAILOR_HOOK_SOCKET` wins (tests, non-standard layouts), else
/// `<state>/hook.sock` where state is `~/.sailor` or `$SAILOR_STATE_DIR`.
pub fn socket_path() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("SAILOR_HOOK_SOCKET") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(crate::paths::state_dir()?.join("hook.sock"))
}

/// An open connection to the daemon, for clients that expect replies.
pub struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    /// Returns `Ok(None)` when no daemon is listening — a normal state, not a
    /// failure the agent should ever hear about.
    pub async fn connect(path: &Path) -> anyhow::Result<Option<Client>> {
        let stream = match UnixStream::connect(path).await {
            Ok(s) => s,
            Err(e) if is_not_listening(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let (read, writer) = stream.into_split();
        Ok(Some(Client {
            reader: BufReader::new(read),
            writer,
        }))
    }

    pub async fn send(&mut self, message: &ClientMessage) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.writer.write_all(&line).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Read one reply. `Ok(None)` means the daemon closed the connection.
    pub async fn recv(&mut self) -> anyhow::Result<Option<ServerMessage>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&line)?))
    }
}

fn is_not_listening(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Bind the socket, replacing a stale one left by a crashed daemon.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket file outlives the process that made it; if nothing is
    // listening on it, it's stale and safe to replace.
    if path.exists() && std::os::unix::net::UnixStream::connect(path).is_err() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    // The socket answers approvals, so it is as sensitive as the agent
    // session itself: owner only.
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    tracing::info!("hook socket listening on {}", path.display());
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Category;

    #[test]
    fn client_messages_are_tagged() {
        let msg = ClientMessage::Decision {
            pending_action_id: uuid::Uuid::nil(),
            allow: true,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "decision");
        assert_eq!(json["allow"], true);

        let msg = ClientMessage::Event {
            event: Box::new(Event::new(Category::TaskComplete, "claude_code", "done")),
            await_decision: false,
        };
        assert_eq!(serde_json::to_value(&msg).unwrap()["type"], "event");
    }

    #[test]
    fn server_messages_round_trip() {
        for msg in [
            ServerMessage::Decision { allow: false },
            ServerMessage::NoDecision,
            ServerMessage::Ack {
                ok: true,
                error: None,
            },
        ] {
            let text = serde_json::to_string(&msg).unwrap();
            let back: ServerMessage = serde_json::from_str(&text).unwrap();
            assert_eq!(
                std::mem::discriminant(&msg),
                std::mem::discriminant(&back),
                "{text}"
            );
        }
    }

    #[tokio::test]
    async fn connecting_without_a_daemon_yields_none() {
        let tmp = tempfile::tempdir().unwrap();
        let client = Client::connect(&tmp.path().join("absent.sock"))
            .await
            .unwrap();
        assert!(client.is_none());
    }

    #[tokio::test]
    async fn replaces_a_stale_socket_file_and_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hook.sock");
        std::fs::write(&path, b"").unwrap();
        let _listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
    }
}
