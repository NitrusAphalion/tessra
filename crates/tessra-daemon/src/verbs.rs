//! The thirteen verbs, each returning the response shape in VERBS.md.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use ciborium::value::Value as Cbor;
use serde_json::{json, Value as Json};
use tessra_core::cbor;
use tessra_core::object::{
    Claim, ClaimTarget, Effect, Memory, MemoryScope, Revision, Snapshot, TrackingRules, Workspace,
    MEMORY_KINDS,
};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build::{self, point_effect};
use tessra_oplog::standard;
use tessra_oplog::verify::landed_revisions;
use tessra_oplog::view::{line_head, Pointer, ViewState};

use crate::principals::Actor;
use crate::tree::{self, Flat};
use crate::{fs, now, paths, Error, Repo, Result};

/// Verb dispatch by name.
pub fn call(repo: &mut Repo, actor: &mut Actor, verb: &str, args: &Json) -> Json {
    let limit = args.get("budget").and_then(Json::as_u64).unwrap_or(4000);
    repo.store().begin_batch();
    let out = call_inner(repo, actor, verb, args, limit);
    if let Err(e) = repo.store().end_batch() {
        return json!({ "ok": false, "code": "STORE", "message": e.to_string() });
    }
    out
}

fn call_inner(repo: &mut Repo, actor: &mut Actor, verb: &str, args: &Json, limit: u64) -> Json {
    // A throttled agent's mutations wait out the cooldown.
    const MUTATIONS: &[&str] = &[
        "edit",
        "snapshot",
        "promote",
        "claim",
        "remember",
        "try",
        "revert",
        "plan",
        "attest",
        "workspace",
    ];
    if actor.kind == "session" && MUTATIONS.contains(&verb) {
        if let Err(e) = crate::anomaly::check(repo, &actor.principal()) {
            // Trying again while throttled is itself a signal.
            let more = crate::anomaly::record(
                repo,
                &actor.principal(),
                &format!("{verb} while throttled"),
                1,
            )
            .unwrap_or(None)
            .unwrap_or(Json::Null);
            let state = state(repo, actor).unwrap_or_else(|e| json!({ "error": e.to_string() }));
            if more.get("revoked").and_then(Json::as_bool).unwrap_or(false) {
                return json!({ "ok": false, "code": "REVOKED", "message": "revoked by the risk monitor after repeated anomalies; unlanded work unwound", "anomaly": more, "state": state });
            }
            return json!({ "ok": false, "code": e.code(), "message": e.to_string(), "anomaly": more, "state": state });
        }
    }
    let outcome = match verb {
        "status" => status(repo, actor, args),
        "context" => context(repo, actor, args),
        "query" => query(repo, actor, args),
        "workspace" => workspace(repo, actor, args),
        "edit" => edit(repo, actor, args),
        "snapshot" => snapshot(repo, actor, args),
        "claim" => claim(repo, actor, args),
        "remember" => remember(repo, actor, args),
        "verify" => verify(repo, actor, args),
        "try" => try_verb(repo, actor, args),
        "approve" => approve_verb(repo, actor, args),
        "channel" => channel_verb(repo, actor, args),
        "promote" => promote(repo, actor, args),
        "revert" => revert(repo, actor, args),
        "undo" => undo(repo, actor, args),
        "standard" => standard_verb(repo, actor, args),
        "attest" => attest_verb(repo, actor, args),
        "hook" => hook_verb(repo, actor, args),
        "grant" => grant_verb(repo, actor, args),
        "plan" => plan_verb(repo, actor, args),
        "revoke" => revoke_verb(repo, actor, args),
        "config" => config_verb(repo, actor, args),
        "import" => import_verb(repo, actor, args),
        "target" => target_verb(repo, actor, args),
        "release" => release_verb(repo, actor, args),
        "observe" => observe_verb(repo, actor, args),
        "export" => export(repo, actor, args),
        "bench" => bench(repo, actor, args),
        other => Err(Error::verb(
            "UNKNOWN_VERB",
            format!("no verb named {other}"),
        )),
    };
    let state = state(repo, actor).unwrap_or_else(|e| json!({ "error": e.to_string() }));
    envelope(state, outcome, limit)
}

/// The response shape of VERBS.md around what a verb returned.
fn envelope(state: Json, outcome: Result<Outcome>, limit: u64) -> Json {
    match outcome {
        Ok(Outcome { result, next }) => {
            // `used` is what the verb produced against the budget it was
            // given; `total` is the whole answer, with the state echo and
            // the suggestions every response carries.
            let used = (json_len(&result) / 4) as u64;
            let mut envelope = json!({
                "ok": true,
                "result": result,
                "state": state,
                "next": next,
                "budget": { "used": used, "limit": limit, "total": 0 }
            });
            envelope["budget"]["total"] = json!((json_len(&envelope) / 4) as u64);
            envelope
        }
        Err(e) => {
            let (code, message, fix, unmet) = match &e {
                Error::Verb { code, message } => {
                    (*code, message.clone(), fix_for(code), Json::Null)
                }
                Error::StandardUnmet { clauses, .. } => (
                    "STANDARD_UNMET",
                    e.to_string(),
                    Some("verify".to_string()),
                    json!(clauses
                        .iter()
                        .map(|(c, r)| json!({ "clause": c, "reason": r, "fix": fix_for_clause(c, r) }))
                        .collect::<Vec<_>>()),
                ),
                Error::OpLog(tessra_oplog::Error::Rejected { step, name, reason }) => (
                    "REJECTED",
                    format!("step {step} ({name}): {reason}"),
                    Some("status".to_string()),
                    Json::Null,
                ),
                Error::OpLog(tessra_oplog::Error::Other(m)) => {
                    ("REFUSED", m.clone(), Some("status".to_string()), Json::Null)
                }
                other => (other.code(), other.to_string(), None, Json::Null),
            };
            let mut err = json!({
                "ok": false,
                "code": code,
                "message": message,
                "state": state,
            });
            if let Error::StandardUnmet { stage, .. } = &e {
                err["stage"] = json!(stage);
            }
            if let Some(f) = fix {
                err["fix"] = json!(f);
            }
            if !unmet.is_null() {
                err["unmet"] = unmet;
            }
            err
        }
    }
}

fn fix_for(code: &str) -> Option<String> {
    match code {
        "NO_WORKSPACE" => Some("workspace --action create".into()),
        "STANDARD_UNMET" => Some("verify".into()),
        "CREDENTIAL_REQUIRED" => Some("the same call with --credential".into()),
        "NOT_YET" => None,
        _ => None,
    }
}

/// What satisfies one unmet clause: the command its reason names when it
/// names one, a verify for an attestation the daemon can produce itself,
/// the exception queue for a judge's verdict, and otherwise the reason.
fn fix_for_clause(clause: &str, reason: &str) -> String {
    if let Some(i) = reason.find("tessra ") {
        return reason[i..].trim_end_matches('.').to_string();
    }
    if clause.contains("attest(") {
        return "verify".into();
    }
    if clause.contains("judge(") {
        return "query --kind exceptions".into();
    }
    reason.to_string()
}

/// Cut a list to a character budget, dropping from the end: how many were
/// dropped.
fn cut_list(list: &mut Vec<Json>, chars: usize) -> usize {
    let total = list.len();
    let mut used = 0usize;
    let mut keep = 0usize;
    for j in list.iter() {
        let cost = json_len(j) + 1;
        if used + cost > chars {
            break;
        }
        used += cost;
        keep += 1;
    }
    list.truncate(keep);
    total - keep
}

/// Acting as the owner takes the owner credential besides the daemon
/// token, so a process that can reach the daemon is not thereby the owner.
/// Sessions pass through untouched, and a human or an external presented
/// their own credential when they were opened.
fn require_owner_credential(repo: &Repo, actor: &Actor, what: &str) -> Result<()> {
    if actor.kind == "daemon" && !actor.credentialed {
        return Err(Error::verb(
            "CREDENTIAL_REQUIRED",
            format!(
                "{what} needs the owner credential: pass --credential, set TESSRA_CREDENTIAL, or answer the prompt; init printed it, and it stays in {} until you move it somewhere agents cannot read",
                repo.owner_credential_path().display()
            ),
        ));
    }
    Ok(())
}

pub struct Outcome {
    pub result: Json,
    pub next: Vec<String>,
}

fn ok(result: Json, next: &[&str]) -> Result<Outcome> {
    Ok(Outcome {
        result,
        next: next.iter().map(|s| s.to_string()).collect(),
    })
}

// ------------------------------------------------------------------ helpers

fn idem_from(args: &Json) -> [u8; 16] {
    if let Some(s) = args.get("idem").and_then(Json::as_str) {
        let mut k = [0u8; 16];
        let h = blake3::hash(s.as_bytes());
        k.copy_from_slice(&h.as_bytes()[..16]);
        return k;
    }
    EntityId::random().0
}

fn arg_str<'a>(args: &'a Json, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Json::as_str)
}

fn actor_workspace(repo: &Repo, actor: &Actor) -> Option<Workspace> {
    match actor.workspace {
        Some(id) => repo.workspace(&id).cloned(),
        None => {
            if actor.kind == "daemon" {
                repo.root_workspace().cloned()
            } else {
                repo.workspaces
                    .iter()
                    .find(|w| w.principal == actor.principal())
                    .cloned()
            }
        }
    }
}

/// A workspace is materialized into a directory of its own: what the tree
/// does not hold is removed from it. An existing directory with contents is
/// refused, since a checkout's ignored files (dependencies, build output,
/// local environment) would be the first to go, and so is any path inside
/// the repository, which is the owner's checkout.
fn refuse_unless_fresh(repo: &Repo, path: &std::path::Path) -> Result<()> {
    let shown = path.display();
    let inside = |p: &std::path::Path| {
        std::fs::canonicalize(p)
            .map(|c| c.starts_with(&repo.root))
            .unwrap_or(false)
    };
    // The path itself, or the nearest ancestor that exists, decides whether
    // the workspace would land inside the checkout.
    let mut probe = Some(path);
    while let Some(p) = probe {
        if p.exists() {
            if inside(p) {
                return Err(Error::verb(
                    "ARGS",
                    format!(
                        "workspace path {shown} is inside the repository at {}; a workspace is a directory of its own outside the checkout, or omit --path for one under the local data directory",
                        repo.root.display()
                    ),
                ));
            }
            break;
        }
        probe = p.parent();
    }
    if path.is_file() {
        return Err(Error::verb(
            "ARGS",
            format!("workspace path {shown} is a file"),
        ));
    }
    if path.is_dir() && std::fs::read_dir(path)?.next().is_some() {
        return Err(Error::verb(
            "ARGS",
            format!(
                "workspace path {shown} exists and is not empty; a workspace is materialized into a new or empty directory, since files the revision does not hold are removed from it"
            ),
        ));
    }
    Ok(())
}

fn require_workspace(repo: &Repo, actor: &Actor) -> Result<Workspace> {
    actor_workspace(repo, actor)
        .ok_or_else(|| Error::verb("NO_WORKSPACE", "you have no workspace yet"))
}

fn ws_path(ws: &Workspace) -> PathBuf {
    PathBuf::from(ws.paths.get("").cloned().unwrap_or_default())
}

fn current_revision(repo: &Repo, ws: &Workspace) -> Result<(ObjectId, Revision)> {
    let id = ws.current.unwrap_or(ws.base);
    Ok((id, repo.store().get(&id)?))
}

fn root_snapshot(repo: &Repo, rev: &Revision) -> Result<(ObjectId, Snapshot)> {
    let id = *rev
        .snapshots
        .get("")
        .ok_or_else(|| Error::verb("ROOT", "revision has no root snapshot"))?;
    Ok((id, repo.store().get(&id)?))
}

/// The tree of the revision a workspace's change is built on.
fn base_flat_of(repo: &Repo, ws: &Workspace) -> Result<Flat> {
    let rev: Revision = repo.store().get(&ws.base)?;
    let (_, snap) = root_snapshot(repo, &rev)?;
    tree::flatten(repo.store(), &snap.root)
}

pub fn trunk_head_of(repo: &Repo) -> Result<(ObjectId, u64)> {
    trunk_head(repo)
}

pub fn trunk_standard_of(repo: &Repo) -> Result<EntityId> {
    trunk_standard(repo)
}

pub fn root_snapshot_of(repo: &Repo, rev: &Revision) -> Result<(ObjectId, Snapshot)> {
    root_snapshot(repo, rev)
}

fn trunk_head(repo: &Repo) -> Result<(ObjectId, u64)> {
    let view = repo.log.current_view()?;
    let ls = view
        .lines
        .get("trunk")
        .ok_or_else(|| Error::verb("LINE", "no trunk line"))?;
    let head = line_head(ls)
        .ok_or_else(|| Error::verb("LINE", "trunk has no head"))?
        .single()
        .map_err(|_| {
            Error::verb(
                "CONFLICT",
                "trunk head is in conflict; a landing must resolve it",
            )
        })?;
    Ok((head, ls.seq))
}

fn trunk_standard(repo: &Repo) -> Result<EntityId> {
    let view = repo.log.current_view()?;
    let ls = view
        .lines
        .get("trunk")
        .ok_or_else(|| Error::verb("LINE", "no trunk line"))?;
    let vs = ViewState::new(repo.store());
    let line: tessra_core::object::Line = match vs.entity(&view, &ls.id)? {
        Some(p) => repo.store().get(&p.single()?)?,
        None => return Err(Error::verb("LINE", "trunk entity missing")),
    };
    Ok(line.standard)
}

fn stage_of(repo: &Repo, rev_id: &ObjectId) -> Result<&'static str> {
    let view = repo.log.current_view()?;
    let landed = landed_revisions(repo.store(), &view)?;
    if landed.contains(rev_id) {
        if let Some(s) = crate::delivery::delivery_stage(repo, rev_id)? {
            return Ok(s);
        }
        return Ok("landed");
    }
    let vs = ViewState::new(repo.store());
    if vs.in_set(&view.proposed, rev_id)? {
        return Ok("proposed");
    }
    Ok("snapshot")
}

/// The trunk standard against a revision with every attestation that could
/// apply to it: met count, unmet clauses, and the attestations considered.
fn standard_status(
    repo: &Repo,
    rev_id: &ObjectId,
    rev: &Revision,
) -> Result<(usize, Vec<standard::Unmet>, Vec<ObjectId>)> {
    // Between ops the answer cannot change: the standard, the attestations,
    // and the revision are all reached through the op log.
    let mut heads: Vec<ObjectId> = repo.log.heads().into_iter().collect();
    heads.sort();
    {
        let memo = repo.standard_memo.borrow();
        if memo.heads == heads {
            if let Some(v) = memo.by_rev.get(rev_id) {
                return Ok(v.clone());
            }
        }
    }
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let std_id = trunk_standard(repo)?;
    let idx = match rev.snapshots.get("") {
        Some(s) => Some(crate::semantic::index_for_snapshot(repo.store(), s)?.1),
        None => None,
    };
    let attests = crate::verifiers::collect_for(repo, rev_id, rev, idx.as_ref())?;
    let unmet = standard::evaluate(repo.store(), &vs, &view, &std_id, rev_id, rev, &attests)?;
    let total = standard::chain(repo.store(), &vs, &view, &std_id)?
        .iter()
        .map(|s| s.clauses.len())
        .sum::<usize>();
    let answer = (total.saturating_sub(unmet.len()), unmet, attests);
    {
        let mut memo = repo.standard_memo.borrow_mut();
        if memo.heads != heads {
            memo.heads = heads;
            memo.by_rev.clear();
        }
        memo.by_rev.insert(*rev_id, answer.clone());
    }
    Ok(answer)
}

fn actor_claims(repo: &Repo, actor: &Actor) -> Result<Vec<(EntityId, Claim)>> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let t = tessra_core::trie::Trie::new(repo.store());
    let mut out = Vec::new();
    for (k, v) in t.entries(&view.claims)? {
        if let (Ok(id), Some(Pointer::Id(oid))) = (EntityId::from_slice(&k), Pointer::from_trie(&v))
        {
            let c: Claim = repo.store().get(&oid)?;
            if c.principal == actor.principal() && c.expires > now() {
                out.push((id, c));
            }
        }
    }
    let _ = vs;
    Ok(out)
}

