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

/// The gateway's bearer token, generating and persisting one on first use.
///
/// Both `serve` (which validates it) and `easy-pair` (which hands it to the
/// phone in the QR) call this, so they have to converge on the same value.
/// That rules out the keychain: a read from a non-interactive process is
/// denied rather than prompted, and a silently-failed read here would make
/// each caller mint its own token and the phone's requests 401. A 0600 file
/// is readable by every process running as this user, which is exactly the
/// set that is already trusted, and it works the same on a headless Linux
/// host with no keychain at all.
pub fn ensure_gateway_token() -> anyhow::Result<String> {
    let path = gateway_file_path()?;
    if let Ok(token) = fs::read_to_string(&path) {
        if !token.trim().is_empty() {
            return Ok(token.trim().to_string());
        }
    }

    let token = uuid::Uuid::new_v4().simple().to_string();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &token)?;
    let mut perms = fs::metadata(&path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&path, perms)?;
    tracing::info!("gateway token generated ({})", path.display());
    Ok(token)
}

fn gateway_file_path() -> anyhow::Result<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    Ok(config.join("sailor").join("gateway-token"))
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
