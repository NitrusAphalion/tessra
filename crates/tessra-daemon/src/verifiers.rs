//! Verifiers and attestations in the daemon, per `spec/04-security.md`
//! verifier isolation and `spec/02-objects.md` attestation: the daemon is
//! the runner. It materializes a snapshot into a directory of its own, runs
//! the verifier with a stripped environment, observes the result, and signs
//! the attestation with its own key, naming the tool's principal as the
//! verifier. Code under test never holds a key or a session token.
//!
//! Attestations are found again through a local index kept in store
//! metadata, keyed by subject and by body hash, so a snapshot that was
//! verified once is verified forever in the same environment.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ciborium::value::Value as Cbor;
use serde_bytes::ByteBuf;
use tessra_core::cbor;
use tessra_core::object::{Attestation, Effect, Environment, Node, NodeIndex, Principal, Revision};
use tessra_core::sig::{SecretKey, SignedObject};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build;
use tessra_store::RedbStore;

use crate::{fs, now, paths, Error, Repo, Result};

// ------------------------------------------------------------- the index

fn att_subject_key(subject: &[u8]) -> String {
    format!("att:s:{}", hex::encode(subject))
}

fn att_body_key(body: &ObjectId) -> String {
    format!("att:b:{}", body.to_hex())
}

fn append_id(store: &RedbStore, key: &str, id: &ObjectId) -> Result<()> {
    let mut cur = store.meta(key)?.unwrap_or_default();
    if cur.chunks(32).any(|c| c == id.as_bytes()) {
        return Ok(());
    }
    cur.extend_from_slice(id.as_bytes());
    store.set_meta(key, &cur)?;
    Ok(())
}

fn ids_at(store: &RedbStore, key: &str) -> Result<Vec<ObjectId>> {
    Ok(store
        .meta(key)?
        .unwrap_or_default()
        .chunks(32)
        .filter_map(|c| ObjectId::from_slice(c).ok())
        .collect())
}

/// Index one stored attestation by its subject and bodies.
pub fn index_attestation(store: &RedbStore, id: &ObjectId, att: &Attestation) -> Result<()> {
    if let Some(s) = &att.subject {
        append_id(store, &att_subject_key(s), id)?;
    }
    if let Some(bodies) = &att.bodies {
        for b in bodies {
            append_id(store, &att_body_key(b), id)?;
        }
    }
    Ok(())
}

