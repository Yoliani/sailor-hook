//! Run the daemon: hook socket + HTTP gateway + LAN advertiser, until Ctrl-C.
//!
//! The pieces share one `Inbox`: agent hooks write into it over the Unix
//! socket, the gateway reads and streams it to the app, and notable rows go
//! out as push notifications (`TECH.md` §2.4).

use std::sync::Arc;
use std::time::Duration;

use crate::context::Kind;
use crate::{discovery, gateway, herdr, inbox::Inbox, ipc, pending::Pending, push, secret, server};

/// How often archived rows are dropped. The window is 3h, so this only has to
/// be often enough to keep memory bounded.
const PRUNE_INTERVAL: Duration = Duration::from_secs(600);

/// How often Herdr's native agent-pane state is folded onto inbox rows.
/// Aggressive enough that the phone's badge tracks Herdr's sidebar, cheap
/// enough that a long-lived daemon doesn't care (one socket call per herdr
/// session with a live row).
const HERDR_POLL_INTERVAL: Duration = Duration::from_secs(5);

pub async fn run(
    port: u16,
    bind: std::net::IpAddr,
    ssh_port: u16,
    advertise: bool,
) -> anyhow::Result<()> {
    tracing::info!("sailor-hook serve starting (gateway {bind}:{port})");

    // Off-loopback means the gateway is on a network (a tailnet, typically),
    // so it needs the bearer token an Easy Pair QR hands to the phone.
    // Loopback keeps the pre-existing no-auth behaviour: getting there at all
    // already required an SSH login.
    let token = if bind.is_loopback() {
        None
    } else {
        Some(secret::ensure_gateway_token()?)
    };

    let inbox = Inbox::new();

    // Read once at startup: `sailor-hook push --set` is a deliberate act, and
    // re-reading the file per event would put disk I/O on the ingest path.
    let push_config = push::load();
    match &push_config {
        Some(c) => tracing::info!("push: {} → {}", c.kind.as_str(), c.url),
        None => tracing::info!("push: not configured (sailor-hook push --set <url>)"),
    }

    let listener = ipc::bind(&ipc::socket_path()?)?;
    let pending = Pending::new();
    let ctx = Arc::new(server::Context {
        inbox: Arc::clone(&inbox),
        pending: Arc::clone(&pending),
        on_row: Box::new(move |row| {
            tracing::info!(session = %row.session_id, "inbox: {}", row.title);

            if let Some(config) = push_config.clone() {
                if push::is_notable(row.category) {
                    // Detached: a slow or unreachable push endpoint must not
                    // hold up the next agent event.
                    tokio::spawn(async move {
                        let (title, body) = push::describe(&row);
                        if let Err(e) = push::deliver(&config, &title, &body).await {
                            tracing::warn!("push delivery failed: {e}");
                        }
                    });
                }
            }
        }),
    });
    let ingest = server::accept_loop(listener, ctx);

    let prune = {
        let inbox = Arc::clone(&inbox);
        async move {
            loop {
                tokio::time::sleep(PRUNE_INTERVAL).await;
                let dropped = inbox.prune();
                if dropped > 0 {
                    tracing::debug!("archived {dropped} idle inbox row(s)");
                }
            }
        }
    };

    let herdr_state = {
        let inbox = Arc::clone(&inbox);
        async move {
            let mut tick = tokio::time::interval(HERDR_POLL_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                refresh_herdr_states(&inbox);
            }
        }
    };

    if !advertise {
        return tokio::select! {
            res = gateway::serve(bind, port, Arc::clone(&inbox), Arc::clone(&pending), token.clone()) => res,
            res = ingest => res,
            _ = prune => Ok(()),
            _ = herdr_state => Ok(()),
        };
    }

    // `discovery::advertise` blocks until Ctrl-C and deregisters the Bonjour
    // service on the way out, so it's what drives a clean shutdown here.
    tokio::select! {
        res = gateway::serve(bind, port, Arc::clone(&inbox), Arc::clone(&pending), token.clone()) => res,
        res = discovery::advertise(ssh_port) => res,
        res = ingest => res,
        _ = prune => Ok(()),
        _ = herdr_state => Ok(()),
    }
}

/// Fold Herdr's native pane states onto inbox rows: for every distinct herdr
/// session with a live row, query `herdr agent list` and overlay the result.
/// A missing binary, a dead server, or a session with no agents all leave
/// the rows untouched — the poll is a nicety, never an error path.
fn refresh_herdr_states(inbox: &Inbox) {
    let mut sessions: Vec<String> = Vec::new();
    for row in inbox.rows() {
        let Some(terminal) = &row.terminal else {
            continue;
        };
        if terminal.kind != Kind::Herdr {
            continue;
        }
        let Some(session) = terminal.session.as_deref() else {
            continue;
        };
        if !sessions.iter().any(|s| s == session) {
            sessions.push(session.to_string());
        }
    }
    for session in sessions {
        let states = herdr::list_states(&session);
        tracing::debug!(session = %session, agents = states.len(), "herdr agent list");
        if !states.is_empty() {
            inbox.apply_agent_states(&session, &states);
        }
    }
}
