//! Remove sailor-owned hook entries from agent config files. Phase 0: stub.

use crate::config;

pub async fn run(agent: Option<String>) -> anyhow::Result<()> {
    let targets = config::targets_for(agent.as_deref())?;
    for t in &targets {
        println!(
            "would remove sailor hook from {} ({})",
            t.agent,
            t.path.display()
        );
    }
    anyhow::bail!("uninstall: not yet implemented (Phase 3)");
}