/// Index whatever attestations an op puts.
pub fn index_op_attestations(store: &RedbStore, effects: &[Effect]) -> Result<()> {
    for e in effects {
        if let Effect::Put { id } = e {
            if let Some(bytes) = store.get_bytes(id)? {
                if cbor::peek_tag(&bytes).ok().as_deref() == Some("attestation") {
                    if let Ok(att) = cbor::decode::<Attestation>(&bytes) {
                        index_attestation(store, id, &att)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Build the index once for a store that predates it.
pub fn ensure_index(store: &RedbStore) -> Result<()> {
    if store.meta("att:indexed")?.is_some() {
        return Ok(());
    }
    for id in store.ids()? {
        if let Some(bytes) = store.get_bytes(&id)? {
            if cbor::peek_tag(&bytes).ok().as_deref() == Some("attestation") {
                if let Ok(att) = cbor::decode::<Attestation>(&bytes) {
                    index_attestation(store, &id, &att)?;
                }
            }
        }
    }
    store.set_meta("att:indexed", &[1])?;
    Ok(())
}

/// Every attestation that could apply to a revision: by subject on the
/// revision, its previous revision, and its snapshots, and by body on
/// every body its index carries.
pub fn collect_for(
    repo: &Repo,
    rev_id: &ObjectId,
    rev: &Revision,
    idx: Option<&NodeIndex>,
) -> Result<Vec<ObjectId>> {
    let store = repo.store();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut out = Vec::new();
    let mut subjects: Vec<Vec<u8>> = vec![rev_id.as_bytes().to_vec()];
    if let Some(p) = rev.prev {
        subjects.push(p.as_bytes().to_vec());
    }
    subjects.extend(rev.snapshots.values().map(|s| s.as_bytes().to_vec()));
    for s in subjects {
        for id in ids_at(store, &att_subject_key(&s))? {
            if seen.insert(id) {
                out.push(id);
            }
        }
    }
    if let Some(idx) = idx {
        let bodies: HashSet<ObjectId> = idx.nodes.iter().map(|n| n.body).collect();
        for b in bodies {
            for id in ids_at(store, &att_body_key(&b))? {
                if seen.insert(id) {
                    out.push(id);
                }
            }
        }
    }
    Ok(out)
}

/// Every attestation on a subject, any kind.
pub fn attestations_on(store: &RedbStore, subject: &[u8]) -> Result<Vec<(ObjectId, Attestation)>> {
    let mut out = Vec::new();
    for id in ids_at(store, &att_subject_key(subject))? {
        out.push((id, store.get(&id)?));
    }
    Ok(out)
}

/// Every attestation naming a body.
pub fn by_body(store: &RedbStore, body: &ObjectId) -> Result<Vec<(ObjectId, Attestation)>> {
    let mut out = Vec::new();
    for id in ids_at(store, &att_body_key(body))? {
        out.push((id, store.get(&id)?));
    }
    Ok(out)
}

/// Whether an attestation's signer is a principal standards trust.
pub fn trusted_signer(repo: &Repo, att: &Attestation) -> bool {
    let signer = att.runner.unwrap_or(att.verifier);
    let Ok(view) = repo.log.current_view() else {
        return false;
    };
    let vs = tessra_oplog::view::ViewState::new(repo.store());
    match vs.entity(&view, &signer) {
        Ok(Some(p)) => p
            .single()
            .ok()
            .and_then(|oid| repo.store().get::<Principal>(&oid).ok())
            .is_some_and(|p| tessra_oplog::standard::TRUSTED_ATTESTERS.contains(&p.kind.as_str())),
        _ => false,
    }
}

/// Attestations of one kind on a subject, newest first.
pub fn on_subject(
    store: &RedbStore,
    kind: &str,
    subject: &[u8],
) -> Result<Vec<(ObjectId, Attestation)>> {
    let mut out = Vec::new();
    for id in ids_at(store, &att_subject_key(subject))? {
        let a: Attestation = store.get(&id)?;
        if a.kind == kind {
            out.push((id, a));
        }
    }
    out.sort_by_key(|(_, a)| std::cmp::Reverse(a.time));
    Ok(out)
}

// ------------------------------------------------------------- verifiers

/// How a verifier selects tests by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// `cargo test -- name1 name2`: substring filters.
    Cargo,
    /// `pytest -k "name1 or name2"`.
    Pytest,
    /// No selection; always a full run.
    None,
}

#[derive(Clone, Debug)]
pub struct Verifier {
    /// Attestation kind it produces.
    pub kind: String,
    /// Principal name.
    pub name: String,
    pub argv: Vec<String>,
    pub filter: Filter,
}

/// Verifiers for a materialized tree: the `verifiers` map in
/// `.tessra/config` (kind to command line), else detected from the tree.
pub fn verifiers_for(repo: &Repo, dir: &Path) -> Vec<Verifier> {
    let mut out = Vec::new();
    if let Some(Cbor::Map(m)) = repo.config().get("verifiers") {
        for (k, v) in m {
            if let (Cbor::Text(kind), Cbor::Text(cmd)) = (k, v) {
                let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
                if argv.is_empty() {
                    continue;
                }
                let filter = match argv[0].as_str() {
                    "cargo" if argv.get(1).map(String::as_str) == Some("test") => Filter::Cargo,
                    "pytest" => Filter::Pytest,
                    "python" | "python3" if argv.iter().any(|a| a == "pytest") => Filter::Pytest,
                    _ => Filter::None,
                };
                out.push(Verifier {
                    kind: kind.clone(),
                    name: argv[0].clone(),
                    argv,
                    filter,
                });
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    if dir.join("Cargo.toml").exists() {
        out.push(Verifier {
            kind: "tests.pass".into(),
            name: "cargo-test".into(),
            argv: vec!["cargo".into(), "test".into()],
            filter: Filter::Cargo,
        });
    } else if dir.join("pyproject.toml").exists()
        || dir.join("pytest.ini").exists()
        || dir.join("setup.py").exists()
        || dir.join("tests").is_dir()
    {
        out.push(Verifier {
            kind: "tests.pass".into(),
            name: "pytest".into(),
            argv: vec!["python".into(), "-m".into(), "pytest".into()],
            filter: Filter::Pytest,
        });
    } else if dir.join("package.json").exists() {
        out.push(Verifier {
            kind: "tests.pass".into(),
            name: "npm-test".into(),
            argv: vec!["npm".into(), "test".into(), "--silent".into()],
            filter: Filter::None,
        });
    }
    out
}

/// Tools that run on Node: their fingerprint records `node --version` too.
const NODE_TOOLS: &[&str] = &["npm", "npx", "pnpm", "yarn", "bun", "corepack"];

/// The tool a verifier's environment is fingerprinted by: the command's
/// first word, or the word after the flags when the command is wrapped in
/// `cmd /c` for Windows, where `cmd --version` would say nothing useful.
fn fingerprint_tool(argv: &[String]) -> Option<&str> {
    let mut words = argv.iter().map(String::as_str);
    let first = words.next()?;
    let stem = Path::new(first)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(first);
    if stem.eq_ignore_ascii_case("cmd") {
        return words.find(|w| !w.starts_with('/'));
    }
    Some(first)
}

/// The environment fingerprint for a verifier run, stored once per distinct
/// toolchain so the cache keys on it.
pub fn environment(repo: &Repo, v: &Verifier) -> Result<ObjectId> {
    let mut tools = BTreeMap::new();
    let probe = |cmd: &str| -> Option<String> {
        let out = crate::quiet(Command::new(crate::program(cmd)))
            .arg("--version")
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s.lines().next().unwrap_or("").to_string())
        }
    };
    let tool = fingerprint_tool(&v.argv).unwrap_or("");
    if let Some(ver) = probe(tool) {
        tools.insert(tool.to_string(), ver);
    }
    if tool == "cargo" {
        if let Some(ver) = probe("rustc") {
            tools.insert("rustc".into(), ver);
        }
    }
    if NODE_TOOLS.contains(&tool) {
        if let Some(ver) = probe("node") {
            tools.insert("node".into(), ver);
        }
    }
    let env = Environment {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        tools,
        image: None,
        // Process isolation without a separate user or a network policy: L0.
        sandbox: Some("L0".into()),
        extra: Some(BTreeMap::from([(
            "isolation".to_string(),
            Cbor::Text("separate directory, stripped environment, no keys or daemon token".into()),
        )])),
    };
    Ok(repo.store().put(&env)?)
}

/// The principal a tool attests as, created on first use and signed by the daemon.
pub fn ensure_verifier(repo: &mut Repo, name: &str) -> Result<EntityId> {
    let dir = repo.keys_dir.join("verifiers");
    std::fs::create_dir_all(&dir)?;
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let id_path = dir.join(format!("{safe}.id"));
    if id_path.exists() {
        return EntityId::from_letters(std::fs::read_to_string(&id_path)?.trim())
            .map_err(|e| Error::verb("VERIFIER", e.to_string()));
    }
    let key = SecretKey::generate();
    let id = EntityId::random();
    let mut p = Principal {
        id,
        prev: None,
        key: Some(key.public()),
        kind: "verifier".into(),
        name: name.into(),
        parent: Some(repo.daemon),
        model: None,
        runtime: None,
        bindings: None,
        status: "active".into(),
        expires: None,
        time: now(),
        sig: None,
    };
    p.sign_with(repo.daemon_key())?;
    let oid = repo.store().put(&p)?;
    let signer = repo.daemon_signer();
    let op = build::build_op(
        &repo.log,
        &signer,
        None,
        "principal",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        vec![
            Effect::Put { id: oid },
            Effect::Point {
                entity: id,
                to: oid,
                from: None,
            },
        ],
        now(),
    )?;
    repo.commit_op(&op)?;
    // The verifier's key never signs; the runner does. Kept so the
    // principal is well formed and could rotate.
    paths::write_key(&dir.join(format!("{safe}.key")), &key)?;
    std::fs::write(&id_path, id.to_letters())?;
    Ok(id)
}

/// Set while a verifier runs. The daemon refuses requests meanwhile, so
/// code under test cannot act through it, and callers know to retry.
pub static VERIFYING: AtomicBool = AtomicBool::new(false);

struct VerifyingGuard;

impl VerifyingGuard {
    fn enter() -> VerifyingGuard {
        VERIFYING.store(true, Ordering::SeqCst);
        VerifyingGuard
    }
}

impl Drop for VerifyingGuard {
    fn drop(&mut self) {
        VERIFYING.store(false, Ordering::SeqCst);
    }
}

/// The last `max` bytes of a runner's output as text, where the summary and
/// the errors are; a cut through a multibyte character is dropped.
pub fn tail_text(output: &[u8], max: usize) -> String {
    let start = output.len().saturating_sub(max);
    String::from_utf8_lossy(&output[start..])
        .trim_start_matches('\u{FFFD}')
        .to_string()
}

/// What one run observed.
pub struct RunOutcome {
    pub ok: bool,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub elapsed_ms: u64,
    /// Per-test results parsed from the output, when the tool reports them.
    pub tests: Vec<(String, bool)>,
    pub output: Vec<u8>,
}

/// Run a verifier in `dir` with a stripped environment and a timeout.
/// `select` names tests to run; empty means the whole suite.
pub fn run(
    repo: &Repo,
    v: &Verifier,
    dir: &Path,
    select: &[String],
    timeout: Duration,
) -> Result<RunOutcome> {
    let mut argv = v.argv.clone();
    match (v.filter, select.is_empty()) {
        (Filter::Cargo, false) => {
            argv.push("--".into());
            argv.extend(select.iter().cloned());
        }
        (Filter::Pytest, false) => {
            argv.push("-k".into());
            argv.push(select.join(" or "));
            argv.push("-v".into());
        }
        (Filter::Pytest, true) => argv.push("-v".into()),
        _ => {}
    }
    let mut cmd = crate::quiet(Command::new(crate::program(&argv[0])));
    cmd.args(&argv[1..]);
    cmd.current_dir(dir);
    cmd.env_clear();
    for (k, val) in std::env::vars_os() {
        let key = k.to_string_lossy().to_ascii_uppercase();
        let keep = matches!(
            key.as_str(),
            "PATH"
                | "HOME"
                | "USERPROFILE"
                | "TEMP"
                | "TMP"
                | "TMPDIR"
                | "SYSTEMROOT"
                | "WINDIR"
                | "COMSPEC"
                | "PATHEXT"
                | "LANG"
                | "LC_ALL"
                | "CARGO_HOME"
                | "RUSTUP_HOME"
                | "RUSTUP_TOOLCHAIN"
                | "PROGRAMDATA"
                | "PROGRAMFILES"
                | "PROGRAMFILES(X86)"
                | "LOCALAPPDATA"
                | "APPDATA"
                | "SYSTEMDRIVE"
                | "NUMBER_OF_PROCESSORS"
                | "PROCESSOR_ARCHITECTURE"
                | "VIRTUAL_ENV"
                | "PYTHONPATH"
        ) || key.starts_with("VS")
            || key.starts_with("VCTOOLS")
            || key.starts_with("WINDOWSSDK")
            || key.starts_with("UCRT")
            || key.starts_with("INCLUDE")
            || key.starts_with("LIB");
        let secretish = key.contains("TOKEN")
            || key.contains("SECRET")
            || key.contains("KEY") && key != "PATHEXT"
            || key.contains("TESSRA");
        if keep && !secretish {
            cmd.env(k, val);
        }
    }
    // One build cache per repository across runs, outside the tree under test.
    if v.argv.first().map(String::as_str) == Some("cargo") {
        cmd.env(
            "CARGO_TARGET_DIR",
            paths::workspaces_dir_for(&repo.repo_id).join("verify-target"),
        );
        cmd.env("CARGO_TERM_COLOR", "never");
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.env("TESSRA_SANDBOX", "1");
    let _guard = VerifyingGuard::enter();
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::verb("VERIFIER", format!("cannot run {}: {e}", argv[0])))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut out = Vec::new();
        let mut err = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut out);
        }
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut err);
        }
        (out, err)
    });
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            timed_out = true;
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let (out, err) = reader.join().unwrap_or_default();
    let mut output = out;
    if !err.is_empty() {
        output.extend_from_slice(b"\n--- stderr ---\n");
        output.extend_from_slice(&err);
    }
    let exit = status.and_then(|s| s.code());
    let tests = parse_tests(v.filter, &output);
    Ok(RunOutcome {
        ok: !timed_out && exit == Some(0),
        exit,
        timed_out,
        elapsed_ms: start.elapsed().as_millis() as u64,
        tests,
        output,
    })
}

