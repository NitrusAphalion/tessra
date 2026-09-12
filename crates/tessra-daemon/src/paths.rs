//! Where the store and keys live: outside the repository, outside cloud sync.
//!
//! Everything a machine holds for a repository sits under one base
//! directory: `stores/<repo>` for the object store, `keys/<repo>` for the
//! daemon's key, and `workspaces/<repo>` for the workspaces it materializes.
//! The base is the platform's per-user local data directory, or the
//! directory `TESSRA_DATA_DIR` names when that variable is set and not
//! empty: a CI runner, a test harness, or a machine whose data directory is
//! itself synced. A repository initialized with `InitOptions::data_dir` is
//! bound to that base for the rest of this process instead, which is how the
//! tests keep every scratch repository inside its own tempdir without
//! touching the environment, which is shared by every test in the process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tessra_core::sig::SecretKey;
use tessra_core::EntityId;

use crate::{Error, Result};

/// The environment variable that replaces the platform data directory.
pub const DATA_DIR_ENV: &str = "TESSRA_DATA_DIR";

/// The per-user local data directory for Tessra: `TESSRA_DATA_DIR` when it
/// is set and not empty, else the platform's.
pub fn local_data_dir() -> PathBuf {
    match std::env::var_os(DATA_DIR_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => platform_data_dir(),
    }
}

/// The platform's per-user local data directory for Tessra.
fn platform_data_dir() -> PathBuf {
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

/// The bases bound to repositories in this process by `bind_data_dir`.
fn bound() -> &'static Mutex<HashMap<EntityId, PathBuf>> {
    static BOUND: OnceLock<Mutex<HashMap<EntityId, PathBuf>>> = OnceLock::new();
    BOUND.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bind `repo_id` to `base` for the rest of this process: its store, keys,
/// and workspaces resolve under `base` instead of `local_data_dir()`. Another
/// process finds them only through `TESSRA_DATA_DIR`.
pub fn bind_data_dir(repo_id: &EntityId, base: &Path) {
    bound()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(*repo_id, base.to_path_buf());
}

/// The base directory a repository's store, keys, and workspaces live under:
/// the one bound to it, else `local_data_dir()`.
pub fn data_dir_for(repo_id: &EntityId) -> PathBuf {
    bound()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(repo_id)
        .cloned()
        .unwrap_or_else(local_data_dir)
}

pub fn store_dir_for(repo_id: &EntityId) -> PathBuf {
    data_dir_for(repo_id)
        .join("stores")
        .join(repo_id.to_letters())
}

pub fn keys_dir_for(repo_id: &EntityId) -> PathBuf {
    data_dir_for(repo_id)
        .join("keys")
        .join(repo_id.to_letters())
}

pub fn workspaces_dir_for(repo_id: &EntityId) -> PathBuf {
    data_dir_for(repo_id)
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

/// Test support: a tempdir holding a repository root at `repo/` and, beside
/// it at `data/`, the base its store, keys, and workspaces go under through
/// `InitOptions::data_dir`, so that nothing a test writes outlives the
/// tempdir. The data directory is beside the root, not inside it, so a
/// snapshot or a `git add` of the checkout never sees the open store.
#[cfg(test)]
pub(crate) struct ScratchDir {
    dir: tempfile::TempDir,
    root: PathBuf,
}

#[cfg(test)]
impl ScratchDir {
    pub(crate) fn new() -> ScratchDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        ScratchDir { dir, root }
    }

    /// The repository root.
    pub(crate) fn path(&self) -> &Path {
        &self.root
    }

    /// The base directory to pass as `InitOptions::data_dir`.
    pub(crate) fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_dir_keeps_the_data_beside_the_root_and_inside_the_tempdir() {
        let scratch = ScratchDir::new();
        assert!(scratch.path().is_dir());
        assert_eq!(scratch.path().parent(), Some(scratch.dir.path()));
        assert_eq!(scratch.data_dir().parent(), Some(scratch.dir.path()));
        assert!(!scratch.data_dir().starts_with(scratch.path()));
    }

    #[test]
    fn a_bound_repository_resolves_under_its_base_and_others_do_not() {
        let base = tempfile::tempdir().unwrap();
        let bound_id = EntityId::random();
        let other_id = EntityId::random();
        bind_data_dir(&bound_id, base.path());
        let letters = bound_id.to_letters();
        assert_eq!(data_dir_for(&bound_id), base.path());
        assert_eq!(
            store_dir_for(&bound_id),
            base.path().join("stores").join(&letters)
        );
        assert_eq!(
            keys_dir_for(&bound_id),
            base.path().join("keys").join(&letters)
        );
        assert_eq!(
            workspaces_dir_for(&bound_id),
            base.path().join("workspaces").join(&letters)
        );
        assert_eq!(data_dir_for(&other_id), local_data_dir());
        assert!(!store_dir_for(&other_id).starts_with(base.path()));
    }

    #[test]
    fn the_platform_data_dir_is_named_tessra() {
        assert_eq!(
            platform_data_dir().file_name().and_then(|n| n.to_str()),
            Some("tessra")
        );
    }
}
