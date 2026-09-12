//! The op verification algorithm of `spec/04-security.md`, draft 2.
//!
//! V is the merged view of the op's parents. C is the receiver's current view.
//! Steps: decode, parents, author at V, author at C, signature, authorization,
//! legality, determinism. Storage and indexing are the caller's.

use std::collections::{HashSet, VecDeque};

use ciborium::value::Value;
use tessra_core::cbor;
use tessra_core::object::{
    Capability, Effect, Hook, Intent, Memory, Op, Principal, Revision, Tree, View,
};
use tessra_core::sig::SignedObject;
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};

use crate::dag::OpLog;
use crate::glob;
use crate::standard;
use crate::view::{line_head, Pointer, ViewState};
use crate::{Error, Result};

/// What verification hands back: the view the op produces, recomputed.
pub struct Verified {
    pub view: View,
}

/// Verbs every principal may use without a grant, within its own workspace.
const UNGATED: &[&str] = &[
    "status",
    "context",
    "query",
    "workspace",
    "snapshot",
    "claim",
    "undo",
    "sync",
    "merge_view",
];

pub fn verify_op<S: ObjectStore>(log: &OpLog<S>, op: &Op) -> Result<Verified> {
    let store = log.store();
    let vs = ViewState::new(store);

    // Step 1: decode. Re-encoding proves the object is canonical and typed.
    cbor::encode(op)?;

    // Step 2: parents.
    if op.parents.is_empty() {
        return verify_init(log, op);
    }
    for p in &op.parents {
        if !log.is_verified(p) {
            return Err(Error::UnknownParent(*p));
        }
    }
    let v = log.merged_view(&op.parents)?;
    let c = log.current_view()?;

    // Step 3: author at V.
    let (author_ver_v, author_v) =
        resolve_principal(store, &vs, &v, &op.author)?.ok_or_else(|| {
            Error::rejected(
                3,
                "author",
                format!("author {} not in parents' view", op.author),
            )
        })?;
    if author_v.status != "active" {
        return Err(Error::rejected(
            3,
            "author",
            "author is not active at the parents",
        ));
    }
    if author_v.key != Some(op.key) {
        return Err(Error::rejected(
            3,
            "author",
            "op key does not match the author's key at the parents",
        ));
    }

    // Step 4: author at C.
    if vs.principal_revoked(&c, &op.author)? {
        return Err(Error::rejected(
            4,
            "revoked",
            "author is revoked in the current view",
        ));
    }
    if let Some((author_ver_c, author_c)) = resolve_principal(store, &vs, &c, &op.author)? {
        if author_ver_c != author_ver_v && author_c.key != Some(op.key) {
            return Err(Error::rejected(
                4,
                "rotated",
                "author's key was rotated after what the op saw",
            ));
        }
        if author_c.status != "active" {
            return Err(Error::rejected(
                4,
                "revoked",
                "author is not active in the current view",
            ));
        }
    }

    // Step 5: signature.
    op.verify_with(&op.key)
        .map_err(|e| Error::rejected(5, "signature", e.to_string()))?;

    // Step 6: authorization.
    let is_owner = v.owners.contains(&op.author);
    let cap = if is_owner {
        None
    } else {
        let cap_id = op
            .cap
            .ok_or_else(|| Error::rejected(6, "capability", "non-owner op without a capability"))?;
        if !vs.in_set(&v.caps, &cap_id)? {
            return Err(Error::rejected(
                6,
                "capability",
                "capability is not active at the parents",
            ));
        }
        if !vs.in_set(&c.caps, &cap_id)? {
            return Err(Error::rejected(
                6,
                "capability",
                "capability is not active in the current view",
            ));
        }
        let cap: Capability = store.get(&cap_id)?;
        if cap.subject != op.author {
            return Err(Error::rejected(
                6,
                "capability",
                "capability subject is not the author",
            ));
        }
        check_chain(store, &vs, &v, &cap, &cap_id)?;
        check_authorizes(store, &vs, &v, &cap, op)?;
        Some(cap)
    };

    // Step 7: legality.
    check_legality(store, &vs, &v, op, is_owner, cap.as_ref())?;

    // Step 8: determinism.
    let produced = vs.apply(&v, &op.effects, op.parents.first().copied())?;
    let produced_id = vs.store_view(&produced)?;
    if produced_id != op.view {
        return Err(Error::rejected(
            8,
            "determinism",
            format!("effects produce view {produced_id}, op claims {}", op.view),
        ));
    }
    Ok(Verified { view: produced })
}

