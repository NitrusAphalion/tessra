//! Import git history from a colocated checkout through the installed `git`
//! command: the last N first-parent commits become trunk, oldest first, each
//! its own change with a deterministic legacy ID and a legacy intent. The
//! git object database is an object source adapter; this is its first form.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use tessra_core::object::{EntryKind, Intent, NodeIndex, Rule, Snapshot, TrackingRules};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build::InitContent;
use tessra_store::RedbStore;

use crate::tree::{self, Flat, Leaf};
use crate::{now, Error, Result};

/// A snapshot's tree and semantic index, carried from one imported commit
/// to the next so that units keep their identity along the history and a
/// diff between two imported revisions shows only what the commit changed.
pub struct Indexed {
    pub flat: Flat,
    pub index_id: ObjectId,
    pub index: NodeIndex,
}

fn git(root: &Path, args: &[&str]) -> Result<Option<String>> {
    let out = crate::quiet(Command::new("git"))
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| Error::Git(format!("running git: {e}")))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

struct Entry {
    mode: String,
    kind: String,
    oid: String,
    path: String,
}

fn ls_tree(root: &Path, commit: &str) -> Result<Vec<Entry>> {
    let listing = crate::quiet(Command::new("git"))
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "-z", commit])
        .output()
        .map_err(|e| Error::Git(format!("git ls-tree: {e}")))?;
    if !listing.status.success() {
        return Err(Error::Git(format!("git ls-tree {commit} failed")));
    }
    let mut entries = Vec::new();
    for rec in listing.stdout.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let rec = String::from_utf8_lossy(rec);
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let parts: Vec<&str> = meta.split(' ').collect();
        if parts.len() != 3 {
            continue;
        }
        entries.push(Entry {
            mode: parts[0].into(),
            kind: parts[1].into(),
            oid: parts[2].into(),
            path: path.into(),
        });
    }
    Ok(entries)
}

/// Fetch blob contents for git object IDs in one `cat-file --batch`.
fn cat_blobs(root: &Path, oids: &[String]) -> Result<HashMap<String, Vec<u8>>> {
    let mut contents = HashMap::new();
    if oids.is_empty() {
        return Ok(contents);
    }
    let mut child = crate::quiet(Command::new("git"))
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Git(format!("git cat-file: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Git("no stdin".into()))?;
    let req: String = oids.iter().map(|o| format!("{o}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(req.as_bytes()));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Git("no stdout".into()))?;
    let mut reader = BufReader::new(stdout);
    for _ in 0..oids.len() {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let parts: Vec<&str> = header.trim_end().split(' ').collect();
        if parts.len() < 3 {
            return Err(Error::Git(format!("unexpected cat-file header {header:?}")));
        }
        let size: usize = parts[2]
            .parse()
            .map_err(|_| Error::Git("bad size".into()))?;
        let mut data = vec![0u8; size];
        reader.read_exact(&mut data)?;
        let mut nl = [0u8; 1];
        reader.read_exact(&mut nl)?;
        contents.insert(parts[0].to_string(), data);
    }
    let _ = writer.join();
    let _ = child.wait();
    Ok(contents)
}

fn tracking_rules(root: &Path, vendored: &[String]) -> TrackingRules {
    let git_dir = root.join(".git");
    let git_dir = git_dir.is_dir().then_some(git_dir);
    tracking_rules_for(
        root,
        git_dir.as_deref(),
        global_excludes_file().as_deref(),
        vendored,
    )
}

/// The tracking rules for `tree`: what git would ignore there, so a file
/// git never sees is never a file of the repository. From lowest to
/// highest precedence, the last matching rule winning as in git: the global
/// excludes file, `.git/info/exclude` when `git_dir` names a checkout, the
/// root `.gitignore`, and every nested `.gitignore` scoped to its
/// directory, parents before children. Then the vendored paths.
pub fn tracking_rules_for(
    tree: &Path,
    git_dir: Option<&Path>,
    global_excludes: Option<&Path>,
    vendored: &[String],
) -> TrackingRules {
    let mut rules = TrackingRules::default();
    if let Some(global) = global_excludes {
        push_ignore_file(&mut rules, global, "");
    }
    if let Some(git) = git_dir {
        push_ignore_file(&mut rules, &git.join("info").join("exclude"), "");
    }
    push_ignore_file(&mut rules, &tree.join(".gitignore"), "");
    nested_ignore_files(&mut rules, tree, "");
    for v in vendored {
        rules.rules.push(Rule {
            pattern: v.clone(),
            class: 2,
            generator: None,
            media: None,
        });
    }
    rules
}

/// Git's global excludes file: `core.excludesFile` from the user's git
/// configuration, else `$XDG_CONFIG_HOME/git/ignore`, else
/// `~/.config/git/ignore`. Read without running git, so a snapshot never
/// needs it installed.
pub fn global_excludes_file() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let expand = |p: &str| -> PathBuf {
        match (p.strip_prefix("~/"), &home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(p),
        }
    };
    let mut configs: Vec<PathBuf> = Vec::new();
    if let Some(x) = &xdg {
        configs.push(x.join("git").join("config"));
    }
    if let Some(h) = &home {
        configs.push(h.join(".gitconfig"));
    }
    for config in configs {
        let Ok(text) = std::fs::read_to_string(&config) else {
            continue;
        };
        let mut in_core = false;
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                in_core = l.eq_ignore_ascii_case("[core]");
                continue;
            }
            if !in_core {
                continue;
            }
            if let Some((k, v)) = l.split_once('=') {
                if k.trim().eq_ignore_ascii_case("excludesfile") {
                    let v = v.trim().trim_matches('"');
                    if !v.is_empty() {
                        return Some(expand(v));
                    }
                }
            }
        }
    }
    xdg.map(|x| x.join("git").join("ignore"))
}

/// Add the patterns of one ignore file, read in `dir` (empty for the root).
fn push_ignore_file(rules: &mut TrackingRules, file: &Path, dir: &str) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (p, class) = match l.strip_prefix('!') {
            Some(p) => (p, 0u8),
            None => (l, 1u8),
        };
        rules.rules.push(Rule {
            pattern: scope_pattern(dir, p),
            class,
            generator: None,
            media: None,
        });
    }
}

