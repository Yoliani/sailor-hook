//! Remove sailor-owned hook entries from agent config files.
//!
//! The inverse of `install`: it only ever removes hooks carrying the sailor
//! marker, so a config file shared with the user's own hooks (and other
//! tools') comes back exactly as it was before install ran.

use crate::commands::install::{
    kimi, opencode, pi, read_settings, strip_cursor_hooks, strip_sailor_hooks, write_settings,
};
use crate::config;

pub async fn run(agent: Option<String>) -> anyhow::Result<()> {
    let targets = config::targets_for(agent.as_deref())?;
    for t in &targets {
        match t.agent {
            "claude_code" | "codex" | "gemini" | "qwen" => {
                uninstall_nested(t).await?;
            }
            "cursor" => {
                uninstall_cursor(t).await?;
            }
            "kimi" => {
                uninstall_kimi(t).await?;
            }
            "opencode" => {
                uninstall_opencode(t).await?;
            }
            "pi" => {
                uninstall_pi(t).await?;
            }
            other => println!("{other}: nothing installed"),
        }
    }
    Ok(())
}

async fn uninstall_nested(t: &config::Target) -> anyhow::Result<()> {
    if !t.path.exists() {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    let mut settings = read_settings(&t.path)?;
    let removed = strip_sailor_hooks(&mut settings);
    if removed == 0 {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    write_settings(&t.path, &settings)?;
    println!(
        "{}: removed {removed} hook{} from {}",
        t.agent,
        if removed == 1 { "" } else { "s" },
        t.path.display()
    );
    Ok(())
}

async fn uninstall_cursor(t: &config::Target) -> anyhow::Result<()> {
    if !t.path.exists() {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    let mut settings = read_settings(&t.path)?;
    let removed = strip_cursor_hooks(&mut settings);
    if removed == 0 {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    write_settings(&t.path, &settings)?;
    println!(
        "{}: removed {removed} hook{} from {}",
        t.agent,
        if removed == 1 { "" } else { "s" },
        t.path.display()
    );
    Ok(())
}

async fn uninstall_kimi(t: &config::Target) -> anyhow::Result<()> {
    if !t.path.exists() {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    let mut value = kimi::read_toml(&t.path)?;
    let removed = kimi::strip_hooks(&mut value);
    if removed == 0 {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    kimi::write_toml(&t.path, &value)?;
    println!(
        "{}: removed {removed} hook{} from {}",
        t.agent,
        if removed == 1 { "" } else { "s" },
        t.path.display()
    );
    Ok(())
}

async fn uninstall_opencode(t: &config::Target) -> anyhow::Result<()> {
    if !t.path.exists() {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    match opencode::uninstall_plugin(&t.path)? {
        true => println!("{}: removed {}", t.agent, t.path.display()),
        false => println!("{}: nothing installed", t.agent),
    }
    Ok(())
}

async fn uninstall_pi(t: &config::Target) -> anyhow::Result<()> {
    if !t.path.exists() {
        println!("{}: nothing installed", t.agent);
        return Ok(());
    }
    match pi::uninstall_extension(&t.path)? {
        true => println!("{}: removed {}", t.agent, t.path.display()),
        false => println!("{}: nothing installed", t.agent),
    }
    Ok(())
}
