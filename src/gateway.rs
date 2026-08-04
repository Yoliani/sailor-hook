//! HTTP gateway bound to 127.0.0.1:<port> — serves the diff viewer, browser
//! preview discovery, and multiplexer context to the sailor app over the
//! existing SSH session. See `TECH.md` §2.2.
//!
//! Phase 3 wires the inbox: `GET /events` returns the current rows,
//! `WS /events` streams every update after that. Phase 4 implements the diff
//! and preview handlers.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRef, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};

use crate::inbox::{Inbox, Row};
use crate::pending::Pending;

/// What the gateway handlers share. `token` is `None` for a loopback-only
/// bind, which is reachable solely through the user's own SSH session and so
/// is already as authenticated as the SSH login was.
#[derive(Clone)]
pub struct GatewayState {
    pub inbox: Arc<Inbox>,
    pub pending: Arc<Pending>,
    pub token: Option<Arc<str>>,
}

impl FromRef<GatewayState> for Arc<Inbox> {
    fn from_ref(state: &GatewayState) -> Self {
        Arc::clone(&state.inbox)
    }
}

pub fn router(state: GatewayState) -> Router {
    // /health stays open: it carries nothing, and `easy-pair` uses it to
    // decide whether there is a daemon worth advertising in the QR at all.
    Router::new()
        .route("/health", get(health))
        .merge(
            Router::new()
                .route("/context", get(context))
                .route("/events", get(events))
                .route("/approve", axum::routing::post(approve))
                .route("/mux-list", get(mux_list))
                .route("/diff", get(diff))
                .route("/preview", get(preview))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    require_token,
                )),
        )
        .with_state(state)
}

/// Bearer-token check for everything except /health.
///
/// The token reaches the phone through the Easy Pair QR, so this is the same
/// trust model as the mosh session key: holding it is the authorization. A
/// constant-time comparison keeps a wrong guess from leaking its correct
/// prefix through response timing.
async fn require_token(
    State(state): State<GatewayState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.token.as_deref() else {
        return next.run(request).await;
    };
    let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(request).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn health() -> &'static str {
    "ok"
}

/// The multiplexer/session/pane/cwd this daemon is running under — the same
/// probe as `sailor-hook context`, over HTTP for the app.
async fn context() -> Json<serde_json::Value> {
    Json(crate::context::detect().to_json())
}

/// Plain GET returns the current rows; the same path upgraded to a
/// WebSocket streams every subsequent row update. The app calls the first to
/// fill the list, then holds the second open.
async fn events(ws: Option<WebSocketUpgrade>, State(inbox): State<Arc<Inbox>>) -> Response {
    match ws {
        Some(ws) => ws.on_upgrade(move |socket| stream_events(socket, inbox)),
        None => Json(inbox.rows()).into_response(),
    }
}

/// Answer a parked approval. The Easy Pair inbox's only write: a mosh-only
/// session has no SSH exec to run `sailor-hook approve` on, so the phone
/// POSTs the decision here instead. Same semantics as the CLI — resolves the
/// parked shim *and* marks the row — so the two paths can't drift.
#[derive(serde::Deserialize)]
struct ApproveBody {
    /// The app's row model is camelCase everywhere (the wire serializes
    /// `Row` that way), so the HTTP body arrives as `pendingActionId` even
    /// though the CLI uses snake_case internally.
    #[serde(alias = "pendingActionId")]
    pending_action_id: uuid::Uuid,
    allow: bool,
}

async fn approve(State(state): State<GatewayState>, Json(body): Json<ApproveBody>) -> Response {
    let resolved = state.pending.resolve(body.pending_action_id, body.allow);
    if resolved {
        state.inbox.resolve(body.pending_action_id, body.allow);
        (StatusCode::OK, "ok").into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            "no approval is waiting on that id (already answered, or it timed out)",
        )
            .into_response()
    }
}

async fn stream_events(mut socket: WebSocket, inbox: Arc<Inbox>) {
    // Subscribe before snapshotting so an event landing in between is
    // delivered late rather than lost.
    let mut rx = inbox.subscribe();
    for row in inbox.rows() {
        if send_row(&mut socket, &row).await.is_err() {
            return;
        }
    }
    // A lagged receiver means the client's picture is stale; drop it and let
    // it reconnect into a fresh snapshot.
    while let Ok(row) = rx.recv().await {
        if send_row(&mut socket, &row).await.is_err() {
            return;
        }
    }
}

async fn send_row(socket: &mut WebSocket, row: &Row) -> anyhow::Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(row)?))
        .await?;
    Ok(())
}

/// Installed multiplexers and their sessions — the same collection
/// `sailor-hook mux-list` prints. This is the reason the gateway can be
/// reached off-loopback at all: an Easy Pair session is plain mosh, and mosh
/// only transmits the visible screen, so a listing large enough to scroll
/// cannot be read back out of the terminal. Over HTTP there is no such cap.
async fn mux_list() -> Json<serde_json::Value> {
    Json(crate::commands::mux_list::collect())
}

async fn diff() -> &'static str {
    // Phase 4: git diff web app (staged/unstaged/untracked) + /browse.
    "diff: not yet implemented"
}

async fn preview() -> &'static str {
    // Phase 4: list detected localhost HTTP servers.
    "preview: not yet implemented"
}

/// Bind the gateway to `bind`:<port>. Called by `commands::serve`.
///
/// Loopback (the default) needs no token — reaching it means already being on
/// the machine. Any other address is on a network, so a token is required and
/// `serve` refuses to start without one.
pub async fn serve(
    bind: IpAddr,
    port: u16,
    inbox: Arc<Inbox>,
    pending: Arc<Pending>,
    token: Option<String>,
) -> anyhow::Result<()> {
    if !bind.is_loopback() && token.is_none() {
        anyhow::bail!("refusing to bind {bind} without a gateway token");
    }
    let addr = std::net::SocketAddr::new(bind, port);
    tracing::info!(
        "gateway listening on http://{addr} ({})",
        if token.is_some() {
            "bearer token required"
        } else {
            "loopback, no auth"
        }
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let state = GatewayState {
        inbox,
        pending,
        token: token.map(Arc::from),
    };
    axum::serve(listener, router(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_identical_values() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        // A correct prefix must not compare equal — that is the whole point.
        assert!(!constant_time_eq(b"abc", b"abc123"));
        assert!(!constant_time_eq(b"", b"abc"));
    }
}
