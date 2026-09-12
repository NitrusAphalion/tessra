//! The Tessra daemon: a repository on disk, workspaces, sessions, and the
//! thirteen verbs. `spec/05-layout.md` for the directories, VERBS.md for
//! the surface.

pub mod anomaly;
pub mod bridge;
pub mod client;
pub mod delivery;
pub mod fs;
pub mod gitimport;
pub mod hooks;
pub mod paths;
pub mod principals;
pub mod risk;
pub mod semantic;
pub mod server;
pub mod swarm;
pub mod tree;
pub mod verbs;
pub mod verifiers;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tessra_core::cbor;
use tessra_core::object::{Op, Workspace};
use tessra_core::sig::SecretKey;
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build::{self, Signer};
use tessra_oplog::OpLog;
use tessra_store::RedbStore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] tessra_core::Error),
    #[error(transparent)]
    OpLog(#[from] tessra_oplog::Error),
    #[error(transparent)]
    Store(#[from] tessra_store::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a tessra repository (no .tessra found from {0})")]
    NotARepo(PathBuf),
    #[error("already initialized at {0}")]
    AlreadyInit(PathBuf),
    #[error("{code}: {message}")]
    Verb { code: &'static str, message: String },
    #[error("git: {0}")]
    Git(String),
}

impl Error {
    pub fn verb(code: &'static str, message: impl Into<String>) -> Self {
        Error::Verb {
            code,
            message: message.into(),
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Error::Verb { code, .. } => code,
            Error::OpLog(tessra_oplog::Error::Rejected { .. }) => "REJECTED",
            Error::NotARepo(_) => "NOT_A_REPO",
            Error::AlreadyInit(_) => "ALREADY_INIT",
            Error::Git(_) => "GIT",
            _ => "INTERNAL",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Nanoseconds since the epoch. Informational only.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// An open repository.
pub struct Repo {
    pub root: PathBuf,
    pub tessra_dir: PathBuf,
    pub store_dir: PathBuf,
    pub keys_dir: PathBuf,
    pub repo_id: EntityId,
    pub log: OpLog<RedbStore>,
    pub daemon: EntityId,
    daemon_key: SecretKey,
    pub workspaces: Vec<Workspace>,
}

pub struct InitOptions {
    pub name: String,
    pub import_git: bool,
    /// How many first-parent commits to import as trunk history. At least 1.
    pub history: usize,
}

impl Default for InitOptions {
    fn default() -> Self {
        InitOptions {
            name: "local".into(),
            import_git: true,
            history: 100,
        }
    }
}

impl Repo {
    /// Create `.tessra/` at `root`, the store and keys outside the tree, and
    /// the init op. Imports the git HEAD tree when present.
    pub fn init(root: &Path, opts: InitOptions) -> Result<Repo> {
        let root = std::fs::canonicalize(root)?;
        let tessra_dir = root.join(".tessra");
        if tessra_dir.exists() {
            return Err(Error::AlreadyInit(tessra_dir));
        }
        let repo_id = EntityId::random();
        let store_dir = paths::store_dir_for(&repo_id);
        let keys_dir = paths::keys_dir_for(&repo_id);
        std::fs::create_dir_all(&tessra_dir)?;
        std::fs::create_dir_all(tessra_dir.join("workspaces"))?;
        std::fs::create_dir_all(&store_dir)?;
        std::fs::create_dir_all(&keys_dir)?;
        std::fs::write(tessra_dir.join("TESSRA"), "tessra 1\n")?;
        std::fs::write(
            tessra_dir.join("store"),
            format!("{}\n", store_dir.display()),
        )?;
        let config = BTreeMap::from([
            (
                "repo_id".to_string(),
                ciborium::value::Value::Bytes(repo_id.0.to_vec()),
            ),
            (
                "cloud_synced".to_string(),
                ciborium::value::Value::Bool(paths::is_cloud_synced(&root)),
            ),
        ]);
        std::fs::write(
            tessra_dir.join("config"),
            cbor::to_canonical_bytes(&config)?,
        )?;

        let daemon_key = SecretKey::generate();
        paths::write_key(&keys_dir.join("daemon.key"), &daemon_key)?;

        let store = RedbStore::open(&store_dir.join("objects.redb"))?;
        store.set_meta("repo_id", &repo_id.0)?;
        let mut log = OpLog::new(store);
        let content = if opts.import_git && root.join(".git").exists() {
            log.store().begin_batch();
            let c = gitimport::import_history(log.store(), &root, opts.history.max(1));
            log.store().end_batch()?;
            c?
        } else {
            Vec::new()
        };
        let init = build::init_with(&mut log, &daemon_key, &opts.name, now(), content)?;
        log.store().set_heads(&log.heads())?;

        let mut repo = Repo {
            root: root.clone(),
            tessra_dir,
            store_dir,
            keys_dir,
            repo_id,
            log,
            daemon: init.daemon,
            daemon_key,
            workspaces: Vec::new(),
        };
        // The colocated checkout is a workspace owned by the daemon's principal.
        let ws = Workspace {
            id: EntityId::random(),
            principal: init.daemon,
            base: init.root_revision,
            current: Some(init.root_revision),
            paths: BTreeMap::from([("".to_string(), root.display().to_string())]),
            env: None,
            created: now(),
            expires: None,
        };
        repo.save_workspace(&ws)?;
        repo.workspaces.push(ws);
        repo.ensure_owner_credential()?;
        Ok(repo)
    }

    /// Open the repository containing `start`.
    pub fn open(start: &Path) -> Result<Repo> {
        let start = std::fs::canonicalize(start)?;
        let mut cur: Option<&Path> = Some(&start);
        let root = loop {
            match cur {
                Some(p) if p.join(".tessra").join("TESSRA").exists() => break p.to_path_buf(),
                Some(p) => cur = p.parent(),
                None => return Err(Error::NotARepo(start.clone())),
            }
        };
        let tessra_dir = root.join(".tessra");
        let store_dir = PathBuf::from(std::fs::read_to_string(tessra_dir.join("store"))?.trim());
        let config: BTreeMap<String, ciborium::value::Value> =
            cbor::from_canonical_bytes(&std::fs::read(tessra_dir.join("config"))?)?;
        let repo_id = match config.get("repo_id") {
            Some(ciborium::value::Value::Bytes(b)) => EntityId::from_slice(b)?,
            _ => return Err(Error::verb("CONFIG", "repo_id missing from .tessra/config")),
        };
        let keys_dir = paths::keys_dir_for(&repo_id);
        let daemon_key = paths::read_key(&keys_dir.join("daemon.key"))?;
        let store = RedbStore::open(&store_dir.join("objects.redb"))?;
        let heads = store.heads()?;
        let log = OpLog::open(store, heads)?;
        // The daemon principal is the owner whose key we hold.
        let view = log.current_view()?;
        let vs = tessra_oplog::ViewState::new(log.store());
        let mut daemon = None;
        for owner in &view.owners {
            if let Some((_, p)) =
                tessra_oplog::verify::resolve_principal(log.store(), &vs, &view, owner)?
            {
                if p.key == Some(daemon_key.public()) {
                    daemon = Some(*owner);
                }
            }
        }
        let daemon =
            daemon.ok_or_else(|| Error::verb("KEYS", "the daemon key matches no owner"))?;
        let mut repo = Repo {
            root,
            tessra_dir,
            store_dir,
            keys_dir,
            repo_id,
            log,
            daemon,
            daemon_key,
            workspaces: Vec::new(),
        };
        repo.workspaces = repo.load_workspaces()?;
        verifiers::ensure_index(repo.store())?;
        Ok(repo)
    }

    /// Write machine-local configuration to `.tessra/config`.
    pub fn set_config(&self, config: &BTreeMap<String, ciborium::value::Value>) -> Result<()> {
        std::fs::write(
            self.tessra_dir.join("config"),
            cbor::to_canonical_bytes(config)?,
        )?;
        Ok(())
    }

    /// Machine-local configuration from `.tessra/config`.
    pub fn config(&self) -> BTreeMap<String, ciborium::value::Value> {
        std::fs::read(self.tessra_dir.join("config"))
            .ok()
            .and_then(|b| cbor::from_canonical_bytes(&b).ok())
            .unwrap_or_default()
    }

    pub fn store(&self) -> &RedbStore {
        self.log.store()
    }

    pub fn daemon_signer(&self) -> Signer {
        Signer {
            principal: self.daemon,
            key: SecretKey::from_bytes(&self.daemon_key.to_bytes()),
        }
    }

    pub fn daemon_key(&self) -> &SecretKey {
        &self.daemon_key
    }

    /// Accept an op and persist heads and the idempotency index.
    pub fn commit_op(&mut self, op: &Op) -> Result<ObjectId> {
        let id = self.log.accept(op)?;
        // Objects first, in one transaction, then the heads that reference them.
        self.log.store().end_batch()?;
        self.log.store().set_heads(&self.log.heads())?;
        verifiers::index_op_attestations(self.log.store(), &op.effects)?;
        self.log.store().begin_batch();
        if let Some(ciborium::value::Value::Bytes(b)) = op.args.get("idem") {
            if b.len() == 16 {
                let mut k = [0u8; 16];
                k.copy_from_slice(b);
                self.log.store().idem_put(&op.author, &k, &id)?;
            }
        }
        Ok(id)
    }

    fn workspaces_dir(&self) -> PathBuf {
        self.tessra_dir.join("workspaces")
    }

    fn load_workspaces(&self) -> Result<Vec<Workspace>> {
        let mut out = Vec::new();
        let dir = self.workspaces_dir();
        if !dir.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) == Some("cbor") {
                let bytes = std::fs::read(entry.path())?;
                if let Ok(ws) = cbor::decode::<Workspace>(&bytes) {
                    out.push(ws);
                }
            }
        }
        out.sort_by_key(|w| w.created);
        Ok(out)
    }

    pub fn save_workspace(&self, ws: &Workspace) -> Result<()> {
        let path = self.workspaces_dir().join(format!("{}.cbor", ws.id));
        std::fs::write(path, cbor::encode(ws)?.bytes)?;
        Ok(())
    }

    /// Semantic operations `edit` recorded in a workspace since its last
    /// snapshot. Daemon-local; the snapshot moves them onto the revision.
    pub fn pending_ops(&self, id: &EntityId) -> Result<Vec<tessra_core::object::SemOp>> {
        let path = self.workspaces_dir().join(format!("{}.ops.cbor", id));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = std::fs::read(path)?;
        Ok(cbor::from_canonical_bytes(&bytes)?)
    }

    pub fn push_pending_op(&self, id: &EntityId, op: tessra_core::object::SemOp) -> Result<()> {
        let mut ops = self.pending_ops(id)?;
        ops.push(op);
        std::fs::create_dir_all(self.workspaces_dir())?;
        let path = self.workspaces_dir().join(format!("{}.ops.cbor", id));
        std::fs::write(path, cbor::to_canonical_bytes(&ops)?)?;
        Ok(())
    }

    pub fn clear_pending_ops(&self, id: &EntityId) -> Result<()> {
        let path = self.workspaces_dir().join(format!("{}.ops.cbor", id));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn remove_workspace(&mut self, id: &EntityId) -> Result<()> {
        let path = self.workspaces_dir().join(format!("{}.cbor", id));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        self.clear_pending_ops(id)?;
        self.workspaces.retain(|w| &w.id != id);
        Ok(())
    }

    pub fn workspace(&self, id: &EntityId) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| &w.id == id)
    }

    pub fn workspace_mut(&mut self, id: &EntityId) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| &w.id == id)
    }

    /// The workspace rooted at the repository directory.
    pub fn root_workspace(&self) -> Option<&Workspace> {
        let root = self.root.display().to_string();
        self.workspaces
            .iter()
            .find(|w| w.paths.get("").map(|p| p == &root).unwrap_or(false))
    }

    /// Fetch any object's bytes by ID.
    pub fn get_bytes(&self, id: &ObjectId) -> Result<Option<Vec<u8>>> {
        Ok(self.store().get_bytes(id)?)
    }
}

