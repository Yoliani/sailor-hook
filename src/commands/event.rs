//! `sailor-hook event --agent <id>` — the shim every agent hook invokes.
//!
//! Reads the agent's hook payload from stdin, normalizes it (`adapters/`),
//! stamps the host id and terminal context, and writes it to the daemon's
//! Unix socket.
//!
//! For a *decidable* approval it then waits, up to `--wait-secs`, for the
//! phone to answer, and prints the agent's decision JSON on stdout.
//!
//! This runs inside somebody's coding agent, so it fails soft everywhere: an
//! unknown hook event, an absent daemon, unparseable JSON, or an unanswered
//! approval all exit 0 printing nothing. Printing nothing is what makes that
//! safe — with no decision on stdout the agent falls back to asking in the
//! terminal, exactly as it would without sailor. **No failure path may ever
//! produce "allow".**

use std::io::Read;
use std::time::Duration;

use crate::ipc::{Client, ClientMessage, ServerMessage};
use crate::{adapters, context, hostid, ipc};

pub async fn run(agent: String, wait_secs: u64) -> anyhow::Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;

    let payload: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sailor-hook event: ignoring unparseable payload ({e})");
            return Ok(());
        }
    };

    let mut event = match adapters::normalize(&agent, &payload) {
        Ok(e) => e,
        Err(e) => {
            // Agents emit more hook events than the inbox models; skipping
            // the rest is expected, not an error worth surfacing loudly.
            eprintln!("sailor-hook event: {e}");
            return Ok(());
        }
    };
    event.host_id = hostid::load_or_create().unwrap_or_default();
    // This process is a child of the agent, which is a child of the pane, so
    // the pane's env vars are right here — the only place in the chain where
    // they can be read at all. A bare shell tags nothing.
    let terminal = context::detect();
    if terminal.kind != context::Kind::None {
        event.terminal = Some(terminal);
    }

    let await_decision = adapters::is_decidable(&agent, &payload);
    let wait = Duration::from_secs(wait_secs);
    if await_decision {
        // The app shows a countdown against this, so it has to be the same
        // deadline this process actually enforces.
        event.expires_at = Some(chrono::Utc::now() + chrono::Duration::seconds(wait_secs as i64));
    }

    let path = ipc::socket_path()?;
    let mut client = match Client::connect(&path).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("sailor-hook event: daemon not running, event dropped");
            return Ok(());
        }
        Err(e) => {
            eprintln!("sailor-hook event: could not connect ({e})");
            return Ok(());
        }
    };

    if let Err(e) = client
        .send(&ClientMessage::Event {
            event: Box::new(event),
            await_decision,
        })
        .await
    {
        eprintln!("sailor-hook event: could not deliver ({e})");
        return Ok(());
    }

    if !await_decision {
        return Ok(());
    }

    // Wait for the phone. Anything other than an explicit answer — timeout,
    // daemon restart, socket error — leaves stdout empty on purpose.
    let allow = match tokio::time::timeout(wait, client.recv()).await {
        Ok(Ok(Some(ServerMessage::Decision { allow }))) => allow,
        Ok(Ok(Some(ServerMessage::NoDecision))) => {
            eprintln!("sailor-hook event: no decision, falling back to the terminal prompt");
            return Ok(());
        }
        Ok(Ok(_)) | Ok(Err(_)) => {
            eprintln!("sailor-hook event: daemon closed the connection, falling back");
            return Ok(());
        }
        Err(_) => {
            eprintln!("sailor-hook event: approval timed out after {wait_secs}s, falling back");
            return Ok(());
        }
    };

    if let Some(decision) = adapters::render_decision(&agent, &payload, allow) {
        println!("{decision}");
    }
    Ok(())
}