/// The state echo.
pub fn state(repo: &Repo, actor: &Actor) -> Result<Json> {
    let heads = repo.log.heads();
    let mut s = json!({
        "principal": actor.principal().to_letters(),
        "kind": actor.kind,
        "cursor": heads.first().map(|h| h.to_hex()),
        "heads": heads.iter().map(|h| h.to_hex()).collect::<Vec<_>>(),
    });
    if let Ok((head, seq)) = trunk_head(repo) {
        s["trunk"] = json!({ "head": head.to_hex(), "seq": seq });
    }
    if repo.verifying.load(std::sync::atomic::Ordering::SeqCst) {
        s["verifying"] = json!(true);
    }
    if let Some(ws) = actor_workspace(repo, actor) {
        let (rev_id, rev) = current_revision(repo, &ws)?;
        let (snap_id, _) = root_snapshot(repo, &rev)?;
        let (met, unmet, _) = standard_status(repo, &rev_id, &rev)?;
        s["workspace"] = json!(ws.id.to_letters());
        s["path"] = json!(display_path(&ws_path(&ws)));
        s["snapshot"] = json!(snap_id.to_hex());
        s["revision"] = json!(rev_id.to_hex());
        s["change"] = json!(rev.id.to_letters());
        s["stage"] = json!(stage_of(repo, &rev_id)?);
        s["standard"] = json!({ "met": met, "unmet": unmet.len() });
    }
    let claims: Vec<Json> = actor_claims(repo, actor)?
        .into_iter()
        .map(|(id, c)| json!({ "id": id.to_letters(), "targets": c.targets.iter().map(|t| cbor_to_json(&t.r#ref)).collect::<Vec<_>>() }))
        .collect();
    s["claims"] = json!(claims);
    Ok(s)
}

/// CBOR to JSON for rendering objects: bytes become hex.
pub fn cbor_to_json(v: &Cbor) -> Json {
    match v {
        Cbor::Integer(i) => {
            let n: i128 = (*i).into();
            json!(n as i64)
        }
        Cbor::Bytes(b) => json!(hex::encode(b)),
        Cbor::Text(t) => json!(t),
        Cbor::Bool(b) => json!(b),
        Cbor::Null => Json::Null,
        Cbor::Array(a) => Json::Array(a.iter().map(cbor_to_json).collect()),
        Cbor::Map(m) => {
            let mut o = serde_json::Map::new();
            for (k, val) in m {
                let key = match k {
                    Cbor::Text(t) => t.clone(),
                    Cbor::Bytes(b) => hex::encode(b),
                    other => format!("{other:?}"),
                };
                o.insert(key, cbor_to_json(val));
            }
            Json::Object(o)
        }
        other => json!(format!("{other:?}")),
    }
}

/// One object as JSON. A blob is paged: `chars` of its text from `offset`,
/// or its last `chars` when `tail` is set, with `next_offset` when more
/// follows, so a verifier's evidence can be read to its end within a budget.
fn object_json(
    repo: &Repo,
    id: &ObjectId,
    offset: usize,
    tail: bool,
    chars: usize,
) -> Result<Json> {
    let bytes = repo
        .get_bytes(id)?
        .ok_or(tessra_core::Error::NotFound(*id))?;
    let tag = cbor::peek_tag(&bytes).unwrap_or_else(|_| "blob".into());
    if tag == "blob" && cbor::peek_tag(&bytes).is_err() {
        let size = bytes.len();
        let start = if tail {
            size.saturating_sub(chars)
        } else {
            offset.min(size)
        };
        let end = start.saturating_add(chars).min(size);
        let window = String::from_utf8_lossy(&bytes[start..end]);
        let (text, from, to) = if tail {
            let s = fit_suffix(&window, chars);
            (&window[s..], start + s, end)
        } else {
            let t = fit_prefix(&window, chars);
            (&window[..t], start, start + t)
        };
        let mut out = json!({ "t": "blob", "size": size, "offset": from, "text": text });
        if to < size {
            out["next_offset"] = json!(to);
        }
        return Ok(out);
    }
    let v: Cbor =
        ciborium::de::from_reader(&bytes[..]).map_err(|e| Error::verb("DECODE", e.to_string()))?;
    Ok(cbor_to_json(&v))
}

fn memory_json(id: &EntityId, m: &Memory) -> Json {
    json!({
        "id": id.to_letters(),
        "kind": m.kind,
        "scope": { "kind": m.scope.kind, "ref": cbor_to_json(&m.scope.r#ref) },
        "body": m.body,
        "confidence": m.confidence as f64 / 1000.0,
        "author": m.author.to_letters(),
        "status": m.status,
        "proposed": m.proposed.unwrap_or(false),
        "visibility": m.visibility,
        "time": m.time,
    })
}

/// All current memories visible to the actor, most relevant first for a path.
fn memories(
    repo: &Repo,
    actor: &Actor,
    path: Option<&str>,
    kinds: Option<&[String]>,
) -> Result<Vec<(EntityId, Memory)>> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let mut out = Vec::new();
    for (id, ptr) in vs.entities(&view)? {
        let oid = match ptr {
            Pointer::Id(o) => o,
            Pointer::Conflict(v) => v[0],
        };
        let Some(bytes) = repo.get_bytes(&oid)? else {
            continue;
        };
        if cbor::peek_tag(&bytes).ok().as_deref() != Some("memory") {
            continue;
        }
        let m: Memory = cbor::decode(&bytes)?;
        if m.status == "retired" {
            continue;
        }
        if m.visibility == "private" && m.author != actor.principal() {
            continue;
        }
        if let Some(ks) = kinds {
            if !ks.is_empty() && !ks.contains(&m.kind) {
                continue;
            }
        }
        let relevant = match (path, m.scope.kind.as_str(), &m.scope.r#ref) {
            (None, _, _) => true,
            (Some(_), "repo" | "root", _) => true,
            (Some(p), "path", Cbor::Text(r)) => p.starts_with(r.as_str()) || r.starts_with(p),
            (Some(_), _, _) => false,
        };
        if relevant {
            out.push((id, m));
        }
    }
    out.sort_by(|a, b| {
        let pa = a.1.proposed.unwrap_or(false);
        let pb = b.1.proposed.unwrap_or(false);
        pa.cmp(&pb)
            .then(b.1.confidence.cmp(&a.1.confidence))
            .then(b.1.time.cmp(&a.1.time))
    });
    Ok(out)
}

/// The ops since a cursor, newest first, each saying what it did, cut to a
/// character budget: the entries, how many more there were, and the newest
/// op, which is the cursor to pass next time.
fn ops_since(
    repo: &Repo,
    cursor: Option<&str>,
    chars: usize,
) -> Result<(Vec<Json>, usize, Option<String>)> {
    let heads = repo.log.heads();
    let mut stop: HashSet<ObjectId> = HashSet::new();
    if let Some(c) = cursor {
        if let Ok(id) = ObjectId::from_hex(c) {
            stop = repo.log.ancestors(&id)?;
        }
    }
    let mut seen = HashSet::new();
    let mut queue: VecDeque<ObjectId> = heads.into_iter().collect();
    let mut ops: Vec<(i64, ObjectId)> = Vec::new();
    while let Some(id) = queue.pop_front() {
        if stop.contains(&id) || !seen.insert(id) {
            continue;
        }
        let op = repo.log.get_op(&id)?;
        ops.push((op.time, id));
        queue.extend(op.parents.iter().copied());
    }
    ops.sort_by_key(|o| std::cmp::Reverse(o.0));
    let newest = ops.first().map(|(_, id)| id.to_hex());
    let total = ops.len();
    let mut names: HashMap<EntityId, String> = HashMap::new();
    let mut out = Vec::new();
    let mut left = chars;
    for (t, id) in ops {
        let op = repo.log.get_op(&id)?;
        let author = names
            .entry(op.author)
            .or_insert_with(|| {
                repo.principal_name(&op.author)
                    .unwrap_or_else(|| op.author.to_letters())
            })
            .clone();
        let entry = json!({
            "op": id.to_hex(), "kind": op.kind, "author": author, "time": t,
            "what": op_summary(repo, &op),
        });
        let cost = json_len(&entry) + 1;
        if cost > left {
            break;
        }
        left -= cost;
        out.push(entry);
    }
    let omitted = total - out.len();
    Ok((out, omitted, newest))
}

/// One line on what an op did, read from its effects.
fn op_summary(repo: &Repo, op: &tessra_core::object::Op) -> String {
    let title_of = |id: &ObjectId| -> String {
        repo.store()
            .get::<Revision>(id)
            .map(|r| r.title)
            .unwrap_or_default()
    };
    let mut parts: Vec<String> = Vec::new();
    for e in &op.effects {
        let part = match e {
            Effect::Point { to, .. } => object_summary(repo, to),
            Effect::Head { line, to, seq, .. } => {
                Some(format!("landed {line} #{seq}: {}", title_of(to)))
            }
            Effect::Propose { rev } => Some(format!("proposed: {}", title_of(rev))),
            Effect::Revoke { principal } => Some(format!(
                "revoked {}",
                repo.principal_name(principal)
                    .unwrap_or_else(|| principal.to_letters())
            )),
            Effect::Put { id } if op.kind == "attest" => repo
                .store()
                .get::<tessra_core::object::Attestation>(id)
                .ok()
                .map(|a| format!("{} = {}", a.kind, cbor_to_json(&a.result))),
            _ => None,
        };
        if let Some(p) = part {
            if !parts.contains(&p) {
                parts.push(p);
            }
        }
    }
    let mut s = parts.join("; ");
    if s.len() > 200 {
        let cut = fit_prefix(&s, 200);
        s.truncate(cut);
        s.push_str("...");
    }
    s
}

/// A short line naming the object a pointer moved to.
fn object_summary(repo: &Repo, id: &ObjectId) -> Option<String> {
    let bytes = repo.get_bytes(id).ok().flatten()?;
    match cbor::peek_tag(&bytes).ok()?.as_str() {
        "revision" => {
            let r = cbor::decode::<Revision>(&bytes).ok()?;
            Some(format!("{} (change {})", r.title, r.id.to_letters()))
        }
        "memory" => {
            let m = cbor::decode::<Memory>(&bytes).ok()?;
            let cut = fit_prefix(&m.body, 80);
            let more = if cut < m.body.len() { "..." } else { "" };
            Some(format!("{}: {}{more}", m.kind, &m.body[..cut]))
        }
        "standard" => Some("the standard".into()),
        "principal" => {
            let p = cbor::decode::<tessra_core::object::Principal>(&bytes).ok()?;
            Some(format!("{} {}", p.kind, p.name))
        }
        "channel" => Some("a channel".into()),
        "hook" => Some("a hook".into()),
        "target" => Some("a target".into()),
        "intent" => Some("an intent".into()),
        _ => None,
    }
}

/// A path for a response: without the verbatim prefix Windows puts on a
/// canonical path, which no tool wants to see.
fn display_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

// ------------------------------------------------------------------ verbs

fn status(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let budget = args.get("budget").and_then(Json::as_u64).unwrap_or(4000) as usize;
    // What happened gets half the budget, newest first; the rest of the
    // budget is the agent's own situation.
    let (since, omitted, cursor) = ops_since(repo, arg_str(args, "since"), budget * 2)?;
    let mut result = json!({ "since": since, "since_omitted": omitted, "cursor": cursor });
    if let Some(ws) = actor_workspace(repo, actor) {
        let (rev_id, rev) = current_revision(repo, &ws)?;
        let (_, unmet, _) = standard_status(repo, &rev_id, &rev)?;
        result["title"] = json!(rev.title);
        result["intent"] = json!(rev.intent.map(|i| i.to_letters()));
        result["flags"] = json!(rev.flags.as_ref().map(flags_json));
        result["unmet"] = json!(unmet
            .iter()
            .map(|u| json!({ "clause": u.clause, "reason": u.reason }))
            .collect::<Vec<_>>());
        let open_questions: Vec<Json> =
            memories(repo, actor, None, Some(&["question".to_string()]))?
                .into_iter()
                .filter(|(_, m)| m.status == "active")
                .map(|(id, m)| memory_json(&id, &m))
                .collect();
        result["questions"] = json!(open_questions);
        if let Some(agent) = repo.session_agent(&actor.principal()) {
            if let Some(a) = crate::swarm::load_assignment(repo, &agent) {
                result["assignment"] = json!({ "intent": a.intent.to_letters(), "title": a.title, "units": a.units, "paths": a.paths, "ops_budget": a.ops });
            }
        }
        return ok(
            result,
            &["context --path <what you will touch>", "snapshot"],
        );
    }
    if let Some(agent) = repo.session_agent(&actor.principal()) {
        if let Some(a) = crate::swarm::load_assignment(repo, &agent) {
            result["assignment"] = json!({ "intent": a.intent.to_letters(), "title": a.title, "units": a.units, "paths": a.paths, "ops_budget": a.ops });
            return ok(
                result,
                &["workspace --action create", "claim --paths <your paths>"],
            );
        }
    }
    ok(result, &["workspace --action create"])
}

fn flags_json(f: &tessra_core::object::Flags) -> Json {
    json!({
        "out_of_scope": f.out_of_scope,
        "secrets": f.secrets.as_ref().map(|v| v.iter().map(|m| json!({ "path": m.path, "pattern": m.pattern })).collect::<Vec<_>>()),
        "case_collisions": f.case_collisions,
        "unportable_names": f.unportable_names,
        "generated_drift": f.generated_drift,
    })
}

fn context(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let budget = args.get("budget").and_then(Json::as_u64).unwrap_or(4000) as usize;
    let path = arg_str(args, "path");
    if let Some(unit) = arg_str(args, "unit") {
        return unit_pack(repo, actor, unit, path, budget);
    }
    // Every part of the pack is charged by its serialized size, so the
    // envelope's `used` never exceeds the limit. The envelope goes first.
    let mut chars_left = (budget * 4).saturating_sub(120);
    let mut truncated = false;
    // The path's units are its map, so they are computed before the content
    // and the content leaves room for them: up to a quarter of the budget.
    // Reading needs no workspace: without one, the pack describes trunk,
    // read from the store instead of a directory.
    let ws = actor_workspace(repo, actor);
    let (_, rev) = match &ws {
        Some(w) => current_revision(repo, w)?,
        None => {
            let (head, _) = trunk_head(repo)?;
            (head, repo.store().get::<Revision>(&head)?)
        }
    };
    let (snap_id, snap) = root_snapshot(repo, &rev)?;
    let units_all: Vec<Json> = match path {
        Some(p) => {
            let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
            let by_nid: HashMap<EntityId, &tessra_core::object::Node> =
                idx.nodes.iter().map(|n| (n.nid, n)).collect();
            let covering = crate::semantic::covering_tests(&idx);
            let label = |n: &tessra_core::object::Node| {
                if n.path == p {
                    n.name.clone()
                } else {
                    format!("{}:{}", n.path, n.name)
                }
            };
            crate::semantic::nodes_for_path(&idx, p)
                .iter()
                .map(|n| {
                    let deps: Vec<String> = n
                        .deps
                        .iter()
                        .flatten()
                        .filter_map(|d| by_nid.get(d).map(|x| label(x)))
                        .collect();
                    let tests: Vec<String> = covering
                        .get(&n.nid)
                        .map(|ts| ts.iter().map(|t| label(t)).collect())
                        .unwrap_or_default();
                    json!({
                        "nid": n.nid.to_letters(), "kind": n.kind, "name": n.name,
                        "span": [n.span.0, n.span.1], "parent": n.parent.map(|x| x.to_letters()),
                        "deps": deps, "tests": tests,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    };
    let reserve = units_all
        .iter()
        .map(|u| json_len(u) + 1)
        .sum::<usize>()
        .min(budget);
    let mut files = Vec::new();
    if let Some(p) = path {
        // What is at the path: a directory's names or a file's bytes, from
        // the workspace when there is one and from the revision's tree
        // when there is not.
        enum At {
            Dir(Vec<String>),
            File(Vec<u8>),
            Missing,
        }
        let at = match &ws {
            Some(w) => {
                let full = ws_path(w).join(p);
                if full.is_dir() {
                    At::Dir(
                        std::fs::read_dir(&full)?
                            .filter_map(|e| e.ok())
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .filter(|n| n != ".git" && n != ".tessra")
                            .collect(),
                    )
                } else if full.is_file() {
                    At::File(std::fs::read(&full)?)
                } else {
                    At::Missing
                }
            }
            None => {
                let flat = tree::flatten(repo.store(), &snap.root)?;
                let key = p.trim_matches('/');
                match flat.get(key).and_then(|leaf| leaf.r#ref) {
                    Some(blob) => match repo.get_bytes(&blob)? {
                        Some(b) => At::File(b),
                        None => At::Missing,
                    },
                    None => {
                        let prefix = if key.is_empty() {
                            String::new()
                        } else {
                            format!("{key}/")
                        };
                        let mut names: Vec<String> = flat
                            .keys()
                            .filter_map(|k| k.strip_prefix(prefix.as_str()))
                            .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
                            .collect();
                        names.sort();
                        names.dedup();
                        if names.is_empty() {
                            At::Missing
                        } else {
                            At::Dir(names)
                        }
                    }
                }
            }
        };
        match at {
            At::Dir(mut names) => {
                names.sort();
                let total = names.len();
                let mut room = chars_left.saturating_sub(64 + p.len() + reserve);
                let mut kept = Vec::new();
                for n in names {
                    let cost = n.len() + 3;
                    if cost > room {
                        break;
                    }
                    room -= cost;
                    kept.push(n);
                }
                let cut = kept.len() < total;
                truncated |= cut;
                let entry = json!({ "path": p, "dir": kept, "truncated": cut, "entries": total });
                chars_left = chars_left.saturating_sub(json_len(&entry) + 1);
                files.push(entry);
            }
            At::File(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let room = chars_left.saturating_sub(96 + p.len() + reserve);
                let take = fit_prefix(&text, room);
                let cut = take < text.len();
                truncated |= cut;
                let entry = json!({ "path": p, "content": &text[..take], "truncated": cut, "size": bytes.len() });
                chars_left = chars_left.saturating_sub(json_len(&entry) + 1);
                files.push(entry);
            }
            At::Missing => {
                let entry = json!({ "path": p, "missing": true });
                chars_left = chars_left.saturating_sub(json_len(&entry) + 1);
                files.push(entry);
            }
        }
    }
    // Then the units, memories, and claims, each list cut where the budget
    // runs out, in that order.
    let mut units = Vec::new();
    let units_total = units_all.len();
    for u in units_all {
        let cost = json_len(&u) + 1;
        if cost > chars_left {
            break;
        }
        chars_left -= cost;
        units.push(u);
    }
    let mut mems = Vec::new();
    let mems_all = memories(repo, actor, path, None)?;
    let mems_total = mems_all.len();
    for (id, m) in mems_all {
        let j = memory_json(&id, &m);
        let cost = json_len(&j) + 1;
        if cost > chars_left {
            break;
        }
        chars_left -= cost;
        mems.push(j);
    }
    let claims_all: Vec<Json> = {
        let view = repo.log.current_view()?;
        let t = tessra_core::trie::Trie::new(repo.store());
        let mut out = Vec::new();
        for (_, v) in t.entries(&view.claims)? {
            if let Some(Pointer::Id(oid)) = Pointer::from_trie(&v) {
                let c: Claim = repo.store().get(&oid)?;
                if c.expires > now() && (path.is_none() || c.targets.iter().any(|t| matches!(&t.r#ref, Cbor::Text(r) if path.map(|p| p.starts_with(r.as_str()) || r.starts_with(p)).unwrap_or(true)))) {
                    out.push(json!({ "principal": c.principal.to_letters(), "targets": c.targets.iter().map(|t| cbor_to_json(&t.r#ref)).collect::<Vec<_>>(), "exclusive": c.exclusive, "note": c.note }));
                }
            }
        }
        out
    };
    let mut claims = Vec::new();
    let claims_total = claims_all.len();
    for c in claims_all {
        let cost = json_len(&c) + 1;
        if cost > chars_left {
            break;
        }
        chars_left -= cost;
        claims.push(c);
    }
    let omitted = json!({
        "units": units_total - units.len(),
        "memories": mems_total - mems.len(),
        "claims": claims_total - claims.len(),
    });
    truncated |=
        units_total > units.len() || mems_total > mems.len() || claims_total > claims.len();
    let mut next: Vec<String> = vec!["edit".into(), "claim --action claim --paths <paths>".into()];
    if truncated {
        if let Some(p) = path {
            next.insert(0, format!("context --path {p} --budget {}", budget * 2));
            next.insert(1, format!("context --unit {p}:<name> --budget {budget}"));
        }
    }
    let next: Vec<&str> = next.iter().map(String::as_str).collect();
    ok(
        json!({ "files": files, "units": units, "memories": mems, "claims": claims, "truncated": truncated, "omitted": omitted }),
        &next,
    )
}

/// What a value costs against a character budget: its serialized size.
fn json_len(v: &Json) -> usize {
    serde_json::to_string(v).map(|s| s.len()).unwrap_or(0)
}

/// The longest prefix of `text`, cut on a char boundary, whose JSON encoding
/// (quotes and escapes included) fits in `room` characters.
fn fit_prefix(text: &str, room: usize) -> usize {
    let mut take = text.len().min(room);
    loop {
        while take > 0 && !text.is_char_boundary(take) {
            take -= 1;
        }
        if take == 0 {
            return 0;
        }
        let enc = json_len(&json!(&text[..take]));
        if enc <= room {
            return take;
        }
        take = (take * room / enc).min(take - 1);
    }
}

/// Where the longest suffix of `text` whose JSON encoding fits in `room`
/// characters starts, on a char boundary.
fn fit_suffix(text: &str, room: usize) -> usize {
    let mut start = text.len().saturating_sub(room);
    loop {
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        if start >= text.len() {
            return text.len();
        }
        let enc = json_len(&json!(&text[start..]));
        if enc <= room {
            return start;
        }
        let keep = text.len() - start;
        let keep = (keep * room / enc).min(keep - 1);
        start = text.len() - keep;
    }
}

/// The context pack for one unit inside a token budget: its text, what it
/// depends on, what depends on it, the tests that cover it, memories about
/// it and its file, claims near it, and who last changed it. Text is cut
/// to the budget in that order.
fn unit_pack(
    repo: &mut Repo,
    actor: &mut Actor,
    unit: &str,
    path: Option<&str>,
    budget: usize,
) -> Result<Outcome> {
    // Reading needs no workspace: without one, the pack describes trunk.
    let (rev_id, rev) = match actor_workspace(repo, actor) {
        Some(ws) => current_revision(repo, &ws)?,
        None => {
            let (head, _) = trunk_head(repo)?;
            (head, repo.store().get::<Revision>(&head)?)
        }
    };
    let (snap_id, snap) = root_snapshot(repo, &rev)?;
    let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
    let (unit_path, unit_name) = match unit.split_once(':') {
        Some((p, n)) => (Some(p.to_string()), n.to_string()),
        None => (path.map(str::to_string), unit.to_string()),
    };
    let matches: Vec<&tessra_core::object::Node> = idx
        .nodes
        .iter()
        .filter(|n| n.name == unit_name && unit_path.as_deref().is_none_or(|p| n.path == p))
        .collect();
    let node = match matches.len() {
        0 => return Err(Error::verb("NOT_FOUND", format!("no unit named {unit_name}; name it as path:name if it lives in a file you did not give"))),
        1 => matches[0].clone(),
        n => {
            let where_: Vec<String> = matches.iter().map(|m| format!("{}:{}", m.path, m.name)).collect();
            return Err(Error::verb("AMBIGUOUS", format!("{n} units named {unit_name}: {}", where_.join(", "))));
        }
    };
    let flat = tree::flatten(repo.store(), &snap.root)?;
    let mut texts: HashMap<String, Vec<u8>> = HashMap::new();
    let mut text_of = |repo: &Repo, p: &str| -> Vec<u8> {
        if let Some(t) = texts.get(p) {
            return t.clone();
        }
        let t = flat
            .get(p)
            .and_then(|l| l.r#ref)
            .and_then(|r| repo.store().get_bytes(&r).ok().flatten())
            .unwrap_or_default();
        texts.insert(p.to_string(), t.clone());
        t
    };
    let slice = |t: &[u8], n: &tessra_core::object::Node| -> String {
        let a = (n.span.0 as usize).min(t.len());
        let b = (n.span.1 as usize).min(t.len());
        String::from_utf8_lossy(&t[a..b]).to_string()
    };
    let mut chars_left = budget * 4;
    let mut truncated = false;
    let take = |s: String, chars_left: &mut usize, truncated: &mut bool| -> String {
        if s.len() <= *chars_left {
            *chars_left -= s.len();
            s
        } else {
            *truncated = true;
            let cut = s
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= *chars_left)
                .last()
                .unwrap_or(0);
            let out = s[..cut].to_string();
            *chars_left = 0;
            out
        }
    };
    let by_nid: HashMap<EntityId, &tessra_core::object::Node> =
        idx.nodes.iter().map(|n| (n.nid, n)).collect();
    let unit_text = {
        let t = text_of(repo, &node.path);
        take(slice(&t, &node), &mut chars_left, &mut truncated)
    };
    let covering = crate::semantic::covering_tests(&idx);
    let mut tests = Vec::new();
    for t in covering.get(&node.nid).cloned().unwrap_or_default() {
        let text = text_of(repo, &t.path);
        let body = take(slice(&text, t), &mut chars_left, &mut truncated);
        tests.push(json!({ "path": t.path, "name": t.name, "text": body }));
    }
    let mut deps = Vec::new();
    for d in node.deps.iter().flatten() {
        if let Some(n) = by_nid.get(d) {
            let text = text_of(repo, &n.path);
            let body = take(slice(&text, n), &mut chars_left, &mut truncated);
            deps.push(json!({ "path": n.path, "name": n.name, "kind": n.kind, "text": body }));
        }
    }
    let dependents: Vec<Json> = idx
        .nodes
        .iter()
        .filter(|n| {
            n.nid != node.nid
                && !matches!(n.kind.as_str(), "test" | "test.skipped" | "module" | "impl")
                && n.deps.iter().flatten().any(|d| *d == node.nid)
        })
        .map(|n| json!({ "path": n.path, "name": n.name, "kind": n.kind }))
        .collect();
    let mut mems = Vec::new();
    for (id, m) in memories(repo, actor, Some(&node.path), None)? {
        let on_node = m.scope.kind == "node"
            && matches!(&m.scope.r#ref, Cbor::Text(r) if r == &node.nid.to_letters() || r == &node.name);
        if !on_node && m.scope.kind == "node" {
            continue;
        }
        let cost = m.body.len() + 80;
        if cost > chars_left {
            truncated = true;
            break;
        }
        chars_left -= cost;
        mems.push(memory_json(&id, &m));
    }
    let claims: Vec<Json> = crate::swarm::overlapping_claims(
        repo,
        actor.principal(),
        std::slice::from_ref(&node.path),
    )?;
    let blame = crate::semantic::blame(repo, rev_id, &node.path)?
        .into_iter()
        .find(|e| e.nid == node.nid)
        .map(|e| json!({ "revision": e.revision.to_hex(), "author": repo.principal_name(&e.author).unwrap_or_else(|| e.author.to_letters()), "title": e.title, "intent": e.intent.map(|i| i.to_letters()), "time": e.time }));
    let used = budget * 4 - chars_left;
    ok(
        json!({
            "unit": { "nid": node.nid.to_letters(), "path": node.path, "name": node.name, "kind": node.kind, "span": [node.span.0, node.span.1], "text": unit_text },
            "deps": deps, "dependents": dependents, "tests": tests, "memories": mems, "claims": claims, "blame": blame,
            "budget": { "used_chars": used, "limit_chars": budget * 4, "truncated": truncated },
        }),
        &["claim --action claim --paths <path>", "edit"],
    )
}

fn query(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let kind = arg_str(args, "kind").unwrap_or("memory");
    let budget = args.get("budget").and_then(Json::as_u64).unwrap_or(4000) as usize;
    match kind {
        "memory" => {
            let scope_ref = args
                .get("scope")
                .and_then(|s| s.get("ref"))
                .and_then(Json::as_str);
            let kinds: Option<Vec<String>> = args
                .get("kinds")
                .and_then(|k| serde_json::from_value(k.clone()).ok());
            let mut chars_left = budget * 4;
            let mut out = Vec::new();
            let mut cursor = None;
            for (id, m) in memories(repo, actor, scope_ref, kinds.as_deref())? {
                let cost = m.body.len() + 80;
                if cost > chars_left {
                    cursor = Some(id.to_letters());
                    break;
                }
                chars_left -= cost;
                out.push(memory_json(&id, &m));
            }
            ok(json!({ "memories": out, "cursor": cursor }), &[])
        }
        "revision" => {
            let change = arg_str(args, "change")
                .ok_or_else(|| Error::verb("ARGS", "query revision needs change"))?;
            let view = repo.log.current_view()?;
            let vs = ViewState::new(repo.store());
            let mut found = None;
            for (id, ptr) in vs.entities(&view)? {
                if id.matches_prefix(change) {
                    found = Some((id, ptr));
                    break;
                }
            }
            let (id, ptr) =
                found.ok_or_else(|| Error::verb("NOT_FOUND", format!("no change {change}")))?;
            let mut history = Vec::new();
            let mut cur = ptr.ids();
            let mut guard = 0;
            while let Some(oid) = cur.pop() {
                guard += 1;
                if guard > 200 {
                    break;
                }
                let r: Revision = repo.store().get(&oid)?;
                history.push(json!({ "revision": oid.to_hex(), "title": r.title, "author": r.author.to_letters(), "time": r.time, "parents": r.parents.iter().map(|p| p.to_hex()).collect::<Vec<_>>(), "stage": stage_of(repo, &oid)? }));
                if let Some(p) = r.prev {
                    cur.push(p);
                }
            }
            let omitted = cut_list(&mut history, (budget * 4).saturating_sub(120));
            ok(
                json!({ "change": id.to_letters(), "history": history, "omitted": omitted }),
                &[],
            )
        }
        "since" => {
            let (ops, omitted, cursor) = ops_since(repo, arg_str(args, "since"), budget * 4)?;
            ok(json!({ "ops": ops, "omitted": omitted, "cursor": cursor }), &[])
        }
        "blame" => {
            let path = arg_str(args, "path")
                .ok_or_else(|| Error::verb("ARGS", "query blame needs path"))?;
            let ws = require_workspace(repo, actor)?;
            let (rev_id, _) = current_revision(repo, &ws)?;
            let entries = crate::semantic::blame(repo, rev_id, path)?;
            let mut chars_left = budget * 4;
            let mut out = Vec::new();
            for e in entries {
                let j = json!({
                    "nid": e.nid.to_letters(), "kind": e.kind, "name": e.name, "span": [e.span.0, e.span.1],
                    "revision": e.revision.to_hex(), "author": e.author.to_letters(), "title": e.title,
                    "intent": e.intent.map(|i| i.to_letters()), "time": e.time,
                });
                let cost = json_len(&j) + 1;
                if cost > chars_left {
                    break;
                }
                chars_left -= cost;
                out.push(j);
            }
            ok(json!({ "path": path, "units": out }), &[])
        }
        "diff" => {
            // Node-level diff of a revision against its first parent: the
            // workspace's current revision, or the change named.
            let ws = require_workspace(repo, actor)?;
            let (rev_id, rev) = match arg_str(args, "change") {
                Some(change) => {
                    let view = repo.log.current_view()?;
                    let vs = ViewState::new(repo.store());
                    let mut found = None;
                    for (id, ptr) in vs.entities(&view)? {
                        if id.matches_prefix(change) {
                            found = ptr.ids().pop();
                            break;
                        }
                    }
                    let oid = found.ok_or_else(|| Error::verb("NOT_FOUND", format!("no change {change}")))?;
                    (oid, repo.store().get::<Revision>(&oid)?)
                }
                None => current_revision(repo, &ws)?,
            };
            let (snap_id, _) = root_snapshot(repo, &rev)?;
            let (_, idx) = crate::semantic::index_for_revision(repo.store(), &rev)?;
            let parent_idx = match rev.parents.first() {
                Some(p) => {
                    let pr: Revision = repo.store().get(p)?;
                    crate::semantic::index_for_revision(repo.store(), &pr)?.1
                }
                None => tessra_core::object::NodeIndex {
                    root: snap_id,
                    grammars: String::new(),
                    parents: vec![],
                    nodes: vec![],
                    aliases: None,
                },
            };
            let only = arg_str(args, "path");
            let mut chars_left = budget * 4;
            let mut out = Vec::new();
            let mut truncated = false;
            for c in crate::semantic::diff_indexes(&parent_idx, &idx) {
                if only.is_some_and(|p| p != c.path) {
                    continue;
                }
                let cost = 60 + c.path.len() + c.name.len();
                if cost > chars_left {
                    truncated = true;
                    break;
                }
                chars_left -= cost;
                out.push(json!({ "path": c.path, "kind": c.kind, "name": c.name, "nid": c.nid.to_letters(), "change": c.change, "from": c.from }));
            }
            ok(
                json!({ "revision": rev_id.to_hex(), "change": rev.id.to_letters(), "units": out, "truncated": truncated }),
                &[],
            )
        }
        "exceptions" => {
            // The exception queue: open requests for a human, each with the
            // change, the clauses, and the judges' reasoning.
            let mut out = Vec::new();
            for (id, m) in memories(repo, actor, None, Some(&["question".to_string()]))? {
                if m.status != "active" || !m.body.starts_with("Approval needed") {
                    continue;
                }
                let rev_id = m.links.iter().flatten().next().copied();
                let (title, author, judges) = match rev_id {
                    Some(r) => match repo.store().get::<Revision>(&r) {
                        Ok(rev) => (rev.title.clone(), repo.principal_name(&rev.author).unwrap_or_default(), judges_reasoning(repo, &r, &rev)?),
                        Err(_) => (String::new(), String::new(), vec![]),
                    },
                    None => (String::new(), String::new(), vec![]),
                };
                out.push(json!({ "request": id.to_letters(), "change": title, "author": author, "judges": judges, "asked": m.time, "reply": format!("tessra --as <human> approve --request {}", id.to_letters()), "body": m.body }));
            }
            let omitted = cut_list(&mut out, (budget * 4).saturating_sub(120));
            ok(json!({ "exceptions": out, "omitted": omitted }), &["approve --request <id>"])
        }
        "activity" => {
            // What happened in a window, at an altitude: summary, changes, or ops.
            let window = arg_str(args, "window").unwrap_or("1h");
            let secs: i64 = {
                let (num, unit) = window.split_at(window.len().saturating_sub(1));
                let n: i64 = num.parse().unwrap_or(1);
                match unit {
                    "s" => n,
                    "m" => n * 60,
                    "h" => n * 3600,
                    "d" => n * 86_400,
                    _ => window.parse::<i64>().unwrap_or(3600),
                }
            };
            let since = now() - secs * 1_000_000_000;
            let altitude = arg_str(args, "altitude").unwrap_or("summary");
            let mut ops: Vec<tessra_core::object::Op> = Vec::new();
            let mut seen: HashSet<ObjectId> = HashSet::new();
            let mut queue: VecDeque<ObjectId> = repo.log.heads().into_iter().collect();
            while let Some(id) = queue.pop_front() {
                if !seen.insert(id) {
                    continue;
                }
                let op = repo.log.get_op(&id)?;
                if op.time < since {
                    continue;
                }
                queue.extend(op.parents.iter().copied());
                ops.push(op);
            }
            ops.sort_by_key(|o| o.time);
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            let mut changes: Vec<Json> = Vec::new();
            let mut attest_kinds: BTreeMap<String, usize> = BTreeMap::new();
            let mut raw: Vec<Json> = Vec::new();
            for op in &ops {
                let who = repo.principal_name(&op.author).unwrap_or_else(|| op.author.to_letters());
                let mut what = op.kind.as_str();
                let mut title = None;
                let mut seq = None;
                for e in &op.effects {
                    match e {
                        Effect::Head { to, seq: s, .. } => {
                            what = "landed";
                            seq = Some(*s);
                            if let Ok(r) = repo.store().get::<Revision>(to) {
                                title = Some(r.title);
                            }
                        }
                        Effect::Propose { rev } => {
                            what = "proposed";
                            if let Ok(r) = repo.store().get::<Revision>(rev) {
                                title = Some(r.title);
                            }
                        }
                        Effect::Revoke { .. } => what = "revoked",
                        Effect::Put { id } => {
                            if let Some(bytes) = repo.store().get_bytes(id)? {
                                match cbor::peek_tag(&bytes).ok().as_deref() {
                                    Some("attestation") => {
                                        if let Ok(a) = cbor::decode::<tessra_core::object::Attestation>(&bytes) {
                                            *attest_kinds.entry(a.kind).or_default() += 1;
                                        }
                                    }
                                    Some("memory") if op.kind == "land" => what = "conflict",
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                *counts.entry(what).or_default() += 1;
                if matches!(what, "landed" | "proposed" | "conflict" | "revoked" | "revert" | "plan" | "restack" | "question") {
                    changes.push(json!({ "what": what, "who": who, "title": title, "seq": seq, "time": op.time }));
                }
                raw.push(json!({ "op": op.kind, "who": who, "time": op.time, "effects": op.effects.len() }));
            }
            let mut result = json!({ "window": window, "ops": ops.len(), "counts": counts, "attestations": attest_kinds });
            let mut chars_left = budget * 4;
            match altitude {
                "summary" => {
                    let landed: Vec<&Json> = changes.iter().filter(|c| c["what"] == "landed").collect();
                    let conflicts: Vec<&Json> = changes.iter().filter(|c| c["what"] == "conflict").collect();
                    result["headline"] = json!(format!(
                        "{} ops in {window}: {} landed, {} proposed, {} conflicts, {} attestations, {} revoked",
                        ops.len(), landed.len(), counts.get("proposed").copied().unwrap_or(0), conflicts.len(),
                        attest_kinds.values().sum::<usize>(), counts.get("revoked").copied().unwrap_or(0)
                    ));
                    result["latest_landings"] = json!(landed.iter().rev().take(5).map(|c| c["title"].clone()).collect::<Vec<_>>());
                }
                "changes" => {
                    let mut out = Vec::new();
                    for c in changes.iter().rev() {
                        let cost = 80 + c["title"].as_str().map(str::len).unwrap_or(0);
                        if cost > chars_left {
                            result["truncated"] = json!(true);
                            break;
                        }
                        chars_left -= cost;
                        out.push(c.clone());
                    }
                    result["changes"] = json!(out);
                }
                _ => {
                    let mut out = Vec::new();
                    for r in raw.iter().rev() {
                        if 60 > chars_left {
                            result["truncated"] = json!(true);
                            break;
                        }
                        chars_left -= 60;
                        out.push(r.clone());
                    }
                    result["ops_list"] = json!(out);
                }
            }
            ok(result, &[])
        }
        "trusted" => {
            // The most attested trunk snapshot that contains a change.
            let change = arg_str(args, "change")
                .ok_or_else(|| Error::verb("ARGS", "query trusted needs change"))?;
            let history = trunk_history(repo, 1000)?;
            let pos = history
                .iter()
                .position(|(_, r)| r.id.matches_prefix(change))
                .ok_or_else(|| Error::verb("NOT_FOUND", format!("change {change} has not landed on trunk")))?;
            let mut ranking = Vec::new();
            for (i, (rid, r)) in history.iter().enumerate().take(pos + 1) {
                let snap = r.snapshots.get("").copied();
                let mut atts = Vec::new();
                if let Some(sid) = snap {
                    atts.extend(crate::verifiers::attestations_on(repo.store(), sid.as_bytes())?);
                }
                atts.extend(crate::verifiers::attestations_on(repo.store(), rid.as_bytes())?);
                if let Some(p) = r.prev {
                    atts.extend(crate::verifiers::attestations_on(repo.store(), p.as_bytes())?);
                }
                let mut score = 0u32;
                let mut kinds: Vec<String> = Vec::new();
                for (_, a) in atts {
                    if !crate::verifiers::trusted_signer(repo, &a) {
                        continue;
                    }
                    let weight = match a.kind.as_str() {
                        "tests.pass" => {
                            if !matches!(a.result, Cbor::Bool(true)) {
                                continue;
                            }
                            let selected = a.scope.as_ref().and_then(|s| s.get("selected")).is_some_and(|v| matches!(v, Cbor::Bool(true)));
                            if selected { 1 } else { 3 }
                        }
                        "risk.change" => match a.scope.as_ref().and_then(|s| s.get("level")).and_then(|v| v.as_text()) {
                            Some("low") => 1,
                            _ => 0,
                        },
                        k if k.starts_with("ci.") => {
                            if !matches!(a.result, Cbor::Bool(true)) {
                                continue;
                            }
                            2
                        }
                        _ => {
                            if matches!(a.result, Cbor::Bool(false)) {
                                continue;
                            }
                            1
                        }
                    };
                    score += weight;
                    if !kinds.contains(&a.kind) {
                        kinds.push(a.kind.clone());
                    }
                }
                ranking.push(json!({
                    "revision": rid.to_hex(), "change": r.id.to_letters(), "title": r.title, "from_head": i,
                    "snapshot": snap.map(|x| x.to_hex()), "score": score, "attestations": kinds,
                }));
            }
            let best = ranking
                .iter()
                .max_by_key(|j| (j["score"].as_u64().unwrap_or(0), std::cmp::Reverse(j["from_head"].as_u64().unwrap_or(0))))
                .cloned();
            let candidates = ranking.len();
            let omitted = cut_list(
                &mut ranking,
                (budget * 4).saturating_sub(best.as_ref().map(json_len).unwrap_or(0) + 160),
            );
            ok(
                json!({ "change": change, "candidates": candidates, "best": best, "ranking": ranking, "omitted": omitted }),
                &["workspace --action create --from <revision>"],
            )
        }
        "bisect" => {
            use crate::verifiers as vf;
            let test_name = arg_str(args, "test")
                .ok_or_else(|| Error::verb("ARGS", "query bisect needs test"))?;
            let ws = require_workspace(repo, actor)?;
            let (_, rev) = current_revision(repo, &ws)?;
            let (snap_id, snap) = root_snapshot(repo, &rev)?;
            let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
            let test = idx
                .nodes
                .iter()
                .find(|n| (n.kind == "test" || n.kind == "test.skipped") && n.name == test_name)
                .cloned()
                .ok_or_else(|| Error::verb("NOT_FOUND", format!("no test unit named {test_name} in the current revision")))?;
            // What the test depends on, transitively, in the current index:
            // the units whose bodies decide its outcome.
            let by_nid: HashMap<EntityId, &tessra_core::object::Node> = idx.nodes.iter().map(|n| (n.nid, n)).collect();
            let mut closure: Vec<EntityId> = Vec::new();
            let mut queue: VecDeque<EntityId> = test.deps.clone().unwrap_or_default().into();
            let mut seen: HashSet<EntityId> = HashSet::new();
            while let Some(n) = queue.pop_front() {
                if n == test.nid || !seen.insert(n) {
                    continue;
                }
                closure.push(n);
                if closure.len() > 500 {
                    break;
                }
                if let Some(node) = by_nid.get(&n) {
                    for d in node.deps.iter().flatten() {
                        queue.push_back(*d);
                    }
                }
            }
            closure.sort();
            let history = trunk_history(repo, 500)?;
            if history.len() < 2 {
                return Err(Error::verb("HISTORY", "trunk needs at least two revisions to bisect"));
            }
            let seq: Vec<(ObjectId, Revision)> = history.into_iter().rev().collect();
            let dir = actor_dir_for(repo, &rev)?;
            let tool = vf::verifiers_for(repo, &dir)
                .into_iter()
                .find(|v| v.kind == "tests.pass" && v.filter != vf::Filter::None)
                .ok_or_else(|| Error::verb("VERIFIER", "no test runner that can select tests by name"))?;
            let env_id = vf::environment(repo, &tool)?;
            let verifier = vf::ensure_verifier(repo, &tool.name)?;
            let current_text = tree::flatten(repo.store(), &snap.root)?
                .get(&test.path)
                .and_then(|l| l.r#ref)
                .and_then(|r| repo.store().get_bytes(&r).ok().flatten())
                .unwrap_or_default();
            let current_nodes = crate::semantic::nodes_for_path(&idx, &test.path);
            let cx = BisectCtx {
                test,
                closure,
                seq: &seq,
                tool,
                env_id,
                verifier,
                current_nodes,
                current_text,
            };
            let n = seq.len();
            let mut probes: Vec<Json> = Vec::new();
            let mut runs = 0usize;
            let mut cached = 0usize;
            let note = |i: usize, passed: bool, was_cached: bool, probes: &mut Vec<Json>, runs: &mut usize, cached: &mut usize| {
                probes.push(json!({ "from_oldest": i, "revision": seq[i].0.to_hex(), "title": seq[i].1.title, "passed": passed, "cached": was_cached }));
                if was_cached { *cached += 1 } else { *runs += 1 }
            };
            let rev_json = |i: usize| json!({ "revision": seq[i].0.to_hex(), "change": seq[i].1.id.to_letters(), "title": seq[i].1.title, "author": seq[i].1.author.to_letters(), "from_oldest": i });
            let (head_ok, c) = bisect_probe(repo, &cx, n - 1)?;
            note(n - 1, head_ok, c, &mut probes, &mut runs, &mut cached);
            if head_ok {
                return ok(json!({ "test": test_name, "history": n, "passes_at_head": true, "runs": runs, "cached": cached, "probes": probes }), &[]);
            }
            let (old_ok, c) = bisect_probe(repo, &cx, 0)?;
            note(0, old_ok, c, &mut probes, &mut runs, &mut cached);
            if !old_ok {
                return ok(json!({ "test": test_name, "history": n, "fails_at_oldest": true, "oldest": rev_json(0), "runs": runs, "cached": cached, "probes": probes }), &[]);
            }
            let (mut lo, mut hi) = (0usize, n - 1);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                let (p, c) = bisect_probe(repo, &cx, mid)?;
                note(mid, p, c, &mut probes, &mut runs, &mut cached);
                if p {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            ok(
                json!({
                    "test": test_name, "history": n, "first_bad": rev_json(hi), "last_good": rev_json(lo),
                    "runs": runs, "cached": cached, "probes": probes,
                }),
                &["query --kind diff --change <change>"],
            )
        }
        "tests" => {
            // The covering-tests relation: for each unit of a path, the test
            // units anywhere in the root whose references name it.
            let path = arg_str(args, "path")
                .ok_or_else(|| Error::verb("ARGS", "query tests needs path"))?;
            let ws = require_workspace(repo, actor)?;
            let (_, rev) = current_revision(repo, &ws)?;
            let (snap_id, _) = root_snapshot(repo, &rev)?;
            let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
            let covering = crate::semantic::covering_tests(&idx);
            let mut chars_left = budget * 4;
            let mut out = Vec::new();
            let mut uncovered = Vec::new();
            for n in crate::semantic::nodes_for_path(&idx, path) {
                if n.kind == "test" || n.kind == "test.skipped" || n.kind == "import" || n.kind == "chunk" {
                    continue;
                }
                let tests: Vec<Json> = covering
                    .get(&n.nid)
                    .map(|ts| {
                        ts.iter()
                            .map(|t| json!({ "path": t.path, "name": t.name, "nid": t.nid.to_letters() }))
                            .collect()
                    })
                    .unwrap_or_default();
                if tests.is_empty() {
                    uncovered.push(n.name.clone());
                }
                let entry = json!({ "nid": n.nid.to_letters(), "kind": n.kind, "name": n.name, "tests": tests });
                let cost = json_len(&entry) + 1;
                if cost > chars_left {
                    break;
                }
                chars_left -= cost;
                out.push(entry);
            }
            let mut uncovered: Vec<Json> = uncovered.into_iter().map(Json::String).collect();
            let uncovered_omitted = cut_list(&mut uncovered, chars_left);
            ok(json!({ "path": path, "units": out, "uncovered": uncovered, "uncovered_omitted": uncovered_omitted }), &[])
        }
        "object" => {
            let prefix =
                arg_str(args, "id").ok_or_else(|| Error::verb("ARGS", "query object needs id"))?;
            let offset = args.get("offset").and_then(Json::as_u64).unwrap_or(0) as usize;
            let tail = args.get("tail").and_then(Json::as_bool).unwrap_or(false);
            let chars = (budget * 4).saturating_sub(200);
            let matches: Vec<ObjectId> = repo
                .store()
                .ids()?
                .into_iter()
                .filter(|i| i.matches_prefix(prefix))
                .collect();
            match matches.len() {
                1 => {
                    let hex = matches[0].to_hex();
                    let object = object_json(repo, &matches[0], offset, tail, chars)?;
                    let next: Vec<String> = match object.get("next_offset").and_then(Json::as_u64) {
                        Some(n) => vec![
                            format!("query --kind object --id {hex} --offset {n}"),
                            format!("query --kind object --id {hex} --tail"),
                        ],
                        None => Vec::new(),
                    };
                    let next: Vec<&str> = next.iter().map(String::as_str).collect();
                    ok(json!({ "id": hex, "object": object }), &next)
                }
                0 => Err(Error::verb(
                    "NOT_FOUND",
                    format!("no object with prefix {prefix}"),
                )),
                n => Err(Error::verb(
                    "AMBIGUOUS",
                    format!("{n} objects match {prefix}; use a longer prefix"),
                )),
            }
        }
        other => Err(Error::verb(
            "ARGS",
            format!("query kind {other}; use memory, revision, since, object, blame, tests, diff, trusted, bisect, activity, or exceptions"),
        )),
    }
}

fn workspace(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let action = arg_str(args, "action").unwrap_or("list");
    match action {
        "list" => {
            let budget = args.get("budget").and_then(Json::as_u64).unwrap_or(4000) as usize;
            let mut list: Vec<Json> = repo
                .workspaces
                .iter()
                .map(|w| json!({ "id": w.id.to_letters(), "principal": repo.principal_name(&w.principal).unwrap_or_else(|| w.principal.to_letters()), "path": display_path(&ws_path(w)), "base": w.base.to_hex(), "current": w.current.map(|c| c.to_hex()) }))
                .collect();
            let omitted = cut_list(&mut list, (budget * 4).saturating_sub(120));
            ok(json!({ "workspaces": list, "omitted": omitted }), &[])
        }
        "create" => {
            let from = arg_str(args, "from").unwrap_or("trunk");
            let base = if from == "trunk" {
                trunk_head(repo)?.0
            } else {
                let matches: Vec<ObjectId> = repo
                    .store()
                    .ids()?
                    .into_iter()
                    .filter(|i| i.matches_prefix(from))
                    .collect();
                if matches.len() != 1 {
                    return Err(Error::verb(
                        "NOT_FOUND",
                        format!("{} revisions match {from}", matches.len()),
                    ));
                }
                matches[0]
            };
            let id = EntityId::random();
            let path = match arg_str(args, "path") {
                Some(p) => {
                    let path = PathBuf::from(p);
                    refuse_unless_fresh(repo, &path)?;
                    path
                }
                None => paths::workspaces_dir_for(&repo.repo_id).join(id.to_letters()),
            };
            let rev: Revision = repo.store().get(&base)?;
            let (_, snap) = root_snapshot(repo, &rev)?;
            let started = std::time::Instant::now();
            let n = fs::materialize(repo.store(), &snap.root, &path, false)?;
            let state_restored = if args
                .get("with_state")
                .and_then(Json::as_bool)
                .unwrap_or(false)
            {
                restore_state(repo, &snap, &path)?
            } else {
                Vec::new()
            };
            let materialize_ms = started.elapsed().as_millis() as u64;
            let ws = Workspace {
                id,
                principal: actor.principal(),
                base,
                current: Some(base),
                paths: BTreeMap::from([("".to_string(), path.display().to_string())]),
                env: None,
                created: now(),
                expires: None,
            };
            repo.save_workspace(&ws)?;
            repo.workspaces.push(ws);
            actor.workspace = Some(id);
            ok(
                json!({ "workspace": id.to_letters(), "path": display_path(&path), "files": n, "base": base.to_hex(), "materialize_ms": materialize_ms, "state_restored": state_restored }),
                &["context --path .", "edit"],
            )
        }
        "rewind" => {
            // Put the workspace back at a revision: its files, and every state
            // path the revision's snapshot carried, so an experiment rewinds
            // together with everything it wrote outside the source tree.
            let ws = require_workspace(repo, actor)?;
            let to = arg_str(args, "to").unwrap_or("trunk");
            let target = if to == "trunk" {
                trunk_head(repo)?.0
            } else {
                let matches: Vec<ObjectId> = repo
                    .store()
                    .ids()?
                    .into_iter()
                    .filter(|i| i.matches_prefix(to))
                    .collect();
                if matches.len() != 1 {
                    return Err(Error::verb(
                        "NOT_FOUND",
                        format!("{} revisions match {to}", matches.len()),
                    ));
                }
                matches[0]
            };
            let rev: Revision = repo.store().get(&target)?;
            let (_, snap) = root_snapshot(repo, &rev)?;
            let dir = ws_path(&ws);
            // Only what the tracking rules would snapshot is put back or
            // removed: a checkout's dependencies, build output, and local
            // environment files were never in the tree and stay.
            let rules: TrackingRules = repo.store().get(&snap.rules)?;
            let n = fs::materialize_tracked(repo.store(), &snap.root, &dir, &rules)?;
            let state = restore_state(repo, &snap, &dir)?;
            if let Some(w) = repo.workspace_mut(&ws.id) {
                w.current = Some(target);
                w.base = target;
                let w2 = w.clone();
                repo.save_workspace(&w2)?;
            }
            repo.clear_pending_ops(&ws.id)?;
            ok(
                json!({ "workspace": ws.id.to_letters(), "revision": target.to_hex(), "title": rev.title, "files": n, "state": state }),
                &["status"],
            )
        }
        "drop" => {
            let id = arg_str(args, "id")
                .and_then(|s| {
                    repo.workspaces
                        .iter()
                        .find(|w| w.id.matches_prefix(s))
                        .map(|w| w.id)
                })
                .or(actor.workspace)
                .ok_or_else(|| Error::verb("ARGS", "workspace drop needs id"))?;
            let ws = repo
                .workspace(&id)
                .cloned()
                .ok_or_else(|| Error::verb("NOT_FOUND", "no such workspace"))?;
            if ws.principal != actor.principal() && actor.kind != "daemon" {
                return Err(Error::verb("SCOPE", "not your workspace"));
            }
            if repo.root_workspace().map(|w| w.id) == Some(id) {
                return Err(Error::verb(
                    "SCOPE",
                    "the repository checkout cannot be dropped",
                ));
            }
            let p = ws_path(&ws);
            if p.exists() && p.starts_with(paths::local_data_dir()) {
                let _ = std::fs::remove_dir_all(&p);
            }
            repo.remove_workspace(&id)?;
            if actor.workspace == Some(id) {
                actor.workspace = None;
            }
            ok(json!({ "dropped": id.to_letters() }), &[])
        }
        other => Err(Error::verb(
            "ARGS",
            format!("workspace action {other}; use list, create, drop"),
        )),
    }
}

fn edit(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let ws = require_workspace(repo, actor)?;
    if let Some(from) = arg_str(args, "rename") {
        return edit_rename(repo, actor, &ws, from, args);
    }
    let path = arg_str(args, "path").ok_or_else(|| Error::verb("ARGS", "edit needs path"))?;
    if path.contains("..") {
        return Err(Error::verb("ARGS", "path may not contain .."));
    }
    let full = ws_path(&ws).join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sidecar = full.with_file_name(format!(
        "{}.tessra-conflict",
        full.file_name().unwrap_or_default().to_string_lossy()
    ));
    if args.get("delete").and_then(Json::as_bool).unwrap_or(false) {
        let _ = std::fs::remove_file(&full);
        let _ = std::fs::remove_file(&sidecar);
        return ok(json!({ "path": path, "deleted": true }), &["snapshot"]);
    }
    let out = if let Some(content) = arg_str(args, "content") {
        std::fs::write(&full, content)?;
        content.len()
    } else if let (Some(old), Some(new)) = (arg_str(args, "old"), arg_str(args, "new")) {
        let cur = std::fs::read_to_string(&full)
            .map_err(|e| Error::verb("EDIT", format!("{path}: {e}")))?;
        let n = cur.matches(old).count();
        if n == 0 {
            return Err(Error::verb("EDIT", format!("old text not found in {path}")));
        }
        if n > 1 && !args.get("all").and_then(Json::as_bool).unwrap_or(false) {
            return Err(Error::verb(
                "EDIT",
                format!("old text occurs {n} times in {path}; pass all=true or make it unique"),
            ));
        }
        let updated = cur.replace(old, new);
        std::fs::write(&full, &updated)?;
        updated.len()
    } else {
        return Err(Error::verb("ARGS", "edit needs content, or old and new"));
    };
    let resolved = if args.get("resolve").and_then(Json::as_bool).unwrap_or(false) {
        std::fs::remove_file(&sidecar).is_ok()
    } else {
        false
    };
    let in_scope = actor
        .write_paths
        .as_ref()
        .map(|ps| tessra_oplog::glob::any_match(ps, path))
        .unwrap_or(true);
    let mut anomaly = Json::Null;
    if !in_scope {
        anomaly = crate::anomaly::record(
            repo,
            &actor.principal(),
            &format!("edit outside write scope: {path}"),
            2,
        )?
        .unwrap_or(Json::Null);
    } else if actor.kind == "session" {
        // Writing outside every claim it holds is the early signal.
        let held: Vec<String> = actor_claims(repo, actor)?
            .into_iter()
            .flat_map(|(_, c)| c.targets.into_iter())
            .filter_map(|t| match t.r#ref {
                Cbor::Text(s) => Some(s),
                _ => None,
            })
            .collect();
        if !held.is_empty()
            && !held
                .iter()
                .any(|h| path.starts_with(h.as_str()) || tessra_oplog::glob::matches(h, path))
        {
            anomaly = crate::anomaly::record(
                repo,
                &actor.principal(),
                &format!("edit outside claims: {path}"),
                1,
            )?
            .unwrap_or(Json::Null);
        }
    }
    ok(
        json!({ "path": path, "bytes": out, "in_write_scope": in_scope, "resolved": resolved, "anomaly": anomaly }),
        &["snapshot"],
    )
}

/// The semantic operation `rename`: replace an identifier where the parse
/// says it names the unit, never in a string or a comment, in every
/// tracked file with a grammar, or in one path, and record the op so the
/// merge can carry it into concurrent changes. Refused when the new name
/// already names a unit of the renamed unit's kind in a file the rename
/// would touch: that would be a second definition.
fn edit_rename(
    repo: &mut Repo,
    actor: &mut Actor,
    ws: &Workspace,
    from: &str,
    args: &Json,
) -> Result<Outcome> {
    use tessra_semantic::rename::{is_ident, rename_source, Rename};
    let to = arg_str(args, "to").ok_or_else(|| Error::verb("ARGS", "edit --rename needs to"))?;
    if !is_ident(from) || !is_ident(to) {
        return Err(Error::verb(
            "ARGS",
            "rename takes identifiers: letters, digits, and underscores",
        ));
    }
    let only = arg_str(args, "path");
    if only.is_some_and(|p| p.contains("..")) {
        return Err(Error::verb("ARGS", "path may not contain .."));
    }
    let (_, cur) = current_revision(repo, ws)?;
    let (_, cur_snap) = root_snapshot(repo, &cur)?;
    let rules: TrackingRules = repo.store().get(&cur_snap.rules)?;
    let dir = ws_path(ws);
    let files = fs::tracked_files(&dir, &rules)?;
    let renames = [Rename::new(from, to)];
    // Every file the rename touches, with its new text, before anything is
    // written: the collision check below sees them all.
    let mut touched: Vec<(String, std::path::PathBuf, Vec<u8>, Vec<u8>)> = Vec::new();
    for (rel, full) in files {
        if only.is_some_and(|p| p != rel) {
            continue;
        }
        let Some(lang) = tessra_semantic::Language::from_path(&rel) else {
            continue;
        };
        let bytes = std::fs::read(&full)?;
        if bytes.iter().take(8192).any(|&b| b == 0) {
            continue;
        }
        let (next, applied) = rename_source(lang, &bytes, &renames);
        if !applied.is_empty() {
            touched.push((rel, full, bytes, next));
        }
    }
    // The kinds of unit `from` names in the touched files; `to` may not
    // already name one of those kinds there, or any unit when `from` names
    // none, since the renamed references would then bind to it.
    let mut units: Vec<(String, tessra_semantic::RawNode)> = Vec::new();
    for (rel, _, bytes, _) in &touched {
        if let Ok(raw) = tessra_semantic::extract(rel, bytes) {
            units.extend(raw.into_iter().map(|r| (rel.clone(), r)));
        }
    }
    let kinds: Vec<&str> = units
        .iter()
        .filter(|(_, r)| r.name == from)
        .map(|(_, r)| r.kind.as_str())
        .collect();
    if let Some((rel, existing)) = units
        .iter()
        .find(|(_, r)| r.name == to && (kinds.is_empty() || kinds.contains(&r.kind.as_str())))
    {
        return Err(Error::verb(
            "RENAME",
            format!(
                "{to} already names a {} in {rel}; renaming {from} to it would define it twice",
                existing.kind
            ),
        ));
    }
    let mut changed = Vec::new();
    let mut out_of_scope = Vec::new();
    for (rel, full, _, next) in touched {
        std::fs::write(&full, next)?;
        let in_scope = actor
            .write_paths
            .as_ref()
            .map(|ps| tessra_oplog::glob::any_match(ps, &rel))
            .unwrap_or(true);
        if !in_scope {
            out_of_scope.push(rel.clone());
        }
        changed.push(rel);
    }
    repo.push_pending_op(&ws.id, crate::semantic::rename_op(from, to, only))?;
    ok(
        json!({
            "rename": from, "to": to, "files": changed, "recorded": true,
            "out_of_scope": out_of_scope,
            "how": "recorded as a semantic operation; landing applies it to concurrent changes that still use the old name",
        }),
        &["snapshot"],
    )
}

fn snapshot(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let ws = require_workspace(repo, actor)?;
    let (cur_id, cur) = current_revision(repo, &ws)?;
    let (_, cur_snap) = root_snapshot(repo, &cur)?;
    let rules: TrackingRules = repo.store().get(&cur_snap.rules)?;
    let base_flat: Flat = tree::flatten(repo.store(), &cur_snap.root)?;
    let mut out = fs::snapshot_dir(
        repo.store(),
        &ws_path(&ws),
        &rules,
        actor.write_paths.as_deref(),
        Some(&base_flat),
    )?;
    // Secrets are the change's flag only on the files it touched: one the
    // base revision already held is trunk's and must not block other work.
    let trunk_flat;
    let trunk_flat: &Flat = if ws.base == cur_id {
        &base_flat
    } else {
        trunk_flat = base_flat_of(repo, &ws)?;
        &trunk_flat
    };
    fs::scope_secrets_to_changes(&mut out.flags, &out.flat, trunk_flat);
    let flags = if out.flags.is_empty() {
        None
    } else {
        Some(out.flags.clone())
    };
    let mut anomaly = Json::Null;
    if let Some(paths) = flags.as_ref().and_then(|f| f.out_of_scope.as_ref()) {
        if !paths.is_empty()
            && cur.flags.as_ref().and_then(|f| f.out_of_scope.as_ref()) != Some(paths)
        {
            anomaly = crate::anomaly::record(
                repo,
                &actor.principal(),
                &format!("snapshot outside write scope: {}", paths.join(", ")),
                2,
            )?
            .unwrap_or(Json::Null);
        }
    }
    if out.tree == cur_snap.root && flags == cur.flags {
        return ok(
            json!({ "unchanged": true, "revision": cur_id.to_hex(), "change": cur.id.to_letters(), "files": out.files }),
            &["verify", "promote --to proposed"],
        );
    }
    // The semantic index, matched against the parent's so units keep their identity.
    let (parent_idx_id, parent_idx) = crate::semantic::index_for_revision(repo.store(), &cur)?;
    let nodes =
        crate::semantic::build_nodes(repo.store(), &out.flat, Some((&base_flat, &parent_idx)))?;
    let node_count = nodes.len();
    let index_id = crate::semantic::put_index(repo.store(), out.tree, vec![parent_idx_id], nodes)?;
    // Dependencies and build state alongside the files: the toolchain,
    // lockfile hashes, and a tree per state path, when asked or configured.
    let with_state = args
        .get("with_state")
        .and_then(Json::as_bool)
        .unwrap_or(false)
        || matches!(repo.config().get("snapshot_state"), Some(Cbor::Bool(true)));
    let mut state_json: Vec<Json> = Vec::new();
    let mut env_json = Json::Null;
    let env = if with_state {
        let (env_id, env_obj, state) = capture_environment(repo, &ws_path(&ws))?;
        state_json = state;
        env_json = json!({ "id": env_id.to_hex(), "os": env_obj.os, "arch": env_obj.arch, "tools": env_obj.tools });
        Some(env_id)
    } else {
        None
    };
    let snap_id = repo.store().put(&Snapshot {
        root: out.tree,
        rules: cur_snap.rules,
        env,
        index: Some(index_id),
    })?;
    let view = repo.log.current_view()?;
    let landed = landed_revisions(repo.store(), &view)?;
    let title = arg_str(args, "title").map(|s| s.to_string());
    // Semantic operations recorded since the last snapshot join the change's ops.
    let pending = repo.pending_ops(&ws.id)?;
    let ops_with = |prior: Option<&Vec<tessra_core::object::SemOp>>| {
        let mut all: Vec<tessra_core::object::SemOp> = prior.cloned().unwrap_or_default();
        all.extend(pending.iter().cloned());
        if all.is_empty() {
            None
        } else {
            Some(all)
        }
    };
    let rev = if landed.contains(&cur_id) {
        Revision {
            id: EntityId::random(),
            prev: None,
            snapshots: BTreeMap::from([("".to_string(), snap_id)]),
            parents: vec![cur_id],
            intent: None,
            title: title.unwrap_or_else(|| "work in progress".into()),
            body: None,
            author: actor.principal(),
            time: now(),
            ops: ops_with(None),
            flags,
        }
    } else {
        Revision {
            id: cur.id,
            prev: Some(cur_id),
            snapshots: BTreeMap::from([("".to_string(), snap_id)]),
            parents: cur.parents.clone(),
            intent: cur.intent,
            title: title.unwrap_or(cur.title.clone()),
            body: cur.body.clone(),
            author: actor.principal(),
            time: now(),
            ops: ops_with(cur.ops.as_ref()),
            flags,
        }
    };
    let rev_id = repo.store().put(&rev)?;
    let effects = vec![
        Effect::Put { id: rev_id },
        point_effect(&repo.log, rev.id, rev_id)?,
    ];
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "snapshot",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        effects,
        now(),
    )?;
    let op_id = repo.commit_op(&op)?;
    // A retry with the same idempotency key recorded nothing new: the
    // answer is the revision the first attempt recorded, and the workspace
    // points at that one.
    let (rev_id, rev, retried) = match repo.log.get_op(&op_id) {
        Ok(recorded) if recorded.effects != op.effects => {
            let earlier = recorded.effects.iter().find_map(|e| match e {
                Effect::Point { to, .. } => Some(*to),
                _ => None,
            });
            match earlier {
                Some(r) => (r, repo.store().get::<Revision>(&r)?, true),
                None => (rev_id, rev, false),
            }
        }
        _ => (rev_id, rev, false),
    };
    repo.clear_pending_ops(&ws.id)?;
    if let Some(w) = repo.workspace_mut(&ws.id) {
        w.current = Some(rev_id);
        let w2 = w.clone();
        repo.save_workspace(&w2)?;
    }
    let mut result = json!({ "revision": rev_id.to_hex(), "change": rev.id.to_letters(), "files": out.files, "nodes": node_count, "flags": rev.flags.as_ref().map(flags_json), "anomaly": anomaly, "state": state_json, "env": env_json, "retried": retried });
    let mut next: Vec<&str> = vec!["verify", "promote --to proposed"];
    match arg_str(args, "then") {
        Some("verify") => {
            let r = verify(repo, actor, &json!({}))?;
            result["verify"] = r.result;
            next = vec!["promote --to proposed"];
        }
        Some("promote") => {
            let r = promote(repo, actor, &json!({ "to": "proposed" }))?;
            result["promote"] = r.result;
            next = vec!["status"];
        }
        Some("promote landed") | Some("land") => {
            let r = promote(repo, actor, &json!({ "to": "landed" }))?;
            result["promote"] = r.result;
            next = vec!["status"];
        }
        _ => {}
    }
    ok(result, &next)
}

fn claim(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let action = arg_str(args, "action").unwrap_or("claim");
    match action {
        "claim" => {
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|p| serde_json::from_value(p.clone()).ok())
                .unwrap_or_default();
            if paths.is_empty() {
                return Err(Error::verb("ARGS", "claim needs paths"));
            }
            let secs = args.get("expires_s").and_then(Json::as_i64).unwrap_or(3600);
            let c = Claim {
                id: EntityId::random(),
                prev: None,
                principal: actor.principal(),
                targets: paths
                    .iter()
                    .map(|p| ClaimTarget {
                        kind: "path".into(),
                        r#ref: Cbor::Text(p.clone()),
                    })
                    .collect(),
                intent: None,
                exclusive: args
                    .get("exclusive")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
                expires: now() + secs * 1_000_000_000,
                note: arg_str(args, "note").map(|s| s.to_string()),
            };
            let oid = repo.store().put(&c)?;
            let op = build::build_op(
                &repo.log,
                &actor.signer,
                actor.cap,
                "claim",
                build::args_with_idem(idem_from(args), BTreeMap::new()),
                vec![
                    Effect::Put { id: oid },
                    Effect::Claim {
                        claim: c.id,
                        to: oid,
                    },
                ],
                now(),
            )?;
            repo.commit_op(&op)?;
            let overlaps = crate::swarm::overlapping_claims(repo, actor.principal(), &paths)?;
            let warning = if overlaps.is_empty() {
                Json::Null
            } else {
                json!(format!(
                    "{} other claim(s) overlap these paths; a merge is coming, keep your edit inside your own units",
                    overlaps.len()
                ))
            };
            ok(
                json!({ "claim": c.id.to_letters(), "paths": paths, "expires": c.expires, "overlaps": overlaps, "warning": warning }),
                &["edit"],
            )
        }
        "release" => {
            let mine = actor_claims(repo, actor)?;
            let target = arg_str(args, "id");
            let mut effects = Vec::new();
            let mut released = Vec::new();
            for (id, _) in mine {
                if target.map(|t| id.matches_prefix(t)).unwrap_or(true) {
                    effects.push(Effect::Unclaim { claim: id });
                    released.push(id.to_letters());
                }
            }
            if effects.is_empty() {
                return ok(json!({ "released": [] }), &[]);
            }
            let op = build::build_op(
                &repo.log,
                &actor.signer,
                actor.cap,
                "claim",
                build::args_with_idem(idem_from(args), BTreeMap::new()),
                effects,
                now(),
            )?;
            repo.commit_op(&op)?;
            ok(json!({ "released": released }), &[])
        }
        other => Err(Error::verb(
            "ARGS",
            format!("claim action {other}; use claim or release"),
        )),
    }
}

fn remember(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let kind = arg_str(args, "kind").unwrap_or("fact");
    if !MEMORY_KINDS.contains(&kind) {
        return Err(Error::verb(
            "ARGS",
            format!("memory kind {kind}; use one of {}", MEMORY_KINDS.join(", ")),
        ));
    }
    let body = arg_str(args, "body").ok_or_else(|| Error::verb("ARGS", "remember needs body"))?;
    let (scope_kind, scope_ref) = match args.get("scope") {
        Some(s) => (
            s.get("kind")
                .and_then(Json::as_str)
                .unwrap_or("repo")
                .to_string(),
            s.get("ref")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        None => ("repo".into(), String::new()),
    };
    // `unit` is the documented name of the `node` scope, `path:name`.
    let scope_kind = if scope_kind == "unit" {
        "node".to_string()
    } else {
        scope_kind
    };
    // A session's capability names the memory scopes it may write, and the
    // default task grant leaves the repository out: repository-wide memory
    // is the owner's. Say so here, with what to do instead, rather than
    // letting the op be rejected at step 6 with nothing but `status`.
    if let Some(cap_id) = actor.cap {
        let cap: tessra_core::object::Capability = repo.store().get(&cap_id)?;
        if let Some(scopes) = &cap.write.memory_scopes {
            if !scopes.contains(&scope_kind) {
                let allowed: Vec<&str> = scopes
                    .iter()
                    .map(|s| if s == "node" { "unit" } else { s.as_str() })
                    .collect();
                let instead = if scope_kind == "repo" {
                    "repository-wide memory is the owner's: run `tessra remember` without --agent, or scope this one with --scope-kind path --scope-ref <path>"
                } else {
                    "use one of those"
                };
                return Err(Error::verb(
                    "SCOPE",
                    format!(
                        "this session records {} memories, not {scope_kind}; {instead}",
                        allowed.join(", ")
                    ),
                ));
            }
        }
    }
    let confidence = args.get("confidence").and_then(Json::as_f64).unwrap_or(0.8);
    let m = Memory {
        id: EntityId::random(),
        prev: None,
        kind: kind.into(),
        scope: MemoryScope {
            kind: scope_kind.clone(),
            r#ref: Cbor::Text(scope_ref.clone()),
        },
        body: body.into(),
        anchor: None,
        confidence: (confidence.clamp(0.0, 1.0) * 1000.0) as u16,
        author: actor.principal(),
        time: now(),
        expires: None,
        proposed: None,
        status: "active".into(),
        links: None,
        visibility: arg_str(args, "visibility").unwrap_or("shared").into(),
    };
    let oid = repo.store().put(&m)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "remember",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        vec![
            Effect::Put { id: oid },
            Effect::Point {
                entity: m.id,
                to: oid,
                from: None,
            },
        ],
        now(),
    )?;
    let op_id = repo.commit_op(&op)?;
    // A retry with the same idempotency key names the memory the first
    // attempt recorded.
    let memory_id = match repo.log.get_op(&op_id) {
        Ok(recorded) if recorded.effects != op.effects => recorded
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Point { entity, .. } => Some(*entity),
                _ => None,
            })
            .unwrap_or(m.id),
        _ => m.id,
    };
    ok(
        json!({ "memory": memory_id.to_letters(), "kind": kind, "scope": { "kind": scope_kind, "ref": scope_ref } }),
        &[],
    )
}

/// What a verify is about, decided before its tools run.
pub struct VerifyCtx {
    rev_id: ObjectId,
    rev: Revision,
    conflicts: Vec<String>,
}

/// A verify the daemon carries out in two halves so a verifier's run holds
/// no lock: what was decided with the repository, and what to run without
/// it. `snapshot --then verify` carries the snapshot's answer as `prefix`.
pub struct Pending {
    ctx: VerifyCtx,
    plan: VerifierPlan,
    prefix: Option<Json>,
    limit: u64,
}

impl Pending {
    pub fn runs(&self) -> &[PlannedRun] {
        &self.plan.runs
    }
}

pub enum Split {
    Done(Json),
    Pending(Box<Pending>),
}

/// Like `call`, but a `verify`, or a `snapshot --then verify`, whose tools
/// have to run comes back pending: the caller runs `Pending::runs` with
/// the repository released and finishes with `finish_pending`.
pub fn call_split(repo: &mut Repo, actor: &mut Actor, verb: &str, args: &Json) -> Split {
    let limit = args.get("budget").and_then(Json::as_u64).unwrap_or(4000);
    let then_verify = verb == "snapshot" && arg_str(args, "then") == Some("verify");
    if verb != "verify" && !then_verify {
        return Split::Done(call(repo, actor, verb, args));
    }
    let mut prefix = None;
    if then_verify {
        let mut without = args.clone();
        if let Some(o) = without.as_object_mut() {
            o.remove("then");
        }
        let out = call(repo, actor, "snapshot", &without);
        if out.get("ok") != Some(&json!(true)) {
            return Split::Done(out);
        }
        prefix = Some(out);
    }
    repo.store().begin_batch();
    let prepared = verify_prepare(repo, actor, args);
    if let Err(e) = repo.store().end_batch() {
        return Split::Done(json!({ "ok": false, "code": "STORE", "message": e.to_string() }));
    }
    match prepared {
        Err(e) => {
            let st = state(repo, actor).unwrap_or_else(|e| json!({ "error": e.to_string() }));
            Split::Done(envelope(st, Err(e), limit))
        }
        Ok((ctx, plan)) if plan.runs.is_empty() => {
            let pending = Pending {
                ctx,
                plan,
                prefix,
                limit,
            };
            Split::Done(finish_pending(repo, actor, pending, Vec::new()))
        }
        Ok((ctx, plan)) => Split::Pending(Box::new(Pending {
            ctx,
            plan,
            prefix,
            limit,
        })),
    }
}

/// The second half: record what the runs did, evaluate the standard, and
/// answer as `call` would have.
pub fn finish_pending(
    repo: &mut Repo,
    actor: &mut Actor,
    pending: Pending,
    outcomes: Vec<Result<crate::verifiers::RunOutcome>>,
) -> Json {
    repo.store().begin_batch();
    let Pending {
        ctx,
        plan,
        prefix,
        limit,
    } = pending;
    let mut ran = plan.ran;
    let mut failed: Option<Error> = None;
    for (run, out) in plan.runs.iter().zip(outcomes) {
        match out.and_then(|o| record_verifier_run(repo, run, o)) {
            Ok(j) => ran.push(j),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    for d in &plan.cleanup {
        let _ = std::fs::remove_dir_all(d);
    }
    let outcome = match failed {
        Some(e) => Err(e),
        None => verify_complete(repo, ctx, ran),
    };
    if let Err(e) = repo.store().end_batch() {
        return json!({ "ok": false, "code": "STORE", "message": e.to_string() });
    }
    let st = state(repo, actor).unwrap_or_else(|e| json!({ "error": e.to_string() }));
    let out = envelope(st, outcome, limit);
    match prefix {
        None => out,
        Some(mut snap) => {
            // The compound form answers as the snapshot did, with the
            // verify's result inside it and the state as it is now.
            if out.get("ok") == Some(&json!(true)) {
                snap["result"]["verify"] = out["result"].clone();
                snap["next"] = json!(["promote --to proposed"]);
                snap["state"] = out["state"].clone();
                snap
            } else {
                out
            }
        }
    }
}

/// The first half of a verify: what to run.
fn verify_prepare(
    repo: &mut Repo,
    actor: &mut Actor,
    args: &Json,
) -> Result<(VerifyCtx, VerifierPlan)> {
    let ws = require_workspace(repo, actor)?;
    let (rev_id, rev) = current_revision(repo, &ws)?;
    let conflicts = conflicted_paths(repo, &rev)?;
    let full = args.get("full").and_then(Json::as_bool).unwrap_or(false);
    let only: Vec<String> = args
        .get("kinds")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let plan = if conflicts.is_empty() {
        plan_verifier_runs(repo, &rev_id, &rev, full, &only)?
    } else {
        VerifierPlan {
            runs: Vec::new(),
            ran: Vec::new(),
            cleanup: Vec::new(),
        }
    };
    Ok((
        VerifyCtx {
            rev_id,
            rev,
            conflicts,
        },
        plan,
    ))
}

fn verify(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let (ctx, plan) = verify_prepare(repo, actor, args)?;
    let ran = run_plan_inline(repo, plan)?;
    verify_complete(repo, ctx, ran)
}

/// The second half of a verify: the standard against what ran.
fn verify_complete(repo: &mut Repo, ctx: VerifyCtx, ran: Vec<Json>) -> Result<Outcome> {
    let VerifyCtx {
        rev_id,
        rev,
        conflicts,
    } = ctx;
    let (_, risk) = crate::risk::ensure_attested(repo, &rev_id, &rev)?;
    let (met, mut unmet, attests) = standard_status(repo, &rev_id, &rev)?;
    let request = ensure_approval_request(repo, &rev_id, &rev, &unmet)?;
    note_approval(&mut unmet, &request);
    let next: &[&str] = if !conflicts.is_empty() {
        &["edit --resolve", "snapshot"]
    } else if unmet.is_empty() {
        &["promote --to proposed"]
    } else {
        &["snapshot"]
    };
    ok(
        json!({
            "revision": rev_id.to_hex(),
            "stage": stage_of(repo, &rev_id)?,
            "met": met,
            "unmet": unmet.iter().map(|u| json!({ "clause": u.clause, "reason": u.reason })).collect::<Vec<_>>(),
            "conflicts": conflicts,
            "ran": ran,
            "attestations": attests.len(),
            "risk": crate::risk::to_json(&risk),
            "approval": request,
        }),
        next,
    )
}

/// The verifier runner, for hooks.
pub fn run_verifiers_for(
    repo: &mut Repo,
    rev_id: &ObjectId,
    rev: &Revision,
    full: bool,
    only: &[String],
) -> Result<Vec<Json>> {
    run_verifiers(repo, rev_id, rev, full, only)
}

/// Run the verifiers the standard still needs for a revision: every
/// `attest` clause that is unmet, or every one when `full`. Whole-suite
/// results attest the snapshot; a run selected by the covering-tests
/// relation attests the changed units and the tests that ran, by body.
/// This runs each tool inline; the daemon plans, runs with the repository
/// released, and records, through the halves below.
fn run_verifiers(
    repo: &mut Repo,
    rev_id: &ObjectId,
    rev: &Revision,
    full: bool,
    only: &[String],
) -> Result<Vec<Json>> {
    let plan = plan_verifier_runs(repo, rev_id, rev, full, only)?;
    run_plan_inline(repo, plan)
}

/// Carry out a plan here and now: each run, then its record.
fn run_plan_inline(repo: &mut Repo, plan: VerifierPlan) -> Result<Vec<Json>> {
    let mut ran = plan.ran;
    let mut failed: Option<Error> = None;
    for run in &plan.runs {
        match crate::verifiers::run_planned(run).and_then(|o| record_verifier_run(repo, run, o)) {
            Ok(j) => ran.push(j),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    for d in &plan.cleanup {
        let _ = std::fs::remove_dir_all(d);
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(ran),
    }
}

/// One verifier run: decided with the repository, carried out without it,
/// recorded with it again.
pub struct PlannedRun {
    pub kind: String,
    pub tool: crate::verifiers::Verifier,
    pub dir: PathBuf,
    pub select: Vec<String>,
    pub timeout: std::time::Duration,
    pub env: crate::verifiers::RunEnv,
    env_id: ObjectId,
    verifier: EntityId,
    how: RunHow,
}

enum RunHow {
    /// New tests laid into the parent's text: they prove the change when
    /// they fail there.
    NewTests {
        names: Vec<String>,
        paths: Vec<String>,
        bodies: Vec<ObjectId>,
    },
    /// The suite, or the tests covering what changed.
    Suite {
        snap_id: ObjectId,
        selected: Vec<String>,
        selected_bodies: Vec<ObjectId>,
    },
}

/// The runs a verify needs, the entries for what it did not need to run,
/// and the scratch directories to remove once the runs are done.
pub struct VerifierPlan {
    pub runs: Vec<PlannedRun>,
    pub ran: Vec<Json>,
    pub cleanup: Vec<PathBuf>,
}

/// Decide the runs: which kinds still need proof, which tool proves each,
/// and a scratch copy of what it runs on. Nothing here waits on a tool.
fn plan_verifier_runs(
    repo: &mut Repo,
    rev_id: &ObjectId,
    rev: &Revision,
    full: bool,
    only: &[String],
) -> Result<VerifierPlan> {
    use crate::verifiers::{self as vf, Filter};
    use std::time::Duration;
    let (snap_id, snap) = root_snapshot(repo, rev)?;
    let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
    let parent: Option<(ObjectId, Snapshot, tessra_core::object::NodeIndex)> =
        match rev.parents.first() {
            Some(p) => {
                let pr: Revision = repo.store().get(p)?;
                let (ps_id, ps) = root_snapshot(repo, &pr)?;
                let (_, pidx) = crate::semantic::index_for_snapshot(repo.store(), &ps_id)?;
                Some((ps_id, ps, pidx))
            }
            None => None,
        };
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let std_id = trunk_standard(repo)?;
    let chain = standard::chain(repo.store(), &vs, &view, &std_id)?;
    let required = standard::required_attestations(&chain);
    let (_, unmet, applying) = standard_status(repo, rev_id, rev)?;
    let needed = |kind: &str| -> bool {
        full || unmet
            .iter()
            .any(|u| u.clause.contains(&format!("attest({kind}")))
    };
    let mut applying_kinds: HashSet<String> = HashSet::new();
    for id in &applying {
        if let Ok(a) = repo.store().get::<tessra_core::object::Attestation>(id) {
            applying_kinds.insert(a.kind);
        }
    }
    let timeout = Duration::from_secs(match repo.config().get("verify_timeout_s") {
        Some(Cbor::Integer(i)) => i128::from(*i).clamp(1, 86_400) as u64,
        _ => 600,
    });
    let ws_dir = actor_dir_for(repo, rev)?;
    let verifiers = vf::verifiers_for(repo, &ws_dir);
    let mut ran = Vec::new();
    let mut runs: Vec<PlannedRun> = Vec::new();
    let mut cleanup: Vec<PathBuf> = Vec::new();
    for (kind, scope) in required {
        if !only.is_empty() && !only.contains(&kind) {
            continue;
        }
        if !needed(&kind) {
            if applying_kinds.contains(&kind) {
                ran.push(json!({ "kind": kind, "cached": true }));
            } else {
                ran.push(json!({ "kind": kind, "skipped": "nothing to prove for this change" }));
            }
            continue;
        }
        // The tool that proves this kind: its own, or the test runner for
        // proofs about tests.
        let tool = verifiers
            .iter()
            .find(|v| v.kind == kind)
            .or_else(|| {
                if kind.starts_with("tests.") {
                    verifiers.iter().find(|v| v.kind == "tests.pass")
                } else {
                    None
                }
            })
            .cloned();
        let Some(tool) = tool else {
            ran.push(json!({ "kind": kind, "skipped": "no local verifier for this kind; an external principal or a hook attests it, or set verifiers in .tessra/config" }));
            continue;
        };
        let env_id = vf::environment(repo, &tool)?;
        let verifier = vf::ensure_verifier(repo, &tool.name)?;
        match scope.as_deref() {
            Some("new_tests") => {
                let Some((_, parent_snap, parent_idx)) = parent.as_ref() else {
                    ran.push(json!({ "kind": kind, "skipped": "no parent revision" }));
                    continue;
                };
                let prev_ids: HashSet<EntityId> = parent_idx.nodes.iter().map(|n| n.nid).collect();
                let prev_bodies: HashSet<ObjectId> =
                    parent_idx.nodes.iter().map(|n| n.body).collect();
                let new_tests: Vec<&tessra_core::object::Node> = idx
                    .nodes
                    .iter()
                    .filter(|n| {
                        (n.kind == "test" || n.kind == "test.skipped")
                            && !prev_ids.contains(&n.nid)
                            && !prev_bodies.contains(&n.body)
                    })
                    .collect();
                if new_tests.is_empty() {
                    ran.push(json!({ "kind": kind, "skipped": "no new tests" }));
                    continue;
                }
                if tool.filter == Filter::None {
                    ran.push(json!({ "kind": kind, "skipped": "the test runner cannot select tests by name" }));
                    continue;
                }
                // The parent's tree with only the new tests laid into it:
                // the code under test stays the parent's.
                let dir = vf::materialize_scratch(
                    repo,
                    &parent_snap.root,
                    &format!("parent-{}", &snap_id.to_hex()[..12]),
                )?;
                let flat = tree::flatten(repo.store(), &snap.root)?;
                let parent_flat = tree::flatten(repo.store(), &parent_snap.root)?;
                let mut paths: Vec<&str> = new_tests.iter().map(|n| n.path.as_str()).collect();
                paths.sort();
                paths.dedup();
                for p in &paths {
                    let text_of = |f: &Flat| -> Vec<u8> {
                        f.get(*p)
                            .and_then(|l| l.r#ref)
                            .and_then(|r| repo.store().get_bytes(&r).ok().flatten())
                            .unwrap_or_default()
                    };
                    let change_text = text_of(&flat);
                    let parent_text = text_of(&parent_flat);
                    let change_nodes = crate::semantic::nodes_for_path(&idx, p);
                    let parent_nodes = crate::semantic::nodes_for_path(parent_idx, p);
                    let here: Vec<&tessra_core::object::Node> =
                        new_tests.iter().copied().filter(|n| n.path == *p).collect();
                    let merged = vf::overlay_tests(
                        &parent_text,
                        &parent_nodes,
                        &change_text,
                        &change_nodes,
                        &here,
                    );
                    let full_path = dir.join(p);
                    if let Some(d) = full_path.parent() {
                        std::fs::create_dir_all(d)?;
                    }
                    std::fs::write(full_path, merged)?;
                }
                let names: Vec<String> = new_tests.iter().map(|n| n.name.clone()).collect();
                cleanup.push(dir.clone());
                runs.push(PlannedRun {
                    kind,
                    tool: tool.clone(),
                    dir,
                    select: names.clone(),
                    timeout,
                    env: vf::run_env(repo),
                    env_id,
                    verifier,
                    how: RunHow::NewTests {
                        names,
                        paths: paths.iter().map(|p| p.to_string()).collect(),
                        bodies: new_tests.iter().map(|n| n.body).collect(),
                    },
                });
            }
            _ => {
                // Select by the covering-tests relation when every changed
                // unit has a covering test and that is fewer than the suite.
                let mut selected: Vec<String> = Vec::new();
                let mut selected_bodies: Vec<ObjectId> = Vec::new();
                if !full && tool.filter != Filter::None {
                    if let Some((_, _, parent_idx)) = parent.as_ref() {
                        let changed: Vec<&tessra_core::object::Node> =
                            vf::changed_units(parent_idx, &idx)
                                .into_iter()
                                .filter(|n| {
                                    !matches!(
                                        n.kind.as_str(),
                                        "import"
                                            | "chunk"
                                            | "impl"
                                            | "module"
                                            | "variant"
                                            | "field"
                                    )
                                })
                                .collect();
                        let covering = crate::semantic::covering_tests(&idx);
                        let all_tests = idx.nodes.iter().filter(|n| n.kind == "test").count();
                        let mut names: Vec<String> = Vec::new();
                        let mut bodies: Vec<ObjectId> = Vec::new();
                        let mut every_covered = !changed.is_empty();
                        for u in &changed {
                            if u.kind == "test" {
                                names.push(u.name.clone());
                                bodies.push(u.body);
                                continue;
                            }
                            match covering.get(&u.nid) {
                                Some(ts) if !ts.is_empty() => {
                                    for t in ts {
                                        names.push(t.name.clone());
                                        bodies.push(t.body);
                                    }
                                    bodies.push(u.body);
                                }
                                _ => {
                                    every_covered = false;
                                    break;
                                }
                            }
                        }
                        // Dependents of what changed bring their covering tests along.
                        let changed_ids: HashSet<EntityId> =
                            changed.iter().map(|n| n.nid).collect();
                        for d in idx.nodes.iter().filter(|n| {
                            !changed_ids.contains(&n.nid)
                                && n.kind != "test"
                                && n.kind != "test.skipped"
                                && n.deps.iter().flatten().any(|x| changed_ids.contains(x))
                        }) {
                            if let Some(ts) = covering.get(&d.nid) {
                                for t in ts {
                                    names.push(t.name.clone());
                                    bodies.push(t.body);
                                }
                                bodies.push(d.body);
                            }
                        }
                        names.sort();
                        names.dedup();
                        if every_covered && names.len() < all_tests {
                            selected = names;
                            selected_bodies = bodies;
                        }
                    }
                }
                // One scratch copy of the snapshot serves every kind that
                // runs on it; it is removed when the runs are done.
                let label = snap_id.to_hex()[..12].to_string();
                let dir = match cleanup
                    .iter()
                    .find(|d| d.ends_with(format!("verify-{label}")))
                {
                    Some(d) => d.clone(),
                    None => {
                        let d = vf::materialize_scratch(repo, &snap.root, &label)?;
                        cleanup.push(d.clone());
                        d
                    }
                };
                runs.push(PlannedRun {
                    kind,
                    tool: tool.clone(),
                    dir,
                    select: selected.clone(),
                    timeout,
                    env: vf::run_env(repo),
                    env_id,
                    verifier,
                    how: RunHow::Suite {
                        snap_id,
                        selected,
                        selected_bodies,
                    },
                });
            }
        }
    }
    Ok(VerifierPlan { runs, ran, cleanup })
}

/// Record what a run showed: the attestation, signed by the daemon as
/// runner, and the entry the caller sees.
fn record_verifier_run(
    repo: &mut Repo,
    run: &PlannedRun,
    out: crate::verifiers::RunOutcome,
) -> Result<Json> {
    use crate::verifiers as vf;
    let kind = &run.kind;
    match &run.how {
        RunHow::NewTests {
            names,
            paths,
            bodies,
        } => {
            // A test proves the change only when it is seen failing on the
            // parent. A clean exit with nothing parsed means every selected
            // test passed there; a run that could not report the tests at
            // all, a build error on the parent for instance, is taken as
            // failing there and said so.
            let passed_on_parent: Vec<String> = if out.ok && out.tests.is_empty() {
                names.clone()
            } else {
                names
                    .iter()
                    .filter(|n| {
                        out.tests
                            .iter()
                            .any(|(t, okk)| *okk && vf::test_matches(t, n))
                    })
                    .cloned()
                    .collect()
            };
            let observed = !out.tests.is_empty();
            let proves = passed_on_parent.is_empty();
            let scope_map = BTreeMap::from([
                ("tests".to_string(), vf::text_list(names)),
                (
                    "passed_on_parent".to_string(),
                    vf::text_list(&passed_on_parent),
                ),
                ("paths".to_string(), vf::text_list(paths)),
                ("observed".to_string(), Cbor::Bool(observed)),
            ]);
            let att = vf::attest_as_runner(
                repo,
                kind,
                None,
                Some(bodies.clone()),
                Some(scope_map),
                Cbor::Bool(proves),
                Some(run.env_id),
                run.verifier,
                Some(&out.output),
            )?;
            Ok(json!({
                "kind": kind, "cached": false, "result": proves, "tests": names,
                "passed_on_parent": passed_on_parent, "observed": observed,
                "elapsed_ms": out.elapsed_ms, "attestation": att.to_hex(),
                "how": if !proves {
                    "a new test passes on the parent too, so it does not prove the change"
                } else if observed {
                    "every new test fails on the parent snapshot"
                } else {
                    "the parent could not run the new tests at all (a build error, or a tool that reports nothing), which is taken as failing there"
                },
            }))
        }
        RunHow::Suite {
            snap_id,
            selected,
            selected_bodies,
        } => {
            let passed = out.tests.iter().filter(|(_, okk)| *okk).count();
            let failed = out.tests.iter().filter(|(_, okk)| !*okk).count();
            let mut scope_map = BTreeMap::from([
                ("passed".to_string(), Cbor::Integer((passed as u64).into())),
                ("failed".to_string(), Cbor::Integer((failed as u64).into())),
                ("selected".to_string(), Cbor::Bool(!selected.is_empty())),
                (
                    "elapsed_ms".to_string(),
                    Cbor::Integer(out.elapsed_ms.into()),
                ),
            ]);
            let ran_names: Vec<String> = out.tests.iter().map(|(t, _)| t.clone()).collect();
            scope_map.insert("tests".to_string(), vf::text_list(&ran_names));
            let failed_names: Vec<String> = out
                .tests
                .iter()
                .filter(|(_, okk)| !*okk)
                .map(|(t, _)| t.clone())
                .collect();
            scope_map.insert("failed".to_string(), vf::text_list(&failed_names));
            scope_map.insert(
                "exit".to_string(),
                Cbor::Integer(i64::from(out.exit.unwrap_or(-1)).into()),
            );
            let (subject, bodies) = if selected.is_empty() {
                (Some((snap_id.as_bytes().as_slice(), "snapshot")), None)
            } else {
                (None, Some(selected_bodies.clone()))
            };
            let att = vf::attest_as_runner(
                repo,
                kind,
                subject,
                bodies,
                Some(scope_map),
                Cbor::Bool(out.ok),
                Some(run.env_id),
                run.verifier,
                Some(&out.output),
            )?;
            let evidence = repo
                .store()
                .get::<tessra_core::object::Attestation>(&att)?
                .evidence
                .map(|e| e.to_hex());
            let mut entry = json!({
                "kind": kind, "cached": false, "result": out.ok, "exit": out.exit, "timed_out": out.timed_out,
                "selected": if selected.is_empty() { json!("all") } else { json!(selected) },
                "passed": passed, "failed": failed,
                "failed_tests": failed_names,
                "elapsed_ms": out.elapsed_ms, "attestation": att.to_hex(), "evidence": evidence,
            });
            // A failed run carries the end of its output, where every
            // runner puts its summary and errors; the evidence blob holds
            // the last 64 KiB for `query --kind object --tail`.
            if !out.ok {
                entry["output_tail"] = json!(vf::tail_text(&out.output, 2000));
            }
            Ok(entry)
        }
    }
}

/// A directory whose files describe the revision, for verifier detection:
/// a workspace holding it, else the repository root.
fn actor_dir_for(repo: &Repo, rev: &Revision) -> Result<PathBuf> {
    let (snap_id, _) = root_snapshot(repo, rev)?;
    for w in &repo.workspaces {
        if let Some(cur) = w.current {
            if let Ok(r) = repo.store().get::<Revision>(&cur) {
                if r.snapshots.get("") == Some(&snap_id) {
                    return Ok(ws_path(w));
                }
            }
        }
    }
    Ok(repo.root.clone())
}

/// Edit the trunk standard: owner only. Clauses are `require`d or
/// `forbid`den predicates in the text form `kind(name, key=value)`.
fn standard_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let std_id = match arg_str(args, "target") {
        Some(t) => crate::delivery::target_by_name(repo, t)?.2.standard,
        None => trunk_standard(repo)?,
    };
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let cur_oid = vs
        .entity(&view, &std_id)?
        .ok_or_else(|| Error::verb("STANDARD", "trunk standard missing"))?
        .single()?;
    let cur: tessra_core::object::Standard = repo.store().get(&cur_oid)?;
    let list = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let require = list("require");
    let forbid = list("forbid");
    let remove = list("remove");
    let when_level = arg_str(args, "when").map(str::to_string);
    if when_level
        .as_deref()
        .is_some_and(|l| !matches!(l, "low" | "medium" | "high" | "critical"))
    {
        return Err(Error::verb(
            "ARGS",
            "when takes low, medium, high, or critical",
        ));
    }
    let clause_text = |c: &tessra_core::object::Clause| -> String {
        let mut t = format!("{} {}", c.op, standard::describe(&c.pred));
        if let Some(u) = &c.unless {
            t.push_str(&format!(" unless {}", standard::describe(u)));
        }
        t
    };
    let render = |s: &tessra_core::object::Standard| -> Vec<String> {
        let mut out: Vec<String> = s.clauses.iter().map(clause_text).collect();
        for w in s.when.iter().flatten() {
            for c in &w.clauses {
                out.push(format!(
                    "when risk>={}: {}",
                    w.risk_at_least,
                    clause_text(c)
                ));
            }
        }
        out
    };
    if require.is_empty() && forbid.is_empty() && remove.is_empty() {
        return ok(
            json!({ "standard": cur.name, "id": std_id.to_letters(), "clauses": render(&cur), "revision": cur_oid.to_hex() }),
            &["standard --require 'attest(tests.pass)'"],
        );
    }
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "only the owner edits the standard"));
    }
    require_owner_credential(repo, actor, "editing the standard")?;
    let mut next = cur.clone();
    next.prev = Some(cur_oid);
    for r in &remove {
        let before = next.clauses.len();
        if let Ok(i) = r.parse::<usize>() {
            if i < next.clauses.len() {
                next.clauses.remove(i);
                continue;
            }
        }
        // Match the text as written or as it would be shown, so argument
        // order does not matter.
        let (op_text, pred_text) = match r.split_once(' ') {
            Some((op, rest)) if matches!(op, "require" | "forbid") => {
                (Some(op.to_string()), rest.to_string())
            }
            _ => (None, r.clone()),
        };
        let normalized = standard::parse_predicate(&pred_text)
            .map(|p| standard::describe(&p))
            .unwrap_or(pred_text.clone());
        let matches = |c: &tessra_core::object::Clause| -> bool {
            let shown = standard::describe(&c.pred);
            (shown == normalized || shown == pred_text)
                && op_text.as_deref().is_none_or(|o| o == c.op)
        };
        next.clauses.retain(|c| !matches(c));
        let mut removed_when = false;
        for w in next.when.iter_mut().flatten() {
            let n = w.clauses.len();
            w.clauses.retain(|c| !matches(c));
            removed_when |= w.clauses.len() != n;
        }
        if next.clauses.len() == before && !removed_when {
            return Err(Error::verb("ARGS", format!("no clause matches {r}")));
        }
    }
    for (op, preds) in [("require", &require), ("forbid", &forbid)] {
        for text in preds {
            let (pred_text, unless_text) = match text.split_once(" unless ") {
                Some((a, b)) => (a, Some(b)),
                None => (text.as_str(), None),
            };
            let pred = standard::parse_predicate(pred_text).map_err(|e| Error::verb("ARGS", e))?;
            let unless = match unless_text {
                Some(u) => Some(standard::parse_predicate(u).map_err(|e| Error::verb("ARGS", e))?),
                None => None,
            };
            let clause = tessra_core::object::Clause {
                op: op.into(),
                pred,
                unless,
            };
            match &when_level {
                Some(level) => {
                    let blocks = next.when.get_or_insert_with(Vec::new);
                    match blocks.iter_mut().find(|w| w.risk_at_least == *level) {
                        Some(w) => w.clauses.push(clause),
                        None => blocks.push(tessra_core::object::RiskClauses {
                            risk_at_least: level.clone(),
                            clauses: vec![clause],
                        }),
                    }
                }
                None => next.clauses.push(clause),
            }
        }
    }
    let new_oid = repo.store().put(&next)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "standard",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        vec![
            Effect::Put { id: new_oid },
            point_effect(&repo.log, std_id, new_oid)?,
        ],
        now(),
    )?;
    repo.commit_op(&op)?;
    ok(
        json!({ "standard": next.name, "id": std_id.to_letters(), "clauses": render(&next), "revision": new_oid.to_hex() }),
        &["verify"],
    )
}

/// Record an attestation under the caller's own principal, as an external
/// verifier or CI system would. Whether it counts is the standard's call:
/// a session's attestation never satisfies an `attest` clause.
fn attest_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let kind = arg_str(args, "kind").ok_or_else(|| Error::verb("ARGS", "attest needs kind"))?;
    if kind.starts_with("approval.") && actor.kind != "human" {
        return Err(Error::verb(
            "SCOPE",
            "approvals are recorded by humans through channels, never by a session or the daemon",
        ));
    }
    // The owner's attestations satisfy standards, so recording one is a
    // policy act.
    require_owner_credential(repo, actor, "attesting as the owner")?;
    let ws = actor_workspace(repo, actor);
    let subject: ObjectId = match arg_str(args, "subject") {
        Some(prefix) => {
            let matches: Vec<ObjectId> = repo
                .store()
                .ids()?
                .into_iter()
                .filter(|i| i.matches_prefix(prefix))
                .collect();
            match matches.len() {
                1 => matches[0],
                0 => {
                    return Err(Error::verb(
                        "NOT_FOUND",
                        format!("no object with prefix {prefix}"),
                    ))
                }
                n => {
                    return Err(Error::verb(
                        "AMBIGUOUS",
                        format!("{n} objects match {prefix}"),
                    ))
                }
            }
        }
        None => {
            let ws =
                ws.ok_or_else(|| Error::verb("ARGS", "attest needs subject or a workspace"))?;
            current_revision(repo, &ws)?.0
        }
    };
    let subject_type = repo
        .get_bytes(&subject)?
        .and_then(|b| cbor::peek_tag(&b).ok())
        .unwrap_or_else(|| "blob".into());
    let result = match args.get("result") {
        None => Cbor::Bool(true),
        Some(Json::Bool(b)) => Cbor::Bool(*b),
        Some(Json::String(s)) => match s.as_str() {
            "true" => Cbor::Bool(true),
            "false" => Cbor::Bool(false),
            other => match other.parse::<i64>() {
                Ok(n) => Cbor::Integer(n.into()),
                Err(_) => match serde_json::from_str::<Json>(other) {
                    Ok(j @ (Json::Object(_) | Json::Array(_))) => json_to_cbor(&j),
                    _ => Cbor::Text(other.into()),
                },
            },
        },
        Some(Json::Number(n)) => Cbor::Integer(n.as_i64().unwrap_or(0).into()),
        Some(other) => json_to_cbor(other),
    };
    let evidence = match arg_str(args, "evidence") {
        Some(e) if !e.is_empty() => Some(repo.store().put_blob(e.as_bytes())?),
        _ => None,
    };
    let mut att = tessra_core::object::Attestation {
        kind: kind.into(),
        subject: Some(serde_bytes::ByteBuf::from(subject.0.to_vec())),
        bodies: None,
        subject_type: Some(subject_type),
        scope: None,
        result,
        env: None,
        verifier: actor.principal(),
        runner: None,
        evidence,
        time: now(),
        sigkind: "ed25519".into(),
        sig: None,
    };
    tessra_core::sig::SignedObject::sign_with(&mut att, &actor.signer.key)?;
    let att_id = repo.store().put(&att)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "attest",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        vec![Effect::Put { id: att_id }],
        now(),
    )?;
    repo.commit_op(&op)?;
    ok(
        json!({ "attestation": att_id.to_hex(), "kind": kind, "subject": subject.to_hex(), "verifier": actor.principal().to_letters(), "counts_for_standards": matches!(actor.kind.as_str(), "daemon" | "external") }),
        &["verify"],
    )
}

fn promote(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let to = arg_str(args, "to").unwrap_or("proposed");
    if to == "landed" && args.get("all").and_then(Json::as_bool).unwrap_or(false) {
        return land_all(repo, actor, args);
    }
    if !matches!(to, "proposed" | "landed") {
        // A target: deploy the trunk head, or the caller's landed revision, to a slice.
        if crate::delivery::target_by_name(repo, to).is_err() {
            return Err(Error::verb(
                "ARGS",
                format!("promote to {to}: not a stage and not a target"),
            ));
        }
        let slice = arg_str(args, "slice").unwrap_or("all");
        if !matches!(slice, "canary" | "all") {
            return Err(Error::verb("ARGS", "slice is canary or all"));
        }
        let revision = match actor_workspace(repo, actor) {
            Some(ws) if actor.kind != "daemon" => Some(current_revision(repo, &ws)?.0),
            _ => None,
        };
        let out = crate::delivery::deploy(repo, actor, to, revision, slice, "deploy")?;
        return ok(
            out,
            &[if slice == "canary" {
                "observe --target <name>"
            } else {
                "status"
            }],
        );
    }
    let ws = require_workspace(repo, actor)?;
    let (rev_id, rev) = current_revision(repo, &ws)?;
    let view = repo.log.current_view()?;
    let landed = landed_revisions(repo.store(), &view)?;
    if landed.contains(&rev_id) {
        return Err(Error::verb("STAGE", "this revision is already landed"));
    }
    crate::risk::ensure_attested(repo, &rev_id, &rev)?;
    let conflicted = conflicted_paths(repo, &rev)?;
    if !conflicted.is_empty() {
        return Err(Error::verb(
            "CONFLICTED",
            format!("resolve conflicts first: {}", conflicted.join(", ")),
        ));
    }
    // Proposing publishes for others to see and help; only landing is
    // gated by the standard (spec/03-operations.md, stages).
    let (_, unmet, _) = if to == "landed" {
        standard_status(repo, &rev_id, &rev)?
    } else {
        (0, Vec::new(), Vec::new())
    };
    if !unmet.is_empty() {
        return Err(Error::unmet(
            to,
            unmet
                .iter()
                .map(|u| (u.clause.clone(), u.reason.clone()))
                .collect(),
        ));
    }
    match to {
        "proposed" => {
            let vs = ViewState::new(repo.store());
            if vs.in_set(&view.proposed, &rev_id)? {
                return ok(
                    json!({ "stage": "proposed", "revision": rev_id.to_hex(), "already": true }),
                    &["promote --to landed"],
                );
            }
            let op = build::build_op(
                &repo.log,
                &actor.signer,
                actor.cap,
                "promote",
                build::args_with_idem(
                    idem_from(args),
                    BTreeMap::from([("to".to_string(), Cbor::Text("proposed".into()))]),
                ),
                vec![Effect::Propose { rev: rev_id }],
                now(),
            )?;
            repo.commit_op(&op)?;
            let hooks = crate::hooks::fire(
                repo,
                &crate::hooks::Event::new(repo, "proposed", rev_id, &rev, json!({}))?,
            )?;
            ok(
                json!({ "stage": "proposed", "revision": rev_id.to_hex(), "hooks": hooks }),
                &["promote --to landed"],
            )
        }
        "landed" => land_one(repo, actor, &ws, rev_id, &rev, args),
        other => Err(Error::verb(
            "ARGS",
            format!("promote to {other}; M1 supports proposed and landed"),
        )),
    }
}

/// Land one revision on trunk as the coordinator: the merge, the
/// standard with cited attestations, the head effect, and the hooks.
fn land_one(
    repo: &mut Repo,
    actor: &mut Actor,
    ws: &Workspace,
    rev_id: ObjectId,
    rev: &Revision,
    args: &Json,
) -> Result<Outcome> {
    let view = repo.log.current_view()?;
    let ws = ws.clone();
    let rev = rev.clone();
    let (head_id, seq) = trunk_head(repo)?;
    let coordinator = view.lines["trunk"].coordinator;
    if actor.principal() != coordinator {
        return Err(Error::verb(
            "SCOPE",
            "only the trunk coordinator lands; propose it and let the coordinator land",
        ));
    }
    let head: Revision = repo.store().get(&head_id)?;
    let (head_snap_id, head_snap) = root_snapshot(repo, &head)?;
    let (rev_snap_id, rev_snap) = root_snapshot(repo, &rev)?;
    let mut semantic_resolved: Vec<String> = Vec::new();
    let (merged_root, merged_index) = if rev.parents.first() == Some(&head_id) {
        (rev_snap.root, rev_snap.index)
    } else {
        let base_id = revision_lca(repo, &head_id, &rev_id)?;
        let (base_root, base_snap_id) = match base_id {
            Some(b) => {
                let br: Revision = repo.store().get(&b)?;
                let (bs_id, bs) = root_snapshot(repo, &br)?;
                (bs.root, Some(bs_id))
            }
            None => (repo.store().put(&tessra_core::object::Tree::empty())?, None),
        };
        let base = tree::flatten(repo.store(), &base_root)?;
        let a = tree::flatten(repo.store(), &head_snap.root)?;
        let b = tree::flatten(repo.store(), &rev_snap.root)?;
        let base_idx = match base_snap_id {
            Some(id) => Some(crate::semantic::index_for_snapshot(repo.store(), &id)?.1),
            None => None,
        };
        let (a_idx_id, a_idx) = crate::semantic::index_for_snapshot(repo.store(), &head_snap_id)?;
        let (_, b_idx) = crate::semantic::index_for_snapshot(repo.store(), &rev_snap_id)?;
        // Renames each side recorded: the change's own ops, and the
        // ops of every trunk revision since the base.
        let ops_b = crate::semantic::recorded_renames(rev.ops.as_deref().unwrap_or(&[]));
        let ops_a = {
            let mut out = Vec::new();
            let mut cur = Some(head_id);
            let mut steps = 0;
            while let Some(id) = cur {
                if Some(id) == base_id || steps > 1000 {
                    break;
                }
                steps += 1;
                let r: Revision = repo.store().get(&id)?;
                out.extend(crate::semantic::recorded_renames(
                    r.ops.as_deref().unwrap_or(&[]),
                ));
                cur = r.parents.first().copied();
            }
            out
        };
        let tm = crate::semantic::merge_trees(
            repo.store(),
            &base,
            &a,
            &b,
            base_idx.as_ref(),
            &a_idx,
            &b_idx,
            &ops_a,
            &ops_b,
        )?;
        if !tm.conflicts.is_empty() {
            return record_conflict(
                repo,
                actor,
                &ws,
                rev_id,
                &rev,
                head_id,
                rev_snap.rules,
                &tm.flat,
                &tm.conflicts,
                args,
            );
        }
        semantic_resolved = tm.resolved;
        let merged = tm.flat;
        let root = tree::build(repo.store(), &merged)?;
        let mut nodes = crate::semantic::build_nodes(repo.store(), &merged, Some((&a, &a_idx)))?;
        let aliases = crate::semantic::reconcile_aliases(&mut nodes, base_idx.as_ref(), &b_idx);
        let idx = crate::semantic::put_index_with_aliases(
            repo.store(),
            root,
            vec![a_idx_id],
            nodes,
            Some(aliases),
        )?;
        (root, Some(idx))
    };
    let merged_snap = repo.store().put(&Snapshot {
        root: merged_root,
        rules: rev_snap.rules,
        // Carry the change's environment onto the landing, so a
        // checkout of trunk can restore its build state (M8).
        env: rev_snap.env,
        index: merged_index,
    })?;
    let landing = Revision {
        id: rev.id,
        prev: Some(rev_id),
        snapshots: BTreeMap::from([("".to_string(), merged_snap)]),
        parents: vec![head_id, rev_id],
        intent: rev.intent,
        title: rev.title.clone(),
        body: rev.body.clone(),
        // The landed revision is the same change; its author stays the change's author.
        author: rev.author,
        time: now(),
        ops: None,
        // What was flagged on the proposed snapshot is still in what lands, so
        // the standard sees at landing the flags that `verify` reported.
        flags: rev.flags.clone(),
    };
    let landing_id = repo.store().put(&landing)?;
    // Landing steps 6 and 7: collect what applies to the landing
    // revision, run only what the merge invalidated, and refuse
    // with the clauses named if the standard still does not hold.
    let (_, mut unmet, mut cited) = standard_status(repo, &landing_id, &landing)?;
    let mut landing_ran: Vec<Json> = Vec::new();
    if !unmet.is_empty() && landing.parents.first() != landing.parents.get(1) {
        landing_ran = run_verifiers(repo, &landing_id, &landing, false, &[])?;
        let (_, u, c) = standard_status(repo, &landing_id, &landing)?;
        unmet = u;
        cited = c;
    }
    if !unmet.is_empty() {
        if unmet
            .iter()
            .any(|u| u.clause.contains("test.weakened") || u.clause.contains("test.deleted"))
        {
            // The author weakened a test once, however many times the frontier
            // re-evaluates the change; charge the revision, not the pass.
            let _ = crate::anomaly::record_once(
                repo,
                &rev.author,
                "landing refused: weakened tests",
                2,
                &rev_id.to_hex(),
            )?;
        }
        // Approvals are asked for on the change's own revision, which
        // is what a human sees and what the landing revision cites as prev.
        let request = ensure_approval_request(repo, &rev_id, &rev, &unmet)?;
        note_approval(&mut unmet, &request);
        return Err(Error::unmet(
            "landed",
            unmet
                .iter()
                .map(|u| (u.clause.clone(), u.reason.clone()))
                .collect(),
        ));
    }
    let vs = ViewState::new(repo.store());
    let mut effects = vec![
        Effect::Put { id: landing_id },
        Effect::Head {
            line: "trunk".into(),
            to: landing_id,
            from: Some(Pointer::Id(head_id).to_value()),
            seq: seq + 1,
            attests: cited.clone(),
            id: None,
        },
    ];
    // Every proposed revision of this change leaves the set, the
    // one landing and any earlier one a conflict or restack left behind.
    for pid in vs.set_members(&view.proposed)? {
        let same_change = pid == rev_id
            || repo
                .store()
                .get::<Revision>(&pid)
                .map(|r| r.id == rev.id)
                .unwrap_or(false);
        if same_change {
            effects.push(Effect::Unpropose { rev: pid });
        }
    }
    effects.push(point_effect(&repo.log, rev.id, landing_id)?);
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "land",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        effects,
        now(),
    )?;
    repo.commit_op(&op)?;
    for w in repo.workspaces.iter_mut() {
        if w.current == Some(rev_id) {
            w.current = Some(landing_id);
            w.base = landing_id;
        }
    }
    let updated: Vec<Workspace> = repo
        .workspaces
        .iter()
        .filter(|w| w.current == Some(landing_id))
        .cloned()
        .collect();
    for w in updated {
        repo.save_workspace(&w)?;
    }
    let restacked = crate::swarm::restack_children(repo, rev_id, landing_id)?;
    close_requests_for(repo, &rev_id)?;
    let hooks = crate::hooks::fire(
        repo,
        &crate::hooks::Event::new(
            repo,
            "landed",
            landing_id,
            &landing,
            json!({ "seq": seq + 1 }),
        )?,
    )?;
    ok(
        json!({
            "stage": "landed", "revision": landing_id.to_hex(), "seq": seq + 1,
            "merged": rev.parents.first() != Some(&head_id),
            "semantic": semantic_resolved,
            "attestations": cited.len(),
            "ran": landing_ran,
            "hooks": hooks,
            "restacked": restacked,
        }),
        &["status"],
    )
}

/// Latest common ancestor of two revisions through `parents`, by minimal combined depth.
fn revision_lca(repo: &Repo, a: &ObjectId, b: &ObjectId) -> Result<Option<ObjectId>> {
    fn depths(repo: &Repo, start: &ObjectId) -> Result<HashMap<ObjectId, u32>> {
        let mut d = HashMap::new();
        let mut q = VecDeque::from([(*start, 0u32)]);
        while let Some((id, depth)) = q.pop_front() {
            if d.contains_key(&id) {
                continue;
            }
            d.insert(id, depth);
            if let Some(bytes) = repo.get_bytes(&id)? {
                if let Ok(r) = cbor::decode::<Revision>(&bytes) {
                    for p in r.parents {
                        q.push_back((p, depth + 1));
                    }
                }
            }
        }
        Ok(d)
    }
    let da = depths(repo, a)?;
    let db = depths(repo, b)?;
    let mut best: Option<(u32, ObjectId)> = None;
    for (id, x) in &da {
        if let Some(y) = db.get(id) {
            let score = x + y;
            if best
                .map(|(s, bid)| score < s || (score == s && *id < bid))
                .unwrap_or(true)
            {
                best = Some((score, *id));
            }
        }
    }
    Ok(best.map(|(_, id)| id))
}

fn undo(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    // The owner's recent ops are standards, grants, and landings.
    require_owner_credential(repo, actor, "undoing as the owner")?;
    // Sessions of the same durable agent are one identity for undo.
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let me = actor.principal();
    let my_parent = tessra_oplog::verify::resolve_principal(repo.store(), &vs, &view, &me)?
        .and_then(|(_, p)| if p.kind == "session" { p.parent } else { None });
    let mut allowed: Vec<EntityId> = vec![me];
    if let Some(agent) = my_parent {
        for (id, ptr) in vs.entities(&view)? {
            if let Pointer::Id(oid) = ptr {
                if let Some(bytes) = repo.get_bytes(&oid)? {
                    if cbor::peek_tag(&bytes).ok().as_deref() == Some("principal") {
                        if let Ok(p) = cbor::decode::<tessra_core::object::Principal>(&bytes) {
                            if p.kind == "session" && p.parent == Some(agent) {
                                allowed.push(id);
                            }
                        }
                    }
                }
            }
        }
    }
    let target = match arg_str(args, "op") {
        Some(h) => ObjectId::from_hex(h)?,
        None => {
            let mut best: Option<(i64, ObjectId)> = None;
            for h in repo.log.heads() {
                for id in repo.log.ancestors(&h)? {
                    let op = repo.log.get_op(&id)?;
                    if allowed.contains(&op.author)
                        && !matches!(op.kind.as_str(), "undo" | "init" | "session" | "principal")
                        && best.map(|(t, _)| op.time > t).unwrap_or(true)
                    {
                        best = Some((op.time, id));
                    }
                }
            }
            best.map(|(_, id)| id)
                .ok_or_else(|| Error::verb("NOTHING", "no op of yours to undo"))?
        }
    };
    let target_op = repo.log.get_op(&target)?;
    let op = build::undo_as(
        &repo.log,
        &actor.signer,
        actor.cap,
        &allowed,
        &target,
        idem_from(args),
        now(),
    )?;
    let id = repo.commit_op(&op)?;
    // If the undone op moved this workspace's change, fall back to what it moved from
    // and materialize that revision so the files match the record.
    let mut restored = None;
    if let Some(ws) = actor_workspace(repo, actor) {
        for e in &target_op.effects {
            if let Effect::Point { to, from, .. } = e {
                if Some(*to) == ws.current {
                    let fallback = match from.as_ref().and_then(Pointer::from_value) {
                        Some(Pointer::Id(prev)) => prev,
                        _ => {
                            let r: Revision = repo.store().get(to)?;
                            r.parents.first().copied().unwrap_or(ws.base)
                        }
                    };
                    let r: Revision = repo.store().get(&fallback)?;
                    let (_, snap) = root_snapshot(repo, &r)?;
                    fs::materialize(repo.store(), &snap.root, &ws_path(&ws), false)?;
                    if let Some(w) = repo.workspace_mut(&ws.id) {
                        w.current = Some(fallback);
                        let w2 = w.clone();
                        repo.save_workspace(&w2)?;
                    }
                    restored = Some(fallback.to_hex());
                }
            }
        }
    }
    ok(
        json!({ "undone": target.to_hex(), "kind": target_op.kind, "op": id.to_hex(), "restored_revision": restored }),
        &["status"],
    )
}

/// Paths whose entries are unresolved conflicts in the revision's root snapshot.
fn conflicted_paths(repo: &Repo, rev: &Revision) -> Result<Vec<String>> {
    let (_, snap) = root_snapshot(repo, rev)?;
    let flat = tree::flatten(repo.store(), &snap.root)?;
    Ok(flat
        .iter()
        .filter(|(_, l)| l.is_conflict())
        .map(|(p, _)| p.clone())
        .collect())
}

/// A landing that conflicts: store the merged snapshot with conflict entries
/// as a new revision of the change, put it in the workspace with markers and
/// sidecars, and open a task. Nothing lands.
#[allow(clippy::too_many_arguments)]
/// Land a revision as the coordinator, for hooks and the frontier.
pub fn land_revision(
    repo: &mut Repo,
    actor: &mut Actor,
    ws: &Workspace,
    rev_id: ObjectId,
    rev: &Revision,
) -> Result<Outcome> {
    land_one(repo, actor, ws, rev_id, rev, &json!({}))
}

/// Revert a landed change: a new change whose snapshot is the head with
/// the landed change's edits undone at unit granularity, attributed to the
/// reverter with the original named, opened as a task. The coordinator
/// lands it at once; anyone else proposes it.
fn revert(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    // The owner's revert lands at once and a rollback moves production.
    require_owner_credential(repo, actor, "reverting as the owner")?;
    if let Some(target) = arg_str(args, "target") {
        if actor.kind != "daemon" {
            return Err(Error::verb("SCOPE", "only the owner rolls a target back"));
        }
        let why = arg_str(args, "reason")
            .unwrap_or("rolled back on request")
            .to_string();
        let out = crate::delivery::rollback(repo, actor, target, &why)?;
        return ok(out, &["target"]);
    }
    let change = arg_str(args, "change")
        .ok_or_else(|| Error::verb("ARGS", "revert needs change or target"))?;
    let reason = arg_str(args, "reason").unwrap_or("reverted").to_string();
    let history = trunk_history(repo, 1000)?;
    let (r_id, r) = history
        .iter()
        .find(|(_, r)| r.id.matches_prefix(change))
        .cloned()
        .ok_or_else(|| {
            Error::verb(
                "NOT_FOUND",
                format!("change {change} has not landed on trunk"),
            )
        })?;
    let before_id = *r
        .parents
        .first()
        .ok_or_else(|| Error::verb("REVERT", "the first revision on trunk cannot be reverted"))?;
    let before: Revision = repo.store().get(&before_id)?;
    let (head_id, _) = trunk_head(repo)?;
    let head: Revision = repo.store().get(&head_id)?;
    let (r_snap_id, r_snap) = root_snapshot(repo, &r)?;
    let (before_snap_id, before_snap) = root_snapshot(repo, &before)?;
    let (head_snap_id, head_snap) = root_snapshot(repo, &head)?;
    // Base is the landed revision; one side is the head, the other the
    // state before it: the merge is the head without the change.
    let base = tree::flatten(repo.store(), &r_snap.root)?;
    let a = tree::flatten(repo.store(), &head_snap.root)?;
    let b = tree::flatten(repo.store(), &before_snap.root)?;
    let (_, base_idx) = crate::semantic::index_for_snapshot(repo.store(), &r_snap_id)?;
    let (a_idx_id, a_idx) = crate::semantic::index_for_snapshot(repo.store(), &head_snap_id)?;
    let (_, b_idx) = crate::semantic::index_for_snapshot(repo.store(), &before_snap_id)?;
    let tm = crate::semantic::merge_trees(
        repo.store(),
        &base,
        &a,
        &b,
        Some(&base_idx),
        &a_idx,
        &b_idx,
        &[],
        &[],
    )?;
    if !tm.conflicts.is_empty() {
        return Err(Error::verb(
            "CONFLICT",
            format!(
                "later changes built on this one; reverting conflicts on: {}",
                tm.conflicts.join(", ")
            ),
        ));
    }
    let root = tree::build(repo.store(), &tm.flat)?;
    let nodes = crate::semantic::build_nodes(repo.store(), &tm.flat, Some((&a, &a_idx)))?;
    let idx = crate::semantic::put_index(repo.store(), root, vec![a_idx_id], nodes)?;
    let snap = repo.store().put(&Snapshot {
        root,
        rules: head_snap.rules,
        env: None,
        index: Some(idx),
    })?;
    let original_author = repo
        .principal_name(&r.author)
        .unwrap_or_else(|| r.author.to_letters());
    let rev = Revision {
        id: EntityId::random(),
        prev: None,
        snapshots: BTreeMap::from([("".to_string(), snap)]),
        parents: vec![head_id],
        intent: r.intent,
        title: format!("revert: {}", r.title),
        body: Some(format!(
            "reverts change {} by {original_author}: {reason}",
            r.id.to_letters()
        )),
        author: actor.principal(),
        time: now(),
        ops: None,
        flags: None,
    };
    let rev_id = repo.store().put(&rev)?;
    let task = Memory {
        id: EntityId::random(),
        prev: None,
        kind: "task".into(),
        scope: MemoryScope {
            kind: match r.intent {
                Some(_) => "intent".into(),
                None => "repo".into(),
            },
            r#ref: match r.intent {
                Some(i) => Cbor::Bytes(i.0.to_vec()),
                None => Cbor::Text(String::new()),
            },
        },
        body: format!(
            "change {} ({}) was reverted: {reason}. Investigate and land a fix.",
            r.id.to_letters(),
            r.title
        ),
        anchor: None,
        confidence: 1000,
        author: actor.principal(),
        time: now(),
        expires: None,
        proposed: None,
        status: "active".into(),
        links: Some(vec![r_id, rev_id]),
        visibility: "shared".into(),
    };
    let task_id = repo.store().put(&task)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "revert",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        vec![
            Effect::Put { id: rev_id },
            Effect::Point {
                entity: rev.id,
                to: rev_id,
                from: None,
            },
            Effect::Put { id: task_id },
            Effect::Point {
                entity: task.id,
                to: task_id,
                from: None,
            },
        ],
        now(),
    )?;
    repo.commit_op(&op)?;
    // A workspace of the actor's holds the reverting change; the coordinator lands it now.
    let ws_id = EntityId::random();
    let path = paths::workspaces_dir_for(&repo.repo_id).join(ws_id.to_letters());
    fs::materialize(repo.store(), &root, &path, false)?;
    let ws = Workspace {
        id: ws_id,
        principal: actor.principal(),
        base: head_id,
        current: Some(rev_id),
        paths: BTreeMap::from([("".to_string(), path.display().to_string())]),
        env: None,
        created: now(),
        expires: None,
    };
    repo.save_workspace(&ws)?;
    repo.workspaces.push(ws.clone());
    let view = repo.log.current_view()?;
    let is_coordinator = view.lines.get("trunk").map(|l| l.coordinator) == Some(actor.principal());
    let outcome = if is_coordinator {
        let o = land_one(repo, actor, &ws, rev_id, &rev, args);
        if o.is_ok() {
            let _ = std::fs::remove_dir_all(&path);
            repo.remove_workspace(&ws_id)?;
        }
        o?
    } else {
        let op = build::build_op(
            &repo.log,
            &actor.signer,
            actor.cap,
            "promote",
            build::args_with_idem(
                EntityId::random().0,
                BTreeMap::from([("to".to_string(), Cbor::Text("proposed".into()))]),
            ),
            vec![Effect::Propose { rev: rev_id }],
            now(),
        )?;
        repo.commit_op(&op)?;
        actor.workspace = Some(ws_id);
        Outcome {
            result: json!({ "stage": "proposed" }),
            next: vec![],
        }
    };
    ok(
        json!({
            "reverted": r.id.to_letters(), "title": r.title, "change": rev.id.to_letters(), "revision": rev_id.to_hex(),
            "stage": outcome.result.get("stage"), "task": task.id.to_letters(), "semantic": tm.resolved,
            "attestations": outcome.result.get("attestations"),
        }),
        &["status"],
    )
}

/// The integration frontier: land every proposed revision that meets the
/// standard, oldest proposal first, each merged onto the head the one
/// before it produced. Refusals and conflicts are reported and do not stop
/// the rest. Coordinator only.
fn land_all(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let view = repo.log.current_view()?;
    let coordinator = view.lines.get("trunk").map(|l| l.coordinator);
    if Some(actor.principal()) != coordinator {
        return Err(Error::verb(
            "SCOPE",
            "only the trunk coordinator lands the frontier",
        ));
    }
    let vs = ViewState::new(repo.store());
    let landed = landed_revisions(repo.store(), &view)?;
    let mut proposed: Vec<(ObjectId, Revision)> = Vec::new();
    for id in vs.set_members(&view.proposed)? {
        if landed.contains(&id) {
            continue;
        }
        if let Ok(r) = repo.store().get::<Revision>(&id) {
            proposed.push((id, r));
        }
    }
    proposed.sort_by_key(|(_, r)| r.time);
    let started = std::time::Instant::now();
    let mut results = Vec::new();
    let mut landed_n = 0usize;
    let mut attempted: HashSet<EntityId> = HashSet::new();
    for (rev_id, rev) in proposed {
        // One attempt per change, whichever proposed revision named it.
        if !attempted.insert(rev.id) {
            continue;
        }
        // A landing may have restacked this change to a newer revision.
        let current = {
            let v = repo.log.current_view()?;
            let vs = ViewState::new(repo.store());
            vs.entity(&v, &rev.id)?.and_then(|p| p.single().ok())
        };
        let (rev_id, rev) = match current {
            Some(cur) if cur != rev_id => (cur, repo.store().get::<Revision>(&cur)?),
            _ => (rev_id, rev),
        };
        let Some(ws) = repo
            .workspaces
            .iter()
            .find(|w| w.current == Some(rev_id))
            .cloned()
        else {
            results.push(json!({ "change": rev.id.to_letters(), "title": rev.title, "landed": false, "why": "no workspace holds this revision" }));
            continue;
        };
        match land_one(repo, actor, &ws, rev_id, &rev, args) {
            Ok(o) => {
                let is_landed = o.result.get("stage").and_then(Json::as_str) == Some("landed");
                if is_landed {
                    landed_n += 1;
                }
                results.push(json!({
                    "change": rev.id.to_letters(), "title": rev.title, "landed": is_landed,
                    "revision": o.result.get("revision"),
                    "seq": o.result.get("seq"), "semantic": o.result.get("semantic"),
                    "conflicts": o.result.get("conflicts"), "restacked": o.result.get("restacked"),
                }));
            }
            Err(e) => {
                results.push(json!({ "change": rev.id.to_letters(), "title": rev.title, "landed": false, "code": e.code(), "why": e.to_string() }));
            }
        }
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let per_hour = if elapsed_ms > 0 {
        (landed_n as f64) * 3_600_000.0 / (elapsed_ms as f64)
    } else {
        0.0
    };
    ok(
        json!({
            "landed": landed_n, "considered": results.len(), "elapsed_ms": elapsed_ms,
            "landings_per_hour": per_hour.round(), "results": results,
        }),
        &["status"],
    )
}

/// Partition an intent across agents. Owner or any planner with the verbs.
fn plan_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let title = arg_str(args, "intent").ok_or_else(|| Error::verb("ARGS", "plan needs intent"))?;
    let list = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut paths = list("paths");
    if paths.is_empty() {
        paths.push("**".into());
    }
    let n = args
        .get("agents")
        .and_then(Json::as_u64)
        .unwrap_or(8)
        .clamp(1, 500) as usize;
    let mut names = list("names");
    if names.is_empty() {
        names = (1..=n).map(|i| format!("agent{i:02}")).collect();
    }
    let ops = args.get("ops").and_then(Json::as_u64).unwrap_or(2000);
    let result = crate::swarm::plan(repo, actor, title, &paths, &names, ops)?;
    ok(result, &["status"])
}

/// Revoke an agent and unwind its unlanded work in one op. Owner only.
fn revoke_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "only the owner revokes"));
    }
    require_owner_credential(repo, actor, "revoking an agent")?;
    let name =
        arg_str(args, "name").ok_or_else(|| Error::verb("ARGS", "revoke needs --name <agent>"))?;
    let agent = repo.agent_id(name)?;
    let result = crate::swarm::revoke_agent(repo, actor, agent)?;
    ok(result, &["status"])
}

/// Speculation: several candidate edits, each materialized on the current
/// revision as its own revision, verified, scored, and ranked. `keep`
/// writes one candidate into the workspace.
fn try_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let ws = require_workspace(repo, actor)?;
    let candidates: Vec<Json> = args
        .get("candidates")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    if candidates.is_empty() {
        return Err(Error::verb(
            "ARGS",
            "try needs candidates: [{path, content} | {path, old, new}, ...]",
        ));
    }
    let (cur_id, cur) = current_revision(repo, &ws)?;
    let (_, cur_snap) = root_snapshot(repo, &cur)?;
    let (cur_idx_id, cur_idx) = crate::semantic::index_for_revision(repo.store(), &cur)?;
    let base_flat = tree::flatten(repo.store(), &cur_snap.root)?;
    let trunk_flat = if ws.base == cur_id {
        base_flat.clone()
    } else {
        base_flat_of(repo, &ws)?
    };
    let rules: TrackingRules = repo.store().get(&cur_snap.rules)?;
    let mut ranked: Vec<(u64, Json)> = Vec::new();
    let mut edits: Vec<Option<(String, Vec<u8>)>> = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        let started = std::time::Instant::now();
        let dir = paths::workspaces_dir_for(&repo.repo_id)
            .join(format!("try-{}-{i}", ws.id.to_letters()));
        let _ = std::fs::remove_dir_all(&dir);
        fs::materialize(repo.store(), &cur_snap.root, &dir, false)?;
        let path = c
            .get("path")
            .and_then(Json::as_str)
            .ok_or_else(|| Error::verb("ARGS", format!("candidate {i} needs path")))?;
        if path.contains("..") {
            return Err(Error::verb("ARGS", "path may not contain .."));
        }
        let full = dir.join(path);
        let new_text: Vec<u8> = if let Some(content) = c.get("content").and_then(Json::as_str) {
            content.as_bytes().to_vec()
        } else if let (Some(old), Some(new)) = (
            c.get("old").and_then(Json::as_str),
            c.get("new").and_then(Json::as_str),
        ) {
            let text = std::fs::read_to_string(&full)
                .map_err(|e| Error::verb("EDIT", format!("{path}: {e}")))?;
            if !text.contains(old) {
                let _ = std::fs::remove_dir_all(&dir);
                ranked.push((
                    u64::MAX,
                    json!({ "candidate": i, "error": format!("old text not found in {path}") }),
                ));
                edits.push(None);
                continue;
            }
            text.replacen(old, new, 1).into_bytes()
        } else {
            return Err(Error::verb(
                "ARGS",
                format!("candidate {i} needs content, or old and new"),
            ));
        };
        if let Some(d) = full.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&full, &new_text)?;
        edits.push(Some((path.to_string(), new_text)));
        repo.store().begin_batch();
        let out = fs::snapshot_dir(
            repo.store(),
            &dir,
            &rules,
            actor.write_paths.as_deref(),
            Some(&base_flat),
        );
        repo.store().end_batch()?;
        let mut out = out?;
        fs::scope_secrets_to_changes(&mut out.flags, &out.flat, &trunk_flat);
        let _ = std::fs::remove_dir_all(&dir);
        if out.tree == cur_snap.root {
            ranked.push((
                u64::MAX - 1,
                json!({ "candidate": i, "path": path, "unchanged": true }),
            ));
            continue;
        }
        let nodes =
            crate::semantic::build_nodes(repo.store(), &out.flat, Some((&base_flat, &cur_idx)))?;
        let index_id = crate::semantic::put_index(repo.store(), out.tree, vec![cur_idx_id], nodes)?;
        let snap_id = repo.store().put(&Snapshot {
            root: out.tree,
            rules: cur_snap.rules,
            env: None,
            index: Some(index_id),
        })?;
        let title = c
            .get("title")
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("try: candidate {i}"));
        let rev = Revision {
            id: EntityId::random(),
            prev: None,
            snapshots: BTreeMap::from([("".to_string(), snap_id)]),
            parents: vec![cur_id],
            intent: cur.intent,
            title: title.clone(),
            body: Some(format!("candidate {i} of a try on {}", cur_id.to_hex())),
            author: actor.principal(),
            time: now(),
            ops: None,
            flags: if out.flags.is_empty() {
                None
            } else {
                Some(out.flags.clone())
            },
        };
        let rev_id = repo.store().put(&rev)?;
        let op = build::build_op(
            &repo.log,
            &actor.signer,
            actor.cap,
            "try",
            build::args_with_idem(EntityId::random().0, BTreeMap::new()),
            vec![
                Effect::Put { id: rev_id },
                Effect::Point {
                    entity: rev.id,
                    to: rev_id,
                    from: None,
                },
            ],
            now(),
        )?;
        repo.commit_op(&op)?;
        let ran = run_verifiers(repo, &rev_id, &rev, false, &[])?;
        let (_, risk) = crate::risk::ensure_attested(repo, &rev_id, &rev)?;
        let (met, unmet, _) = standard_status(repo, &rev_id, &rev)?;
        let failed: usize = ran
            .iter()
            .filter_map(|r| r.get("failed").and_then(Json::as_u64))
            .sum::<u64>() as usize;
        let passed: usize = ran
            .iter()
            .filter_map(|r| r.get("passed").and_then(Json::as_u64))
            .sum::<u64>() as usize;
        let changed = crate::verifiers::changed_units(
            &cur_idx,
            &repo
                .store()
                .get::<tessra_core::object::NodeIndex>(&index_id)?,
        )
        .len();
        // Lower is better: unmet clauses, then failed tests, then risk, then size.
        let score = (unmet.len() as u64) * 1_000_000
            + (failed as u64) * 10_000
            + (risk.score as u64) * 100
            + changed as u64;
        ranked.push((score, json!({
            "candidate": i, "title": title, "path": path, "revision": rev_id.to_hex(), "change": rev.id.to_letters(),
            "met": met, "unmet": unmet.iter().map(|u| json!({ "clause": u.clause, "reason": u.reason })).collect::<Vec<_>>(),
            "tests": { "passed": passed, "failed": failed }, "risk": { "level": risk.level, "score": risk.score },
            "changed_units": changed, "elapsed_ms": started.elapsed().as_millis() as u64,
        })));
    }
    ranked.sort_by_key(|(sc, j)| (*sc, j.get("candidate").and_then(Json::as_u64).unwrap_or(0)));
    let mut list: Vec<Json> = Vec::new();
    for (rank, (_, mut j)) in ranked.into_iter().enumerate() {
        j["rank"] = json!(rank + 1);
        list.push(j);
    }
    let best = list.first().cloned();
    let mut kept = Json::Null;
    if let Some(k) = args.get("keep").and_then(Json::as_u64) {
        if let Some(Some((path, text))) = edits.get(k as usize) {
            let full = ws_path(&ws).join(path);
            if let Some(d) = full.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&full, text)?;
            kept = json!({ "candidate": k, "path": path });
        }
    }
    ok(
        json!({ "ranked": list, "best": best, "kept": kept, "how": "candidates are revisions of their own; try again with keep=<candidate> to write the winner into your workspace, then snapshot" }),
        &["try --keep <candidate>", "snapshot"],
    )
}

/// Ask the humans: open a question memory for an unmet approval on a
/// revision, once, and deliver it through every channel. Returns the
/// request id and where it went.
fn needs_human(u: &standard::Unmet) -> bool {
    // An approval clause always asks; a judged clause only when a judge
    // said no, not while the judges have yet to look.
    u.clause.contains("approved(")
        || (u.clause.contains("judge(") && u.reason.starts_with("a judge said no"))
}

fn ensure_approval_request(
    repo: &mut Repo,
    rev_id: &ObjectId,
    rev: &Revision,
    unmet: &[standard::Unmet],
) -> Result<Option<Json>> {
    if !unmet.iter().any(needs_human) {
        return Ok(None);
    }
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    // One request per revision.
    let mut channels: Vec<tessra_core::object::Channel> = Vec::new();
    let mut existing: Option<EntityId> = None;
    for (id, ptr) in vs.entities(&view)? {
        let Ok(oid) = ptr.single() else { continue };
        let Some(bytes) = repo.store().get_bytes(&oid)? else {
            continue;
        };
        match cbor::peek_tag(&bytes).ok().as_deref() {
            Some("memory") => {
                if let Ok(m) = cbor::decode::<Memory>(&bytes) {
                    if m.kind == "question"
                        && m.status == "active"
                        && m.links.iter().flatten().any(|l| l == rev_id)
                        && m.body.starts_with("Approval needed")
                    {
                        existing = Some(id);
                    }
                }
            }
            Some("channel") => {
                if let Ok(c) = cbor::decode::<tessra_core::object::Channel>(&bytes) {
                    channels.push(c);
                }
            }
            _ => {}
        }
    }
    let clauses: Vec<String> = unmet
        .iter()
        .filter(|u| needs_human(u))
        .map(|u| format!("{}: {}", u.clause, u.reason))
        .collect();
    let judges = judges_reasoning(repo, rev_id, rev)?;
    let risk = crate::risk::ensure_attested(repo, rev_id, rev)?.1;
    let author = repo
        .principal_name(&rev.author)
        .unwrap_or_else(|| rev.author.to_letters());
    let request_id = match existing {
        Some(id) => id,
        None => {
            let id = EntityId::random();
            let humans: Vec<String> = channels
                .iter()
                .flat_map(|c| c.principals.iter())
                .filter_map(|p| repo.principal_name(p))
                .collect();
            let m = Memory {
                id,
                prev: None,
                kind: "question".into(),
                scope: MemoryScope {
                    kind: match rev.intent { Some(_) => "intent".into(), None => "repo".into() },
                    r#ref: match rev.intent { Some(i) => Cbor::Bytes(i.0.to_vec()), None => Cbor::Text(String::new()) },
                },
                body: format!(
                    "Approval needed for change {} ({}) by {author}: {}. Risk {} ({}).{} Reply: tessra --as <{}> approve --request {}",
                    rev.id.to_letters(), rev.title, clauses.join("; "), risk.level, risk.score,
                    if judges.is_empty() { String::new() } else { format!(" Judges: {}.", judges.iter().map(|j| format!("{} said {} ({}): {}", j["who"].as_str().unwrap_or(""), if j["verdict"].as_bool().unwrap_or(false) { "yes" } else { "no" }, j["confidence"], j["reasoning"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ")) },
                    if humans.is_empty() { "human".to_string() } else { humans.join("|") }, id.to_letters()
                ),
                anchor: None,
                confidence: 1000,
                author: repo.daemon,
                time: now(),
                expires: None,
                proposed: None,
                status: "active".into(),
                links: Some(vec![*rev_id]),
                visibility: "shared".into(),
            };
            let oid = repo.store().put(&m)?;
            let signer = repo.daemon_signer();
            let op = build::build_op(
                &repo.log,
                &signer,
                None,
                "question",
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
            id
        }
    };
    let payload = json!({
        "request": request_id.to_letters(), "change": rev.id.to_letters(), "revision": rev_id.to_hex(), "title": rev.title,
        "author": author, "clauses": clauses, "risk": crate::risk::to_json(&risk), "judges": judges,
        "exception": unmet.iter().any(|u| u.clause.contains("judge(")),
        "reply": format!("tessra --as <human> approve --request {}", request_id.to_letters()),
    });
    let delivered = if existing.is_none() {
        deliver_to_channels(repo, &payload, None)?
    } else {
        Vec::new()
    };
    Ok(Some(
        json!({ "request": request_id.to_letters(), "delivered": delivered, "reply": payload["reply"] }),
    ))
}

/// Deliver a message to every channel, or to one by name: an inbox gets a
/// JSON file, a webhook a POST.
pub fn deliver_to_channels(repo: &Repo, payload: &Json, only: Option<&str>) -> Result<Vec<Json>> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let mut delivered = Vec::new();
    let stamp = now();
    for (_, ptr) in vs.entities(&view)? {
        let Ok(oid) = ptr.single() else { continue };
        let Some(bytes) = repo.store().get_bytes(&oid)? else {
            continue;
        };
        if cbor::peek_tag(&bytes).ok().as_deref() != Some("channel") {
            continue;
        }
        let Ok(c) = cbor::decode::<tessra_core::object::Channel>(&bytes) else {
            continue;
        };
        let name = c
            .config
            .get("name")
            .and_then(|v| v.as_text())
            .unwrap_or("channel")
            .to_string();
        if only.is_some_and(|o| o != name) {
            continue;
        }
        let file_stem = payload
            .get("request")
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{}-{stamp}",
                    payload
                        .get("event")
                        .and_then(Json::as_str)
                        .unwrap_or("message")
                )
            });
        let outcome = match c.kind.as_str() {
            "inbox" => {
                let dir = c
                    .config
                    .get("path")
                    .and_then(|v| v.as_text())
                    .unwrap_or(".");
                let p = std::path::Path::new(dir).join(format!("{file_stem}.json"));
                std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&p, serde_json::to_vec_pretty(payload).unwrap_or_default()))
                    .map(|_| json!({ "channel": name, "kind": "inbox", "file": p.display().to_string() }))
                    .map_err(|e| e.to_string())
            }
            "webhook" => {
                let url = c.config.get("url").and_then(|v| v.as_text()).unwrap_or("");
                let body = serde_json::to_vec(payload).unwrap_or_default();
                crate::hooks::http_post(
                    url,
                    &[("Content-Type".into(), "application/json".into())],
                    &body,
                    std::time::Duration::from_secs(10),
                )
                .map(|(status, _)| json!({ "channel": name, "kind": "webhook", "status": status }))
                .map_err(|e| e.to_string())
            }
            other => Err(format!("channel kind {other} is not available yet")),
        };
        delivered.push(match outcome {
            Ok(j) => j,
            Err(e) => json!({ "channel": name, "error": e }),
        });
    }
    Ok(delivered)
}

/// A landed revision needs no more asking: close its open requests.
fn close_requests_for(repo: &mut Repo, rev_id: &ObjectId) -> Result<()> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let mut effects = Vec::new();
    for (id, ptr) in vs.entities(&view)? {
        let Ok(oid) = ptr.single() else { continue };
        let Ok(m) = repo.store().get::<Memory>(&oid) else {
            continue;
        };
        if m.kind == "question"
            && m.status == "active"
            && m.body.starts_with("Approval needed")
            && m.links.iter().flatten().any(|l| l == rev_id)
        {
            let mut closed = m.clone();
            closed.prev = Some(oid);
            closed.status = "resolved".into();
            closed.body = format!("{} Landed.", m.body);
            let new_oid = repo.store().put(&closed)?;
            effects.push(Effect::Put { id: new_oid });
            effects.push(Effect::Point {
                entity: id,
                to: new_oid,
                from: Some(Pointer::Id(oid).to_value()),
            });
        }
    }
    if !effects.is_empty() {
        let signer = repo.daemon_signer();
        let op = build::build_op(
            &repo.log,
            &signer,
            None,
            "question",
            build::args_with_idem(EntityId::random().0, BTreeMap::new()),
            effects,
            now(),
        )?;
        repo.commit_op(&op)?;
    }
    Ok(())
}

/// What the judges said about a revision: who, model, verdict, confidence, reasoning.
fn judges_reasoning(repo: &Repo, rev_id: &ObjectId, rev: &Revision) -> Result<Vec<Json>> {
    let mut out = Vec::new();
    let mut subjects: Vec<Vec<u8>> = vec![rev_id.as_bytes().to_vec()];
    if let Some(p) = rev.prev {
        subjects.push(p.as_bytes().to_vec());
    }
    for s in subjects {
        for (_, a) in crate::verifiers::attestations_on(repo.store(), &s)? {
            if !a.kind.starts_with("judge.") {
                continue;
            }
            let (conf, reasoning) = standard::judge_result(&a.result);
            let verdict = matches!(a.result, Cbor::Bool(true))
                || matches!(&a.result, Cbor::Map(m) if m.iter().any(|(k, v)| matches!(k, Cbor::Text(t) if t == "ok") && matches!(v, Cbor::Bool(true))));
            let who = repo
                .principal_name(&a.verifier)
                .unwrap_or_else(|| a.verifier.to_letters());
            let model = {
                let view = repo.log.current_view()?;
                let vs = ViewState::new(repo.store());
                tessra_oplog::verify::resolve_principal(repo.store(), &vs, &view, &a.verifier)?
                    .and_then(|(_, p)| p.model)
            };
            out.push(json!({ "rubric": a.kind.trim_start_matches("judge."), "who": who, "model": model, "verdict": verdict, "confidence": conf, "reasoning": reasoning }));
        }
    }
    Ok(out)
}

/// Attach the approval request to the unmet clauses that need one.
fn note_approval(unmet: &mut [standard::Unmet], request: &Option<Json>) {
    if let Some(r) = request {
        let id = r.get("request").and_then(Json::as_str).unwrap_or("");
        let where_: Vec<String> = r
            .get("delivered")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.get("channel").and_then(Json::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for u in unmet.iter_mut() {
            if needs_human(u) {
                u.reason = format!("{}; request {id}{}; a human replies with `tessra --as <name> approve --request {id}`", u.reason,
                    if where_.is_empty() { String::new() } else { format!(" delivered to {}", where_.join(", ")) });
            }
        }
    }
}

/// A human answers an approval request: a key-signed approval.human
/// attestation on the revision the request names, and the question closes.
fn approve_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    if actor.kind != "human" {
        return Err(Error::verb("SCOPE", "only a human principal approves; the owner grants one with `tessra grant --human <name>` and acts with `--as <name>`"));
    }
    let request =
        arg_str(args, "request").ok_or_else(|| Error::verb("ARGS", "approve needs request"))?;
    let decision = !args.get("no").and_then(Json::as_bool).unwrap_or(false);
    let note = arg_str(args, "note").map(str::to_string);
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let mut found: Option<(EntityId, ObjectId, Memory)> = None;
    for (id, ptr) in vs.entities(&view)? {
        if !id.matches_prefix(request) {
            continue;
        }
        let Ok(oid) = ptr.single() else { continue };
        if let Ok(m) = repo.store().get::<Memory>(&oid) {
            if m.kind == "question" {
                found = Some((id, oid, m));
                break;
            }
        }
    }
    let (qid, q_oid, q) =
        found.ok_or_else(|| Error::verb("NOT_FOUND", format!("no approval request {request}")))?;
    let rev_id = q
        .links
        .iter()
        .flatten()
        .next()
        .copied()
        .ok_or_else(|| Error::verb("REQUEST", "the request names no revision"))?;
    let rev: Revision = repo.store().get(&rev_id)?;
    let mut scope = BTreeMap::from([
        ("request".to_string(), Cbor::Text(qid.to_letters())),
        (
            "nonce".to_string(),
            Cbor::Bytes(EntityId::random().0.to_vec()),
        ),
    ]);
    if let Some(n) = &note {
        scope.insert("note".to_string(), Cbor::Text(n.clone()));
    }
    let mut att = tessra_core::object::Attestation {
        kind: "approval.human".into(),
        subject: Some(serde_bytes::ByteBuf::from(rev_id.0.to_vec())),
        bodies: None,
        subject_type: Some("revision".into()),
        scope: Some(scope),
        result: Cbor::Bool(decision),
        env: None,
        verifier: actor.principal(),
        runner: None,
        evidence: None,
        time: now(),
        sigkind: "ed25519".into(),
        sig: None,
    };
    tessra_core::sig::SignedObject::sign_with(&mut att, &actor.signer.key)?;
    let att_id = repo.store().put(&att)?;
    let mut closed = q.clone();
    closed.prev = Some(q_oid);
    closed.status = if decision {
        "resolved".into()
    } else {
        "declined".into()
    };
    closed.body = format!(
        "{} Answered by {}: {}{}",
        q.body,
        repo.principal_name(&actor.principal()).unwrap_or_default(),
        if decision { "approved" } else { "declined" },
        note.as_ref().map(|n| format!(" ({n})")).unwrap_or_default()
    );
    let closed_oid = repo.store().put(&closed)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "attest",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        vec![
            Effect::Put { id: att_id },
            Effect::Put { id: closed_oid },
            Effect::Point {
                entity: qid,
                to: closed_oid,
                from: Some(Pointer::Id(q_oid).to_value()),
            },
        ],
        now(),
    )?;
    repo.commit_op(&op)?;
    ok(
        json!({ "approved": decision, "request": qid.to_letters(), "change": rev.id.to_letters(), "revision": rev_id.to_hex(), "attestation": att_id.to_hex(), "keysigned": true, "by": actor.principal().to_letters() }),
        &["promote --to landed --all"],
    )
}

/// Define a channel humans are reachable on (owner only), or list them.
fn channel_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let name = arg_str(args, "name");
    if name.is_none() {
        let mut out = Vec::new();
        for (id, ptr) in vs.entities(&view)? {
            let Ok(oid) = ptr.single() else { continue };
            if let Ok(c) = repo.store().get::<tessra_core::object::Channel>(&oid) {
                out.push(json!({ "id": id.to_letters(), "kind": c.kind, "name": c.config.get("name").and_then(|v| v.as_text()), "config": cbor_to_json(&Cbor::Map(c.config.iter().map(|(k, v)| (Cbor::Text(k.clone()), v.clone())).collect())), "principals": c.principals.iter().map(|p| repo.principal_name(p).unwrap_or_else(|| p.to_letters())).collect::<Vec<_>>(), "signing": c.signing }));
            }
        }
        return ok(
            json!({ "channels": out }),
            &["channel --name oncall --kind inbox --path <dir> --principals <human>"],
        );
    }
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "only the owner defines channels"));
    }
    require_owner_credential(repo, actor, "defining a channel")?;
    let name = name.unwrap_or_default();
    let kind = arg_str(args, "kind").unwrap_or("inbox");
    let mut config: BTreeMap<String, Cbor> =
        BTreeMap::from([("name".to_string(), Cbor::Text(name.into()))]);
    match kind {
        "inbox" => {
            let p = arg_str(args, "path")
                .ok_or_else(|| Error::verb("ARGS", "an inbox channel needs path"))?;
            config.insert("path".into(), Cbor::Text(p.into()));
        }
        "webhook" => {
            let u = arg_str(args, "url")
                .ok_or_else(|| Error::verb("ARGS", "a webhook channel needs url"))?;
            config.insert("url".into(), Cbor::Text(u.into()));
        }
        other => {
            return Err(Error::verb(
                "ARGS",
                format!("channel kind {other}; use inbox or webhook"),
            ))
        }
    }
    let principals: Vec<EntityId> = args
        .get("principals")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter_map(|n| repo.named_principal(n))
                .collect()
        })
        .unwrap_or_default();
    let c = tessra_core::object::Channel {
        id: EntityId::random(),
        prev: None,
        kind: kind.into(),
        config,
        principals: principals.clone(),
        signing: "key".into(),
        time: now(),
    };
    let oid = repo.store().put(&c)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "channel",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        vec![
            Effect::Put { id: oid },
            Effect::Point {
                entity: c.id,
                to: oid,
                from: None,
            },
        ],
        now(),
    )?;
    repo.commit_op(&op)?;
    ok(
        json!({ "channel": c.id.to_letters(), "name": name, "kind": kind, "principals": principals.len() }),
        &["standard --when high --require 'approved(human)'"],
    )
}