/// A gitignore pattern read in `dir`, as a pattern over paths from the
/// root: one without a slash matches at any depth below `dir`, one with a
/// slash is anchored to `dir`, and a trailing slash still means a directory.
fn scope_pattern(dir: &str, p: &str) -> String {
    if dir.is_empty() {
        return p.to_string();
    }
    let dir_only = p.ends_with('/');
    let body = p.trim_end_matches('/');
    let scoped = if body.contains('/') {
        format!("{dir}/{}", body.trim_start_matches('/'))
    } else {
        format!("{dir}/**/{body}")
    };
    if dir_only {
        format!("{scoped}/")
    } else {
        scoped
    }
}

/// Walk `tree` for nested `.gitignore` files. `.git`, `.tessra`, and any
/// directory the rules so far ignore are never entered, so an ignored
/// build tree costs nothing.
fn nested_ignore_files(rules: &mut TrackingRules, tree: &Path, dir: &str) {
    let here = if dir.is_empty() {
        tree.to_path_buf()
    } else {
        tree.join(dir)
    };
    let Ok(entries) = std::fs::read_dir(here) else {
        return;
    };
    let mut subdirs: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().to_string();
        if !ft.is_dir() || name == ".git" || name == ".tessra" {
            continue;
        }
        let rel = if dir.is_empty() {
            name
        } else {
            format!("{dir}/{name}")
        };
        if crate::fs::ignored(rules, &rel, true) {
            continue;
        }
        subdirs.push(rel);
    }
    subdirs.sort();
    for rel in subdirs {
        push_ignore_file(rules, &tree.join(&rel).join(".gitignore"), &rel);
        nested_ignore_files(rules, tree, &rel);
    }
}

/// Import HEAD only.
pub fn import_head(store: &RedbStore, root: &Path) -> Result<Option<InitContent>> {
    Ok(import_history(store, root, 1)?.pop())
}