/// Per-test results from a tool's output.
fn parse_tests(filter: Filter, output: &[u8]) -> Vec<(String, bool)> {
    let text = String::from_utf8_lossy(output);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        match filter {
            Filter::Cargo => {
                // `test tests::add_works ... ok` / `... FAILED` / `... ignored`
                if let Some(rest) = line.strip_prefix("test ") {
                    if let Some((name, status)) = rest.rsplit_once(" ... ") {
                        let status = status.trim();
                        if status == "ok" {
                            out.push((name.trim().to_string(), true));
                        } else if status.starts_with("FAILED") {
                            out.push((name.trim().to_string(), false));
                        }
                    }
                }
            }
            Filter::Pytest => {
                // `tests/test_x.py::test_add PASSED [ 50%]`
                if let Some((name, status)) = line.split_once(' ') {
                    if name.contains("::") {
                        let s = status.trim_start();
                        if s.starts_with("PASSED") {
                            out.push((name.to_string(), true));
                        } else if s.starts_with("FAILED") || s.starts_with("ERROR") {
                            out.push((name.to_string(), false));
                        }
                    }
                }
            }
            Filter::None => {}
        }
    }
    out
}

/// Whether a parsed test name refers to a unit named `unit`.
pub fn test_matches(parsed: &str, unit: &str) -> bool {
    parsed == unit
        || parsed.ends_with(&format!("::{unit}"))
        || parsed.rsplit("::").next() == Some(unit)
}

