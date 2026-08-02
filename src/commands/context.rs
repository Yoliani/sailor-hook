//! One-shot terminal-context probe: detect tmux / Zellij / Herdr from the
//! current shell environment and print `{kind, session, pane, workspace, cwd}`
//! as JSON. This is fully implemented — it's a pure env-var read, useful for
//! debugging the multiplexer detection that drives the session picker.

use crate::context;

pub fn run() -> anyhow::Result<()> {
    let ctx = context::detect();
    println!("{}", serde_json::to_string_pretty(&ctx.to_json())?);
    Ok(())
}