/// JSON to CBOR for attestation results and hook arguments.
fn json_to_cbor(v: &Json) -> Cbor {
    match v {
        Json::Null => Cbor::Null,
        Json::Bool(b) => Cbor::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Cbor::Integer(i.into()),
            None => Cbor::Float(n.as_f64().unwrap_or(0.0)),
        },
        Json::String(s) => Cbor::Text(s.clone()),
        Json::Array(a) => Cbor::Array(a.iter().map(json_to_cbor).collect()),
        Json::Object(o) => Cbor::Map(
            o.iter()
                .map(|(k, v)| (Cbor::Text(k.clone()), json_to_cbor(v)))
                .collect(),
        ),
    }
}

/// Machine-local configuration: show it, or set keys (owner only). Values
/// are text, or integers and booleans when they read as such; keys with a
/// `verifiers.` prefix fill the verifiers map.
fn config_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let sets: Vec<String> = args
        .get("set")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut config = repo.config();
    if !sets.is_empty() {
        if actor.kind != "daemon" {
            return Err(Error::verb("SCOPE", "only the owner sets configuration"));
        }
        require_owner_credential(repo, actor, "setting configuration")?;
        for kv in &sets {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| Error::verb("ARGS", format!("expected key=value, got {kv}")))?;
            let value = match v {
                "true" => Cbor::Bool(true),
                "false" => Cbor::Bool(false),
                _ => match v.parse::<i64>() {
                    Ok(n) => Cbor::Integer(n.into()),
                    Err(_) => Cbor::Text(v.to_string()),
                },
            };
            if let Some(kind) = k.strip_prefix("verifiers.") {
                let entry = config
                    .entry("verifiers".to_string())
                    .or_insert_with(|| Cbor::Map(vec![]));
                if let Cbor::Map(m) = entry {
                    m.retain(|(key, _)| !matches!(key, Cbor::Text(t) if t == kind));
                    m.push((Cbor::Text(kind.to_string()), value));
                }
            } else {
                config.insert(k.to_string(), value);
            }
        }
        repo.set_config(&config)?;
    }
    let shown: serde_json::Map<String, Json> = config
        .iter()
        .filter(|(k, _)| k.as_str() != "repo_id")
        .map(|(k, v)| (k.clone(), cbor_to_json(v)))
        .collect();
    ok(json!({ "config": shown }), &[])
}

