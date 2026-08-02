//! HTTP gateway bound to 127.0.0.1:<port> — serves the diff viewer, browser
//! preview discovery, and multiplexer context to the sailor app over the
//! existing SSH session. See `TECH.md` §2.2.
//!
//! Phase 3 wires the inbox: `GET /events` returns the current rows,
//! `WS /events` streams every update after that. Phase 4 implements the diff
//! and preview handlers.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};

use crate::inbox::{Inbox, Row};

pub fn router(inbox: Arc<Inbox>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/context", get(context))
        .route("/events", get(events))
        .route("/diff", get(diff))
        .route("/preview", get(preview))
        .with_state(inbox)
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

async fn diff() -> &'static str {
    // Phase 4: git diff web app (staged/unstaged/untracked) + /browse.
    "diff: not yet implemented"
}

async fn preview() -> &'static str {
    // Phase 4: list detected localhost HTTP servers.
    "preview: not yet implemented"
}

/// Bind the gateway to 127.0.0.1:<port>. Called by `commands::serve`.
pub async fn serve(port: u16, inbox: Arc<Inbox>) -> anyhow::Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("gateway listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(inbox)).await?;
    Ok(())
}
