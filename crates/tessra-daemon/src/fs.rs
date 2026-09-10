//! Snapshotting a directory into a tree, and materializing a tree into a
//! directory. Tracking rules, line-ending normalization, and the flags the
//! spec names: case collisions, unportable names, secrets, out-of-scope paths.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tessra_core::object::{ConflictTerm, EntryKind, Flags, SecretMatch, TrackingRules};
use tessra_core::store::ObjectStore;
use tessra_core::ObjectId;
use tessra_oplog::glob;

use crate::tree::{self, Flat, Leaf};
use crate::Result;

/// Directories never tracked, at any depth.
const ALWAYS_SKIP: &[&str] = &[".git", ".tessra"];

pub struct SnapshotOutcome {
    pub tree: ObjectId,
    pub flat: Flat,
    pub flags: Flags,
    pub files: usize,
}

fn ignored(rules: &TrackingRules, rel: &str, is_dir: bool) -> bool {
    let mut class = 0u8;
    for r in &rules.rules {
        let pat = r.pattern.trim_end_matches('/');
        let dir_only = r.pattern.ends_with('/');
        if dir_only && !is_dir {
            continue;
        }
        if glob::matches(pat, rel) {
            class = r.class;
        }
    }
    class == 1
}

fn is_text(bytes: &[u8]) -> bool {
    !bytes.iter().take(8192).any(|&b| b == 0)
}

fn normalize_eol(bytes: Vec<u8>) -> Vec<u8> {
    if !bytes.contains(&b'\r') {
        return bytes;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn unportable(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return true;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return true;
    }
    name.chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
}

/// A small secrets scanner. No regex crate; a handful of high-signal shapes.
fn scan_secrets(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        hits.push("private-key-block".into());
    }
    let alnum = |c: char| c.is_ascii_alphanumeric();
    let check = |prefix: &str, min: usize, label: &str, hits: &mut Vec<String>| {
        let mut start = 0;
        while let Some(i) = text[start..].find(prefix) {
            let after = &text[start + i + prefix.len()..];
            let run = after
                .chars()
                .take_while(|c| alnum(*c) || *c == '_' || *c == '-')
                .count();
            if run >= min {
                hits.push(label.to_string());
                break;
            }
            start += i + prefix.len();
        }
    };
    check("AKIA", 16, "aws-access-key", &mut hits);
    check("ghp_", 36, "github-token", &mut hits);
    check("xoxb-", 20, "slack-token", &mut hits);
    check("sk-", 32, "api-key", &mut hits);
    hits
}

/// Snapshot `dir` into a tree. `write_scope` flags paths outside it rather
/// than refusing. `base` lets out-of-scope detection consider only changes.
pub fn snapshot_dir<S: ObjectStore>(
    store: &S,
    dir: &Path,
    rules: &TrackingRules,
    write_scope: Option<&[String]>,
    base: Option<&Flat>,
) -> Result<SnapshotOutcome> {
    let mut flat = Flat::new();
    let mut flags = Flags::default();
    let mut secrets: Vec<SecretMatch> = Vec::new();
    let mut collisions: Vec<Vec<String>> = Vec::new();
    let mut unportable_names: Vec<String> = Vec::new();
    walk_dir(
        store,
        dir,
        dir,
        rules,
        &mut flat,
        &mut secrets,
        &mut collisions,
        &mut unportable_names,
    )?;
    if let (Some(scope), Some(b)) = (write_scope, base) {
        let mut out = Vec::new();
        for (p, l) in &flat {
            if b.get(p) != Some(l) && !glob::any_match(scope, p) {
                out.push(p.clone());
            }
        }
        for p in b.keys() {
            if !flat.contains_key(p) && !glob::any_match(scope, p) {
                out.push(p.clone());
            }
        }
        if !out.is_empty() {
            out.sort();
            flags.out_of_scope = Some(out);
        }
    }
    if !secrets.is_empty() {
        flags.secrets = Some(secrets);
    }
    if !collisions.is_empty() {
        flags.case_collisions = Some(collisions);
    }
    if !unportable_names.is_empty() {
        flags.unportable_names = Some(unportable_names);
    }
    let files = flat.len();
    let tree = tree::build(store, &flat)?;
    Ok(SnapshotOutcome {
        tree,
        flat,
        flags,
        files,
    })
}

