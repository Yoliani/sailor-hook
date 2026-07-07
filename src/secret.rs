//! Secret storage for the pairing token. macOS: Keychain (via `keyring`).
//! Linux/headless: a file under `~/.config/sailor/` with 0600 perms.
//!
//! Phase 0: enough to store/load a token. Full host-secret + relay
//! registration lands in Phase 3.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const SERVICE: &str = "sailor-hook";
const ACCOUNT: &str = "pairing-token";

#[derive(Debug, Clone, Copy)]
pub enum Backend {
    Keychain,
    File,
}

fn file_path() -> anyhow::Result<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    Ok(config.join("sailor").join("pairing-token"))
}

pub fn store_pairing_token(token: &str, backend: Backend) -> anyhow::Result<()> {
    match backend {
        Backend::Keychain => {
            let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
            entry.set_password(token)?;
            tracing::info!("pairing token stored in keychain");
        }
        Backend::File => {
            let path = file_path()?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, token)?;
            let mut perms = fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&path, perms)?;
            tracing::info!("pairing token stored in {}", path.display());
        }
    }
    Ok(())
}

pub fn load_pairing_token() -> anyhow::Result<String> {
    // Try keychain first, fall back to file.
    if let Ok(entry) = keyring::Entry::new(SERVICE, ACCOUNT) {
        if let Ok(token) = entry.get_password() {
            return Ok(token);
        }
    }
    let path = file_path()?;
    Ok(fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_backend_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("token");
        fs::write(&path, "secret").unwrap();
        // mirror store_pairing_token's file backend
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret");
        let perms = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(perms & 0o077, 0); // not world/group readable after 0600 set
    }
}