/// The init op: no parents, author becomes an owner, principal self-signed.
fn verify_init<S: ObjectStore>(log: &OpLog<S>, op: &Op) -> Result<Verified> {
    let store = log.store();
    let vs = ViewState::new(store);
    if op.kind != "init" {
        return Err(Error::rejected(
            2,
            "parents",
            "only init may have no parents",
        ));
    }
    if !log.heads().is_empty() {
        return Err(Error::rejected(
            2,
            "parents",
            "init on a log that already has ops",
        ));
    }
    op.verify_with(&op.key)
        .map_err(|e| Error::rejected(5, "signature", e.to_string()))?;
    let mut owner_effect = false;
    let mut principal_ok = false;
    for e in &op.effects {
        match e {
            Effect::Owner { principal } if *principal == op.author => owner_effect = true,
            Effect::Point { entity, to, .. } if *entity == op.author => {
                let p: Principal = store.get(to)?;
                if p.id == op.author && p.key == Some(op.key) && p.status == "active" {
                    p.verify_with(&op.key).map_err(|e| {
                        Error::rejected(5, "signature", format!("init principal: {e}"))
                    })?;
                    principal_ok = true;
                }
            }
            _ => {}
        }
    }
    if !owner_effect || !principal_ok {
        return Err(Error::rejected(
            7,
            "legality",
            "init must create its author as an owner with a self-signed principal",
        ));
    }
    let empty = vs.empty()?;
    let produced = vs.apply(&empty, &op.effects, None)?;
    let produced_id = vs.store_view(&produced)?;
    if produced_id != op.view {
        return Err(Error::rejected(
            8,
            "determinism",
            "init effects do not produce the claimed view",
        ));
    }
    Ok(Verified { view: produced })
}

/// The principal's current version object ID and content at a view.
pub fn resolve_principal<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    view: &View,
    id: &EntityId,
) -> Result<Option<(ObjectId, Principal)>> {
    match vs.entity(view, id)? {
        None => Ok(None),
        Some(Pointer::Conflict(_)) => Err(Error::rejected(
            3,
            "author",
            "principal version is in conflict",
        )),
        Some(Pointer::Id(oid)) => Ok(Some((oid, store.get(&oid)?))),
    }
}

fn check_chain<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    view: &View,
    cap: &Capability,
    cap_id: &ObjectId,
) -> Result<()> {
    let mut cur = cap.clone();
    let mut cur_id = *cap_id;
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 64 {
            return Err(Error::rejected(
                6,
                "capability",
                "delegation chain too long",
            ));
        }
        let (_, issuer) = resolve_principal(store, vs, view, &cur.issuer)?
            .ok_or_else(|| Error::rejected(6, "capability", "capability issuer unknown"))?;
        if issuer.status != "active" || vs.principal_revoked(view, &cur.issuer)? {
            return Err(Error::rejected(
                6,
                "capability",
                "capability issuer is revoked",
            ));
        }
        let key = issuer
            .key
            .ok_or_else(|| Error::rejected(6, "capability", "issuer has no key"))?;
        cur.verify_with(&key)
            .map_err(|e| Error::rejected(6, "capability", format!("capability {cur_id}: {e}")))?;
        match cur.parent {
            None => {
                if view.owners.contains(&cur.issuer) {
                    return Ok(());
                }
                return Err(Error::rejected(
                    6,
                    "capability",
                    "chain does not terminate at an owner",
                ));
            }
            Some(pid) => {
                if !vs.in_set(&view.caps, &pid)? {
                    return Err(Error::rejected(
                        6,
                        "capability",
                        "parent capability is not active",
                    ));
                }
                let parent: Capability = store.get(&pid)?;
                if parent.subject != cur.issuer {
                    return Err(Error::rejected(
                        6,
                        "capability",
                        "issuer is not the parent's subject",
                    ));
                }
                if !parent.delegable {
                    return Err(Error::rejected(
                        6,
                        "capability",
                        "parent capability is not delegable",
                    ));
                }
                if !contained_in(&cur, &parent) {
                    return Err(Error::rejected(
                        6,
                        "capability",
                        "child capability exceeds its parent",
                    ));
                }
                cur_id = pid;
                cur = parent;
            }
        }
    }
}

