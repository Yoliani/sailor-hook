use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Host daemon that bridges AI coding agents to the sailor mobile app.
#[derive(Debug, Parser)]
#[command(name = "sailor-hook", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Pair with the sailor app using a token from Settings.
    Pair {
        /// Pairing token from the sailor app.
        #[arg(long)]
        token: String,
        /// Secret storage backend: `keychain` (default on macOS) or `file`.
        #[arg(long, default_value = "keychain")]
        store: String,
    },
    /// Write sailor-owned hook entries into supported agent config files.
    Install {
        /// Limit to one agent (claude_code, codex, opencode, gemini, cursor, kimi, qwen).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Remove sailor-owned hook entries from agent config files.
    Uninstall {
        #[arg(long)]
        agent: Option<String>,
    },
    /// Run the daemon: Unix socket + HTTP gateway + WebSocket + push.
    Serve {
        /// Gateway port (default 24543, matching moshi-hook for familiarity).
        #[arg(long, default_value = "24543")]
        port: u16,
    },
    /// Print daemon + hook status.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// One-shot terminal-context probe (tmux / Zellij / Herdr detection).
    Context,
    /// Standalone diff viewer server for a repo, opened in a local browser.
    Diff {
        /// Repository directory (default: cwd).
        dir: Option<PathBuf>,
    },
    /// Tail daemon logs.
    Logs {
        #[arg(short, long)]
        follow: bool,
    },
    /// Sync + print agent usage / rate-limit windows.
    Usage {
        #[arg(long)]
        sync: bool,
    },
    /// Print version.
    Version,
}
