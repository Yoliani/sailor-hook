//! sailor-hook — host daemon that bridges AI coding agents to the sailor mobile app.
//!
//! See `TECH.md` §2.2 for the full design. Phase 3 wires the agent event path
//! end to end: `install` writes hooks into the agent's config, those hooks run
//! `event`, which normalizes the payload and posts it to `serve`'s Unix
//! socket, and the gateway streams the resulting inbox rows to the app. The
//! `diff`/`preview` gateway routes remain Phase 4 stubs.

#![allow(dead_code)]

mod adapters;
mod cli;
mod commands;
mod config;
mod context;
mod discovery;
mod events;
mod gateway;
mod hostid;
mod inbox;
mod ipc;
mod pending;
mod push;
mod secret;
mod server;

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
        Command::EasyPair { host, colors } => commands::easy_pair::run(host, colors).await,
        Command::Event { agent, wait_secs } => commands::event::run(agent, wait_secs).await,
        Command::Approve {
            pending_action_id,
            allow,
            deny,
        } => {
            if !allow && !deny {
                anyhow::bail!("pass --allow or --deny");
            }
            commands::approve::run(pending_action_id, allow).await
        }
        Command::Inbox { watch } => commands::inbox::run(watch).await,
        Command::Install { agent } => commands::install::run(agent).await,
        Command::Uninstall { agent } => commands::uninstall::run(agent).await,
        Command::Serve {
            port,
            ssh_port,
            no_advertise,
        } => commands::serve::run(port, ssh_port, !no_advertise).await,
        Command::Advertise { ssh_port } => discovery::advertise(ssh_port).await,
        Command::Push {
            set,
            kind,
            token,
            test,
        } => commands::push::run(set, kind, token, test).await,
        Command::Status { json } => commands::status::run(json),
        Command::Context => commands::context::run(),
        Command::CwdList => commands::cwd_list::run(),
        Command::Diff { dir } => commands::diff::run(dir).await,
        Command::Logs { follow } => commands::logs::run(follow),
        Command::Usage { sync } => commands::usage::run(sync),
        Command::Version => commands::version::run(),
    }
}