/// Materialize a root tree into a scratch directory of the daemon's own.
pub fn materialize_scratch(repo: &Repo, root: &ObjectId, label: &str) -> Result<PathBuf> {
    let dir = paths::workspaces_dir_for(&repo.repo_id).join(format!("verify-{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    fs::materialize(repo.store(), root, &dir, false)?;
    Ok(dir)
}

/// Build and record an attestation signed by the daemon as runner.
#[allow(clippy::too_many_arguments)]
pub fn attest_as_runner(
    repo: &mut Repo,
    kind: &str,
    subject: Option<(&[u8], &str)>,
    bodies: Option<Vec<ObjectId>>,
    scope: Option<BTreeMap<String, Cbor>>,
    result: Cbor,
    env: Option<ObjectId>,
    verifier: EntityId,
    evidence: Option<&[u8]>,
) -> Result<ObjectId> {
    let evidence_id = match evidence {
        Some(bytes) if !bytes.is_empty() => {
            let cut = if bytes.len() > 65_536 {
                &bytes[bytes.len() - 65_536..]
            } else {
                bytes
            };
            Some(repo.store().put_blob(cut)?)
        }
        _ => None,
    };
    let mut bodies = bodies;
    if let Some(b) = bodies.as_mut() {
        b.sort();
        b.dedup();
    }
    let mut att = Attestation {
        kind: kind.into(),
        subject: subject.map(|(s, _)| ByteBuf::from(s.to_vec())),
        bodies,
        subject_type: subject.map(|(_, t)| t.to_string()),
        scope,
        result,
        env,
        verifier,
        runner: Some(repo.daemon),
        evidence: evidence_id,
        time: now(),
        sigkind: "ed25519".into(),
        sig: None,
    };
    att.sign_with(repo.daemon_key())?;
    let id = repo.store().put(&att)?;
    let signer = repo.daemon_signer();
    let op = build::build_op(
        &repo.log,
        &signer,
        None,
        "attest",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        vec![Effect::Put { id }],
        now(),
    )?;
    repo.commit_op(&op)?;
    Ok(id)
}

/// The parent's text with only the new test units added to it, each
/// placed inside the container it lives in on the change, or at the end
/// of the file. The code under test stays the parent's, which is the point.
pub fn overlay_tests(
    parent_text: &[u8],
    parent_nodes: &[Node],
    change_text: &[u8],
    change_nodes: &[Node],
    tests: &[&Node],
) -> Vec<u8> {
    let mut inserts: Vec<(usize, Vec<u8>)> = Vec::new();
    for t in tests {
        let span = (t.span.0 as usize, t.span.1 as usize);
        let line_start = change_text[..span.0]
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let indent = &change_text[line_start..span.0];
        let indent: &[u8] = if indent.iter().all(|b| *b == b' ' || *b == b'\t') {
            indent
        } else {
            &[]
        };
        let mut text = Vec::new();
        text.extend_from_slice(b"\n\n");
        text.extend_from_slice(indent);
        text.extend_from_slice(&change_text[span.0..span.1]);
        let pos = match t.parent {
            Some(pid) => {
                let cont = change_nodes.iter().find(|n| n.nid == pid);
                let in_parent = parent_nodes.iter().find(|n| n.nid == pid).or_else(|| {
                    cont.and_then(|c| {
                        parent_nodes
                            .iter()
                            .find(|n| n.kind == c.kind && n.name == c.name)
                    })
                });
                match in_parent {
                    Some(pc) => {
                        let last_child_end = parent_nodes
                            .iter()
                            .filter(|n| n.parent == Some(pc.nid))
                            .map(|n| n.span.1 as usize)
                            .max();
                        match last_child_end {
                            Some(e) => e,
                            None => {
                                let end = (pc.span.1 as usize).min(parent_text.len());
                                if parent_text[..end].ends_with(b"}") {
                                    end - 1
                                } else {
                                    end
                                }
                            }
                        }
                    }
                    None => parent_text.len(),
                }
            }
            None => parent_text.len(),
        };
        inserts.push((pos.min(parent_text.len()), text));
    }
    inserts.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut out = parent_text.to_vec();
    for (pos, text) in inserts {
        out.splice(pos..pos, text);
    }
    out
}

/// Units changed against a parent index, by identity.
pub fn changed_units<'a>(parent: &NodeIndex, idx: &'a NodeIndex) -> Vec<&'a Node> {
    let prev: HashMap<EntityId, &Node> = parent.nodes.iter().map(|n| (n.nid, n)).collect();
    idx.nodes
        .iter()
        .filter(|n| match prev.get(&n.nid) {
            None => true,
            Some(p) => p.body != n.body || p.name != n.name,
        })
        .collect()
}

