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
    /// Remove the pairing token stored by `pair` — this host is unpaired
    /// until `pair` runs again.
    Unpair,
    /// Start mosh-server and print a QR code the sailor app scans to
    /// connect directly — no SSH credentials on the phone.
    EasyPair {
        /// Host address to encode in the QR. When omitted, an interactive
        /// picker offers the discovered addresses (Tailscale, Bonjour, LAN);
        /// non-interactive runs fall back to this machine's primary IPv4.
        /// Use a Tailscale MagicDNS name to roam networks.
        #[arg(long)]
        host: Option<String>,
        /// Terminal color count mosh-server advertises (8 or 256).
        #[arg(long, default_value = "256")]
        colors: u16,
        /// Gateway port to probe. When a daemon answers there, the QR also
        /// carries its token so the app can list sessions over HTTP instead
        /// of by typing probes into your terminal.
        #[arg(long, default_value = "24543")]
        gateway_port: u16,
        /// Don't start a daemon if none is running. By default `easy-pair`
        /// launches one bound to `--host` (when that address belongs to this
        /// machine) so session listing works without a second command; this
        /// leaves the phone falling back to probing your terminal.
        #[arg(long)]
        no_serve: bool,
    },
    /// Normalize one agent hook payload (read from stdin) and post it to the
    /// running daemon. This is what `install` wires into each agent's config;
    /// it is not meant to be run by hand.
    Event {
        /// Agent whose payload format to expect
        /// (claude_code, codex, gemini, cursor, kimi, qwen, opencode, pi).
        #[arg(long)]
        agent: String,
        /// How long to park a decidable approval waiting for the phone
        /// before falling back to the agent's own terminal prompt. Keep it
        /// below the hook's configured `timeout` so this process always gets
        /// to answer rather than being killed mid-write.
        #[arg(long, default_value = "240")]
        wait_secs: u64,
    },
    /// Answer an approval the agent is parked on. Run by the app over its
    /// SSH exec channel. Exits non-zero if nothing was waiting on that id.
    Approve {
        /// The row's `pendingActionId`.
        pending_action_id: String,
        /// Approve the tool call.
        #[arg(long, conflicts_with = "deny")]
        allow: bool,
        /// Refuse the tool call.
        #[arg(long)]
        deny: bool,
    },
    /// Print the inbox as NDJSON, one row per line.
    Inbox {
        /// Keep the connection open and stream updates as they arrive.
        #[arg(long)]
        watch: bool,
    },
    /// Write sailor-owned hook entries into supported agent config files.
    Install {
        /// Limit to one agent (claude_code, codex, opencode, gemini, cursor, kimi, qwen, pi).
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
        /// Set `SAILOR_HOOK_GATEWAY_LISTEN=host:port` to override both
        /// address and port at once (flag wins over env).
        #[arg(long)]
        port: Option<u16>,
        /// Address the gateway binds. Loopback (the default) is reachable
        /// only through your own SSH session. Bind a tailnet address (or
        /// 0.0.0.0) to let an Easy Pair phone reach it directly — that
        /// requires the bearer token, which the QR then carries.
        #[arg(long)]
        bind: Option<std::net::IpAddr>,
        /// SSH port to advertise for LAN auto-discovery (mosh bootstraps over SSH).
        #[arg(long, default_value = "22")]
        ssh_port: u16,
        /// Skip advertising this host as `_sailor._tcp` on the LAN.
        #[arg(long)]
        no_advertise: bool,
    },
    /// Run the daemon persistently. Linux: installs a systemd user unit and
    /// starts it; `uninstall` removes it; `status` shows the unit. On macOS
    /// this points at `brew services` instead.
    Service {
        /// `install` | `uninstall` | `status`.
        verb: String,
    },
    /// Advertise this host as a `_sailor._tcp` Bonjour service on the LAN
    /// so the sailor app can auto-discover it. The TXT record carries the
    /// Tailscale MagicDNS name (when Tailscale is running) so a host found
    /// once on WiFi keeps connecting over the tailnet, including on cellular.
    Advertise {
        /// SSH port the phone should connect to (mosh bootstraps over SSH).
        #[arg(long, default_value = "22")]
        ssh_port: u16,
    },
    /// List local HTTP dev servers the app can open in an in-app browser,
    /// or kill one (`servers kill --pid N --port N`). Same-port forwarding:
    /// the app loads each origin over an SSH forward, no proxy involved.
    Servers {
        #[command(subcommand)]
        cmd: Option<ServersCommand>,
    },
    /// Configure or test self-hostable push (ntfy / Gotify / UnifiedPush).
    /// With no flags, prints the current setup.
    Push {
        /// Endpoint URL: an ntfy topic, a Gotify server base, or a
        /// UnifiedPush endpoint.
        #[arg(long)]
        set: Option<String>,
        /// Endpoint type.
        #[arg(long, default_value = "ntfy")]
        kind: String,
        /// ntfy access token (optional) or Gotify application token.
        #[arg(long)]
        token: Option<String>,
        /// Send a test notification through the configured endpoint.
        #[arg(long)]
        test: bool,
    },
    /// Print daemon + hook status.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// One-shot terminal-context probe (tmux / Zellij / Herdr detection).
    Context,
    /// Detect installed multiplexers and list their sessions as one line of
    /// JSON. The app's session browser runs this over an Easy Pair mosh
    /// session, where the only channel is the user's visible shell — one
    /// short command instead of a screenful of raw probing.
    MuxList,
    /// Recent project directories from agent transcript history, as JSON.
    CwdList,
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

#[derive(Debug, clap::Subcommand)]
pub enum ServersCommand {
    /// Terminate a discovered dev server after re-validating that the PID
    /// and port still belong to one (arbitrary PIDs are rejected).
    Kill {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        port: u16,
        /// SIGKILL after a 2s grace if SIGTERM didn't take.
        #[arg(long)]
        force: bool,
    },
}