/// Every tracked file under `dir`, as `(root-relative path, full path)`,
/// sorted. Follows the tracking rules the same way a snapshot does.
pub fn tracked_files(dir: &Path, rules: &TrackingRules) -> Result<Vec<(String, PathBuf)>> {
    fn walk(
        root: &Path,
        dir: &Path,
        rules: &TrackingRules,
        out: &mut Vec<(String, PathBuf)>,
    ) -> Result<()> {
        let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)?;
            names.push((name, path, meta.is_dir()));
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path, is_dir) in names {
            if ALWAYS_SKIP.contains(&name.as_str()) || name.ends_with(".tessra-conflict") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if ignored(rules, &rel, is_dir) {
                continue;
            }
            if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                continue;
            }
            if is_dir {
                walk(root, &path, rules, out)?;
            } else {
                out.push((rel, path));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, rules, &mut out)?;
    Ok(out)
}

/// Everything under `dir`, no tracking rules, as a tree: build state,
/// experiment output, anything a workspace should carry along with its
/// files. `None` when the directory does not exist. Returns the tree and
/// the file count.
pub fn state_tree<S: ObjectStore>(store: &S, dir: &Path) -> Result<Option<(ObjectId, usize)>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut flat = Flat::new();
    for rel in list_files(dir, dir)? {
        let full = dir.join(&rel);
        let meta = std::fs::symlink_metadata(&full)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let bytes = std::fs::read(&full)?;
        let leaf = if bytes.len() > 8 * 1024 * 1024 {
            Leaf {
                kind: EntryKind::Artifact,
                mode: None,
                r#ref: Some(artifact_put(store, &bytes)?),
                terms: None,
            }
        } else {
            Leaf::file(store.put_blob(&bytes)?, executable_bit(&meta))
        };
        flat.insert(rel, leaf);
    }
    let n = flat.len();
    Ok(Some((tree::build(store, &flat)?, n)))
}

#[allow(clippy::too_many_arguments)]
fn walk_dir<S: ObjectStore>(
    store: &S,
    root: &Path,
    dir: &Path,
    rules: &TrackingRules,
    flat: &mut Flat,
    secrets: &mut Vec<SecretMatch>,
    collisions: &mut Vec<Vec<String>>,
    unportable_names: &mut Vec<String>,
) -> Result<()> {
    let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        names.push((name, path, meta.is_dir()));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    if rules.case == "check" {
        let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (n, _, _) in &names {
            seen.entry(n.to_lowercase()).or_default().push(n.clone());
        }
        for group in seen.into_values() {
            if group.len() > 1 {
                collisions.push(group);
            }
        }
    }
    for (name, path, is_dir) in names {
        if ALWAYS_SKIP.contains(&name.as_str()) || name.ends_with(".tessra-conflict") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if ignored(rules, &rel, is_dir) {
            continue;
        }
        if rules.portable_names && unportable(&name) {
            unportable_names.push(rel.clone());
        }
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path)?;
            let id = store.put_blob(target.to_string_lossy().as_bytes())?;
            flat.insert(
                rel,
                Leaf {
                    kind: EntryKind::Symlink,
                    mode: None,
                    r#ref: Some(id),
                    terms: None,
                },
            );
            continue;
        }
        if is_dir {
            walk_dir(
                store,
                root,
                &path,
                rules,
                flat,
                secrets,
                collisions,
                unportable_names,
            )?;
            continue;
        }
        // A sidecar means the entry is still conflicted, whatever the file says.
        let sidecar = path.with_file_name(format!("{name}.tessra-conflict"));
        if sidecar.exists() {
            let terms: Vec<ConflictTerm> =
                tessra_core::cbor::from_canonical_bytes(&std::fs::read(&sidecar)?)?;
            flat.insert(
                rel,
                Leaf {
                    kind: EntryKind::Conflict,
                    mode: None,
                    r#ref: None,
                    terms: Some(terms),
                },
            );
            continue;
        }
        let mut bytes = std::fs::read(&path)?;
        let text = is_text(&bytes);
        if rules.eol == "lf" && text {
            bytes = normalize_eol(bytes);
        }
        if text {
            if let Ok(s) = std::str::from_utf8(&bytes) {
                for hit in scan_secrets(s) {
                    secrets.push(SecretMatch {
                        path: rel.clone(),
                        pattern: hit,
                    });
                }
            }
        }
        let threshold = rules.artifact_threshold.unwrap_or(8 * 1024 * 1024) as usize;
        if bytes.len() > threshold {
            let id = artifact_put(store, &bytes)?;
            flat.insert(
                rel,
                Leaf {
                    kind: EntryKind::Artifact,
                    mode: None,
                    r#ref: Some(id),
                    terms: None,
                },
            );
            continue;
        }
        let executable = executable_bit(&meta);
        let id = store.put_blob(&bytes)?;
        flat.insert(rel, Leaf::file(id, executable));
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perm = meta.permissions();
        perm.set_mode(perm.mode() | 0o111);
        let _ = std::fs::set_permissions(path, perm);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