pub fn text_list(items: &[String]) -> Cbor {
    Cbor::Array(items.iter().map(|s| Cbor::Text(s.clone())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_semantic::matching::assign_ids;

    fn nodes(path: &str, src: &str) -> Vec<Node> {
        let raw = tessra_semantic::extract(path, src.as_bytes()).unwrap();
        assign_ids(path, &raw, &[])
    }

    #[test]
    fn the_fingerprint_names_the_tool_behind_a_cmd_wrapper() {
        let plain: Vec<String> = ["cargo", "test"].iter().map(|s| s.to_string()).collect();
        assert_eq!(fingerprint_tool(&plain), Some("cargo"));
        let wrapped: Vec<String> = ["cmd", "/d", "/c", "pnpm", "test"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fingerprint_tool(&wrapped), Some("pnpm"));
        let exe: Vec<String> = ["CMD.EXE", "/C", "npm", "test"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fingerprint_tool(&exe), Some("npm"));
        assert_eq!(fingerprint_tool(&[]), None);
    }

    #[test]
    fn tail_text_keeps_the_end_of_the_output_whole() {
        let out = "héllo wörld\nFAIL something\n Tests  2 failed | 10 passed\n".as_bytes();
        let tail = tail_text(out, 32);
        assert!(tail.ends_with("2 failed | 10 passed\n"), "{tail:?}");
        assert!(tail.len() <= 32);
        assert!(!tail.contains('\u{FFFD}'));
        // A cut inside `ö` drops the broken char rather than showing a marker.
        let cut = tail_text("wörld".as_bytes(), 4);
        assert_eq!(cut, "rld");
        assert_eq!(tail_text(b"ab", 10), "ab");
        assert_eq!(tail_text(b"", 10), "");
    }

    #[test]
    fn cargo_and_pytest_output_parse_per_test() {
        let cargo = b"running 3 tests\ntest tests::add_works ... ok\ntest tests::sub_fails ... FAILED\ntest tests::slow ... ignored\n";
        let t = parse_tests(Filter::Cargo, cargo);
        assert_eq!(
            t,
            vec![
                ("tests::add_works".to_string(), true),
                ("tests::sub_fails".to_string(), false)
            ]
        );
        assert!(test_matches("tests::add_works", "add_works"));
        assert!(!test_matches("tests::add_works_more", "add_works"));
        let py =
            b"tests/test_x.py::test_add PASSED [ 50%]\ntests/test_x.py::test_sub FAILED [100%]\n";
        let t = parse_tests(Filter::Pytest, py);
        assert_eq!(t.len(), 2);
        assert!(t[0].1 && !t[1].1);
    }

    #[test]
    fn new_tests_are_laid_into_the_parent_without_the_code_change() {
        let parent = "pub fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn old() {\n        assert_eq!(sub(3, 1), 2);\n    }\n}\n";
        let change = parent
            .replace("    a - b\n", "    a.saturating_sub(b)\n")
            .replace("    }\n}\n", "    }\n\n    #[test]\n    fn saturates() {\n        assert_eq!(sub(i32::MIN, 1), i32::MIN);\n    }\n}\n");
        let pn = nodes("src/lib.rs", parent);
        let raw = tessra_semantic::extract("src/lib.rs", change.as_bytes()).unwrap();
        let cn = assign_ids("src/lib.rs", &raw, &pn);
        let new_tests: Vec<&Node> = cn
            .iter()
            .filter(|n| n.kind == "test" && !pn.iter().any(|p| p.nid == n.nid))
            .collect();
        assert_eq!(new_tests.len(), 1);
        assert_eq!(new_tests[0].name, "saturates");
        let out = overlay_tests(parent.as_bytes(), &pn, change.as_bytes(), &cn, &new_tests);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("    a - b\n"),
            "the code under test stays the parent's: {text}"
        );
        assert!(!text.contains("saturating_sub"), "{text}");
        assert!(text.contains("    #[test]\n    fn saturates() {"), "{text}");
        assert!(text.contains("fn old()"), "{text}");
        let reparsed = tessra_semantic::extract("src/lib.rs", text.as_bytes()).unwrap();
        assert_eq!(
            reparsed.iter().filter(|n| n.kind == "test").count(),
            2,
            "{text}"
        );
        assert!(text.trim_end().ends_with('}'), "{text}");
    }
}
