//! `sailor-hook usage [--sync]` — print the usage/context snapshots the
//! inbox currently holds.
//!
//! The usage data comes from the agents themselves: today Qwen's `Stop`
//! hook is the one payload in the supported set that reports
//! `context_usage`, so that is where `context_remaining` arrives from.
//! Claude Code's hooks carry no token or context fields (anthropics/
//! claude-code#66564, #11008), and its transcripts only hold per-request
//! token counts — not a context fraction — so there is nothing to parse
//! out of them. `--sync` is accepted for CLI compatibility; with no
//! persistent store, re-reading the daemon's live rows *is* the sync.

use crate::ipc::{socket_path, Client, ClientMessage, ServerMessage};

pub async fn run(sync: bool) -> anyhow::Result<()> {
    let _ = sync; // see module docs: live rows are the store
    let mut client = Client::connect(&socket_path()?)
        .await?
        .ok_or_else(|| anyhow::anyhow!("sailor-hook daemon is not running"))?;

    client
        .send(&ClientMessage::Subscribe { once: true })
        .await?;

    let mut any = false;
    while let Some(message) = client.recv().await? {
        if let ServerMessage::Row { row } = message {
            if row.usage.is_empty() && row.context_remaining.is_none() {
                continue;
            }
            any = true;
            print_row(&row);
        }
    }
    if !any {
        println!(
            "no usage data (agents report it on their hook events; Qwen's Stop hook does today)"
        );
    }
    Ok(())
}

fn print_row(row: &crate::inbox::Row) {
    let project = row.project.as_deref().unwrap_or("-");
    println!("{} · {}", row.source, project);
    if let Some(remaining) = row.context_remaining {
        println!(
            "  context: {}% remaining",
            (remaining * 100.0).round() as i64
        );
    }
    for window in &row.usage {
        println!(
            "  window {}: {}% used{}",
            window.label,
            (window.used * 100.0).round() as i64,
            window
                .resets_at
                .map(|t| format!(", resets {}", t.format("%H:%M")))
                .unwrap_or_default()
        );
    }
}
