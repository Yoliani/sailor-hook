//! Stable per-machine id. Every event carries it so the app can group the
//! inbox by host (`TECH.md` §4) even when one phone talks to several hosts.
//!
//! Generated once and cached in `~/.config/sailor/host-id`; the file is the
//! source of truth so the id survives daemon restarts and upgrades.

use std::fs;
use std::path::{Path, PathBuf};

pub fn path() -> anyhow::Result<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    Ok(config.join("sailor").join("host-id"))
}

/// Read the cached host id, creating it on first call.
pub fn load_or_create() -> anyhow::Result<String> {
    load_or_create_at(&path()?)
}

fn load_or_create_at(path: &Path) -> anyhow::Result<String> {
    if let Ok(existing) = fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, &id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reuses() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("nested").join("host-id");
        let first = load_or_create_at(&p).unwrap();
        assert!(uuid::Uuid::parse_str(&first).is_ok());
        assert_eq!(load_or_create_at(&p).unwrap(), first);
    }

    #[test]
    fn regenerates_when_blank() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("host-id");
        fs::write(&p, "  \n").unwrap();
        let id = load_or_create_at(&p).unwrap();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }
}
