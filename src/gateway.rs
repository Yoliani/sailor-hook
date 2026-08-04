//! HTTP gateway bound to 127.0.0.1:<port> — serves the diff viewer, browser
//! preview discovery, and multiplexer context to the sailor app over the
//! existing SSH session. See `TECH.md` §2.2.
//!
//! Phase 3 wires the inbox: `GET /events` returns the current rows,
//! `WS /events` streams every update after that. Phase 4 implements the diff
//! and preview handlers.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::Query;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRef, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};

use crate::inbox::{Inbox, Row};
use crate::pending::Pending;
use crate::{diff, preview};

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
                .route("/diff", get(diff_handler))
                .route("/browse", get(browse_handler))
                .route("/browse/list", get(browse_list_handler))
                .route("/preview", get(preview_handler))
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

// --- Phase 4: diff viewer & browser preview ---

/// /diff query params
#[derive(serde::Deserialize)]
struct DiffQuery {
    staged: Option<bool>,
    unstaged: Option<bool>,
    untracked: Option<bool>,
    commit: Option<String>,
    dir: Option<String>,
}

/// The repository a request operates on: an explicit path, or the directory
/// the daemon was started in.
fn repo_or_cwd(explicit: Option<String>) -> String {
    explicit.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .to_string_lossy()
            .into_owned()
    })
}

async fn diff_handler(State(_state): State<Arc<Inbox>>, Query(q): Query<DiffQuery>) -> Response {
    let dir = repo_or_cwd(q.dir);
    // Naming a scope selects it exclusively; naming none asks for all three.
    // Without this, `?staged=true` would leave the other two defaulted to
    // true and every tab would return an identical, unfiltered list.
    let any_scope = q.staged.is_some() || q.unstaged.is_some() || q.untracked.is_some();
    let staged = q.staged.unwrap_or(!any_scope);
    let unstaged = q.unstaged.unwrap_or(!any_scope);
    let untracked = q.untracked.unwrap_or(!any_scope);
    match diff::collect_changes(
        std::path::Path::new(&dir),
        staged,
        unstaged,
        untracked,
        q.commit.as_deref(),
    ) {
        Ok(Some(changes)) => Json(serde_json::json!({
            "repo": dir,
            "changes": changes,
            "total": changes.len(),
        }))
        .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not a git repository").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// /browse query params
#[derive(serde::Deserialize)]
struct BrowseQuery {
    repo: Option<String>,
    file: String,
    commit: Option<String>,
}

async fn browse_handler(
    State(_state): State<Arc<Inbox>>,
    Query(q): Query<BrowseQuery>,
) -> Response {
    let repo = repo_or_cwd(q.repo);
    match diff::read_file(std::path::Path::new(&repo), q.commit.as_deref(), &q.file) {
        Ok(Some(contents)) => Json(serde_json::json!({
            "repo": repo,
            "file": q.file,
            "contents": contents,
        }))
        .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// /browse/list query params
#[derive(serde::Deserialize)]
struct BrowseListQuery {
    repo: Option<String>,
    commit: Option<String>,
}

async fn browse_list_handler(
    State(_state): State<Arc<Inbox>>,
    Query(q): Query<BrowseListQuery>,
) -> Response {
    let repo = repo_or_cwd(q.repo);
    match diff::list_files(std::path::Path::new(&repo), q.commit.as_deref()) {
        Ok(Some(files)) => Json(serde_json::json!({
            "repo": repo,
            "files": files,
        }))
        .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not a git repository").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn preview_handler(
    State(_state): State<Arc<Inbox>>,
) -> Response {
    match preview::discover() {
        Ok(servers) => Json(serde_json::json!({
            "servers": servers,
            "total": servers.len(),
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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