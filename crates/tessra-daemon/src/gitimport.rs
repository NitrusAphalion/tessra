//! Import git history from a colocated checkout through the installed `git`
//! command: the last N first-parent commits become trunk, oldest first, each
//! its own change with a deterministic legacy ID and a legacy intent. The
//! git object database is an object source adapter; this is its first form.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use tessra_core::object::{EntryKind, Intent, Rule, Snapshot, TrackingRules};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build::InitContent;

use crate::tree::{self, Flat, Leaf};
use crate::{now, Error, Result};

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
    let mut rules = TrackingRules::default();
    if let Ok(text) = std::fs::read_to_string(root.join(".gitignore")) {
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let (pattern, class) = match l.strip_prefix('!') {
                Some(p) => (p.to_string(), 0u8),
                None => (l.to_string(), 1u8),
            };
            rules.rules.push(Rule {
                pattern,
                class,
                generator: None,
                media: None,
            });
        }
    }
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

/// Import HEAD only.
pub fn import_head<S: ObjectStore>(store: &S, root: &Path) -> Result<Option<InitContent>> {
    Ok(import_history(store, root, 1)?.pop())
}

/// Import the last `max` first-parent commits, oldest first. Empty when
/// the repository has no commits.
pub fn import_history<S: ObjectStore>(
    store: &S,
    root: &Path,
    max: usize,
) -> Result<Vec<InitContent>> {
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
    for commit in &commits {
        out.push(content_for_commit(store, root, commit, &mut blob_cache, &mut rules_id)?);
    }
    Ok(out)
}

/// One commit's tree, rules, title, body, intent, and legacy change ID.
/// `blob_cache` maps git blob ids to stored objects across calls.
pub fn content_for_commit<S: ObjectStore>(
    store: &S,
    root: &Path,
    commit: &str,
    blob_cache: &mut HashMap<String, (ObjectId, bool)>,
    rules_id: &mut Option<ObjectId>,
) -> Result<InitContent> {
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
    let snapshot = store.put(&Snapshot {
        root: tree_id,
        rules,
        env: None,
        index: None,
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
    Ok(InitContent {
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
    })
}

#[allow(dead_code)]
fn _unused(_: BTreeMap<String, String>) {}
