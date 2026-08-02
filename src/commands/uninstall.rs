//! Remove sailor-owned hook entries from agent config files.
//!
//! The inverse of `install`: it only ever removes hooks carrying the sailor
//! marker, so a config file shared with the user's own hooks (and other
//! tools') comes back exactly as it was before install ran.

use crate::commands::install::{read_settings, strip_sailor_hooks, write_settings};
use crate::config::{self, Agent};

pub async fn run(agent: Option<String>) -> anyhow::Result<()> {
    let targets = config::targets_for(agent.as_deref())?;
    for t in &targets {
        if t.agent != Agent::ClaudeCode.id() {
            continue;
        }
        if !t.path.exists() {
            println!("{}: nothing installed", t.agent);
            continue;
        }
        let mut settings = read_settings(&t.path)?;
        let removed = strip_sailor_hooks(&mut settings);
        if removed == 0 {
            println!("{}: nothing installed", t.agent);
            continue;
        }
        write_settings(&t.path, &settings)?;
        println!(
            "{}: removed {removed} hook{} from {}",
            t.agent,
            if removed == 1 { "" } else { "s" },
            t.path.display()
        );
    }
    Ok(())
}