fn contained_in(child: &Capability, parent: &Capability) -> bool {
    if !child.verbs.iter().all(|v| parent.verbs.contains(v)) {
        return false;
    }
    let paths_ok = |c: &Option<Vec<String>>, p: &Option<Vec<String>>| match (c, p) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(cs), Some(ps)) => cs
            .iter()
            .all(|cp| ps.iter().any(|pp| glob::contained(cp, pp))),
    };
    let list_ok = |c: &Option<Vec<String>>, p: &Option<Vec<String>>| match (c, p) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(cs), Some(ps)) => cs.iter().all(|x| ps.contains(x)),
    };
    let ids_ok = |c: &Option<Vec<EntityId>>, p: &Option<Vec<EntityId>>| match (c, p) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(cs), Some(ps)) => cs.iter().all(|x| ps.contains(x)),
    };
    paths_ok(&child.write.paths, &parent.write.paths)
        && paths_ok(&child.read.paths, &parent.read.paths)
        && list_ok(&child.write.roots, &parent.write.roots)
        && list_ok(&child.write.stages, &parent.write.stages)
        && list_ok(&child.write.memory_scopes, &parent.write.memory_scopes)
        && ids_ok(&child.write.nodes, &parent.write.nodes)
        && ids_ok(&child.write.lines, &parent.write.lines)
        && ids_ok(&child.write.targets, &parent.write.targets)
        && match (&child.hard, &parent.hard) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(c), Some(p)) => c.iter().all(|(k, v)| p.get(k).is_some_and(|pv| v <= pv)),
        }
}

