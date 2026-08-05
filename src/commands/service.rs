//! Persistent daemon management (`service install/uninstall/status`),
//! moshi-hook parity (`moshi-hook service install`).
//!
//! Linux: installs a systemd *user* unit so the daemon survives reboots and
//! keeps the agent-hook socket + push delivery alive without a terminal.
//! macOS/other: no equivalent is wired here — the Homebrew formula is the
//! service mechanism there (`brew services start sailor-hook`), and this
//! command points the user at it rather than pretending.

use std::path::PathBuf;
use std::process::Command;

const UNIT_NAME: &str = "sailor-hook.service";

fn unit_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(UNIT_NAME))
}

/// The unit runs the current binary's path. `%` must be doubled for
/// systemd, and the path is quoted in case it contains spaces.
fn unit_contents(exe: &std::path::Path) -> String {
    let escaped = exe.display().to_string().replace('%', "%%");
    format!(
        "# Managed by `sailor-hook service install`; `service uninstall` removes it.\n\
         [Unit]\n\
         Description=sailor-hook — AI agent events to the sailor app\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={escaped} serve\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

pub fn run(verb: String) -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        anyhow::bail!(
            "`service` is only wired up on Linux (systemd user units). On macOS, \
             run the daemon persistently with `brew services start sailor-hook` \
             (Homebrew formula) or start `sailor-hook serve` in a terminal."
        );
    }
    match verb.as_str() {
        "install" => install(),
        "uninstall" => uninstall(),
        "status" => status(),
        other => anyhow::bail!("unknown service verb: {other} (install | uninstall | status)"),
    }
}

fn install() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let path = unit_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, unit_contents(&exe))?;
    println!("wrote {}", path.display());

    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", "--now", UNIT_NAME])?;
    println!("sailor-hook service installed and started (`sailor-hook service status` to check).");
    Ok(())
}

fn uninstall() -> anyhow::Result<()> {
    let path = unit_path()?;
    // `disable --now` before removing the file: systemd reads the unit at
    // disable time, so deleting first would leave the service half-torn-down.
    let _ = run_systemctl(&["--user", "disable", "--now", UNIT_NAME]);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let _ = run_systemctl(&["--user", "daemon-reload"]);
    println!("sailor-hook service removed.");
    Ok(())
}

fn status() -> anyhow::Result<()> {
    run_systemctl(&["--user", "status", UNIT_NAME, "--no-pager"])
}

/// Run `systemctl` and surface its exit status to the user (systemctl prints
/// its own human-readable output and errors).
fn run_systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("could not run systemctl ({e}) — is systemd available?"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_escapes_percent_and_quotes_path() {
        let rendered = unit_contents(std::path::Path::new("/opt/sailor/bin/sailor-hook"));
        assert!(rendered.contains("ExecStart=/opt/sailor/bin/sailor-hook serve"));
        let rendered = unit_contents(std::path::Path::new("/home/u/sailor 100%/sailor-hook"));
        assert!(rendered.contains("ExecStart=/home/u/sailor 100%%/sailor-hook serve"));
    }
}