/// Targets: list them, or create one (owner only).
fn target_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let name = arg_str(args, "name");
    let deployer = arg_str(args, "deployer");
    let (Some(name), Some(deployer)) = (name, deployer) else {
        let mut out = Vec::new();
        for (id, _, t) in crate::delivery::all_targets(repo)? {
            out.push(crate::delivery::describe_target(repo, &id, &t)?);
        }
        let releases: Vec<Json> = crate::delivery::releases(repo)?
            .iter()
            .take(5)
            .map(|(id, r)| crate::delivery::describe_release(repo, id, r))
            .collect();
        return ok(
            json!({ "targets": out, "releases": releases }),
            &["target --name prod --deployer 'dir(<path>)' --observer 'run(<cmd>)'"],
        );
    };
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "only the owner defines targets"));
    }
    require_owner_credential(repo, actor, "defining a target")?;
    let list = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let canary = args.get("canary").and_then(Json::as_u64).unwrap_or(10);
    let out = crate::delivery::create_target(
        repo,
        actor,
        name,
        deployer,
        &list("observers"),
        canary,
        &list("secrets"),
    )?;
    ok(
        out,
        &[
            "standard --target <name> --require 'observe(error_rate, max=50)'",
            "promote --to <name> --slice canary",
        ],
    )
}

