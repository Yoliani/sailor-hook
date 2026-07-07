//! Standalone diff viewer server for a repo. Phase 0: stub. Phase 4 wires
//! the real `/diff` + `/browse` gateway (see `gateway.rs`).

use std::path::PathBuf;

pub async fn run(dir: Option<PathBuf>) -> anyhow::Result<()> {
    let dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    tracing::info!(
        "would serve diff viewer for {} on 127.0.0.1:24543",
        dir.display()
    );
    anyhow::bail!("diff: not yet implemented (Phase 4)");
}
