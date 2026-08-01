//! sailor-hook — host daemon that bridges AI coding agents to the sailor mobile app.
//!
//! See `TECH.md` §2.2 for the full design. Phase 0 skeleton: subcommands are
//! wired with clap; `serve`, `install`, and the gateway are stubs that compile
//! and print what they would do. `context` and `status` are implemented enough
//! to be useful for debugging.

// Phase 0 scaffold: stub modules (events, gateway) are intentionally unused
// until Phase 3/4. Remove this allow as each module is wired up.
#![allow(dead_code)]

mod cli;
mod commands;
mod config;
mod context;
mod discovery;
mod events;
mod gateway;
mod secret;

use clap::Parser;
use cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Pair { token, store } => commands::pair::run(token, store).await,
        Command::Install { agent } => commands::install::run(agent).await,
        Command::Uninstall { agent } => commands::uninstall::run(agent).await,
        Command::Serve { port } => commands::serve::run(port).await,
        Command::Advertise { ssh_port } => discovery::advertise(ssh_port).await,
        Command::Status { json } => commands::status::run(json),
        Command::Context => commands::context::run(),
        Command::CwdList => commands::cwd_list::run(),
        Command::Diff { dir } => commands::diff::run(dir).await,
        Command::Logs { follow } => commands::logs::run(follow),
        Command::Usage { sync } => commands::usage::run(sync),
        Command::Version => commands::version::run(),
    }
}