/// Cut a release of the trunk head (owner or coordinator).
fn release_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let Some(name) = arg_str(args, "name") else {
        let releases: Vec<Json> = crate::delivery::releases(repo)?
            .iter()
            .map(|(id, r)| crate::delivery::describe_release(repo, id, r))
            .collect();
        return ok(json!({ "releases": releases }), &["release --name v1"]);
    };
    let out = crate::delivery::cut_release(repo, actor, name)?;
    ok(out, &["promote --to <target> --slice canary"])
}

/// Observe a target: run its observers, attest the signals, evaluate its
/// standard, and roll back if an observe clause trips.
fn observe_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let target =
        arg_str(args, "target").ok_or_else(|| Error::verb("ARGS", "observe needs target"))?;
    let out = crate::delivery::observe(repo, actor, target)?;
    let next: &[&str] = if out["tripped"].as_bool().unwrap_or(false) {
        &["target", "query --kind memory --kinds task"]
    } else {
        &["promote --to <target> --slice all"]
    };
    ok(out, next)
}

/// Import commits that appeared on a git branch since the last export or
/// import, each landed on trunk. Owner only.
fn import_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "only the owner imports from git"));
    }
    let branch = arg_str(args, "branch").unwrap_or("main");
    let out = crate::bridge::import_git(repo, actor, branch)?;
    ok(out, &["export --format git"])
}