#[cfg(unix)]
fn executable_bit(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_bit(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Store large content as fixed 1 MiB chunks behind an artifact pointer.
/// Content-defined chunking is an open spec item; this is the placeholder.
pub fn artifact_put<S: ObjectStore>(store: &S, bytes: &[u8]) -> Result<ObjectId> {
    let mut chunks = Vec::new();
    for c in bytes.chunks(1024 * 1024) {
        chunks.push(store.put_bytes("chunk", c)?);
    }
    let art = tessra_core::object::Artifact {
        hash: tessra_core::hash::artifact_content_hash(bytes),
        size: bytes.len() as u64,
        chunks,
    };
    Ok(store.put(&art)?)
}

/// Write a tree into `dir`. Existing files not in the tree are removed
/// unless `keep_extra`. Returns the number of files written.
pub fn materialize<S: ObjectStore>(
    store: &S,
    tree_id: &ObjectId,
    dir: &Path,
    keep_extra: bool,
) -> Result<usize> {
    std::fs::create_dir_all(dir)?;
    let flat = tree::flatten(store, tree_id)?;
    if !keep_extra {
        let existing = list_files(dir, dir)?;
        for p in existing {
            if !flat.contains_key(&p) {
                let _ = std::fs::remove_file(dir.join(&p));
            }
        }
    }
    // Plain files are written by a few threads at once; everything else in order.
    let files: Vec<(PathBuf, ObjectId, bool)> = flat
        .iter()
        .filter(|(_, l)| l.kind == EntryKind::File)
        .filter_map(|(p, l)| l.r#ref.map(|id| (dir.join(p), id, l.mode.unwrap_or(0) & 1 == 1)))
        .collect();
    for (full, _, _) in &files {
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let chunk = files.len().div_ceil(workers).max(1);
    let mut n = std::thread::scope(|scope| -> Result<usize> {
        let mut handles = Vec::new();
        for part in files.chunks(chunk) {
            handles.push(scope.spawn(move || -> Result<usize> {
                let mut written = 0;
                for (full, id, exec) in part {
                    let bytes = store
                        .get_bytes(id)?
                        .ok_or(tessra_core::Error::NotFound(*id))?;
                    std::fs::write(full, bytes)?;
                    if *exec {
                        set_executable(full);
                    }
                    written += 1;
                }
                Ok(written)
            }));
        }
        let mut total = 0;
        for h in handles {
            total += h.join().map_err(|_| crate::Error::verb("MATERIALIZE", "a writer thread panicked"))??;
        }
        Ok(total)
    })?;
    for (path, leaf) in &flat {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match leaf.kind {
            EntryKind::File => {}
            EntryKind::Symlink => {
                let id = leaf
                    .r#ref
                    .ok_or_else(|| crate::Error::verb("TREE", "symlink without ref"))?;
                let target = store
                    .get_bytes(&id)?
                    .ok_or(tessra_core::Error::NotFound(id))?;
                write_symlink(&full, &String::from_utf8_lossy(&target))?;
                n += 1;
            }
            EntryKind::Artifact => {
                let id = leaf
                    .r#ref
                    .ok_or_else(|| crate::Error::verb("TREE", "artifact without ref"))?;
                let art: tessra_core::object::Artifact = store.get(&id)?;
                let mut out = Vec::with_capacity(art.size as usize);
                for c in &art.chunks {
                    out.extend(store.get_bytes(c)?.ok_or(tessra_core::Error::NotFound(*c))?);
                }
                std::fs::write(&full, out)?;
                n += 1;
            }
            EntryKind::Conflict => {
                let terms = leaf.terms.clone().unwrap_or_default();
                let mut text = String::new();
                let adds: Vec<&tessra_core::object::ConflictTerm> =
                    terms.iter().filter(|t| t.sign > 0).collect();
                let removes: Vec<&tessra_core::object::ConflictTerm> =
                    terms.iter().filter(|t| t.sign < 0).collect();
                for (i, a) in adds.iter().enumerate() {
                    let content = store.get_bytes(&a.r#ref)?.unwrap_or_default();
                    text.push_str(&format!("<<<<<<< side {}\n", (b'A' + i as u8) as char));
                    text.push_str(&String::from_utf8_lossy(&content));
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    if i == 0 {
                        for r in &removes {
                            let base = store.get_bytes(&r.r#ref)?.unwrap_or_default();
                            text.push_str("||||||| base\n");
                            text.push_str(&String::from_utf8_lossy(&base));
                            if !text.ends_with('\n') {
                                text.push('\n');
                            }
                        }
                        text.push_str("=======\n");
                    }
                }
                text.push_str(">>>>>>> end\n");
                std::fs::write(&full, text)?;
                let sidecar = full.with_file_name(format!(
                    "{}.tessra-conflict",
                    full.file_name().unwrap_or_default().to_string_lossy()
                ));
                std::fs::write(sidecar, tessra_core::cbor::to_canonical_bytes(&terms)?)?;
                n += 1;
            }
            EntryKind::Dir => {}
        }
    }
    Ok(n)
}

#[cfg(unix)]
fn write_symlink(path: &Path, target: &str) -> Result<()> {
    let _ = std::fs::remove_file(path);
    std::os::unix::fs::symlink(target, path)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_symlink(path: &Path, target: &str) -> Result<()> {
    // Symlink creation needs a privilege on Windows; write the target as text.
    std::fs::write(path, target)?;
    Ok(())
}

fn list_files(root: &Path, dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if ALWAYS_SKIP.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            out.extend(list_files(root, &path)?);
        } else {
            out.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_core::store::MemoryStore;

    #[test]
    fn snapshot_and_materialize_round_trip_with_flags() {
        let s = MemoryStore::new();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: x").unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}\r\n").unwrap();
        std::fs::write(root.join("README.md"), "# hi\n").unwrap();
        std::fs::write(root.join("secret.txt"), "AKIAABCDEFGHIJKLMNOP is a key").unwrap();
        std::fs::write(root.join("con.txt"), "reserved").unwrap();
        std::fs::write(root.join("ignored.log"), "x").unwrap();
        let mut rules = TrackingRules::default();
        rules.rules.push(tessra_core::object::Rule {
            pattern: "*.log".into(),
            class: 1,
            generator: None,
            media: None,
        });
        let out = snapshot_dir(
            &s,
            root,
            &rules,
            Some(&["src/**".to_string()]),
            Some(&Flat::new()),
        )
        .unwrap();
        assert!(out.flat.contains_key("src/a.rs"));
        assert!(!out.flat.contains_key("ignored.log"));
        assert!(!out.flat.contains_key(".git/HEAD"));
        // CRLF normalized.
        let id = out.flat["src/a.rs"].r#ref.unwrap();
        assert_eq!(s.get_bytes(&id).unwrap().unwrap(), b"fn a() {}\n");
        let flags = out.flags;
        assert_eq!(flags.secrets.as_ref().unwrap()[0].path, "secret.txt");
        assert_eq!(
            flags.unportable_names.as_ref().unwrap(),
            &vec!["con.txt".to_string()]
        );
        let oos = flags.out_of_scope.unwrap();
        assert!(oos.contains(&"README.md".to_string()));
        assert!(!oos.contains(&"src/a.rs".to_string()));

        let dst = tempfile::tempdir().unwrap();
        let n = materialize(&s, &out.tree, dst.path(), false).unwrap();
        assert_eq!(n, 4);
        assert_eq!(
            std::fs::read(dst.path().join("src/a.rs")).unwrap(),
            b"fn a() {}\n"
        );
        let again = snapshot_dir(&s, dst.path(), &rules, None, None).unwrap();
        assert_eq!(again.tree, out.tree);
    }
}
