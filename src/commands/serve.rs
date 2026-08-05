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
    port: Option<u16>,
    bind: Option<std::net::IpAddr>,
    ssh_port: u16,
    advertise: bool,
) -> anyhow::Result<()> {
    let (bind, port) = resolve_listen(bind, port)?;
    let _instance = acquire_instance_lock()?;
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

/// Resolve the gateway listen address with moshi's precedence, minus the
/// config file tier: explicit `--bind`/`--port` flags win, then
/// `SAILOR_HOOK_GATEWAY_LISTEN=host:port`, then loopback:24543.
fn resolve_listen(
    bind: Option<std::net::IpAddr>,
    port: Option<u16>,
) -> anyhow::Result<(std::net::IpAddr, u16)> {
    if let Some(b) = bind {
        return Ok((b, port.unwrap_or(24543)));
    }
    if let Ok(listen) = std::env::var("SAILOR_HOOK_GATEWAY_LISTEN") {
        if !listen.is_empty() {
            return parse_listen(&listen);
        }
    }
    Ok((
        std::net::IpAddr::from([127, 0, 0, 1]),
        port.unwrap_or(24543),
    ))
}

/// Parse `host:port` where host is an IP literal or a resolvable name.
fn parse_listen(listen: &str) -> anyhow::Result<(std::net::IpAddr, u16)> {
    use std::net::ToSocketAddrs;
    let (host, port_s) = listen.rsplit_once(':').ok_or_else(|| {
        anyhow::anyhow!("SAILOR_HOOK_GATEWAY_LISTEN must be host:port, got {listen:?}")
    })?;
    let port: u16 = port_s
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid port in SAILOR_HOOK_GATEWAY_LISTEN: {port_s:?}"))?;
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok((ip, port));
    }
    let ip = (host, 0u16)
        .to_socket_addrs()?
        .next()
        .map(|a| a.ip())
        .ok_or_else(|| anyhow::anyhow!("could not resolve {host:?}"))?;
    Ok((ip, port))
}

/// A held exclusive lock on `<state>/serve.lock`. The daemon is one per
/// host; a second `serve` exits with a clear message instead of colliding
/// on the socket. The lock is released when the daemon exits (advisory
/// flock, dropped with the file).
struct InstanceLock {
    _file: std::fs::File,
}

fn acquire_instance_lock() -> anyhow::Result<InstanceLock> {
    use fs2::FileExt;
    let path = crate::paths::state_dir()?.join("serve.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(_) => anyhow::bail!(
            "another sailor-hook serve is already running (lock {}). \
             Check `sailor-hook status` or stop it first.",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ip_and_named_listen_targets() {
        assert_eq!(
            parse_listen("0.0.0.0:3000").unwrap(),
            (std::net::IpAddr::from([0, 0, 0, 0]), 3000)
        );
        assert_eq!(
            parse_listen("127.0.0.1:24543").unwrap(),
            (std::net::IpAddr::from([127, 0, 0, 1]), 24543)
        );
        assert!(parse_listen("nonsense").is_err());
        assert!(parse_listen("host:notaport").is_err());
        // An unresolvable name errors rather than guessing.
        assert!(parse_listen("definitely-not-a-host.invalid:80").is_err());
    }

    #[test]
    fn instance_lock_is_exclusive_within_one_process() {
        use fs2::FileExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serve.lock");
        let file = std::fs::File::create(&path).unwrap();
        file.lock_exclusive().unwrap();
        // A second, independently-opened descriptor cannot take the lock
        // while the first holds it — the mechanism the single-instance
        // guard relies on. (Deliberately no re-lock-after-close assertion:
        // close/re-open flock release timing is platform-dependent and
        // flaky under parallel test load; release is exercised e2e.)
        let second = std::fs::File::create(&path).unwrap();
        assert!(second.try_lock_exclusive().is_err());
        drop(second);
        // Unlock + re-lock the same descriptor is deterministic everywhere.
        file.unlock().unwrap();
        assert!(file.try_lock_exclusive().is_ok());
    }
}