/// Does the capability authorize this op's kind and write effects?
fn check_authorizes<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    view: &View,
    cap: &Capability,
    op: &Op,
) -> Result<()> {
    let kind = op.kind.as_str();
    if !UNGATED.contains(&kind) && !cap.verbs.iter().any(|v| v == kind) {
        return Err(Error::rejected(
            6,
            "verb",
            format!("capability does not grant {kind}"),
        ));
    }
    for e in &op.effects {
        match e {
            Effect::Propose { rev } => {
                check_write_scope(store, vs, view, cap, rev)?;
                if let Some(stages) = &cap.write.stages {
                    if !stages.iter().any(|s| s == "proposed") {
                        return Err(Error::rejected(
                            6,
                            "scope",
                            "capability does not allow the proposed stage",
                        ));
                    }
                }
            }
            Effect::Head { line, to, .. } => {
                check_write_scope(store, vs, view, cap, to)?;
                let ls = view
                    .lines
                    .get(line)
                    .ok_or_else(|| Error::rejected(6, "scope", format!("unknown line {line}")))?;
                if let Some(lines) = &cap.write.lines {
                    if !lines.contains(&ls.id) {
                        return Err(Error::rejected(
                            6,
                            "scope",
                            format!("capability does not cover line {line}"),
                        ));
                    }
                }
                if let Some(stages) = &cap.write.stages {
                    if !stages.iter().any(|s| s == "landed") {
                        return Err(Error::rejected(
                            6,
                            "scope",
                            "capability does not allow landing",
                        ));
                    }
                }
            }
            Effect::Deploy { target, .. } => {
                if let Some(ts) = &cap.write.targets {
                    if !ts.contains(target) {
                        return Err(Error::rejected(
                            6,
                            "scope",
                            "capability does not cover this target",
                        ));
                    }
                }
            }
            Effect::Point { to, .. } => {
                let bytes = store
                    .get_bytes(to)?
                    .ok_or(tessra_core::Error::NotFound(*to))?;
                match cbor::peek_tag(&bytes)?.as_str() {
                    "memory" => {
                        let m: Memory = cbor::decode(&bytes)?;
                        if let Some(ms) = &cap.write.memory_scopes {
                            if !ms.contains(&m.scope.kind) {
                                return Err(Error::rejected(
                                    6,
                                    "scope",
                                    format!(
                                        "capability does not allow memory scope {}",
                                        m.scope.kind
                                    ),
                                ));
                            }
                        }
                    }
                    "standard" | "hook" | "channel" | "target" | "line" => {
                        let needed = cbor::peek_tag(&bytes)?;
                        if !cap.verbs.contains(&needed) {
                            return Err(Error::rejected(
                                6,
                                "verb",
                                format!("changing a {needed} needs the {needed} verb"),
                            ));
                        }
                    }
                    _ => {}
                }
            }
            Effect::Owner { .. }
            | Effect::Unowner { .. }
            | Effect::Coordinator { .. }
            | Effect::Root { .. }
            | Effect::Unroot { .. } => {
                return Err(Error::rejected(
                    6,
                    "scope",
                    "only owners may change owners, roots, or coordinators",
                ));
            }
            Effect::Revoke { .. } => {
                if !cap.verbs.iter().any(|v| v == "revoke") {
                    return Err(Error::rejected(6, "verb", "revoke needs the revoke verb"));
                }
            }
            Effect::Tomb { .. } if !cap.verbs.iter().any(|v| v == "redact") => {
                return Err(Error::rejected(6, "verb", "tomb needs the redact verb"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The paths a revision changed relative to its first parent must be inside the write scope.
fn check_write_scope<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    view: &View,
    cap: &Capability,
    rev_id: &ObjectId,
) -> Result<()> {
    let rev: Revision = store.get(rev_id)?;
    let parent: Option<Revision> = match rev.parents.first() {
        Some(p) => Some(store.get(p)?),
        None => None,
    };
    let _ = (vs, view);
    for (root, snap_id) in &rev.snapshots {
        if let Some(roots) = &cap.write.roots {
            if !roots.contains(root) {
                return Err(Error::rejected(
                    6,
                    "scope",
                    format!("root {root:?} is outside the write scope"),
                ));
            }
        }
        let new_snap: tessra_core::object::Snapshot = store.get(snap_id)?;
        let old_root = match &parent {
            Some(p) => match p.snapshots.get(root) {
                Some(s) => Some(store.get::<tessra_core::object::Snapshot>(s)?.root),
                None => None,
            },
            None => None,
        };
        let changed = changed_paths(store, old_root.as_ref(), &new_snap.root)?;
        if let Some(paths) = &cap.write.paths {
            for p in &changed {
                if !glob::any_match(paths, p) {
                    return Err(Error::rejected(
                        6,
                        "scope",
                        format!("path {p} is outside the write scope"),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Paths whose content differs between two trees. Expands shards.
pub fn changed_paths<S: ObjectStore>(
    store: &S,
    old: Option<&ObjectId>,
    new: &ObjectId,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    diff_trees(store, old, Some(new), "", &mut out)?;
    Ok(out)
}

fn load_entries<S: ObjectStore>(
    store: &S,
    id: &ObjectId,
) -> Result<Vec<tessra_core::object::TreeEntry>> {
    let t: Tree = store.get(id)?;
    if let Some(e) = t.entries {
        return Ok(e);
    }
    let mut all = Vec::new();
    for shard in t.shards.unwrap_or_default().into_iter().flatten() {
        let st: Tree = store.get(&shard)?;
        all.extend(st.entries.unwrap_or_default());
    }
    all.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    Ok(all)
}

fn diff_trees<S: ObjectStore>(
    store: &S,
    old: Option<&ObjectId>,
    new: Option<&ObjectId>,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<()> {
    if old == new {
        return Ok(());
    }
    let oe = match old {
        Some(id) => load_entries(store, id)?,
        None => Vec::new(),
    };
    let ne = match new {
        Some(id) => load_entries(store, id)?,
        None => Vec::new(),
    };
    let mut names: Vec<&String> = oe
        .iter()
        .map(|e| &e.name)
        .chain(ne.iter().map(|e| &e.name))
        .collect();
    names.sort();
    names.dedup();
    for name in names {
        let o = oe.iter().find(|e| &e.name == name);
        let n = ne.iter().find(|e| &e.name == name);
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match (o, n) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b))
                if a.kind == tessra_core::object::EntryKind::Dir
                    && b.kind == tessra_core::object::EntryKind::Dir =>
            {
                diff_trees(store, a.r#ref.as_ref(), b.r#ref.as_ref(), &path, out)?;
            }
            (Some(a), None) if a.kind == tessra_core::object::EntryKind::Dir => {
                diff_trees(store, a.r#ref.as_ref(), None, &path, out)?;
            }
            (None, Some(b)) if b.kind == tessra_core::object::EntryKind::Dir => {
                diff_trees(store, None, b.r#ref.as_ref(), &path, out)?;
            }
            _ => out.push(path),
        }
    }
    Ok(())
}

/// Step 7.
fn check_legality<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    v: &View,
    op: &Op,
    is_owner: bool,
    _cap: Option<&Capability>,
) -> Result<()> {
    let landed = landed_revisions(store, v)?;
    for e in &op.effects {
        match e {
            Effect::Point { entity, to, from } => {
                let current = vs.entity(v, entity)?;
                match (&current, from.as_ref().and_then(Pointer::from_value)) {
                    (None, None) => {}
                    (Some(cur), Some(f)) if *cur == f => {}
                    (Some(cur), _) => {
                        return Err(Error::rejected(
                            7,
                            "from",
                            format!("entity {entity} is at {:?}, op expected {from:?}", cur),
                        ))
                    }
                    (None, Some(_)) => {
                        return Err(Error::rejected(
                            7,
                            "from",
                            format!("entity {entity} does not exist yet"),
                        ))
                    }
                }
                let bytes = store
                    .get_bytes(to)?
                    .ok_or(tessra_core::Error::NotFound(*to))?;
                let tag = cbor::peek_tag(&bytes)?;
                match tag.as_str() {
                    "revision" => {
                        let r: Revision = cbor::decode(&bytes)?;
                        if r.id != *entity {
                            return Err(Error::rejected(
                                7,
                                "entity",
                                "revision id does not match the entity",
                            ));
                        }
                        if let Some(Pointer::Id(cur)) = &current {
                            if landed.contains(cur) && op.kind != "restack" {
                                return Err(Error::rejected(
                                    7,
                                    "immutable",
                                    format!(
                                        "change {entity} is landed; correction is a new change"
                                    ),
                                ));
                            }
                            if r.prev != Some(*cur) {
                                return Err(Error::rejected(
                                    7,
                                    "prev",
                                    "revision prev must be the current version",
                                ));
                            }
                        }
                    }
                    "hook" => {
                        let h: Hook = cbor::decode(&bytes)?;
                        if h.r#as != op.author && !is_owner {
                            return Err(Error::rejected(
                                7,
                                "hook",
                                "a hook may only be created by its principal or an owner",
                            ));
                        }
                    }
                    "principal" => {
                        let p: Principal = cbor::decode(&bytes)?;
                        if p.id != *entity {
                            return Err(Error::rejected(
                                7,
                                "entity",
                                "principal id does not match the entity",
                            ));
                        }
                        match &current {
                            None => {
                                // Creation.
                                if p.kind == "session" {
                                    let parent_id = p.parent.ok_or_else(|| {
                                        Error::rejected(7, "principal", "session without parent")
                                    })?;
                                    let (_, parent) = resolve_principal(store, vs, v, &parent_id)?
                                        .ok_or_else(|| {
                                            Error::rejected(
                                                7,
                                                "principal",
                                                "session parent unknown",
                                            )
                                        })?;
                                    if parent.kind != "agent" {
                                        return Err(Error::rejected(
                                            7,
                                            "principal",
                                            "session parent must be an agent",
                                        ));
                                    }
                                    let k = parent.key.ok_or_else(|| {
                                        Error::rejected(7, "principal", "parent agent has no key")
                                    })?;
                                    p.verify_with(&k).map_err(|e| {
                                        Error::rejected(
                                            7,
                                            "principal",
                                            format!("session not signed by its agent: {e}"),
                                        )
                                    })?;
                                } else if !is_owner {
                                    return Err(Error::rejected(
                                        7,
                                        "principal",
                                        "only owners create non-session principals",
                                    ));
                                } else {
                                    p.verify_with(&op.key).map_err(|e| {
                                        Error::rejected(
                                            7,
                                            "principal",
                                            format!("principal not signed by the owner: {e}"),
                                        )
                                    })?;
                                }
                            }
                            Some(Pointer::Id(cur)) => {
                                let old: Principal = store.get(cur)?;
                                if p.prev != Some(*cur) {
                                    return Err(Error::rejected(
                                        7,
                                        "prev",
                                        "principal prev must be the current version",
                                    ));
                                }
                                if p.status == "revoked" {
                                    if !is_owner && !vs.principal_revoked(v, &p.id)? {
                                        return Err(Error::rejected(
                                            7,
                                            "principal",
                                            "revocation needs an owner",
                                        ));
                                    }
                                } else {
                                    let k = old.key.ok_or_else(|| {
                                        Error::rejected(
                                            7,
                                            "principal",
                                            "rotating a keyless principal",
                                        )
                                    })?;
                                    p.verify_with(&k).map_err(|e| {
                                        Error::rejected(
                                            7,
                                            "principal",
                                            format!("rotation not signed by the previous key: {e}"),
                                        )
                                    })?;
                                }
                            }
                            Some(Pointer::Conflict(_)) => {}
                        }
                    }
                    _ => {}
                }
            }
            Effect::Head {
                line,
                to,
                from,
                seq,
                attests,
                id,
            } => {
                let to_rev: Revision = store.get(to)?;
                match v.lines.get(line) {
                    None => {
                        if !is_owner {
                            return Err(Error::rejected(7, "line", "only owners create lines"));
                        }
                        if id.is_none() || from.is_some() || *seq != 0 {
                            return Err(Error::rejected(
                                7,
                                "line",
                                "line creation needs an id, no from, and seq 0",
                            ));
                        }
                    }
                    Some(ls) => {
                        if ls.coordinator != op.author {
                            return Err(Error::rejected(
                                7,
                                "coordinator",
                                format!("only the coordinator of {line} may advance it"),
                            ));
                        }
                        let cur = line_head(ls);
                        let f = from.as_ref().and_then(Pointer::from_value);
                        if cur != f {
                            return Err(Error::rejected(
                                7,
                                "from",
                                format!("line {line} head is {cur:?}, op expected {f:?}"),
                            ));
                        }
                        if *seq != ls.seq + 1 {
                            return Err(Error::rejected(
                                7,
                                "seq",
                                format!("line {line} seq must be {}", ls.seq + 1),
                            ));
                        }
                        for a in attests {
                            if !store.has(a)? {
                                return Err(Error::rejected(
                                    7,
                                    "attests",
                                    format!("cited attestation {a} is not present"),
                                ));
                            }
                        }
                        // A git commit landed as history: an owner's `import`
                        // op whose revision carries a legacy intent and sits
                        // on the head, the way `init --history` lands the
                        // commits before it. It cites nothing and the standard
                        // is not evaluated. An owner could reach the same
                        // state by emptying the standard and restoring it, so
                        // this grants nothing new; it keeps the record honest.
                        // A coordinator that is not an owner has no such path.
                        if is_owner
                            && op.kind == "import"
                            && attests.is_empty()
                            && legacy_intent(store, vs, v, op, &to_rev)?
                        {
                            let on_head = matches!(
                                (&cur, to_rev.parents.first()),
                                (Some(Pointer::Id(h)), Some(p)) if h == p
                            );
                            if !on_head {
                                return Err(Error::rejected(
                                    7,
                                    "history",
                                    format!("a history landing on {line} sits on the head itself"),
                                ));
                            }
                            continue;
                        }
                        let line_obj: tessra_core::object::Line = match vs.entity(v, &ls.id)? {
                            Some(p) => store.get(&p.single()?)?,
                            None => return Err(Error::rejected(7, "line", "line entity missing")),
                        };
                        let unmet = standard::evaluate(
                            store,
                            vs,
                            v,
                            &line_obj.standard,
                            to,
                            &to_rev,
                            attests,
                        )?;
                        if !unmet.is_empty() {
                            let list: Vec<String> = unmet
                                .iter()
                                .map(|u| format!("{}: {}", u.clause, u.reason))
                                .collect();
                            return Err(Error::rejected(
                                7,
                                "standard",
                                format!("standard unmet: {}", list.join("; ")),
                            ));
                        }
                    }
                }
            }
            Effect::Tomb { .. } if op.kind != "redact" => {
                return Err(Error::rejected(7, "tomb", "tombstones only in a redact op"));
            }
            Effect::Owner { .. } | Effect::Unowner { .. } | Effect::Coordinator { .. }
                if !is_owner =>
            {
                return Err(Error::rejected(
                    7,
                    "owner",
                    "only owners change owners or coordinators",
                ));
            }
            Effect::Claim { claim, to } => {
                let c: tessra_core::object::Claim = store.get(to)?;
                if c.principal != op.author {
                    return Err(Error::rejected(
                        7,
                        "claim",
                        "a claim names its author as principal",
                    ));
                }
                if c.id != *claim {
                    return Err(Error::rejected(7, "claim", "claim id mismatch"));
                }
            }
            Effect::Unclaim { claim } => {
                if let Some(Pointer::Id(cur)) = vs.claim(v, claim)? {
                    let c: tessra_core::object::Claim = store.get(&cur)?;
                    if c.principal != op.author && !is_owner {
                        return Err(Error::rejected(
                            7,
                            "claim",
                            "only the claimant or an owner releases a claim",
                        ));
                    }
                }
            }
            Effect::Revoke { principal } if !is_owner => {
                let _ = principal;
                // Verb checked in authorization; nothing further here.
            }
            _ => {}
        }
    }
    Ok(())
}

/// Does the revision carry a legacy intent, a git commit imported as history?
/// The intent is usually put by the same op that lands the revision, so it
/// is looked for among the op's own `point` effects before the parents' view.
fn legacy_intent<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    v: &View,
    op: &Op,
    rev: &Revision,
) -> Result<bool> {
    let Some(intent_id) = rev.intent else {
        return Ok(false);
    };
    let in_op = op.effects.iter().find_map(|e| match e {
        Effect::Point { entity, to, .. } if *entity == intent_id => Some(*to),
        _ => None,
    });
    let oid = match in_op {
        Some(id) => Some(id),
        None => match vs.entity(v, &intent_id)? {
            Some(Pointer::Id(id)) => Some(id),
            _ => None,
        },
    };
    let Some(oid) = oid else {
        return Ok(false);
    };
    let intent: Intent = store.get(&oid)?;
    Ok(intent.legacy == Some(true))
}

/// Every revision reachable from a line head through `parents`.
/// The landed sets last computed, by the line heads they were computed
/// from. The set depends on nothing else in a view, so a status, a
/// legality check, or a frontier pass after the first for the same heads
/// is a lookup instead of a walk over every landed revision.
type LandedEntry = (Vec<ObjectId>, std::sync::Arc<HashSet<ObjectId>>);
static LANDED: std::sync::Mutex<Vec<LandedEntry>> = std::sync::Mutex::new(Vec::new());
const LANDED_KEPT: usize = 16;

pub fn landed_revisions<S: ObjectStore>(store: &S, view: &View) -> Result<HashSet<ObjectId>> {
    let mut key: Vec<ObjectId> = Vec::new();
    for ls in view.lines.values() {
        if let Some(p) = line_head(ls) {
            key.extend(p.ids());
        }
    }
    key.sort();
    if let Ok(cache) = LANDED.lock() {
        if let Some((_, set)) = cache.iter().find(|(k, _)| *k == key) {
            return Ok((**set).clone());
        }
    }
    let mut seen = HashSet::new();
    let mut queue: VecDeque<ObjectId> = key.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(bytes) = store.get_bytes(&id)? {
            if let Ok(r) = cbor::decode::<Revision>(&bytes) {
                queue.extend(r.parents.iter().copied());
            }
        }
    }
    if let Ok(mut cache) = LANDED.lock() {
        if cache.len() >= LANDED_KEPT {
            cache.remove(0);
        }
        cache.push((key, std::sync::Arc::new(seen.clone())));
    }
    Ok(seen)
}

/// A helper for callers that need a `from` value for the current pointer.
pub fn from_value(p: Option<&Pointer>) -> Option<Value> {
    p.map(Pointer::to_value)
}
