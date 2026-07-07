//! HTTP gateway bound to 127.0.0.1:<port> — serves the diff viewer, browser
//! preview discovery, and multiplexer context to the sailor app over the
//! existing SSH session. See `TECH.md` §2.2.
//!
//! Phase 0: route skeleton only. Phase 3/4 implement the handlers.

use axum::{routing::get, Router};

pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/context", get(context))
        .route("/diff", get(diff))
        .route("/preview", get(preview))
}

async fn health() -> &'static str {
    "ok"
}

async fn context() -> &'static str {
    // Phase 3: return detected multiplexer kind/session/pane/workspace/cwd.
    "{}"
}

async fn diff() -> &'static str {
    // Phase 4: git diff web app (staged/unstaged/untracked) + /browse.
    "diff: not yet implemented"
}

async fn preview() -> &'static str {
    // Phase 4: list detected localhost HTTP servers.
    "preview: not yet implemented"
}

/// Bind the gateway to 127.0.0.1:<port>. Called by `serve` in Phase 3.
pub async fn serve(port: u16) -> anyhow::Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("gateway listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router()).await?;
    Ok(())
}