/// Import the last `max` first-parent commits, oldest first, each indexed
/// against the one before. Empty when the repository has no commits.
pub fn import_history(store: &RedbStore, root: &Path, max: usize) -> Result<Vec<InitContent>> {
    let n = max.max(1).to_string();
    let Some(list) = git(
        root,
        &["rev-list", "--first-parent", "--reverse", "-n", &n, "HEAD"],
    )?
    else {
        return Ok(Vec::new());
    };
    let commits: Vec<String> = list
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    let mut blob_cache: HashMap<String, (ObjectId, bool)> = HashMap::new();
    let mut rules_id: Option<ObjectId> = None;
    let mut out = Vec::with_capacity(commits.len());
    let mut previous: Option<Indexed> = None;
    for commit in &commits {
        let (content, indexed) = content_for_commit(
            store,
            root,
            commit,
            &mut blob_cache,
            &mut rules_id,
            previous.as_ref(),
        )?;
        out.push(content);
        previous = Some(indexed);
    }
    Ok(out)
}

/// One commit's tree, rules, title, body, intent, and legacy change ID,
/// with its snapshot indexed against `parent`, the revision it follows.
/// `blob_cache` maps git blob ids to stored objects across calls.
pub fn content_for_commit(
    store: &RedbStore,
    root: &Path,
    commit: &str,
    blob_cache: &mut HashMap<String, (ObjectId, bool)>,
    rules_id: &mut Option<ObjectId>,
    parent: Option<&Indexed>,
) -> Result<(InitContent, Indexed)> {
    let entries = ls_tree(root, commit)?;
    let missing: Vec<String> = entries
        .iter()
        .filter(|e| e.kind == "blob" && !blob_cache.contains_key(&e.oid))
        .map(|e| e.oid.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let contents = cat_blobs(root, &missing)?;
    for (oid, data) in contents {
        let large = data.len() > 8 * 1024 * 1024;
        let id = if large {
            crate::fs::artifact_put(store, &data)?
        } else {
            store.put_blob(&data)?
        };
        blob_cache.insert(oid, (id, large));
    }

    let mut flat = Flat::new();
    let mut vendored = Vec::new();
    for e in &entries {
        match (e.kind.as_str(), e.mode.as_str()) {
            ("commit", _) => {
                let id = store.put_blob(e.oid.as_bytes())?;
                flat.insert(e.path.clone(), Leaf::file(id, false));
                vendored.push(e.path.clone());
            }
            ("blob", mode) => {
                let Some((id, large)) = blob_cache.get(&e.oid).copied() else {
                    continue;
                };
                let leaf = if mode == "120000" {
                    Leaf {
                        kind: EntryKind::Symlink,
                        mode: None,
                        r#ref: Some(id),
                        terms: None,
                    }
                } else if large {
                    Leaf {
                        kind: EntryKind::Artifact,
                        mode: None,
                        r#ref: Some(id),
                        terms: None,
                    }
                } else {
                    Leaf::file(id, mode == "100755")
                };
                flat.insert(e.path.clone(), leaf);
            }
            _ => {}
        }
    }
    let rules = match *rules_id {
        Some(r) if vendored.is_empty() => r,
        _ => {
            let r = store.put(&tracking_rules(root, &vendored))?;
            if vendored.is_empty() {
                *rules_id = Some(r);
            }
            r
        }
    };
    let tree_id = tree::build(store, &flat)?;
    let nodes = crate::semantic::build_nodes(store, &flat, parent.map(|p| (&p.flat, &p.index)))?;
    let index_id = crate::semantic::put_index(
        store,
        tree_id,
        parent.iter().map(|p| p.index_id).collect(),
        nodes,
    )?;
    let index: NodeIndex = store.get(&index_id)?;
    let snapshot = store.put(&Snapshot {
        root: tree_id,
        rules,
        env: None,
        index: Some(index_id),
    })?;

    let meta = git(
        root,
        &["log", "-1", "--format=%s%x00%b%x00%an <%ae>%x00%ct", commit],
    )?
    .unwrap_or_default();
    let mut fields = meta.split('\0');
    let title = fields.next().unwrap_or("").trim().to_string();
    let body_text = fields.next().unwrap_or("").trim().to_string();
    let author = fields.next().unwrap_or("unknown").trim().to_string();
    let ctime: i64 = fields
        .next()
        .and_then(|t| t.trim().parse::<i64>().ok())
        .map(|s| s * 1_000_000_000)
        .unwrap_or_else(now);
    let body = format!(
        "{}{}Imported from git commit {commit} by {author}.",
        body_text,
        if body_text.is_empty() { "" } else { "\n\n" }
    );
    let intent = Intent {
        id: EntityId::derive("legacy-intent", commit.as_bytes()),
        prev: None,
        title: title.clone(),
        body: Some(body.clone()),
        spec: None,
        evals: None,
        parent: None,
        depends: None,
        priority: 0,
        status: "done".into(),
        assignee: None,
        created_by: EntityId([0; 16]),
        time: ctime,
        legacy: Some(true),
    };
    Ok((
        InitContent {
            snapshot,
            title: if title.is_empty() {
                "import".into()
            } else {
                title
            },
            body: Some(body),
            intent: Some(intent),
            change: Some(EntityId::derive("legacy-cid", commit.as_bytes())),
            time: Some(ctime),
        },
        Indexed {
            flat,
            index_id,
            index,
        },
    ))
}

#[allow(dead_code)]
fn _unused(_: BTreeMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules are what git would ignore: the global excludes, the
    /// checkout's info/exclude, the root .gitignore, and every nested
    /// .gitignore scoped to its directory, later files winning.
    #[test]
    fn tracking_rules_read_every_ignore_source_git_does() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        let git = dir.path().join("git");
        std::fs::create_dir_all(git.join("info")).unwrap();
        std::fs::create_dir_all(tree.join("notes").join("deep")).unwrap();
        std::fs::create_dir_all(tree.join("a").join("b")).unwrap();
        std::fs::create_dir_all(tree.join("target").join("debug")).unwrap();
        std::fs::write(tree.join(".gitignore"), "/target\n*.pdb\n").unwrap();
        std::fs::write(tree.join("notes").join(".gitignore"), "*\n").unwrap();
        std::fs::write(
            tree.join("a").join("b").join(".gitignore"),
            "*.log\n!keep.log\nbuild/\n",
        )
        .unwrap();
        // A .gitignore inside an ignored tree is never read.
        std::fs::write(tree.join("target").join(".gitignore"), "!debug\n").unwrap();
        std::fs::write(git.join("info").join("exclude"), "**/.claude/local.json\n").unwrap();
        let global = dir.path().join("global-ignore");
        std::fs::write(&global, "*.pem\n").unwrap();
        let rules = tracking_rules_for(&tree, Some(&git), Some(&global), &[]);
        let ignored = |p: &str, is_dir: bool| crate::fs::ignored(&rules, p, is_dir);
        assert!(ignored("target", true), "root .gitignore");
        assert!(ignored("x/y.pdb", false), "a basename pattern at any depth");
        assert!(ignored("notes/secret.md", false), "a nested *");
        assert!(ignored("notes/deep/x", false), "a nested * at any depth");
        assert!(ignored("notes/deep", true));
        assert!(ignored("a/b/run.log", false), "a nested basename pattern");
        assert!(ignored("a/b/c/run.log", false));
        assert!(!ignored("a/b/keep.log", false), "a nested negation");
        assert!(!ignored("a/run.log", false), "scoped to its directory");
        assert!(ignored("a/b/build", true), "a nested directory pattern");
        assert!(!ignored("a/b/build", false), "which is directories only");
        assert!(ignored(".claude/local.json", false), "info/exclude");
        assert!(ignored("x/.claude/local.json", false));
        assert!(ignored("key.pem", false), "the global excludes");
        assert!(!ignored("src/lib.rs", false));
        // Without a checkout, only the tree's own files count.
        let rules = tracking_rules_for(&tree, None, None, &[]);
        assert!(!crate::fs::ignored(&rules, "key.pem", false));
        assert!(!crate::fs::ignored(&rules, ".claude/local.json", false));
        assert!(crate::fs::ignored(&rules, "notes/secret.md", false));
    }
}