/// The environment of a workspace: toolchain versions, lockfile hashes,
/// and a tree per configured state path. Returns the stored environment,
/// the object, and a description of the state captured.
fn capture_environment(
    repo: &Repo,
    dir: &std::path::Path,
) -> Result<(ObjectId, tessra_core::object::Environment, Vec<Json>)> {
    let paths: Vec<String> = match repo.config().get("state_paths") {
        Some(Cbor::Text(t)) => t
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        _ => vec![
            "target".into(),
            "data".into(),
            ".venv".into(),
            "node_modules".into(),
        ],
    };
    let mut state = Vec::new();
    let mut state_json = Vec::new();
    for p in &paths {
        if let Some((tree, n)) = fs::state_tree(repo.store(), &dir.join(p))? {
            state.push(Cbor::Map(vec![
                (Cbor::Text("path".into()), Cbor::Text(p.clone())),
                (Cbor::Text("tree".into()), Cbor::Bytes(tree.0.to_vec())),
                (Cbor::Text("files".into()), Cbor::Integer((n as u64).into())),
            ]));
            state_json.push(json!({ "path": p, "tree": tree.to_hex(), "files": n }));
        }
    }
    let probe = |cmd: &str| -> Option<String> {
        let out = crate::quiet(std::process::Command::new(cmd))
            .arg("--version")
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout)
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    let mut tools = BTreeMap::new();
    if dir.join("Cargo.toml").exists() {
        for t in ["cargo", "rustc"] {
            if let Some(v) = probe(t) {
                tools.insert(t.to_string(), v);
            }
        }
    }
    if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() {
        if let Some(v) = probe("python") {
            tools.insert("python".into(), v);
        }
    }
    if dir.join("package.json").exists() {
        if let Some(v) = probe("node") {
            tools.insert("node".into(), v);
        }
    }
    let mut locks = Vec::new();
    for name in [
        "Cargo.lock",
        "package-lock.json",
        "poetry.lock",
        "uv.lock",
        "requirements.txt",
        "go.sum",
    ] {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            locks.push((
                Cbor::Text(name.into()),
                Cbor::Text(blake3::hash(&bytes).to_hex().to_string()),
            ));
        }
    }
    let env = tessra_core::object::Environment {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        tools,
        image: None,
        sandbox: None,
        extra: Some(BTreeMap::from([
            ("state".to_string(), Cbor::Array(state)),
            ("locks".to_string(), Cbor::Map(locks)),
        ])),
    };
    let id = repo.store().put(&env)?;
    Ok((id, env, state_json))
}