/// A command the daemon runs for itself. On Windows the daemon has no
/// console, so a child would otherwise open a console window of its own and
/// pop it in front of whatever the person is doing; this keeps the child
/// windowless. Output still flows through whatever stdio the caller sets.
pub(crate) fn quiet(cmd: std::process::Command) -> std::process::Command {
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd
    };
    cmd
}

/// The program to spawn for the first word of a command line. On Windows a
/// bare name is resolved through `PATH` and `PATHEXT` the way a shell does,
/// which `Command::new` does not: the Node installer ships `npm.cmd` and no
/// `npm.exe`, so `npm` works from any shell and fails from a spawn. A name
/// that carries an extension or a directory is returned as it is, and so is
/// one that resolves to nothing, so the spawn reports the error.
pub(crate) fn program(name: &str) -> String {
    #[cfg(windows)]
    {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        if let Some(found) = find_program(name, &path, &pathext) {
            return found.display().to_string();
        }
    }
    name.to_string()
}

/// Search the directories of `path` for `name` with each extension in
/// `pathext`, in order. `None` when `name` already names an extension or a
/// directory, or when no directory holds a match.
#[cfg_attr(not(windows), allow(dead_code))]
fn find_program(name: &str, path: &std::ffi::OsStr, pathext: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.extension().is_some() || p.components().count() != 1 {
        return None;
    }
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{name}{}", ext.to_ascii_lowercase()));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod program_tests {
    use super::find_program;

    #[test]
    fn a_bare_name_resolves_through_pathext_and_names_with_extensions_do_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("npm.cmd"), "@echo off\r\n").unwrap();
        std::fs::write(dir.path().join("npm"), "#!/bin/sh\n").unwrap();
        let path = std::env::join_paths([dir.path().to_path_buf()]).unwrap();
        let found = find_program("npm", &path, ".COM;.EXE;.BAT;.CMD").unwrap();
        assert_eq!(found, dir.path().join("npm.cmd"));
        assert_eq!(find_program("node", &path, ".COM;.EXE;.BAT;.CMD"), None);
        assert_eq!(find_program("npm.cmd", &path, ".COM;.EXE;.BAT;.CMD"), None);
        assert_eq!(find_program("./npm", &path, ".COM;.EXE;.BAT;.CMD"), None);
    }
}
