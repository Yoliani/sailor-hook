//! Run the daemon: hook socket + HTTP gateway + LAN advertiser, until Ctrl-C.
//!
//! The pieces share one `Inbox`: agent hooks write into it over the Unix
//! socket, the gateway reads and streams it to the app, and notable rows go
//! out as push notifications (`TECH.md` §2.4).

use std::sync::Arc;
use std::time::Duration;

use crate::{discovery, gateway, inbox::Inbox, ipc, pending::Pending, push, server};

/// How often archived rows are dropped. The window is 3h, so this only has to
/// be often enough to keep memory bounded.
const PRUNE_INTERVAL: Duration = Duration::from_secs(600);

pub async fn run(port: u16, ssh_port: u16, advertise: bool) -> anyhow::Result<()> {
    tracing::info!("sailor-hook serve starting (gateway 127.0.0.1:{port})");

    let inbox = Inbox::new();

    // Read once at startup: `sailor-hook push --set` is a deliberate act, and
    // re-reading the file per event would put disk I/O on the ingest path.
    let push_config = push::load();
    match &push_config {
        Some(c) => tracing::info!("push: {} → {}", c.kind.as_str(), c.url),
        None => tracing::info!("push: not configured (sailor-hook push --set <url>)"),
    }

    let listener = ipc::bind(&ipc::socket_path()?)?;
    let ctx = Arc::new(server::Context {
        inbox: Arc::clone(&inbox),
        pending: Pending::new(),
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

    if !advertise {
        return tokio::select! {
            res = gateway::serve(port, Arc::clone(&inbox)) => res,
            res = ingest => res,
            _ = prune => Ok(()),
        };
    }

    // `discovery::advertise` blocks until Ctrl-C and deregisters the Bonjour
    // service on the way out, so it's what drives a clean shutdown here.
    tokio::select! {
        res = gateway::serve(port, Arc::clone(&inbox)) => res,
        res = discovery::advertise(ssh_port) => res,
        res = ingest => res,
        _ = prune => Ok(()),
    }
}