/// Materialize the state paths a snapshot's environment carries into a directory.
fn restore_state(repo: &Repo, snap: &Snapshot, dir: &std::path::Path) -> Result<Vec<Json>> {
    let Some(env_id) = snap.env else {
        return Ok(Vec::new());
    };
    let env: tessra_core::object::Environment = repo.store().get(&env_id)?;
    let mut out = Vec::new();
    if let Some(Cbor::Array(list)) = env.extra.as_ref().and_then(|e| e.get("state")) {
        for item in list {
            let Cbor::Map(m) = item else { continue };
            let get = |k: &str| {
                m.iter()
                    .find(|(key, _)| matches!(key, Cbor::Text(t) if t == k))
                    .map(|(_, v)| v)
            };
            let (Some(Cbor::Text(path)), Some(Cbor::Bytes(tree))) = (get("path"), get("tree"))
            else {
                continue;
            };
            if path.contains("..") {
                continue;
            }
            let Ok(tree_id) = ObjectId::from_slice(tree) else {
                continue;
            };
            let target = dir.join(path);
            let n = fs::materialize(repo.store(), &tree_id, &target, false)?;
            out.push(json!({ "path": path, "files": n }));
        }
    }
    Ok(out)
}

/// Trunk from its head back through first parents, newest first.
fn trunk_history(repo: &Repo, max: usize) -> Result<Vec<(ObjectId, Revision)>> {
    let (head, _) = trunk_head(repo)?;
    let mut out = Vec::new();
    let mut cur = Some(head);
    while let Some(id) = cur {
        if out.len() >= max {
            break;
        }
        let r: Revision = repo.store().get(&id)?;
        cur = r.parents.first().copied();
        out.push((id, r));
    }
    Ok(out)
}

struct BisectCtx<'a> {
    test: tessra_core::object::Node,
    closure: Vec<EntityId>,
    seq: &'a [(ObjectId, Revision)],
    tool: crate::verifiers::Verifier,
    env_id: ObjectId,
    verifier: EntityId,
    current_nodes: Vec<tessra_core::object::Node>,
    current_text: Vec<u8>,
}

/// Does the test pass at trunk revision `i`? Cached by the bodies of the
/// units the test depends on, so revisions that did not touch them share
/// one result. Returns `(passed, from_cache)`.
fn bisect_probe(repo: &mut Repo, cx: &BisectCtx<'_>, i: usize) -> Result<(bool, bool)> {
    use crate::verifiers as vf;
    let (rid, r) = &cx.seq[i];
    let (snap_id, snap) = root_snapshot(repo, r)?;
    let (_, idx_i) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
    let by_nid: HashMap<EntityId, &tessra_core::object::Node> =
        idx_i.nodes.iter().map(|n| (n.nid, n)).collect();
    let mut h = blake3::Hasher::new();
    h.update(cx.test.body.as_bytes());
    let mut bodies: Vec<ObjectId> = vec![cx.test.body];
    for n in &cx.closure {
        h.update(n.as_bytes());
        match by_nid.get(n) {
            Some(node) => {
                h.update(node.body.as_bytes());
                bodies.push(node.body);
            }
            None => {
                h.update(b"missing");
            }
        }
    }
    let fp = hex::encode(h.finalize().as_bytes());
    for (_, a) in vf::by_body(repo.store(), &cx.test.body)? {
        if a.kind == "test.result"
            && a.scope
                .as_ref()
                .and_then(|s| s.get("fingerprint"))
                .and_then(|v| v.as_text())
                == Some(fp.as_str())
        {
            return Ok((matches!(a.result, Cbor::Bool(true)), true));
        }
    }
    let dir = vf::materialize_scratch(
        repo,
        &snap.root,
        &format!("bisect-{}", &snap_id.to_hex()[..12]),
    )?;
    let already_there = idx_i.nodes.iter().any(|n| {
        n.path == cx.test.path
            && (n.nid == cx.test.nid || (n.kind == cx.test.kind && n.name == cx.test.name))
    });
    if !already_there {
        let flat = tree::flatten(repo.store(), &snap.root)?;
        let text = flat
            .get(&cx.test.path)
            .and_then(|l| l.r#ref)
            .and_then(|x| repo.store().get_bytes(&x).ok().flatten())
            .unwrap_or_default();
        let nodes = crate::semantic::nodes_for_path(&idx_i, &cx.test.path);
        let merged = vf::overlay_tests(
            &text,
            &nodes,
            &cx.current_text,
            &cx.current_nodes,
            &[&cx.test],
        );
        let full = dir.join(&cx.test.path);
        if let Some(d) = full.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&full, merged)?;
    }
    let out = vf::run(
        repo,
        &cx.tool,
        &dir,
        std::slice::from_ref(&cx.test.name),
        std::time::Duration::from_secs(600),
    )?;
    let _ = std::fs::remove_dir_all(&dir);
    let passed = out
        .tests
        .iter()
        .any(|(t, okk)| *okk && vf::test_matches(t, &cx.test.name));
    bodies.sort();
    bodies.dedup();
    vf::attest_as_runner(
        repo,
        "test.result",
        None,
        Some(bodies),
        Some(BTreeMap::from([
            ("test".to_string(), Cbor::Text(cx.test.name.clone())),
            ("fingerprint".to_string(), Cbor::Text(fp)),
            ("revision".to_string(), Cbor::Text(rid.to_hex())),
        ])),
        Cbor::Bool(passed),
        Some(cx.env_id),
        cx.verifier,
        Some(&out.output),
    )?;
    Ok((passed, false))
}

/// Show hooks, define one, or toggle one. Definitions are owner only.
fn hook_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let name = arg_str(args, "name").map(str::to_string);
    let on = arg_str(args, "on").map(str::to_string);
    let dos: Vec<String> = args
        .get("do")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let toggle = args.get("enable").and_then(Json::as_bool);
    if name.is_none() || (on.is_none() && toggle.is_none()) {
        let hooks: Vec<Json> = crate::hooks::all_hooks(repo)?
            .into_iter()
            .map(|(_, _, h)| crate::hooks::describe(&h))
            .collect();
        return ok(
            json!({ "hooks": hooks }),
            &["hook --name <name> --on proposed --do 'webhook(http://ci.local/run)'"],
        );
    }
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "only the owner defines hooks"));
    }
    require_owner_credential(repo, actor, "defining a hook")?;
    let name = name.unwrap_or_default();
    let existing = crate::hooks::all_hooks(repo)?
        .into_iter()
        .find(|(_, _, h)| h.name == name);
    let (hook, effects) = match (toggle, existing) {
        (Some(enabled), Some((id, oid, mut h))) => {
            h.prev = Some(oid);
            h.enabled = enabled;
            let new_oid = repo.store().put(&h)?;
            (
                h,
                vec![
                    Effect::Put { id: new_oid },
                    point_effect(&repo.log, id, new_oid)?,
                ],
            )
        }
        (Some(_), None) => return Err(Error::verb("NOT_FOUND", format!("no hook named {name}"))),
        (None, existing) => {
            let on = on.unwrap_or_default();
            let where_ = match arg_str(args, "where") {
                Some(w) => Some(standard::parse_predicate(w).map_err(|e| Error::verb("ARGS", e))?),
                None => None,
            };
            let actions: Vec<tessra_core::object::HookAction> = dos
                .iter()
                .map(|d| crate::hooks::parse_action(d))
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| Error::verb("ARGS", e))?;
            if actions.is_empty() {
                return Err(Error::verb("ARGS", "a hook needs at least one --do action"));
            }
            let (id, prev) = match existing {
                Some((id, oid, _)) => (id, Some(oid)),
                None => (EntityId::random(), None),
            };
            let h = tessra_core::object::Hook {
                id,
                prev,
                name: name.clone(),
                on,
                r#where: where_,
                r#do: actions,
                r#as: repo.daemon,
                enabled: true,
                order: None,
            };
            let new_oid = repo.store().put(&h)?;
            let eff = match prev {
                Some(_) => point_effect(&repo.log, id, new_oid)?,
                None => Effect::Point {
                    entity: id,
                    to: new_oid,
                    from: None,
                },
            };
            (h, vec![Effect::Put { id: new_oid }, eff])
        }
    };
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "hook",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        effects,
        now(),
    )?;
    repo.commit_op(&op)?;
    ok(crate::hooks::describe(&hook), &["hook"])
}

