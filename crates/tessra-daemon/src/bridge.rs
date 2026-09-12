//! The bridge to git, per `spec/05-layout.md` roots and git colocation and
//! ROADMAP.md M7: export the landed trunk as a clean, linear git history
//! that reuses every commit it came from, and import commits that appeared
//! on the branch since as landed revisions. The mapping between revisions
//! and commits lives in store metadata and in the bodies of imported
//! revisions, so a round trip rewrites nothing.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value as Json};
use tessra_core::object::{EntryKind, Revision, Snapshot};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build;

use crate::gitimport;
use crate::principals::Actor;
use crate::tree;
use crate::{now, Error, Repo, Result};

/// A path as git accepts it: without Windows' verbatim prefix.
fn plain(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

fn rev_key(rev: &ObjectId) -> String {
    format!("git:rev:{}", rev.to_hex())
}

fn commit_key(commit: &str) -> String {
    format!("git:commit:{commit}")
}

/// The git commit a revision came from or was exported as.
pub fn commit_of(repo: &Repo, rev_id: &ObjectId, rev: &Revision) -> Result<Option<String>> {
    if let Some(bytes) = repo.store().meta(&rev_key(rev_id))? {
        return Ok(Some(String::from_utf8_lossy(&bytes).to_string()));
    }
    // Imported revisions name their commit in the body.
    if let Some(body) = &rev.body {
        if let Some(pos) = body.find("Imported from git commit ") {
            let rest = &body[pos + "Imported from git commit ".len()..];
            let hash: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if hash.len() == 40 {
                return Ok(Some(hash));
            }
        }
    }
    Ok(None)
}

fn remember(repo: &Repo, rev_id: &ObjectId, commit: &str) -> Result<()> {
    repo.store().set_meta(&rev_key(rev_id), commit.as_bytes())?;
    repo.store()
        .set_meta(&commit_key(commit), rev_id.as_bytes())?;
    Ok(())
}

pub fn revision_of_commit(repo: &Repo, commit: &str) -> Result<Option<ObjectId>> {
    Ok(repo
        .store()
        .meta(&commit_key(commit))?
        .and_then(|b| ObjectId::from_slice(&b).ok()))
}

/// After the checkout moved with git, point the colocated workspace at the
/// landed revision its files now come from: the one HEAD's commit maps to.
/// A workspace whose current revision is not landed carries unlanded work
/// of the owner's and stays put; landing restacks it. Reports what it did.
fn follow_checkout(repo: &mut Repo, work: &Path) -> Result<Json> {
    let head = git_out(work, &[], &["rev-parse", "HEAD"], None)?;
    let Some(rev_id) = revision_of_commit(repo, &head)? else {
        return Ok(json!({ "workspace": "left alone", "why": "HEAD is not a commit trunk knows" }));
    };
    let Some(ws) = repo.root_workspace().cloned() else {
        return Ok(json!({ "workspace": "none" }));
    };
    let at = ws.current.unwrap_or(ws.base);
    if at == rev_id {
        return Ok(json!({ "workspace": "at HEAD", "revision": rev_id.to_hex() }));
    }
    let view = repo.log.current_view()?;
    let landed = tessra_oplog::verify::landed_revisions(repo.store(), &view)?;
    if !landed.contains(&at) {
        return Ok(
            json!({ "workspace": "left alone", "why": "it carries unlanded work", "revision": at.to_hex() }),
        );
    }
    let mut ws = ws;
    ws.base = rev_id;
    ws.current = Some(rev_id);
    repo.save_workspace(&ws)?;
    if let Some(w) = repo.workspace_mut(&ws.id) {
        *w = ws;
    }
    Ok(json!({ "workspace": "followed", "revision": rev_id.to_hex() }))
}

fn git_out(
    dir: &Path,
    envs: &[(&str, String)],
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<String> {
    let mut c = crate::quiet(Command::new("git"));
    c.arg("-C").arg(plain(dir)).args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    let mut child = c
        .spawn()
        .map_err(|e| Error::Git(format!("running git: {e}")))?;
    if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
        si.write_all(data)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(Error::Git(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The trunk revisions oldest first, with their snapshot IDs.
fn trunk_chain(repo: &Repo) -> Result<Vec<(ObjectId, Revision)>> {
    let (head, _) = crate::verbs::trunk_head_of(repo)?;
    let mut out = Vec::new();
    let mut cur = Some(head);
    while let Some(id) = cur {
        let r: Revision = repo.store().get(&id)?;
        cur = r.parents.first().copied();
        out.push((id, r));
        if out.len() > 100_000 {
            break;
        }
    }
    out.reverse();
    Ok(out)
}

/// Write one revision's tree into the git object database and return the
/// tree id, using a temporary index.
fn write_git_tree(repo: &Repo, git_dir: &Path, work: &Path, rev: &Revision) -> Result<String> {
    let snap_id = rev
        .snapshots
        .get("")
        .ok_or_else(|| Error::verb("ROOT", "revision has no root snapshot"))?;
    let snap: Snapshot = repo.store().get(snap_id)?;
    let flat = tree::flatten(repo.store(), &snap.root)?;
    let scratch = crate::verifiers::materialize_scratch(
        repo,
        &snap.root,
        &format!("export-{}", &snap_id.to_hex()[..12]),
    )?;
    let index = work.join(format!("tessra-index-{}", &snap_id.to_hex()[..12]));
    let _ = std::fs::remove_file(&index);
    let envs = [
        ("GIT_INDEX_FILE", plain(&index)),
        ("GIT_DIR", plain(git_dir)),
    ];
    let envs_ref: Vec<(&str, String)> = envs.iter().map(|(k, v)| (*k, v.clone())).collect();
    // Blobs, in one call, paths relative to the scratch tree.
    let mut paths: Vec<&String> = flat
        .iter()
        .filter(|(_, l)| l.kind == EntryKind::File)
        .map(|(p, _)| p)
        .collect();
    paths.sort();
    let mut info = String::new();
    if !paths.is_empty() {
        let listing = paths
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let hashes = git_out(
            &scratch,
            &envs_ref,
            &["hash-object", "-w", "--stdin-paths"],
            Some(listing.as_bytes()),
        )?;
        for (p, h) in paths.iter().zip(hashes.lines()) {
            let mode = if flat.get(*p).and_then(|l| l.mode).unwrap_or(0) & 1 == 1 {
                "100755"
            } else {
                "100644"
            };
            info.push_str(&format!("{mode} {h}\t{p}\n"));
        }
    }
    for (p, l) in flat.iter().filter(|(_, l)| l.kind == EntryKind::Symlink) {
        if let Some(target) = l
            .r#ref
            .and_then(|r| repo.store().get_bytes(&r).ok().flatten())
        {
            let h = git_out(
                &scratch,
                &envs_ref,
                &["hash-object", "-w", "--stdin"],
                Some(&target),
            )?;
            info.push_str(&format!("120000 {h}\t{p}\n"));
        }
    }
    git_out(&scratch, &envs_ref, &["read-tree", "--empty"], None)?;
    if !info.is_empty() {
        git_out(
            &scratch,
            &envs_ref,
            &["update-index", "--add", "--index-info"],
            Some(info.as_bytes()),
        )?;
    }
    let tree_id = git_out(&scratch, &envs_ref, &["write-tree"], None)?;
    let _ = std::fs::remove_file(&index);
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(tree_id)
}

/// Export trunk to a branch: every landed revision that has no commit yet
/// becomes one commit on top of the newest revision that has, authored by
/// the change's author, in trunk order. Nothing already exported or
/// imported is rewritten.
pub fn export_git(
    repo: &mut Repo,
    branch: &str,
    dir: Option<PathBuf>,
    push: Option<&str>,
) -> Result<Json> {
    let work = dir.unwrap_or_else(|| repo.root.clone());
    let git_dir = PathBuf::from(git_out(
        &work,
        &[],
        &["rev-parse", "--absolute-git-dir"],
        None,
    )?);
    let chain = trunk_chain(repo)?;
    // The newest revision that already is a commit.
    let mut parent: Option<String> = None;
    let mut start = 0;
    for (i, (id, r)) in chain.iter().enumerate() {
        if let Some(c) = commit_of(repo, id, r)? {
            parent = Some(c);
            start = i + 1;
        }
    }
    let mut created = Vec::new();
    for (id, r) in chain.iter().skip(start) {
        let tree_id = write_git_tree(repo, &git_dir, &work, r)?;
        let author_name = repo
            .principal_name(&r.author)
            .unwrap_or_else(|| "tessra".into());
        let secs = r.time / 1_000_000_000;
        let message = format!(
            "{}\n\n{}{}Tessra-Change: {}\nTessra-Revision: {}\n",
            r.title,
            r.body.clone().unwrap_or_default(),
            if r.body.as_ref().is_some_and(|b| !b.is_empty()) {
                "\n\n"
            } else {
                ""
            },
            r.id.to_letters(),
            id.to_hex()
        );
        let envs: Vec<(&str, String)> = vec![
            ("GIT_DIR", plain(&git_dir)),
            ("GIT_AUTHOR_NAME", author_name.clone()),
            (
                "GIT_AUTHOR_EMAIL",
                format!("{}@tessra.local", author_name.replace(' ', ".")),
            ),
            ("GIT_AUTHOR_DATE", format!("{secs} +0000")),
            ("GIT_COMMITTER_NAME", "tessra".into()),
            ("GIT_COMMITTER_EMAIL", "tessra@tessra.local".into()),
            ("GIT_COMMITTER_DATE", format!("{secs} +0000")),
        ];
        let mut args: Vec<String> = vec!["commit-tree".into(), tree_id.clone()];
        if let Some(p) = &parent {
            args.push("-p".into());
            args.push(p.clone());
        }
        args.push("-F".into());
        args.push("-".into());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let commit = git_out(&work, &envs, &arg_refs, Some(message.as_bytes()))?;
        remember(repo, id, &commit)?;
        created.push(json!({ "commit": commit, "title": r.title, "author": author_name, "change": r.id.to_letters() }));
        parent = Some(commit);
    }
    let Some(tip) = parent.clone() else {
        return Err(Error::verb("EXPORT", "trunk has no revisions to export"));
    };
    let refname = format!("refs/heads/{branch}");
    let current =
        git_out(&work, &[], &["rev-parse", "--abbrev-ref", "HEAD"], None).unwrap_or_default();
    let checkout = if current != branch {
        // Nothing is checked out from the branch: the ref alone moves.
        git_out(&work, &[], &["update-ref", &refname, &tip], None)?;
        "left alone"
    } else {
        // The branch is checked out: fast-forward it, so the ref, the index,
        // and the working tree move together. Local edits to files the export
        // did not touch stay in place; when one overlaps, or the branch holds
        // commits trunk does not know, git refuses and nothing has moved. The
        // ref must never move alone under a checkout: an index left behind
        // HEAD reads as staged deletions of the files the export added.
        if let Err(e) = git_out(&work, &[], &["merge", "--ff-only", "--quiet", &tip], None) {
            return Err(Error::verb(
                "EXPORT",
                format!(
                    "the checkout of {branch} did not fast-forward to the export, so the branch was not moved: {e}. \
                     Commit or stash local changes that overlap the exported files, or run `import --branch {branch}` first if the branch has commits trunk does not know; then export again, or export to another branch"
                ),
            ));
        }
        "updated"
    };
    let pushed = push.map(|remote| {
        git_out(
            &work,
            &[],
            &["push", remote, &format!("{tip}:{refname}")],
            None,
        )
        .map(|_| json!({ "remote": remote, "ok": true }))
        .unwrap_or_else(|e| json!({ "remote": remote, "ok": false, "error": e.to_string() }))
    });
    // The checkout that was reset is the colocated workspace's: it follows.
    let is_root = std::fs::canonicalize(&work).ok().as_ref() == Some(&repo.root);
    let workspace = if checkout == "updated" && is_root {
        follow_checkout(repo, &work)?
    } else {
        json!({ "workspace": "left alone", "why": format!("checkout {checkout}") })
    };
    Ok(json!({
        "branch": branch, "tip": tip, "created": created.len(), "reused": start, "commits": created,
        "checkout": checkout, "pushed": pushed, "workspace": workspace,
    }))
}

/// Import commits on a branch that trunk does not know yet, first-parent
/// order, each as a change landed by the coordinator: fast-forward when
/// the commit's parent is the head, a merge otherwise.
pub fn import_git(repo: &mut Repo, actor: &mut Actor, branch: &str) -> Result<Json> {
    let work = repo.root.clone();
    let tip = git_out(
        &work,
        &[],
        &["rev-parse", &format!("refs/heads/{branch}")],
        None,
    )?;
    // The newest trunk revision with a commit is where we start.
    let chain = trunk_chain(repo)?;
    let mut known: Option<String> = None;
    for (id, r) in chain.iter() {
        if let Some(c) = commit_of(repo, id, r)? {
            known = Some(c);
        }
    }
    let range = match &known {
        Some(k) => format!("{k}..{tip}"),
        None => tip.clone(),
    };
    let listing = git_out(
        &work,
        &[],
        &["rev-list", "--first-parent", "--reverse", &range],
        None,
    )?;
    let commits: Vec<String> = listing
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let mut imported = Vec::new();
    let mut blob_cache: HashMap<String, (ObjectId, bool)> = HashMap::new();
    let mut rules_id: Option<ObjectId> = None;
    for commit in &commits {
        if revision_of_commit(repo, commit)?.is_some() {
            continue;
        }
        // The commit's snapshot is indexed against the trunk head it lands
        // on, so its units keep the identity trunk knows them by.
        let (head_id, _) = crate::verbs::trunk_head_of(repo)?;
        let head_rev: Revision = repo.store().get(&head_id)?;
        let (_, head_snap) = crate::verbs::root_snapshot_of(repo, &head_rev)?;
        let (index_id, index) = crate::semantic::index_for_revision(repo.store(), &head_rev)?;
        let parent = gitimport::Indexed {
            flat: tree::flatten(repo.store(), &head_snap.root)?,
            index_id,
            index,
        };
        repo.store().begin_batch();
        let content = gitimport::content_for_commit(
            repo.store(),
            &work,
            commit,
            &mut blob_cache,
            &mut rules_id,
            Some(&parent),
        );
        repo.store().end_batch()?;
        let (content, _) = content?;
        let rev = Revision {
            id: content.change.unwrap_or_else(EntityId::random),
            prev: None,
            snapshots: std::collections::BTreeMap::from([("".to_string(), content.snapshot)]),
            parents: vec![head_id],
            intent: content.intent.as_ref().map(|i| i.id),
            title: content.title.clone(),
            body: content.body.clone(),
            author: actor.principal(),
            time: content.time.unwrap_or_else(now),
            ops: None,
            flags: None,
        };
        let rev_id = repo.store().put(&rev)?;
        let mut effects = vec![
            tessra_core::object::Effect::Put { id: rev_id },
            tessra_core::object::Effect::Point {
                entity: rev.id,
                to: rev_id,
                from: None,
            },
        ];
        if let Some(intent) = &content.intent {
            let ioid = repo.store().put(intent)?;
            effects.push(tessra_core::object::Effect::Put { id: ioid });
            effects.push(tessra_core::object::Effect::Point {
                entity: intent.id,
                to: ioid,
                from: None,
            });
        }
        let op = build::build_op(
            &repo.log,
            &actor.signer,
            actor.cap,
            "import",
            build::args_with_idem(EntityId::random().0, std::collections::BTreeMap::new()),
            effects,
            now(),
        )?;
        repo.commit_op(&op)?;
        // Land it through the normal path, in a workspace of the owner's.
        let ws_id = EntityId::random();
        let path = crate::paths::workspaces_dir_for(&repo.repo_id).join(ws_id.to_letters());
        let snap: Snapshot = repo.store().get(&content.snapshot)?;
        crate::fs::materialize(repo.store(), &snap.root, &path, false)?;
        let ws = tessra_core::object::Workspace {
            id: ws_id,
            principal: actor.principal(),
            base: head_id,
            current: Some(rev_id),
            paths: std::collections::BTreeMap::from([("".to_string(), path.display().to_string())]),
            env: None,
            created: now(),
            expires: None,
        };
        repo.save_workspace(&ws)?;
        repo.workspaces.push(ws.clone());
        let landed = crate::verbs::land_revision(repo, actor, &ws, rev_id, &rev);
        let _ = std::fs::remove_dir_all(&path);
        repo.remove_workspace(&ws_id)?;
        match landed {
            Ok(o) => {
                let landing = o
                    .result
                    .get("revision")
                    .and_then(Json::as_str)
                    .and_then(|h| ObjectId::from_hex(h).ok())
                    .unwrap_or(rev_id);
                remember(repo, &landing, commit)?;
                imported.push(json!({ "commit": commit, "title": content.title, "landed": o.result.get("stage") == Some(&json!("landed")), "revision": landing.to_hex() }));
            }
            Err(e) => {
                imported.push(json!({ "commit": commit, "title": content.title, "landed": false, "why": e.to_string() }));
                break;
            }
        }
    }
    // The checkout itself moved with git; its workspace follows HEAD.
    let workspace = follow_checkout(repo, &work)?;
    Ok(
        json!({ "branch": branch, "tip": tip, "imported": imported.len(), "commits": imported, "workspace": workspace }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitOptions;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A git checkout on `main` with one commit, under Tessra.
    fn checkout_with_one_commit() -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        let repo = Repo::init(
            dir.path(),
            InitOptions {
                name: "t".into(),
                import_git: true,
                history: 1,
            },
        )
        .unwrap();
        (dir, repo)
    }

    fn root_at(repo: &Repo) -> ObjectId {
        let ws = repo.root_workspace().unwrap();
        ws.current.unwrap_or(ws.base)
    }

    fn root_at_on_disk(repo: &Repo) -> ObjectId {
        let ws = repo.root_workspace().unwrap();
        let bytes = std::fs::read(
            repo.tessra_dir
                .join("workspaces")
                .join(format!("{}.cbor", ws.id)),
        )
        .unwrap();
        let saved: tessra_core::object::Workspace = tessra_core::cbor::decode(&bytes).unwrap();
        saved.current.unwrap_or(saved.base)
    }

    #[test]
    fn import_moves_the_checkout_workspace_to_the_landed_revision() {
        let (dir, mut repo) = checkout_with_one_commit();
        let mut actor = repo.daemon_actor();
        let before = root_at(&repo);
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "two"]);
        let out = import_git(&mut repo, &mut actor, "main").unwrap();
        assert_eq!(out["imported"], json!(1), "{out}");
        assert_eq!(out["commits"][0]["landed"], json!(true), "{out}");
        assert_eq!(out["workspace"]["workspace"], json!("followed"), "{out}");
        let (head, _) = crate::verbs::trunk_head_of(&repo).unwrap();
        assert_ne!(head, before);
        assert_eq!(
            root_at(&repo),
            head,
            "the checkout's workspace sits on the landed revision"
        );
        assert_eq!(root_at_on_disk(&repo), head, "and it is persisted");
        // The owner's status describes the imported commit now.
        let status = crate::verbs::call(&mut repo, &mut actor, "status", &json!({}));
        assert_eq!(
            status["state"]["revision"],
            json!(head.to_hex()),
            "{status}"
        );
        assert_eq!(status["result"]["title"], json!("two"), "{status}");
        // Nothing new to import: the workspace is already there.
        let out = import_git(&mut repo, &mut actor, "main").unwrap();
        assert_eq!(out["imported"], json!(0), "{out}");
        assert_eq!(out["workspace"]["workspace"], json!("at HEAD"), "{out}");
    }

    #[test]
    fn import_leaves_a_checkout_workspace_with_unlanded_work_alone() {
        let (dir, mut repo) = checkout_with_one_commit();
        let mut actor = repo.daemon_actor();
        // The owner snapshots a draft in the checkout: unlanded work.
        std::fs::write(dir.path().join("c.txt"), "draft\n").unwrap();
        let out = crate::verbs::call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "draft" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let draft = root_at(&repo);
        // A commit arrives with git meanwhile.
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        git(dir.path(), &["add", "b.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "two"]);
        let out = import_git(&mut repo, &mut actor, "main").unwrap();
        assert_eq!(out["imported"], json!(1), "{out}");
        assert_eq!(out["workspace"]["workspace"], json!("left alone"), "{out}");
        assert_eq!(
            out["workspace"]["why"],
            json!("it carries unlanded work"),
            "{out}"
        );
        assert_eq!(
            root_at(&repo),
            draft,
            "the draft stays the workspace's revision"
        );
    }

    /// Land a change that writes `content` at `path`, from a workspace of the
    /// owner's own, so the checkout lags trunk. The workspace directory is
    /// returned to keep it alive.
    fn land_from_another_workspace(
        repo: &mut Repo,
        path: &str,
        content: &str,
    ) -> tempfile::TempDir {
        let wsdir = tempfile::tempdir().unwrap();
        let (head_id, _) = crate::verbs::trunk_head_of(repo).unwrap();
        let head_rev: Revision = repo.store().get(&head_id).unwrap();
        let snap: Snapshot = repo.store().get(&head_rev.snapshots[""]).unwrap();
        crate::fs::materialize(repo.store(), &snap.root, wsdir.path(), false).unwrap();
        let ws_id = EntityId::random();
        let owner = repo.daemon_actor();
        let ws = tessra_core::object::Workspace {
            id: ws_id,
            principal: owner.principal(),
            base: head_id,
            current: Some(head_id),
            paths: std::collections::BTreeMap::from([(
                "".to_string(),
                wsdir.path().display().to_string(),
            )]),
            env: None,
            created: now(),
            expires: None,
        };
        repo.save_workspace(&ws).unwrap();
        repo.workspaces.push(ws);
        let mut in_ws = repo.daemon_actor();
        in_ws.workspace = Some(ws_id);
        std::fs::write(wsdir.path().join(path), content).unwrap();
        let out = crate::verbs::call(repo, &mut in_ws, "snapshot", &json!({ "title": path }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = crate::verbs::call(repo, &mut in_ws, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        assert_eq!(out["result"]["stage"], json!("landed"), "{out}");
        wsdir
    }

    fn status(dir: &Path) -> String {
        git_out(
            dir,
            &[],
            &["status", "--porcelain", "--untracked-files=no"],
            None,
        )
        .unwrap()
    }

    #[test]
    fn export_moves_the_checkout_workspace_to_the_head_it_reset_to() {
        let (dir, mut repo) = checkout_with_one_commit();
        let before = root_at(&repo);
        // A change lands from a workspace of its own, so the checkout lags trunk.
        let _wsdir = land_from_another_workspace(&mut repo, "d.txt", "landed\n");
        assert_eq!(root_at(&repo), before, "the checkout lags trunk");
        let out = export_git(&mut repo, "main", None, None).unwrap();
        assert_eq!(out["created"], json!(1), "{out}");
        assert_eq!(out["checkout"], json!("updated"), "{out}");
        assert_eq!(out["workspace"]["workspace"], json!("followed"), "{out}");
        let (head, _) = crate::verbs::trunk_head_of(&repo).unwrap();
        assert_eq!(root_at(&repo), head);
        assert_eq!(root_at_on_disk(&repo), head);
        // The reset checked the landed file out; git may have given it CRLF.
        let d = std::fs::read_to_string(dir.path().join("d.txt")).unwrap();
        assert_eq!(d.replace("\r\n", "\n"), "landed\n");
    }

    #[test]
    fn an_imported_history_keeps_unit_identity_so_diff_shows_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(
            dir.path().join("notes.md"),
            "# notes\n\nsome prose\n\nmore prose\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn a() { 1 }\nfn b() { 2 }\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        std::fs::write(dir.path().join("lib.rs"), "fn a() { 1 }\nfn b() { 22 }\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "two"]);
        let mut repo = Repo::init(
            dir.path(),
            InitOptions {
                name: "t".into(),
                import_git: true,
                history: 10,
            },
        )
        .unwrap();
        let mut owner = repo.daemon_actor();
        let out = crate::verbs::call(
            &mut repo,
            &mut owner,
            "query",
            &json!({ "kind": "diff", "budget": 10_000 }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let units = out["result"]["units"].as_array().expect("units");
        let shown: Vec<(String, String, String)> = units
            .iter()
            .map(|u| {
                (
                    u["path"].as_str().unwrap_or("").to_string(),
                    u["name"].as_str().unwrap_or("").to_string(),
                    u["change"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![("lib.rs".to_string(), "b".to_string(), "changed".to_string())],
            "{out}"
        );
        assert_eq!(out["result"]["truncated"], json!(false), "{out}");
    }

    #[test]
    fn export_fast_forwards_a_dirty_checkout_whose_edits_do_not_overlap() {
        let (dir, mut repo) = checkout_with_one_commit();
        let _wsdir = land_from_another_workspace(&mut repo, "d.txt", "landed\n");
        // An unrelated, uncommitted edit in the checkout.
        std::fs::write(dir.path().join("a.txt"), "one\nlocal edit\n").unwrap();
        let out = export_git(&mut repo, "main", None, None).unwrap();
        assert_eq!(out["created"], json!(1), "{out}");
        assert_eq!(out["checkout"], json!("updated"), "{out}");
        assert_eq!(out["workspace"]["workspace"], json!("followed"), "{out}");
        // The branch moved, the exported file arrived, and the index follows
        // HEAD: the only difference git sees is the local edit.
        let head = git_out(dir.path(), &[], &["rev-parse", "HEAD"], None).unwrap();
        assert_eq!(json!(head), out["tip"], "{out}");
        let d = std::fs::read_to_string(dir.path().join("d.txt")).unwrap();
        assert_eq!(d.replace("\r\n", "\n"), "landed\n");
        assert_eq!(
            status(dir.path()),
            "M a.txt",
            "git_out trims the porcelain line"
        );
        let a = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(a.replace("\r\n", "\n"), "one\nlocal edit\n");
    }

    #[test]
    fn export_refuses_when_a_local_edit_overlaps_the_export_and_moves_nothing() {
        let (dir, mut repo) = checkout_with_one_commit();
        let _wsdir = land_from_another_workspace(&mut repo, "a.txt", "one\nlanded\n");
        std::fs::write(dir.path().join("a.txt"), "one\nlocal edit\n").unwrap();
        let before = git_out(dir.path(), &[], &["rev-parse", "HEAD"], None).unwrap();
        let err = export_git(&mut repo, "main", None, None).unwrap_err();
        assert_eq!(err.code(), "EXPORT");
        assert!(err.to_string().contains("stash"), "{err}");
        // Nothing moved: not the branch, not the index, not the working tree.
        let after = git_out(dir.path(), &[], &["rev-parse", "HEAD"], None).unwrap();
        assert_eq!(after, before);
        assert_eq!(
            status(dir.path()),
            "M a.txt",
            "git_out trims the porcelain line"
        );
        let a = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(a.replace("\r\n", "\n"), "one\nlocal edit\n");
        // After the edit is stashed, the same export goes through and the
        // commits made the first time are reused.
        git(dir.path(), &["stash", "-q"]);
        let out = export_git(&mut repo, "main", None, None).unwrap();
        assert_eq!(out["created"], json!(0), "{out}");
        assert_eq!(out["checkout"], json!("updated"), "{out}");
        let a = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(a.replace("\r\n", "\n"), "one\nlanded\n");
    }
}
