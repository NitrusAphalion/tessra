//! Where the store and keys live: outside the repository, outside cloud sync.

use std::path::{Path, PathBuf};

use tessra_core::sig::SecretKey;
use tessra_core::EntityId;

use crate::{Error, Result};

/// The per-user local data directory for Tessra.
pub fn local_data_dir() -> PathBuf {
    if cfg!(windows) {
        if let Ok(p) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(p).join("tessra");
        }
    } else if cfg!(target_os = "macos") {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("tessra");
        }
    } else {
        if let Ok(p) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(p).join("tessra");
        }
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join(".local").join("share").join("tessra");
        }
    }
    std::env::temp_dir().join("tessra")
}

pub fn store_dir_for(repo_id: &EntityId) -> PathBuf {
    local_data_dir().join("stores").join(repo_id.to_letters())
}

pub fn keys_dir_for(repo_id: &EntityId) -> PathBuf {
    local_data_dir().join("keys").join(repo_id.to_letters())
}

pub fn workspaces_dir_for(repo_id: &EntityId) -> PathBuf {
    local_data_dir()
        .join("workspaces")
        .join(repo_id.to_letters())
}

/// Heuristic: is this path under a cloud-synced folder?
pub fn is_cloud_synced(path: &Path) -> bool {
    let s = path.to_string_lossy().to_ascii_lowercase();
    [
        "onedrive",
        "dropbox",
        "icloud",
        "google drive",
        "googledrive",
    ]
    .iter()
    .any(|m| s.contains(m))
}

pub fn write_key(path: &Path, key: &SecretKey) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, key.to_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn read_key(path: &Path) -> Result<SecretKey> {
    let bytes = std::fs::read(path)?;
    if bytes.len() != 32 {
        return Err(Error::verb(
            "KEYS",
            format!("{} is not a 32-byte key", path.display()),
        ));
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&bytes);
    Ok(SecretKey::from_bytes(&b))
}
