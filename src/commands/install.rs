//! Write sailor-owned hook entries into supported agent config files.
//!
//! Phase 0: stub. Phase 3 will write the actual entries per agent (see
//! `config.rs` for the target locations and `events.rs` for the schema).

use crate::config;

pub async fn run(agent: Option<String>) -> anyhow::Result<()> {
    let targets = config::targets_for(agent.as_deref())?;
    if targets.is_empty() {
        println!("no supported agents selected");
        return Ok(());
    }
    for t in &targets {
        println!(
            "would install sailor hook into {} ({})",
            t.agent,
            t.path.display()
        );
    }
    anyhow::bail!("install: not yet implemented (Phase 3)");
}
