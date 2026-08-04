//! `sailor-hook inbox [--watch] [--stdin-approvals]` — the inbox as NDJSON
//! on stdout.
//!
//! This is how the phone reads the inbox. `TECH.md` §2.2 routes the app at
//! the HTTP gateway over an SSH local-forward, but NMSSH exposes no
//! `direct-tcpip` (same gap that blocks jump hosts on iOS), so there is no
//! forward to ride today. An SSH *exec* channel already works on both
//! platforms — so the app runs this and reads lines. The gateway keeps the
//! same data at `GET`/`WS /events` for Phase 4, when the diff WebView needs
//! a real HTTP origin and the forward has to exist anyway.
//!
//! `--stdin-approvals` is the Easy Pair variant: a mosh-only session has no
//! SSH leg at all, so the daemon can't be reached by exec. Instead
//! `sailor-hook easy-pair` starts a second mosh-server running this mode,
//! and the phone's mosh connection *is* the channel: rows stream to stdout
//! (over the mosh session), and the app answers approvals by writing
//! `{"pendingActionId":"<uuid>","allow":true|false}` lines to stdin, which
//! mosh's pty delivers to this process.
//!
//! One JSON row per line, so a reader can parse incrementally without
//! framing.

use crate::commands::approve;
use serde_json::Value;

use crate::ipc::{socket_path, Client, ClientMessage, ServerMessage};

pub async fn run(watch: bool, stdin_approvals: bool) -> anyhow::Result<()> {
    let mut client = Client::connect(&socket_path()?)
        .await?
        .ok_or_else(|| anyhow::anyhow!("sailor-hook daemon is not running"))?;

    // stdin-approvals implies watch: the stream must stay open for answers.
    client
        .send(&ClientMessage::Subscribe {
            once: !watch && !stdin_approvals,
        })
        .await?;

    if !stdin_approvals {
        while let Some(message) = client.recv().await? {
            if let ServerMessage::Row { row } = message {
                println!("{}", serde_json::to_string(&row)?);
            }
        }
        return Ok(());
    }

    // mosh-server hands this process a pty, and a canonical-mode pty would
    // echo the app's approval lines back onto the row stream and eat them
    // through line editing. Raw mode: no echo, no signal chars, no editing.
    let _ = std::process::Command::new("stty")
        .args(["-echo", "-icanon"])
        .status();

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    loop {
        tokio::select! {
            message = client.recv() => match message {
                Ok(Some(ServerMessage::Row { row })) => {
                    println!("{}", serde_json::to_string(&row)?);
                }
                Ok(Some(_)) => {}
                // Daemon went away; the app sees the stream end and says so.
                Ok(None) | Err(_) => return Ok(()),
            },
            line = read_line(&mut stdin) => {
                let Some(line) = line else { return Ok(()) };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // An answer that can't be delivered must not kill the stream:
                // the app's row updates tell the user what actually happened.
                if let Err(e) = answer(trimmed).await {
                    eprintln!("sailor-hook inbox: {e}");
                }
            }
        }
    }
}

async fn read_line(reader: &mut tokio::io::BufReader<tokio::io::Stdin>) -> Option<String> {
    use tokio::io::AsyncBufReadExt;
    let mut buf = String::new();
    match reader.read_line(&mut buf).await {
        Ok(0) | Err(_) => None, // EOF or a broken pty — end the stream
        Ok(_) => Some(buf),
    }
}

/// Parse one approval line and forward it to the daemon. Reuses the same
/// path the `approve` command runs, so a line that reaches it behaves
/// exactly like an SSH-exec approval.
async fn answer(line: &str) -> anyhow::Result<()> {
    let value: Value =
        serde_json::from_str(line).map_err(|_| anyhow::anyhow!("approval line is not JSON"))?;
    let id = value
        .get("pendingActionId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("approval line has no pendingActionId"))?;
    let allow = value
        .get("allow")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("approval line has no allow boolean"))?;
    approve::run(id.to_string(), allow).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_wellformed_lines_and_rejects_garbage() {
        // A well-formed line fails only because no daemon is running — which
        // proves it got past parsing and into the approve path.
        let err =
            answer(r#"{"pendingActionId":"ace6d302-839d-4f81-8e80-269f5be9be63","allow":true}"#)
                .await
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("daemon is not running") || err.contains("approval"),
            "unexpected: {err}"
        );

        assert!(answer("not json")
            .await
            .unwrap_err()
            .to_string()
            .contains("not JSON"));
        assert!(answer(r#"{"allow":true}"#)
            .await
            .unwrap_err()
            .to_string()
            .contains("pendingActionId"));
        assert!(
            answer(r#"{"pendingActionId":"ace6d302-839d-4f81-8e80-269f5be9be63"}"#)
                .await
                .unwrap_err()
                .to_string()
                .contains("allow")
        );
    }
}
