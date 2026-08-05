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
mod diff;
mod discovery;
mod events;
mod gateway;
mod herdr;
mod hostid;
mod inbox;
mod ipc;
mod paths;
mod pending;
mod preview;
mod push;
mod secret;
mod server;
mod servers;

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
        Command::Unpair => commands::unpair::run(),
        Command::EasyPair {
            host,
            colors,
            gateway_port,
            no_serve,
        } => commands::easy_pair::run(host, colors, gateway_port, !no_serve).await,
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
            bind,
            ssh_port,
            no_advertise,
        } => commands::serve::run(port, bind, ssh_port, !no_advertise).await,
        Command::Service { verb } => commands::service::run(verb),
        Command::Servers { cmd } => match cmd {
            None => commands::servers::run_list().await,
            Some(cli::ServersCommand::Kill { pid, port, force }) => {
                commands::servers::run_kill(pid, port, force).await
            }
        },
        Command::Advertise { ssh_port } => discovery::advertise(ssh_port).await,
        Command::Push {
            set,
            kind,
            token,
            test,
        } => commands::push::run(set, kind, token, test).await,
        Command::Status { json } => commands::status::run(json),
        Command::Context => commands::context::run(),
        Command::MuxList => commands::mux_list::run(),
        Command::CwdList => commands::cwd_list::run(),
        Command::Diff { dir } => commands::diff::run(dir).await,
        Command::Logs { follow } => commands::logs::run(follow),
        Command::Usage { sync } => commands::usage::run(sync).await,
        Command::Version => commands::version::run(),
    }
}