/// Grant a principal. `--external <name>` creates a CI-style principal
/// whose only verb is `attest`; its attestations count for standards.
fn grant_verb(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "only the owner grants principals"));
    }
    require_owner_credential(repo, actor, "granting a principal")?;
    if let Some(agent) = arg_str(args, "to") {
        // A standing grant for an agent's sessions: extra verbs, a write
        // scope, delegability, and an op budget.
        let list = |key: &str| -> Vec<String> {
            args.get(key)
                .and_then(Json::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let t = crate::principals::GrantTemplate {
            verbs: list("verbs"),
            paths: list("paths"),
            delegable: args
                .get("delegable")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            ops: args.get("ops").and_then(Json::as_u64),
        };
        let agent_id = repo.agent_id(agent)?;
        repo.set_grant_template(agent, &t)?;
        return ok(
            json!({
                "agent": agent, "principal": agent_id.to_letters(), "verbs": t.verbs, "paths": t.paths,
                "delegable": t.delegable, "ops": t.ops,
                "how": "the agent's next session carries this grant; a delegable one lets it plan and hand scoped, budgeted capabilities to the agents it assigns",
            }),
            &[],
        );
    }
    let credential_note = "shown once; Tessra keeps only its hash. Present it with --credential or TESSRA_CREDENTIAL, or at the prompt; granting the same name again reissues it";
    if let Some(name) = arg_str(args, "human") {
        let (id, cap, credential) = repo.grant_human(name)?;
        return ok(
            json!({
                "principal": id.to_letters(), "kind": "human", "name": name, "capability": cap.to_hex(), "verbs": ["attest"],
                "credential": credential, "credential_note": credential_note,
                "use": format!("tessra --as {name} approve --request <id>"),
            }),
            &[],
        );
    }
    let name = arg_str(args, "external").ok_or_else(|| {
        Error::verb(
            "ARGS",
            "grant needs --external <name>, --human <name>, or --to <agent>",
        )
    })?;
    let (id, cap, credential) = repo.grant_external(name)?;
    ok(
        json!({
            "principal": id.to_letters(), "kind": "external", "name": name, "capability": cap.to_hex(), "verbs": ["attest"],
            "credential": credential, "credential_note": credential_note,
            "use": format!("tessra --as {name} attest --kind ci.pass --subject <snapshot or revision> --result true"),
        }),
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn record_conflict(
    repo: &mut Repo,
    actor: &mut Actor,
    ws: &Workspace,
    rev_id: ObjectId,
    rev: &Revision,
    head_id: ObjectId,
    rules: ObjectId,
    merged: &Flat,
    conflicts: &[String],
    args: &Json,
) -> Result<Outcome> {
    let root = tree::build(repo.store(), merged)?;
    let snap = repo.store().put(&Snapshot {
        root,
        rules,
        env: None,
        index: None,
    })?;
    let conflicted = Revision {
        id: rev.id,
        prev: Some(rev_id),
        snapshots: BTreeMap::from([("".to_string(), snap)]),
        parents: vec![head_id, rev_id],
        intent: rev.intent,
        title: rev.title.clone(),
        body: rev.body.clone(),
        author: rev.author,
        time: now(),
        ops: None,
        flags: rev.flags.clone(),
    };
    let conflicted_id = repo.store().put(&conflicted)?;
    let task = Memory {
        id: EntityId::random(),
        prev: None,
        kind: "task".into(),
        scope: MemoryScope {
            kind: if rev.intent.is_some() {
                "intent".into()
            } else {
                "repo".into()
            },
            r#ref: match rev.intent {
                Some(i) => Cbor::Bytes(i.0.to_vec()),
                None => Cbor::Text(String::new()),
            },
        },
        body: format!(
            "Landing change {} conflicts with trunk on: {}. Resolve in the workspace and snapshot.",
            rev.id.to_letters(),
            conflicts.join(", ")
        ),
        anchor: None,
        confidence: 1000,
        author: actor.principal(),
        time: now(),
        expires: None,
        proposed: None,
        status: "active".into(),
        links: Some(vec![conflicted_id]),
        visibility: "shared".into(),
    };
    let task_id = repo.store().put(&task)?;
    let effects = vec![
        Effect::Put { id: conflicted_id },
        point_effect(&repo.log, rev.id, conflicted_id)?,
        Effect::Put { id: task_id },
        Effect::Point {
            entity: task.id,
            to: task_id,
            from: None,
        },
    ];
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "land",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        effects,
        now(),
    )?;
    repo.commit_op(&op)?;
    fs::materialize(repo.store(), &root, &ws_path(ws), false)?;
    let affected: Vec<EntityId> = repo
        .workspaces
        .iter()
        .filter(|w| w.current == Some(rev_id))
        .map(|w| w.id)
        .collect();
    for id in affected {
        if let Some(w) = repo.workspace_mut(&id) {
            w.current = Some(conflicted_id);
            let w2 = w.clone();
            repo.save_workspace(&w2)?;
        }
    }
    let hooks = crate::hooks::fire(
        repo,
        &crate::hooks::Event::new(
            repo,
            "conflict.opened",
            conflicted_id,
            &conflicted,
            json!({ "conflicts": conflicts }),
        )?,
    )?;
    ok(
        json!({
            "landed": false,
            "conflicts": conflicts,
            "revision": conflicted_id.to_hex(),
            "hooks": hooks,
            "task": task.id.to_letters(),
            "how": "each conflicted file carries markers and a .tessra-conflict sidecar; edit it with resolve=true, then snapshot and promote again"
        }),
        &[
            "context --path <conflicted file>",
            "edit --resolve",
            "snapshot",
        ],
    )
}

/// Render shared memory into a file for tools that do not speak Tessra.
fn export(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    let format = arg_str(args, "format").unwrap_or("agents-md");
    if format == "git" {
        if actor.kind != "daemon" {
            return Err(Error::verb("SCOPE", "only the owner exports trunk to git"));
        }
        let branch = arg_str(args, "branch").unwrap_or("main");
        let dir = arg_str(args, "path").map(PathBuf::from);
        let out = crate::bridge::export_git(repo, branch, dir, arg_str(args, "push"))?;
        return ok(out, &["import --branch <branch>"]);
    }
    if !matches!(format, "agents-md" | "claude-md") {
        return Err(Error::verb(
            "ARGS",
            "export format: agents-md, claude-md, or git",
        ));
    }
    let ws = require_workspace(repo, actor)?;
    let default_name = if format == "claude-md" {
        "CLAUDE.md"
    } else {
        "AGENTS.md"
    };
    let path = arg_str(args, "path")
        .map(PathBuf::from)
        .unwrap_or_else(|| ws_path(&ws).join(default_name));
    let mems = memories(repo, actor, None, None)?;
    let mut groups: BTreeMap<String, Vec<&Memory>> = BTreeMap::new();
    let mut count = 0usize;
    for (_, m) in &mems {
        if m.visibility != "shared" || m.status != "active" || m.proposed.unwrap_or(false) {
            continue;
        }
        let label = match (m.scope.kind.as_str(), &m.scope.r#ref) {
            ("repo", _) | ("root", _) => "Repository".to_string(),
            (k, Cbor::Text(r)) => format!("{}: {}", capitalize(k), r),
            (k, other) => format!("{}: {}", capitalize(k), cbor_to_json(other)),
        };
        groups.entry(label).or_default().push(m);
        count += 1;
    }
    let mut out = String::new();
    out.push_str("# Agent notes\n\n");
    out.push_str("Generated by `tessra export` from shared memory. Do not edit; record with `tessra remember` and export again.\n\n");
    for (label, list) in &groups {
        out.push_str(&format!("## {label}\n\n"));
        for m in list {
            out.push_str(&format!(
                "- **{}** ({:.2}): {}\n",
                m.kind,
                m.confidence as f64 / 1000.0,
                m.body.replace('\n', " ")
            ));
        }
        out.push('\n');
    }
    std::fs::write(&path, out.as_bytes())?;
    ok(
        json!({ "path": path.display().to_string(), "memories": count, "format": format }),
        &[],
    )
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Measure the M1 performance budgets and record them as a signed attestation.
fn bench(repo: &mut Repo, actor: &mut Actor, args: &Json) -> Result<Outcome> {
    if actor.kind != "daemon" {
        return Err(Error::verb("SCOPE", "bench runs as the owner"));
    }
    use std::time::Instant;
    let ws = require_workspace(repo, actor)?;
    let (rev_id, rev) = current_revision(repo, &ws)?;
    let (_, snap) = root_snapshot(repo, &rev)?;
    let rules: TrackingRules = repo.store().get(&snap.rules)?;

    let t = Instant::now();
    let _ = state(repo, actor)?;
    let status_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    repo.store().begin_batch();
    let snap_out = fs::snapshot_dir(repo.store(), &ws_path(&ws), &rules, None, None)?;
    repo.store().end_batch()?;
    let snapshot_ms = t.elapsed().as_secs_f64() * 1000.0;
    let files = snap_out.files;

    let tmp = paths::workspaces_dir_for(&repo.repo_id).join("bench-tmp");
    let t = Instant::now();
    let n = fs::materialize(repo.store(), &snap.root, &tmp, false)?;
    let workspace_ms = t.elapsed().as_secs_f64() * 1000.0;
    let _ = std::fs::remove_dir_all(&tmp);

    let m = Memory {
        id: EntityId::random(),
        prev: None,
        kind: "fact".into(),
        scope: MemoryScope {
            kind: "repo".into(),
            r#ref: Cbor::Text(String::new()),
        },
        body: "bench probe".into(),
        anchor: None,
        confidence: 1,
        author: actor.principal(),
        time: now(),
        expires: None,
        proposed: None,
        status: "retired".into(),
        links: None,
        visibility: "private".into(),
    };
    let mid = repo.store().put(&m)?;
    let probe = build::build_op(
        &repo.log,
        &actor.signer,
        None,
        "remember",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        vec![
            Effect::Put { id: mid },
            Effect::Point {
                entity: m.id,
                to: mid,
                from: None,
            },
        ],
        now(),
    )?;
    let t = Instant::now();
    tessra_oplog::verify::verify_op(&repo.log, &probe)?;
    let verify_ms = t.elapsed().as_secs_f64() * 1000.0;

    let view = repo.log.current_view()?;
    let trie = tessra_core::trie::Trie::new(repo.store());
    let key = EntityId::random();
    let t = Instant::now();
    let _ = trie.insert(
        &view.entities,
        key.as_bytes(),
        tessra_core::trie::TrieValue::Id(mid),
    )?;
    let trie_us = t.elapsed().as_secs_f64() * 1_000_000.0;

    let within = json!({
        "status_under_50ms": status_ms < 50.0,
        "snapshot_under_1s_per_1000_files": snapshot_ms < 1000.0 * (files.max(1) as f64 / 1000.0).max(1.0),
        "workspace_copy_under_2s_per_10k_files": workspace_ms < 2000.0 * (n.max(1) as f64 / 10_000.0).max(1.0),
        "verify_under_5ms": verify_ms < 5.0,
        "trie_update_under_1ms": trie_us < 1000.0,
    });
    let result = json!({
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "files": files,
        "status_ms": status_ms,
        "snapshot_ms": snapshot_ms,
        "workspace_copy_ms": workspace_ms,
        "verify_ms": verify_ms,
        "trie_update_us": trie_us,
        "within_budget": within,
    });

    // Record as a signed attestation on the current revision, by the daemon as verifier.
    let mut att = tessra_core::object::Attestation {
        kind: "perf.m1".into(),
        subject: Some(serde_bytes::ByteBuf::from(rev_id.0.to_vec())),
        bodies: None,
        subject_type: Some("revision".into()),
        scope: None,
        result: Cbor::Map(vec![
            (
                Cbor::Text("files".into()),
                Cbor::Integer((files as u64).into()),
            ),
            (
                Cbor::Text("status_us".into()),
                Cbor::Integer(((status_ms * 1000.0) as u64).into()),
            ),
            (
                Cbor::Text("snapshot_us".into()),
                Cbor::Integer(((snapshot_ms * 1000.0) as u64).into()),
            ),
            (
                Cbor::Text("workspace_us".into()),
                Cbor::Integer(((workspace_ms * 1000.0) as u64).into()),
            ),
            (
                Cbor::Text("verify_us".into()),
                Cbor::Integer(((verify_ms * 1000.0) as u64).into()),
            ),
            (
                Cbor::Text("trie_ns".into()),
                Cbor::Integer(((trie_us * 1000.0) as u64).into()),
            ),
        ]),
        env: None,
        verifier: actor.principal(),
        runner: None,
        evidence: None,
        time: now(),
        sigkind: "ed25519".into(),
        sig: None,
    };
    tessra_core::sig::SignedObject::sign_with(&mut att, &actor.signer.key)?;
    let att_id = repo.store().put(&att)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        None,
        "attest",
        build::args_with_idem(idem_from(args), BTreeMap::new()),
        vec![Effect::Put { id: att_id }],
        now(),
    )?;
    repo.commit_op(&op)?;
    let mut result = result;
    result["attestation"] = json!(att_id.to_hex());
    ok(result, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitOptions;

    /// A repository of its own in a temp dir, with its store, keys, and
    /// workspaces beside it in the same temp dir, and its owner.
    fn scratch() -> (paths::ScratchDir, Repo, Actor) {
        let dir = paths::ScratchDir::new();
        let repo = Repo::init(
            dir.path(),
            InitOptions {
                name: "t".into(),
                import_git: false,
                history: 1,
                data_dir: Some(dir.data_dir()),
            },
        )
        .unwrap();
        // The owner at a terminal, credential presented.
        let actor = repo.owner_actor();
        (dir, repo, actor)
    }

    #[test]
    fn a_workspace_is_created_only_in_a_fresh_directory_outside_the_checkout() {
        let (dir, mut repo, mut actor) = scratch();
        // The checkout holds a file no tree has, as a checkout does.
        std::fs::write(dir.path().join("local.env"), "SECRET=1\n").unwrap();
        let create = |repo: &mut Repo, actor: &mut Actor, path: &std::path::Path| {
            call(
                repo,
                actor,
                "workspace",
                &json!({ "action": "create", "path": path.display().to_string() }),
            )
        };
        // The checkout itself, and a new directory inside it.
        for path in [dir.path().to_path_buf(), dir.path().join("ws")] {
            let out = create(&mut repo, &mut actor, &path);
            assert_eq!(out["ok"], json!(false), "{out}");
            assert_eq!(out["code"], json!("ARGS"), "{out}");
            assert!(
                out["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("inside the repository"),
                "{out}"
            );
        }
        assert!(dir.path().join("local.env").exists(), "nothing was removed");
        // A directory elsewhere that already has contents.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("keep.txt"), "keep\n").unwrap();
        let out = create(&mut repo, &mut actor, other.path());
        assert_eq!(out["ok"], json!(false), "{out}");
        assert!(
            out["message"].as_str().unwrap_or("").contains("not empty"),
            "{out}"
        );
        assert!(
            other.path().join("keep.txt").exists(),
            "nothing was removed"
        );
        // An empty directory, and one that does not exist yet, are fine.
        let empty = tempfile::tempdir().unwrap();
        let out = create(&mut repo, &mut actor, empty.path());
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = create(&mut repo, &mut actor, &other.path().join("fresh"));
        assert_eq!(out["ok"], json!(true), "{out}");
    }

    #[test]
    fn risk_sensitive_accepts_the_text_that_config_set_stores() {
        let (_dir, mut repo, mut actor) = scratch();
        let out = call(
            &mut repo,
            &mut actor,
            "config",
            &json!({ "set": ["risk_sensitive=vault/**,infra/**"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "vault/policy.toml", "content": "ttl = 30\n" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "policy" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut actor, "verify", &json!({}));
        assert_eq!(out["ok"], json!(true), "{out}");
        let factors = out["result"]["risk"]["factors"]
            .as_array()
            .expect("risk factors");
        assert!(
            factors
                .iter()
                .any(|f| f["name"] == json!("sensitive_scope")),
            "{out}"
        );
    }

    #[test]
    fn a_flagged_snapshot_does_not_land_under_flags_none() {
        let (_dir, mut repo, mut actor) = scratch();
        // A key-shaped fixture, built at run time so this source is not itself flagged.
        let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "notes.txt", "content": format!("token {key}\n") }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "oops" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        assert!(out["result"]["flags"]["secrets"].is_array(), "{out}");
        // The default standard requires flags.none, and the landing sees the flags.
        let out = call(&mut repo, &mut actor, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(false), "{out}");
        assert_eq!(out["code"], json!("STANDARD_UNMET"), "{out}");
        assert!(
            out["message"].as_str().unwrap_or("").contains("flags.none"),
            "{out}"
        );
    }

    #[test]
    fn a_session_is_told_which_memory_scopes_it_may_write() {
        let (_dir, mut repo, _owner) = scratch();
        let mut bot = repo.open_session("bot", None, vec!["**".into()]).unwrap();
        // No scope means the repository, which the default grant leaves out.
        let out = call(
            &mut repo,
            &mut bot,
            "remember",
            &json!({ "kind": "gotcha", "body": "x" }),
        );
        assert_eq!(out["ok"], json!(false), "{out}");
        assert_eq!(out["code"], json!("SCOPE"), "{out}");
        let message = out["message"].as_str().unwrap_or("");
        assert!(message.contains("unit, path, intent"), "{out}");
        assert!(message.contains("owner"), "{out}");
        // The documented `unit` scope is accepted and stored as `node`.
        let out = call(
            &mut repo,
            &mut bot,
            "remember",
            &json!({ "kind": "gotcha", "body": "x", "scope": { "kind": "unit", "ref": "src/a.rs:one" } }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        assert_eq!(out["result"]["scope"]["kind"], json!("node"), "{out}");
        let out = call(
            &mut repo,
            &mut bot,
            "remember",
            &json!({ "kind": "gotcha", "body": "x", "scope": { "kind": "path", "ref": "src" } }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
    }

    #[test]
    fn a_secret_trunk_already_holds_does_not_flag_unrelated_work() {
        let (_dir, mut repo, mut actor) = scratch();
        // A key-shaped file reaches trunk with the flags clause lifted, as a
        // git import would put one there.
        let out = call(
            &mut repo,
            &mut actor,
            "standard",
            &json!({ "remove": ["structural(flags.none)"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "key.txt", "content": format!("token {key}\n") }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "key" }),
        );
        assert!(out["result"]["flags"]["secrets"].is_array(), "{out}");
        let out = call(&mut repo, &mut actor, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "standard",
            &json!({ "require": ["structural(flags.none)"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        // A change that touches another file is not flagged for trunk's key.
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "notes.txt", "content": "hello\n" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "notes" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        assert!(out["result"]["flags"]["secrets"].is_null(), "{out}");
        let out = call(&mut repo, &mut actor, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        // A change that edits the key file carries the flag and is refused.
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "key.txt", "content": format!("token {key} again\n") }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "touch the key" }),
        );
        assert!(out["result"]["flags"]["secrets"].is_array(), "{out}");
        let out = call(&mut repo, &mut actor, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(false), "{out}");
        assert_eq!(out["code"], json!("STANDARD_UNMET"), "{out}");
    }

    #[test]
    fn an_unless_approved_escape_opens_an_approval_request() {
        let (_dir, mut repo, mut actor) = scratch();
        let out = call(
            &mut repo,
            &mut actor,
            "standard",
            &json!({ "forbid": ["structural(test.weakened) unless approved(human)"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        // A landed test, then a change that edits it.
        let with_test = "pub fn one() -> i32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one_is_one() {\n        assert_eq!(super::one(), 1);\n    }\n}\n";
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "src/lib.rs", "content": with_test }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "one" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut actor, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let edited =
            with_test.replace("assert_eq!(super::one(), 1);", "assert!(super::one() > 0);");
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "src/lib.rs", "content": edited }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "loosen the test" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "promote",
            &json!({ "to": "proposed" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        // The frontier refuses it, names the escape, and opens a request a human can answer.
        let out = call(
            &mut repo,
            &mut actor,
            "promote",
            &json!({ "to": "landed", "all": true }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let result = &out["result"]["results"][0];
        assert_eq!(result["landed"], json!(false), "{out}");
        let why = result["why"].as_str().unwrap_or("");
        assert!(why.contains("unless approved(human)"), "{out}");
        assert!(why.contains("request "), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "query",
            &json!({ "kind": "exceptions" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        assert_eq!(
            out["result"]["exceptions"].as_array().map(|a| a.len()),
            Some(1),
            "{out}"
        );
    }

    #[test]
    fn a_refused_landing_charges_the_author_once_per_revision() {
        let (_dir, mut repo, mut owner) = scratch();
        let out = call(
            &mut repo,
            &mut owner,
            "standard",
            &json!({ "forbid": ["structural(test.weakened)"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let with_test = "pub fn one() -> i32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one_is_one() {\n        assert_eq!(super::one(), 1);\n    }\n}\n";
        let out = call(
            &mut repo,
            &mut owner,
            "edit",
            &json!({ "path": "src/lib.rs", "content": with_test }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut owner,
            "snapshot",
            &json!({ "title": "one" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut owner, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        // An agent loosens the test and proposes it.
        let mut bot = repo.open_session("bot", None, vec!["**".into()]).unwrap();
        let out = call(
            &mut repo,
            &mut bot,
            "workspace",
            &json!({ "action": "create" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        bot.workspace = repo
            .workspaces
            .iter()
            .find(|w| w.principal == bot.principal())
            .map(|w| w.id);
        assert!(bot.workspace.is_some(), "{out}");
        let edited =
            with_test.replace("assert_eq!(super::one(), 1);", "assert!(super::one() > 0);");
        let out = call(
            &mut repo,
            &mut bot,
            "edit",
            &json!({ "path": "src/lib.rs", "content": edited }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut bot,
            "snapshot",
            &json!({ "title": "loosen the test" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut bot, "promote", &json!({ "to": "proposed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        // The frontier refuses it as often as the owner asks; the author pays once.
        for _ in 0..3 {
            let out = call(
                &mut repo,
                &mut owner,
                "promote",
                &json!({ "to": "landed", "all": true }),
            );
            assert_eq!(out["ok"], json!(true), "{out}");
            assert_eq!(out["result"]["landed"], json!(0), "{out}");
        }
        let agent = repo
            .session_agent(&bot.principal())
            .expect("bot is a session");
        assert_eq!(crate::anomaly::state_for(&repo, &agent).score, 2);
        let out = call(&mut repo, &mut bot, "status", &json!({}));
        assert_eq!(
            out["ok"],
            json!(true),
            "the author must not be revoked: {out}"
        );
    }

    #[test]
    fn snapshot_then_verify_runs_the_verifiers() {
        let (_dir, mut repo, mut actor) = scratch();
        // A verifier that passes wherever the tests can run at all.
        let out = call(
            &mut repo,
            &mut actor,
            "config",
            &json!({ "set": ["verifiers.tests.pass=cargo --version"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "standard",
            &json!({ "require": ["attest(tests.pass)"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "src/lib.rs", "content": "pub fn one() -> i32 {\n    1\n}\n" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "one", "then": "verify" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        // The follow-on ran the verifier and attested, rather than only reading the standard.
        let ran = out["result"]["verify"]["ran"]
            .as_array()
            .expect("then verify runs the verifiers");
        assert!(
            ran.iter()
                .any(|r| r["kind"] == json!("tests.pass") && r["result"] == json!(true)),
            "{out}"
        );
        assert_eq!(out["result"]["verify"]["met"], json!(2), "{out}");
        assert_eq!(out["next"], json!(["promote --to proposed"]), "{out}");
    }

    #[test]
    fn context_path_stays_inside_its_budget_and_keeps_the_map() {
        let (_dir, mut repo, mut actor) = scratch();
        let mut src = String::new();
        for i in 0..30 {
            src.push_str(&format!(
                "pub fn f{i}(x: i32) -> i32 {{\n    x + {i}\n}}\n\n"
            ));
        }
        let out = call(
            &mut repo,
            &mut actor,
            "edit",
            &json!({ "path": "src/lib.rs", "content": src }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut actor,
            "snapshot",
            &json!({ "title": "thirty functions" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        // Whatever the budget, the response fits in it and the units survive.
        for budget in [150u64, 400, 1000, 4000] {
            let out = call(
                &mut repo,
                &mut actor,
                "context",
                &json!({ "path": "src/lib.rs", "budget": budget }),
            );
            assert_eq!(out["ok"], json!(true), "{out}");
            let used = out["budget"]["used"].as_u64().unwrap();
            assert!(used <= budget, "budget {budget}: used {used}\n{out}");
            assert!(
                !out["result"]["units"].as_array().unwrap().is_empty(),
                "budget {budget}: no units\n{out}"
            );
        }
        let small = call(
            &mut repo,
            &mut actor,
            "context",
            &json!({ "path": "src/lib.rs", "budget": 150 }),
        );
        assert_eq!(small["result"]["truncated"], json!(true), "{small}");
        assert_eq!(
            small["result"]["files"][0]["truncated"],
            json!(true),
            "{small}"
        );
        assert!(
            small["result"]["omitted"]["units"].as_u64().unwrap() > 0,
            "{small}"
        );
        assert!(
            small["next"][0]
                .as_str()
                .unwrap()
                .starts_with("context --path src/lib.rs --budget 300"),
            "{small}"
        );
        let large = call(
            &mut repo,
            &mut actor,
            "context",
            &json!({ "path": "src/lib.rs", "budget": 4000 }),
        );
        assert_eq!(large["result"]["truncated"], json!(false), "{large}");
        assert_eq!(
            large["result"]["files"][0]["truncated"],
            json!(false),
            "{large}"
        );
        assert_eq!(
            large["result"]["files"][0]["content"],
            json!(src),
            "{large}"
        );
        assert_eq!(
            large["result"]["units"].as_array().unwrap().len(),
            30,
            "{large}"
        );
        assert_eq!(
            large["result"]["omitted"],
            json!({ "units": 0, "memories": 0, "claims": 0 })
        );
    }

    #[test]
    fn object_query_pages_a_blob_to_its_end() {
        let (_dir, mut repo, mut actor) = scratch();
        let mut bytes = vec![b'a'; 6000];
        bytes.extend(std::iter::repeat_n(b'z', 2000));
        let id = repo.store().put_blob(&bytes).unwrap();
        let hex = id.to_hex();
        let first = call(
            &mut repo,
            &mut actor,
            "query",
            &json!({ "kind": "object", "id": hex, "budget": 500 }),
        );
        assert_eq!(first["ok"], json!(true), "{first}");
        assert!(first["budget"]["used"].as_u64().unwrap() <= 500, "{first}");
        let obj = &first["result"]["object"];
        assert_eq!(obj["size"], json!(8000));
        assert_eq!(obj["offset"], json!(0));
        let text = obj["text"].as_str().unwrap();
        assert!(
            text.len() < 2000 && text.bytes().all(|b| b == b'a'),
            "{first}"
        );
        let next = obj["next_offset"].as_u64().unwrap() as usize;
        assert_eq!(next, text.len());
        assert!(
            first["next"][0]
                .as_str()
                .unwrap()
                .contains(&format!("--offset {next}")),
            "{first}"
        );
        // The tail is where a runner's summary is.
        let tail = call(
            &mut repo,
            &mut actor,
            "query",
            &json!({ "kind": "object", "id": hex, "budget": 500, "tail": true }),
        );
        let obj = &tail["result"]["object"];
        let text = obj["text"].as_str().unwrap();
        assert!(
            text.len() > 100 && text.bytes().all(|b| b == b'z'),
            "{tail}"
        );
        assert_eq!(obj["offset"].as_u64().unwrap() as usize + text.len(), 8000);
        assert!(obj.get("next_offset").is_none(), "{tail}");
        assert!(tail["next"].as_array().unwrap().is_empty());
        // An offset reads from there, and the last page has no next.
        let page = call(
            &mut repo,
            &mut actor,
            "query",
            &json!({ "kind": "object", "id": hex, "budget": 500, "offset": 7990 }),
        );
        assert_eq!(
            page["result"]["object"]["text"],
            json!("zzzzzzzzzz"),
            "{page}"
        );
        assert!(page["result"]["object"].get("next_offset").is_none());
    }

    #[test]
    fn text_is_cut_by_its_encoded_size_on_char_boundaries() {
        let text = "tab\tnew\nline \"quoted\" and ünïcödé";
        for room in [0usize, 1, 2, 3, 8, 20, 40, 200] {
            let take = fit_prefix(text, room);
            assert!(text.is_char_boundary(take));
            assert!(
                json_len(&json!(&text[..take])) <= room.max(2) || take == 0,
                "room {room} take {take}"
            );
            let start = fit_suffix(text, room);
            assert!(text.is_char_boundary(start));
            assert!(
                json_len(&json!(&text[start..])) <= room.max(2) || start == text.len(),
                "room {room} start {start}"
            );
        }
        assert_eq!(fit_prefix(text, 200), text.len());
        assert_eq!(fit_suffix(text, 200), 0);
        // Escapes cost: eight characters of room hold fewer than eight bytes of tabs.
        assert!(fit_prefix("\t\t\t\t\t\t\t\t", 8) < 8);
    }

    #[test]
    fn owner_policy_verbs_need_the_owner_credential() {
        let (_dir, mut repo, _) = scratch();
        // Reaching the daemon is not being the owner.
        let mut owner = repo.daemon_actor();
        assert!(!owner.credentialed);
        let out = call(
            &mut repo,
            &mut owner,
            "standard",
            &json!({ "require": ["attest(tests.pass)"] }),
        );
        assert_eq!(out["code"], json!("CREDENTIAL_REQUIRED"), "{out}");
        // Reads stay free.
        let out = call(&mut repo, &mut owner, "standard", &json!({}));
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut owner, "grant", &json!({ "human": "maria" }));
        assert_eq!(out["code"], json!("CREDENTIAL_REQUIRED"), "{out}");
        // The credential init issued is what makes the owner.
        let secret = std::fs::read_to_string(repo.owner_credential_path()).unwrap();
        assert!(repo.owner_credential_ok(Some(secret.trim())));
        assert!(!repo.owner_credential_ok(Some("nope")));
        assert!(!repo.owner_credential_ok(None));
        owner.credentialed = repo.owner_credential_ok(Some(secret.trim()));
        let out = call(&mut repo, &mut owner, "grant", &json!({ "human": "maria" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let credential = out["result"]["credential"].as_str().unwrap().to_string();
        assert_eq!(credential.len(), 64);
        // Maria is reached only with hers: the token alone or a wrong one is refused.
        assert_eq!(
            repo.open_external("maria", None)
                .err()
                .expect("refused")
                .code(),
            "CREDENTIAL_REQUIRED"
        );
        assert_eq!(
            repo.open_external("maria", Some("wrong"))
                .err()
                .expect("refused")
                .code(),
            "CREDENTIAL_BAD"
        );
        assert_eq!(
            repo.open_external("maria", Some(&credential)).unwrap().kind,
            "human"
        );
        // Granting the same name again rotates the credential.
        let out = call(&mut repo, &mut owner, "grant", &json!({ "human": "maria" }));
        let rotated = out["result"]["credential"].as_str().unwrap().to_string();
        assert_ne!(rotated, credential);
        assert_eq!(
            repo.open_external("maria", Some(&credential))
                .err()
                .expect("refused")
                .code(),
            "CREDENTIAL_BAD"
        );
        assert!(repo.open_external("maria", Some(&rotated)).is_ok());
        // A channel still finds her without acting as her.
        assert!(repo.named_principal("maria").is_some());
    }

    #[test]
    fn an_approval_binds_to_the_revision_it_was_given_on() {
        let (_dir, mut repo, mut owner) = scratch();
        let out = call(
            &mut repo,
            &mut owner,
            "standard",
            &json!({ "forbid": ["structural(test.weakened) unless approved(human)"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let with_test = "pub fn one() -> i32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one_is_one() {\n        assert_eq!(super::one(), 1);\n    }\n}\n";
        let out = call(
            &mut repo,
            &mut owner,
            "edit",
            &json!({ "path": "src/lib.rs", "content": with_test }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut owner,
            "snapshot",
            &json!({ "title": "one" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut owner, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut owner, "grant", &json!({ "human": "maria" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let credential = out["result"]["credential"].as_str().unwrap().to_string();
        // An agent weakens the test and proposes it.
        let mut bot = repo.open_session("bot", None, vec!["**".into()]).unwrap();
        let out = call(
            &mut repo,
            &mut bot,
            "workspace",
            &json!({ "action": "create" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        bot.workspace = repo
            .workspaces
            .iter()
            .find(|w| w.principal == bot.principal())
            .map(|w| w.id);
        let loosened =
            with_test.replace("assert_eq!(super::one(), 1);", "assert!(super::one() > 0);");
        let out = call(
            &mut repo,
            &mut bot,
            "edit",
            &json!({ "path": "src/lib.rs", "content": loosened }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut bot,
            "snapshot",
            &json!({ "title": "loosen the test" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut bot, "promote", &json!({ "to": "proposed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        // The frontier refuses it and asks a human.
        let out = call(
            &mut repo,
            &mut owner,
            "promote",
            &json!({ "to": "landed", "all": true }),
        );
        assert_eq!(out["result"]["landed"], json!(0), "{out}");
        let q = call(
            &mut repo,
            &mut owner,
            "query",
            &json!({ "kind": "exceptions" }),
        );
        let request = q["result"]["exceptions"][0]["request"]
            .as_str()
            .expect("an approval request")
            .to_string();
        let mut maria = repo.open_external("maria", Some(&credential)).unwrap();
        let out = call(
            &mut repo,
            &mut maria,
            "approve",
            &json!({ "request": request }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        // The agent edits again before it lands: the approval was for what
        // Maria saw, not for whatever comes after.
        let loosened_more =
            loosened.replace("assert!(super::one() > 0);", "assert!(super::one() >= 0);");
        let out = call(
            &mut repo,
            &mut bot,
            "edit",
            &json!({ "path": "src/lib.rs", "content": loosened_more }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut bot,
            "snapshot",
            &json!({ "title": "loosen it more" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut bot, "promote", &json!({ "to": "proposed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut owner,
            "promote",
            &json!({ "to": "landed", "all": true }),
        );
        assert_eq!(
            out["result"]["landed"],
            json!(0),
            "an approval on the earlier revision must not carry: {out}"
        );
        // Approved on the revision that will land, the landing the system
        // derives from it carries the approval and lands.
        let q = call(
            &mut repo,
            &mut owner,
            "query",
            &json!({ "kind": "exceptions" }),
        );
        let request2 = q["result"]["exceptions"][0]["request"]
            .as_str()
            .expect("a second request")
            .to_string();
        assert_ne!(request, request2);
        let out = call(
            &mut repo,
            &mut maria,
            "approve",
            &json!({ "request": request2 }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut owner,
            "promote",
            &json!({ "to": "landed", "all": true }),
        );
        assert_eq!(out["result"]["landed"], json!(1), "{out}");
    }

    #[test]
    fn status_since_is_newest_first_bounded_and_says_what_happened() {
        let (_dir, mut repo, mut owner) = scratch();
        for i in 0..12 {
            let out = call(
                &mut repo,
                &mut owner,
                "remember",
                &json!({ "kind": "gotcha", "body": format!("gotcha number {i} about the build"), "scope": { "kind": "path", "ref": "src" } }),
            );
            assert_eq!(out["ok"], json!(true), "{out}");
        }
        let out = call(&mut repo, &mut owner, "status", &json!({ "budget": 800 }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let since = out["result"]["since"].as_array().unwrap();
        assert!(!since.is_empty(), "{out}");
        assert!(since.len() < 13, "cut to the budget: {out}");
        assert!(
            out["result"]["since_omitted"].as_u64().unwrap() > 0,
            "{out}"
        );
        assert_eq!(since[0]["kind"], json!("remember"), "newest first: {out}");
        assert!(
            since[0]["what"]
                .as_str()
                .unwrap()
                .contains("gotcha number 11"),
            "{out}"
        );
        assert!(out["result"]["cursor"].is_string(), "{out}");
        assert!(out["budget"]["used"].as_u64().unwrap() <= 800, "{out}");
        // Since that cursor, nothing has happened.
        let cursor = out["result"]["cursor"].as_str().unwrap().to_string();
        let out = call(&mut repo, &mut owner, "status", &json!({ "since": cursor }));
        assert_eq!(out["result"]["since"].as_array().unwrap().len(), 0, "{out}");
        assert_eq!(out["result"]["since_omitted"], json!(0), "{out}");
    }

    #[test]
    fn context_path_without_a_workspace_describes_trunk() {
        let (_dir, mut repo, mut owner) = scratch();
        let out = call(
            &mut repo,
            &mut owner,
            "edit",
            &json!({ "path": "src/lib.rs", "content": "pub fn one() -> i32 { 1 }" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut owner,
            "snapshot",
            &json!({ "title": "one" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut owner, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["ok"], json!(true), "{out}");
        let mut bot = repo.open_session("bot", None, vec!["**".into()]).unwrap();
        assert!(actor_workspace(&repo, &bot).is_none());
        let out = call(
            &mut repo,
            &mut bot,
            "context",
            &json!({ "path": "src/lib.rs", "budget": 2000 }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        assert!(
            out["result"]["files"][0]["content"]
                .as_str()
                .unwrap()
                .contains("pub fn one"),
            "{out}"
        );
        assert_eq!(out["result"]["units"][0]["name"], json!("one"), "{out}");
        let out = call(
            &mut repo,
            &mut bot,
            "context",
            &json!({ "path": "src", "budget": 2000 }),
        );
        assert_eq!(out["result"]["files"][0]["dir"], json!(["lib.rs"]), "{out}");
        let out = call(
            &mut repo,
            &mut bot,
            "context",
            &json!({ "path": "nowhere.rs", "budget": 2000 }),
        );
        assert_eq!(out["result"]["files"][0]["missing"], json!(true), "{out}");
    }

    #[test]
    fn a_refused_promotion_lists_each_unmet_clause_with_a_fix() {
        let (_dir, mut repo, mut owner) = scratch();
        let out = call(
            &mut repo,
            &mut owner,
            "standard",
            &json!({ "require": ["attest(tests.pass)"] }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut owner,
            "edit",
            &json!({ "path": "src/lib.rs", "content": "pub fn one() -> i32 { 1 }" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(
            &mut repo,
            &mut owner,
            "snapshot",
            &json!({ "title": "one" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let out = call(&mut repo, &mut owner, "promote", &json!({ "to": "landed" }));
        assert_eq!(out["code"], json!("STANDARD_UNMET"), "{out}");
        assert_eq!(out["stage"], json!("landed"), "{out}");
        assert_eq!(out["fix"], json!("verify"), "{out}");
        let unmet = out["unmet"].as_array().unwrap();
        assert_eq!(unmet.len(), 1, "{out}");
        assert!(
            unmet[0]["clause"]
                .as_str()
                .unwrap()
                .contains("attest(tests.pass)"),
            "{out}"
        );
        assert_eq!(unmet[0]["fix"], json!("verify"), "{out}");
        assert!(unmet[0]["reason"].is_string(), "{out}");
        assert!(
            out["message"]
                .as_str()
                .unwrap()
                .contains("attest(tests.pass)"),
            "{out}"
        );
    }

    #[test]
    fn a_retried_snapshot_reports_what_the_first_one_recorded() {
        let (_dir, mut repo, mut owner) = scratch();
        let out = call(
            &mut repo,
            &mut owner,
            "edit",
            &json!({ "path": "a.txt", "content": "one" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let first = call(
            &mut repo,
            &mut owner,
            "snapshot",
            &json!({ "title": "one", "idem": "same-key" }),
        );
        assert_eq!(first["ok"], json!(true), "{first}");
        assert_eq!(first["result"]["retried"], json!(false), "{first}");
        let out = call(
            &mut repo,
            &mut owner,
            "edit",
            &json!({ "path": "a.txt", "content": "two" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        let again = call(
            &mut repo,
            &mut owner,
            "snapshot",
            &json!({ "title": "two", "idem": "same-key" }),
        );
        assert_eq!(again["ok"], json!(true), "{again}");
        assert_eq!(
            again["result"]["revision"], first["result"]["revision"],
            "{again}"
        );
        assert_eq!(again["result"]["retried"], json!(true), "{again}");
        assert_eq!(
            again["state"]["revision"], first["result"]["revision"],
            "{again}"
        );
        let m1 = call(
            &mut repo,
            &mut owner,
            "remember",
            &json!({ "kind": "gotcha", "body": "once", "scope": { "kind": "path", "ref": "src" }, "idem": "mem-key" }),
        );
        let m2 = call(
            &mut repo,
            &mut owner,
            "remember",
            &json!({ "kind": "gotcha", "body": "twice", "scope": { "kind": "path", "ref": "src" }, "idem": "mem-key" }),
        );
        assert_eq!(m1["result"]["memory"], m2["result"]["memory"], "{m2}");
    }

    #[test]
    fn a_session_finds_its_workspace_again_after_the_daemon_forgets() {
        let (_dir, mut repo, _) = scratch();
        let mut bot = repo.open_session("bot", None, vec!["**".into()]).unwrap();
        let out = call(
            &mut repo,
            &mut bot,
            "workspace",
            &json!({ "action": "create" }),
        );
        assert_eq!(out["ok"], json!(true), "{out}");
        assert!(bot.workspace.is_some(), "{out}");
        repo.remember_session_workspace(&bot).unwrap();
        let reopened = repo.open_session("bot", None, vec!["**".into()]).unwrap();
        assert_eq!(reopened.workspace, bot.workspace);
    }
}
