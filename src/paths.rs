//! Central path resolution for state and config, honoring the same env
//! overrides moshi-hook documents (`MOSHI_STATE_DIR` / `MOSHI_CONFIG_DIR`).
//!
//! `SAILOR_STATE_DIR` redirects the daemon's runtime state (socket, lock,
//! gateway log); `SAILOR_CONFIG_DIR` redirects the file-backed secret store.
//! On a scratch run, setting either — especially `SAILOR_CONFIG_DIR` — makes
//! a fresh daemon start *unpaired* rather than inheriting the real host's
//! identity, which is what an e2e or smoke-test harness wants. See
//! `secret.rs`: when the config dir is overridden, load_pairing_token reads
//! the file store only, never the login keychain.

use std::path::PathBuf;

/// Daemon runtime state: socket, lockfile, gateway log. Defaults to
/// `~/.sailor`, overridable with `SAILOR_STATE_DIR`.
pub fn state_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("SAILOR_STATE_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    Ok(home.join(".sailor"))
}

/// File-backed secrets and gateway token. Defaults to
/// `~/.config/sailor`, overridable with `SAILOR_CONFIG_DIR`.
pub fn config_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("SAILOR_CONFIG_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let config = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    Ok(config.join("sailor"))
}
