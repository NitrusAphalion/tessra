//! The semantic index in the daemon: building indexes at snapshot time
//! against the parent's index, finding or computing an index for any
//! snapshot, node-level merge at landing with renames carried across the
//! merge, node ID reconciliation into aliases, the covering-tests relation,
//! and blame by node.

use std::collections::{BTreeMap, HashMap, HashSet};

use ciborium::value::Value as Cbor;
use serde_bytes::ByteBuf;
use tessra_core::object::{EntryKind, Node, NodeIndex, Revision, SemOp, Snapshot};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_semantic::matching::{assign_ids_with_refs, inferred_renames};
use tessra_semantic::merge::{merge_file, FileNodes};
use tessra_semantic::rename::{contains_ident, is_ident, rename_source, Rename};
use tessra_semantic::{extract, Language, GRAMMARS};
use tessra_store::RedbStore;

use crate::tree::{self, Flat, Leaf};
use crate::{Error, Repo, Result};

/// Nodes of one path from an index, in document order.
pub fn nodes_for_path(idx: &NodeIndex, path: &str) -> Vec<Node> {
    idx.nodes
        .iter()
        .filter(|n| n.path == path)
        .cloned()
        .collect()
}

/// Build the nodes for a flat tree. Files unchanged from the parent keep the
/// parent's nodes; changed files are extracted and matched against the
/// parent's nodes for that path, so identities survive edits.
pub fn build_nodes(
    store: &RedbStore,
    flat: &Flat,
    parent: Option<(&Flat, &NodeIndex)>,
) -> Result<Vec<Node>> {
    let mut parent_by_path: HashMap<&str, Vec<&Node>> = HashMap::new();
    if let Some((_, idx)) = parent {
        for n in &idx.nodes {
            parent_by_path.entry(n.path.as_str()).or_default().push(n);
        }
    }
    let mut out: Vec<Node> = Vec::new();
    // References of re-extracted units that did not resolve within their
    // file, resolved below against the top-level units of the files the
    // file imports, and never against the whole root: a bare `Error` or
    // `describe` must not bind to a same-named unit in an unrelated package.
    let mut pending: Vec<(usize, Vec<String>)> = Vec::new();
    let mut imports_of: HashMap<String, Vec<String>> = HashMap::new();
    for (path, leaf) in flat {
        if leaf.kind != EntryKind::File {
            continue;
        }
        let Some(blob) = leaf.r#ref else { continue };
        if let Some((pflat, _)) = parent {
            if pflat.get(path).and_then(|l| l.r#ref) == Some(blob) {
                if let Some(nodes) = parent_by_path.get(path.as_str()) {
                    out.extend(nodes.iter().map(|n| (*n).clone()));
                }
                continue;
            }
        }
        let Some(bytes) = store.get_bytes(&blob)? else {
            continue;
        };
        let raw = extract(path, &bytes).map_err(|e| Error::verb("INDEX", e.to_string()))?;
        let previous: Vec<Node> = parent_by_path
            .get(path.as_str())
            .map(|v| v.iter().map(|n| (*n).clone()).collect())
            .unwrap_or_default();
        // The parent's text for the path, so a unit renamed and edited in
        // one step can be followed by similarity.
        let previous_source: Option<Vec<u8>> = match parent {
            Some((pflat, _)) if !previous.is_empty() => match pflat.get(path).and_then(|l| l.r#ref)
            {
                Some(prev_blob) => store.get_bytes(&prev_blob)?,
                None => None,
            },
            _ => None,
        };
        let (nodes, unresolved) =
            assign_ids_with_refs(path, &raw, &previous, previous_source.as_deref());
        let mut left_any = false;
        for (i, left) in unresolved.into_iter().enumerate() {
            if !left.is_empty() {
                pending.push((out.len() + i, left));
                left_any = true;
            }
        }
        if left_any {
            let statements: Vec<&str> = raw
                .iter()
                .filter(|r| r.kind == "import" || r.kind == "export")
                .map(|r| r.name.as_str())
                .collect();
            let imported = tessra_semantic::imports::imported_paths(path, &statements, &|p| {
                flat.contains_key(p)
            });
            imports_of.insert(path.clone(), imported);
        }
        out.extend(nodes);
    }
    if !pending.is_empty() {
        // Top-level units by path, then name.
        let mut by_path: HashMap<&str, HashMap<&str, Vec<EntityId>>> = HashMap::new();
        for n in &out {
            if n.parent.is_none() && is_ident(&n.name) {
                by_path
                    .entry(n.path.as_str())
                    .or_default()
                    .entry(n.name.as_str())
                    .or_default()
                    .push(n.nid);
            }
        }
        let resolved: Vec<(usize, Vec<EntityId>)> = pending
            .iter()
            .map(|(i, names)| {
                let unit = &out[*i];
                let files: &[String] = imports_of.get(&unit.path).map(Vec::as_slice).unwrap_or(&[]);
                // A name resolves when exactly one imported file defines it.
                let ids: Vec<EntityId> = names
                    .iter()
                    .filter_map(|name| {
                        let mut found: Vec<EntityId> = files
                            .iter()
                            .filter_map(|f| by_path.get(f.as_str()))
                            .filter_map(|m| m.get(name.as_str()))
                            .flatten()
                            .copied()
                            .collect();
                        found.sort();
                        found.dedup();
                        match found.as_slice() {
                            [one] if *one != unit.nid => Some(*one),
                            _ => None,
                        }
                    })
                    .collect();
                (*i, ids)
            })
            .collect();
        for (i, ids) in resolved {
            if ids.is_empty() {
                continue;
            }
            let deps = out[i].deps.get_or_insert_with(Vec::new);
            deps.extend(ids);
            deps.sort();
            deps.dedup();
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.span.0.cmp(&b.span.0)));
    Ok(out)
}

/// The aliases the given indexes carry, merged-away ID to survivor, so a
/// unit a landing merged under another ID lines up with itself on every
/// side of a later merge.
fn aliases_of(indexes: &[Option<&NodeIndex>]) -> HashMap<EntityId, EntityId> {
    let mut map = HashMap::new();
    for idx in indexes.iter().flatten() {
        if let Some(aliases) = &idx.aliases {
            for (k, v) in aliases {
                if let Ok(k) = EntityId::from_slice(k) {
                    map.insert(k, *v);
                }
            }
        }
    }
    map
}

/// The nodes with their IDs and parents resolved through `aliases`.
fn canonical(mut nodes: Vec<Node>, aliases: &HashMap<EntityId, EntityId>) -> Vec<Node> {
    if aliases.is_empty() {
        return nodes;
    }
    let resolve = |mut id: EntityId| {
        for _ in 0..64 {
            match aliases.get(&id) {
                Some(next) if *next != id => id = *next,
                _ => break,
            }
        }
        id
    };
    for n in nodes.iter_mut() {
        n.nid = resolve(n.nid);
        n.parent = n.parent.map(resolve);
    }
    nodes
}

/// The renames a side made relative to the base, inferred from unit
/// identity across the whole index, with the path each was seen in.
pub fn renames_between(base: &NodeIndex, side: &NodeIndex) -> Vec<Rename> {
    let aliases = aliases_of(&[Some(base), Some(side)]);
    let mut by_path_base: HashMap<&str, Vec<Node>> = HashMap::new();
    for n in &base.nodes {
        by_path_base
            .entry(n.path.as_str())
            .or_default()
            .push(n.clone());
    }
    let mut by_path_side: HashMap<&str, Vec<Node>> = HashMap::new();
    for n in &side.nodes {
        by_path_side
            .entry(n.path.as_str())
            .or_default()
            .push(n.clone());
    }
    if !aliases.is_empty() {
        for nodes in by_path_base.values_mut().chain(by_path_side.values_mut()) {
            *nodes = canonical(std::mem::take(nodes), &aliases);
        }
    }
    let mut out = Vec::new();
    for (path, after) in &by_path_side {
        let Some(before) = by_path_base.get(path) else {
            continue;
        };
        for (nid, r) in inferred_renames(path, before, after) {
            // A method's rename stays within its file; a top-level unit's
            // rename reaches every file that names it.
            let top = after.iter().any(|n| n.nid == nid && n.parent.is_none());
            out.push(Rename {
                path: if top { None } else { r.path },
                ..r
            });
        }
    }
    out
}

/// The renames recorded on a revision's ops.
pub fn recorded_renames(ops: &[SemOp]) -> Vec<Rename> {
    ops.iter()
        .filter(|o| o.kind == "rename")
        .filter_map(|o| {
            let text = |k: &str| match o.args.get(k) {
                Some(Cbor::Text(s)) => Some(s.clone()),
                _ => None,
            };
            Some(Rename {
                from: text("from")?,
                to: text("to")?,
                path: text("path"),
            })
        })
        .collect()
}

/// Build the rename op `edit` records.
pub fn rename_op(from: &str, to: &str, path: Option<&str>) -> SemOp {
    let mut args = BTreeMap::new();
    args.insert("from".to_string(), Cbor::Text(from.into()));
    args.insert("to".to_string(), Cbor::Text(to.into()));
    if let Some(p) = path {
        args.insert("path".to_string(), Cbor::Text(p.into()));
    }
    SemOp {
        kind: "rename".into(),
        args,
    }
}

/// Store an index for a root tree.
pub fn put_index(
    store: &RedbStore,
    root: ObjectId,
    parents: Vec<ObjectId>,
    nodes: Vec<Node>,
) -> Result<ObjectId> {
    put_index_with_aliases(store, root, parents, nodes, None)
}

pub fn put_index_with_aliases(
    store: &RedbStore,
    root: ObjectId,
    parents: Vec<ObjectId>,
    nodes: Vec<Node>,
    aliases: Option<BTreeMap<ByteBuf, EntityId>>,
) -> Result<ObjectId> {
    let idx = NodeIndex {
        root,
        grammars: GRAMMARS.into(),
        parents,
        nodes,
        aliases: aliases.filter(|a| !a.is_empty()),
    };
    Ok(store.put(&idx)?)
}

/// Landing step 3: where both sides introduced a unit with equal
/// `(path, kind, name)` or equal body, the lexically lower ID survives in
/// the merged nodes and the other is recorded as its alias. The merged
/// nodes were matched against the trunk side, so the other side's fresh
/// IDs are the ones looked for.
pub fn reconcile_aliases(
    merged: &mut [Node],
    base: Option<&NodeIndex>,
    other: &NodeIndex,
) -> BTreeMap<ByteBuf, EntityId> {
    let base_ids: HashSet<EntityId> = base
        .map(|b| b.nodes.iter().map(|n| n.nid).collect())
        .unwrap_or_default();
    let merged_ids: HashSet<EntityId> = merged.iter().map(|n| n.nid).collect();
    let mut aliases: BTreeMap<ByteBuf, EntityId> = BTreeMap::new();
    let mut renumber: HashMap<EntityId, EntityId> = HashMap::new();
    let mut taken: HashSet<EntityId> = HashSet::new();
    for o in &other.nodes {
        if base_ids.contains(&o.nid) || merged_ids.contains(&o.nid) {
            continue;
        }
        let candidate = merged
            .iter()
            .filter(|m| m.path == o.path && !base_ids.contains(&m.nid) && !taken.contains(&m.nid))
            .find(|m| {
                m.kind == o.kind && m.name == o.name && m.parent.is_some() == o.parent.is_some()
            })
            .or_else(|| {
                merged
                    .iter()
                    .filter(|m| {
                        m.path == o.path && !base_ids.contains(&m.nid) && !taken.contains(&m.nid)
                    })
                    .find(|m| m.kind == o.kind && m.body == o.body)
            });
        let Some(m) = candidate else { continue };
        taken.insert(m.nid);
        if o.nid < m.nid {
            renumber.insert(m.nid, o.nid);
            aliases.insert(ByteBuf::from(m.nid.0.to_vec()), o.nid);
        } else {
            aliases.insert(ByteBuf::from(o.nid.0.to_vec()), m.nid);
        }
    }
    if !renumber.is_empty() {
        for n in merged.iter_mut() {
            if let Some(to) = renumber.get(&n.nid) {
                n.nid = *to;
            }
            if let Some(p) = n.parent {
                if let Some(to) = renumber.get(&p) {
                    n.parent = Some(*to);
                }
            }
            if let Some(deps) = n.deps.as_mut() {
                for d in deps.iter_mut() {
                    if let Some(to) = renumber.get(d) {
                        *d = *to;
                    }
                }
                deps.sort();
                deps.dedup();
            }
        }
    }
    aliases
}

/// One unit's change between two indexes.
pub struct UnitChange {
    pub path: String,
    pub kind: String,
    pub name: String,
    pub nid: EntityId,
    /// `added`, `removed`, `changed`, or `renamed`.
    pub change: &'static str,
    /// The previous name, for `renamed`.
    pub from: Option<String>,
}

/// Node-level diff: units added, removed, changed, or renamed from `before`
/// to `after`, by identity, in path then span order of the side they are on.
pub fn diff_indexes(before: &NodeIndex, after: &NodeIndex) -> Vec<UnitChange> {
    let prev: HashMap<EntityId, &Node> = before.nodes.iter().map(|n| (n.nid, n)).collect();
    let next: HashSet<EntityId> = after.nodes.iter().map(|n| n.nid).collect();
    let mut out = Vec::new();
    for n in &after.nodes {
        let change = match prev.get(&n.nid) {
            None => "added",
            Some(p) if p.name != n.name => "renamed",
            Some(p) if p.body != n.body => "changed",
            Some(_) => continue,
        };
        out.push(UnitChange {
            path: n.path.clone(),
            kind: n.kind.clone(),
            name: n.name.clone(),
            nid: n.nid,
            change,
            from: prev
                .get(&n.nid)
                .map(|p| p.name.clone())
                .filter(|_| change == "renamed"),
        });
    }
    for n in &before.nodes {
        if !next.contains(&n.nid) {
            out.push(UnitChange {
                path: n.path.clone(),
                kind: n.kind.clone(),
                name: n.name.clone(),
                nid: n.nid,
                change: "removed",
                from: None,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.name.cmp(&b.name)));
    out
}

/// Tests covering each unit: the `test` units whose deps name it, across
/// the root. Keyed by the covered unit's ID.
pub fn covering_tests(idx: &NodeIndex) -> HashMap<EntityId, Vec<&Node>> {
    let mut out: HashMap<EntityId, Vec<&Node>> = HashMap::new();
    for t in idx.nodes.iter().filter(|n| n.kind == "test") {
        if let Some(deps) = &t.deps {
            for d in deps {
                out.entry(*d).or_default().push(t);
            }
        }
    }
    out
}

/// The index for a snapshot: the one it references, or one computed on
/// demand without a parent and cached against the snapshot ID. Callers
/// that hold the revision use `index_for_revision`, which computes a
/// missing index against the parent's so identities survive.
pub fn index_for_snapshot(store: &RedbStore, snap_id: &ObjectId) -> Result<(ObjectId, NodeIndex)> {
    if let Some(found) = cached_index(store, snap_id)? {
        return Ok(found);
    }
    build_index(store, snap_id, None)
}

/// The index for a revision's root snapshot. A snapshot that carries none,
/// as those of a history imported before indexes were built at import do,
/// gets one computed against its parent revision's index, oldest first
/// along the chain, so units keep their identity and a diff between two
/// such revisions shows only what changed between them.
pub fn index_for_revision(store: &RedbStore, rev: &Revision) -> Result<(ObjectId, NodeIndex)> {
    let snap_of = |r: &Revision| r.snapshots.get("").copied();
    let Some(snap_id) = snap_of(rev) else {
        return Err(Error::verb("ROOT", "revision has no root snapshot"));
    };
    // The ancestors still without an index, nearest first, each with the
    // snapshot of its first parent.
    let mut todo: Vec<(ObjectId, Option<ObjectId>)> = Vec::new();
    let mut cur_snap = snap_id;
    let mut cur_parents = rev.parents.clone();
    while cached_index(store, &cur_snap)?.is_none() {
        let parent_snap = match cur_parents.first() {
            Some(p) => {
                let pr: Revision = store.get(p)?;
                cur_parents = pr.parents.clone();
                snap_of(&pr)
            }
            None => None,
        };
        todo.push((cur_snap, parent_snap));
        match parent_snap {
            Some(ps) if todo.len() < 100_000 => cur_snap = ps,
            _ => break,
        }
    }
    for (snap, parent) in todo.into_iter().rev() {
        build_index(store, &snap, parent.as_ref())?;
    }
    index_for_snapshot(store, &snap_id)
}

/// The index a snapshot references, or the one cached for it.
fn cached_index(store: &RedbStore, snap_id: &ObjectId) -> Result<Option<(ObjectId, NodeIndex)>> {
    let snap: Snapshot = store.get(snap_id)?;
    if let Some(id) = snap.index {
        return Ok(Some((id, store.get(&id)?)));
    }
    if let Some(bytes) = store.meta(&format!("idx:{}", snap_id.to_hex()))? {
        if let Ok(id) = ObjectId::from_slice(&bytes) {
            if let Ok(idx) = store.get::<NodeIndex>(&id) {
                return Ok(Some((id, idx)));
            }
        }
    }
    Ok(None)
}

/// Compute a snapshot's index, against the index of `parent_snap` when
/// given, and cache it against the snapshot ID.
fn build_index(
    store: &RedbStore,
    snap_id: &ObjectId,
    parent_snap: Option<&ObjectId>,
) -> Result<(ObjectId, NodeIndex)> {
    let snap: Snapshot = store.get(snap_id)?;
    let flat = tree::flatten(store, &snap.root)?;
    let parent = match parent_snap {
        Some(p) => {
            let (pid, pidx) = index_for_snapshot(store, p)?;
            let psnap: Snapshot = store.get(p)?;
            Some((tree::flatten(store, &psnap.root)?, pid, pidx))
        }
        None => None,
    };
    store.begin_batch();
    let nodes = build_nodes(store, &flat, parent.as_ref().map(|(f, _, i)| (f, i)))?;
    let id = put_index(
        store,
        snap.root,
        parent.iter().map(|(_, pid, _)| *pid).collect(),
        nodes,
    )?;
    store.end_batch()?;
    store.set_meta(&format!("idx:{}", snap_id.to_hex()), &id.0)?;
    Ok((id, store.get(&id)?))
}

/// The result of a three-way tree merge.
pub struct TreeMerge {
    pub flat: Flat,
    /// Unresolved, `path: kind name, ...`.
    pub conflicts: Vec<String>,
    /// Semantic conflicts resolved by carrying a rename across, `path: kind name: old renamed to new`.
    pub resolved: Vec<String>,
}

/// Merge three trees. Files changed on both sides merge at node level; only
/// units changed on both sides conflict. Each side's renames, recorded on
/// its revisions or inferred from its index, are applied to what the other
/// side contributes, in files both changed and in files only the other
/// side changed.
#[allow(clippy::too_many_arguments)]
pub fn merge_trees(
    store: &RedbStore,
    base: &Flat,
    a: &Flat,
    b: &Flat,
    base_idx: Option<&NodeIndex>,
    a_idx: &NodeIndex,
    b_idx: &NodeIndex,
    ops_a: &[Rename],
    ops_b: &[Rename],
) -> Result<TreeMerge> {
    let mut ren_a: Vec<Rename> = ops_a.to_vec();
    let mut ren_b: Vec<Rename> = ops_b.to_vec();
    if let Some(bi) = base_idx {
        for r in renames_between(bi, a_idx) {
            if !ren_a.iter().any(|x| x.from == r.from && x.to == r.to) {
                ren_a.push(r);
            }
        }
        for r in renames_between(bi, b_idx) {
            if !ren_b.iter().any(|x| x.from == r.from && x.to == r.to) {
                ren_b.push(r);
            }
        }
    }
    let (mut merged, path_conflicts) = tree::merge3(base, a, b);
    let mut conflicts = Vec::new();
    let mut resolved = Vec::new();
    // Units line up by node ID in the merge, resolved through whatever
    // aliases the three indexes wrote at earlier landings.
    let aliases = aliases_of(&[base_idx, Some(a_idx), Some(b_idx)]);
    // Files only one side changed: carry the other side's renames into them.
    let mut carried: Vec<(String, ObjectId, Vec<u8>)> = Vec::new();
    for (path, leaf) in &merged {
        if leaf.kind != EntryKind::File {
            continue;
        }
        let Some(lang) = Language::from_path(path) else {
            continue;
        };
        let Some(blob) = leaf.r#ref else { continue };
        let base_ref = base.get(path).and_then(|l| l.r#ref);
        let a_ref = a.get(path).and_then(|l| l.r#ref);
        let b_ref = b.get(path).and_then(|l| l.r#ref);
        let from_b_only = b_ref == Some(blob) && a_ref == base_ref && b_ref != base_ref;
        let from_a_only = a_ref == Some(blob) && b_ref == base_ref && a_ref != base_ref;
        let renames = if from_b_only {
            &ren_a
        } else if from_a_only {
            &ren_b
        } else {
            continue;
        };
        let applicable: Vec<&Rename> = renames.iter().filter(|r| r.applies_to(path)).collect();
        if applicable.is_empty() {
            continue;
        }
        let Some(text) = store.get_bytes(&blob)? else {
            continue;
        };
        if !applicable.iter().any(|r| contains_ident(&text, &r.from)) {
            continue;
        }
        let applicable: Vec<Rename> = applicable.into_iter().cloned().collect();
        let (cur, applied) = rename_source(lang, &text, &applicable);
        if applied.is_empty() {
            continue;
        }
        for r in applied {
            resolved.push(format!("{path}: {} renamed to {}", r.from, r.to));
        }
        carried.push((path.clone(), blob, cur));
    }
    for (path, _, text) in carried {
        let id = store.put_blob(&text)?;
        let exec = merged.get(&path).and_then(|l| l.mode).unwrap_or(0) & 1 == 1;
        merged.insert(path, Leaf::file(id, exec));
    }
    for path in path_conflicts {
        let (Some(la), Some(lb)) = (a.get(&path), b.get(&path)) else {
            conflicts.push(format!("{path}: deleted on one side, changed on the other"));
            continue;
        };
        let (Some(ra), Some(rb)) = (la.r#ref, lb.r#ref) else {
            conflicts.push(format!("{path}: both sides changed"));
            continue;
        };
        if la.kind != EntryKind::File || lb.kind != EntryKind::File {
            conflicts.push(format!("{path}: both sides changed"));
            continue;
        }
        let ta = store.get_bytes(&ra)?.unwrap_or_default();
        let tb = store.get_bytes(&rb)?.unwrap_or_default();
        let na = canonical(nodes_for_path(a_idx, &path), &aliases);
        let nb = canonical(nodes_for_path(b_idx, &path), &aliases);
        let base_leaf = base.get(&path).filter(|l| l.kind == EntryKind::File);
        let base_text = match base_leaf.and_then(|l| l.r#ref) {
            Some(r) => store.get_bytes(&r)?,
            None => None,
        };
        let base_nodes = base_idx
            .map(|i| canonical(nodes_for_path(i, &path), &aliases))
            .unwrap_or_default();
        let base_fn = match (&base_text, base_leaf) {
            (Some(t), Some(_)) => Some(FileNodes {
                text: t,
                nodes: &base_nodes,
            }),
            _ => None,
        };
        let ops_for = |ren: &[Rename]| -> Vec<Rename> {
            ren.iter()
                .filter(|r| r.applies_to(&path))
                .cloned()
                .collect()
        };
        let m = merge_file(
            Language::from_path(&path),
            base_fn,
            FileNodes {
                text: &ta,
                nodes: &na,
            },
            FileNodes {
                text: &tb,
                nodes: &nb,
            },
            &ops_for(&ren_a),
            &ops_for(&ren_b),
        );
        for r in &m.resolved {
            resolved.push(format!("{path}: {r}"));
        }
        if m.conflicts.is_empty() {
            let id = store.put_blob(&m.text)?;
            let exec = la.mode.unwrap_or(0) & 1 == 1;
            merged.insert(path, Leaf::file(id, exec));
        } else {
            conflicts.push(format!("{path}: {}", m.conflicts.join(", ")));
        }
    }
    Ok(TreeMerge {
        flat: merged,
        conflicts,
        resolved,
    })
}

/// Who last changed each unit of a path, walking first parents.
pub struct BlameEntry {
    pub nid: EntityId,
    pub kind: String,
    pub name: String,
    pub span: (u64, u64),
    pub revision: ObjectId,
    pub author: EntityId,
    pub title: String,
    pub intent: Option<EntityId>,
    pub time: i64,
}

pub fn blame(repo: &Repo, rev_id: ObjectId, path: &str) -> Result<Vec<BlameEntry>> {
    let store = repo.store();
    let mut revs: HashMap<ObjectId, Revision> = HashMap::new();
    let mut nodes_cache: HashMap<ObjectId, Vec<Node>> = HashMap::new();
    // Per revision, the aliases its index wrote: merged-away ID to survivor.
    let mut alias_cache: HashMap<ObjectId, HashMap<EntityId, EntityId>> = HashMap::new();
    fn rev_of<'a>(
        store: &RedbStore,
        cache: &'a mut HashMap<ObjectId, Revision>,
        id: ObjectId,
    ) -> Result<&'a Revision> {
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(id) {
            e.insert(store.get(&id)?);
        }
        Ok(cache.get(&id).unwrap())
    }
    fn nodes_of<'a>(
        store: &RedbStore,
        revs: &mut HashMap<ObjectId, Revision>,
        cache: &'a mut HashMap<ObjectId, Vec<Node>>,
        aliases: &mut HashMap<ObjectId, HashMap<EntityId, EntityId>>,
        id: ObjectId,
        path: &str,
    ) -> Result<&'a Vec<Node>> {
        #[allow(clippy::map_entry)]
        if !cache.contains_key(&id) {
            let rev = rev_of(store, revs, id)?.clone();
            let nodes = match rev.snapshots.get("") {
                Some(_) => {
                    let (_, idx) = index_for_revision(store, &rev)?;
                    let map: HashMap<EntityId, EntityId> = idx
                        .aliases
                        .as_ref()
                        .map(|a| {
                            a.iter()
                                .filter_map(|(k, v)| EntityId::from_slice(k).ok().map(|k| (k, *v)))
                                .collect()
                        })
                        .unwrap_or_default();
                    aliases.insert(id, map);
                    nodes_for_path(&idx, path)
                }
                None => Vec::new(),
            };
            cache.insert(id, nodes);
        }
        Ok(cache.get(&id).unwrap())
    }
    let start_nodes = nodes_of(
        store,
        &mut revs,
        &mut nodes_cache,
        &mut alias_cache,
        rev_id,
        path,
    )?
    .clone();
    let mut out = Vec::new();
    for n in start_nodes {
        // Walk to the oldest revision in which this unit has the same body,
        // following whichever parent carries it: the first parent when the
        // unit was untouched by a landing, the merged-in side when it came
        // from there. An alias written at a landing maps the ID the
        // merged-in side used to the one that survived.
        let mut cur = rev_id;
        let mut track = n.nid;
        let mut depth = 0;
        loop {
            depth += 1;
            if depth > 1000 {
                break;
            }
            let parents = rev_of(store, &mut revs, cur)?.parents.clone();
            let cur_aliases = alias_cache.get(&cur).cloned().unwrap_or_default();
            let mut next = None;
            for p in parents {
                let pn = nodes_of(
                    store,
                    &mut revs,
                    &mut nodes_cache,
                    &mut alias_cache,
                    p,
                    path,
                )?;
                let found = pn
                    .iter()
                    .find(|q| q.nid == track || cur_aliases.get(&q.nid) == Some(&track))
                    .or_else(|| {
                        let c: Vec<&Node> = pn
                            .iter()
                            .filter(|q| q.kind == n.kind && q.name == n.name)
                            .collect();
                        if c.len() == 1 {
                            Some(c[0])
                        } else {
                            None
                        }
                    })
                    .filter(|q| q.body == n.body)
                    .map(|q| q.nid);
                if let Some(id) = found {
                    track = id;
                    next = Some(p);
                    break;
                }
            }
            match next {
                Some(p) => cur = p,
                None => break,
            }
        }
        let r = rev_of(store, &mut revs, cur)?;
        out.push(BlameEntry {
            nid: n.nid,
            kind: n.kind.clone(),
            name: n.name.clone(),
            span: n.span,
            revision: cur,
            author: r.author,
            title: r.title.clone(),
            intent: r.intent,
            time: r.time,
        });
    }
    out.sort_by_key(|e| e.span.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_semantic::matching::assign_ids;

    fn index(nodes: Vec<Node>) -> NodeIndex {
        NodeIndex {
            root: ObjectId([0; 32]),
            grammars: GRAMMARS.into(),
            parents: vec![],
            nodes,
            aliases: None,
        }
    }

    fn nodes(path: &str, src: &str, prev: &[Node]) -> Vec<Node> {
        let raw = extract(path, src.as_bytes()).unwrap();
        assign_ids(path, &raw, prev)
    }

    #[test]
    fn references_resolve_through_imports_and_never_repository_wide() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&dir.path().join("o.redb")).unwrap();
        let mut flat = Flat::new();
        let mut put = |path: &str, text: &str| {
            flat.insert(
                path.to_string(),
                Leaf::file(store.put_blob(text.as_bytes()).unwrap(), false),
            );
        };
        // A story in another package defines a unit named Error; a docsite
        // module defines describe; neither is imported by the adapter.
        put(
            "basecomponents/src/lib/Badge/Badge.stories.js",
            "export function Error() { return 1; }\n",
        );
        put(
            "docsite/src/lib/reference/options.ts",
            "export function describe() { return 2; }\n",
        );
        put(
            "platform/src/lib/adapters/kalshi/keys.ts",
            "export function parseKey(s: string) { return s; }\n",
        );
        put(
            "platform/src/lib/adapters/kalshi/signing.ts",
            "import { parseKey } from './keys';\nexport class KalshiSigningError extends Error {}\nexport function sign(k: string) { return parseKey(k); }\n",
        );
        put(
            "platform/src/lib/adapters/kalshi/signing.test.ts",
            "import { describe, it } from 'vitest';\nimport { sign } from './signing';\ndescribe('sign', () => { it('works', () => sign('k')); });\n",
        );
        let nodes = build_nodes(&store, &flat, None).unwrap();
        let find = |path: &str, name: &str| {
            nodes
                .iter()
                .find(|n| n.path == path && n.name == name)
                .unwrap_or_else(|| panic!("{path}:{name}"))
                .clone()
        };
        let story_error = find("basecomponents/src/lib/Badge/Badge.stories.js", "Error");
        let docsite_describe = find("docsite/src/lib/reference/options.ts", "describe");
        let parse_key = find("platform/src/lib/adapters/kalshi/keys.ts", "parseKey");
        let sign = find("platform/src/lib/adapters/kalshi/signing.ts", "sign");
        let error_class = find(
            "platform/src/lib/adapters/kalshi/signing.ts",
            "KalshiSigningError",
        );
        let deps = |n: &Node| n.deps.clone().unwrap_or_default();
        assert!(
            !deps(&error_class).contains(&story_error.nid),
            "the global Error is not the story's unit"
        );
        assert!(
            deps(&sign).contains(&parse_key.nid),
            "an imported name resolves to the imported file's unit"
        );
        for n in nodes
            .iter()
            .filter(|n| n.path == "platform/src/lib/adapters/kalshi/signing.test.ts")
        {
            assert!(
                !deps(n).contains(&docsite_describe.nid),
                "vitest's describe is not the docsite's unit: {}",
                n.name
            );
        }
        let test_import = find(
            "platform/src/lib/adapters/kalshi/signing.test.ts",
            "import { sign } from './signing';",
        );
        assert!(deps(&test_import).contains(&sign.nid));
    }

    #[test]
    fn a_revision_whose_snapshot_has_no_index_is_indexed_against_its_parent() {
        // Two revisions as a history import wrote them before indexes were
        // built at import: snapshots without an index, chained by parent.
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&dir.path().join("o.redb")).unwrap();
        let mut revs = Vec::new();
        let mut parent: Option<ObjectId> = None;
        for lib in [
            "fn a() { 1 }\nfn b() { 2 }\n",
            "fn a() { 1 }\nfn b() { 22 }\n",
        ] {
            let mut flat = Flat::new();
            flat.insert(
                "notes.md".into(),
                Leaf::file(store.put_blob(b"# notes\n\nprose\n").unwrap(), false),
            );
            flat.insert(
                "lib.rs".into(),
                Leaf::file(store.put_blob(lib.as_bytes()).unwrap(), false),
            );
            let root = tree::build(&store, &flat).unwrap();
            let rules = store
                .put(&tessra_core::object::TrackingRules::default())
                .unwrap();
            let snap = store
                .put(&Snapshot {
                    root,
                    rules,
                    env: None,
                    index: None,
                })
                .unwrap();
            let rev = Revision {
                id: EntityId::random(),
                prev: None,
                snapshots: BTreeMap::from([("".to_string(), snap)]),
                parents: parent.into_iter().collect(),
                intent: None,
                title: "import".into(),
                body: None,
                author: EntityId::random(),
                time: 0,
                ops: None,
                flags: None,
            };
            parent = Some(store.put(&rev).unwrap());
            revs.push(rev);
        }
        let (_, head_idx) = index_for_revision(&store, &revs[1]).unwrap();
        let (_, base_idx) = index_for_revision(&store, &revs[0]).unwrap();
        let changes: Vec<(String, String, &str)> = diff_indexes(&base_idx, &head_idx)
            .into_iter()
            .map(|c| (c.path, c.name, c.change))
            .collect();
        assert_eq!(
            changes,
            vec![("lib.rs".to_string(), "b".to_string(), "changed")]
        );
    }

    #[test]
    fn renames_are_inferred_across_the_index_and_scoped_by_level() {
        let base_lib = nodes(
            "lib.rs",
            "fn a() { 1 }\nimpl S { fn m(&self) { 2 } }\n",
            &[],
        );
        let base = index(base_lib.clone());
        let side = index(nodes(
            "lib.rs",
            "fn b() { 1 }\nimpl S { fn n(&self) { 2 } }\n",
            &base_lib,
        ));
        let mut r = renames_between(&base, &side);
        r.sort_by(|x, y| x.from.cmp(&y.from));
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(
            (r[0].from.as_str(), r[0].to.as_str(), r[0].path.as_deref()),
            ("a", "b", None)
        );
        assert_eq!(
            (r[1].from.as_str(), r[1].to.as_str(), r[1].path.as_deref()),
            ("m", "n", Some("lib.rs"))
        );
        // The functions are renames; the impl that holds one of them changed,
        // since its body covers its children.
        let d = diff_indexes(&base, &side);
        let mut changes: Vec<(&str, &str, Option<&str>)> = d
            .iter()
            .map(|c| (c.kind.as_str(), c.change, c.from.as_deref()))
            .collect();
        changes.sort();
        assert_eq!(
            changes,
            vec![
                ("function", "renamed", Some("a")),
                ("function", "renamed", Some("m")),
                ("impl", "changed", None)
            ]
        );
    }

    #[test]
    fn same_unit_introduced_on_both_sides_gets_one_id_and_an_alias() {
        let base_nodes = nodes("lib.rs", "fn a() { 1 }\n", &[]);
        let a_side = nodes("lib.rs", "fn a() { 1 }\nfn new_one() { 2 }\n", &base_nodes);
        let b_side = nodes("lib.rs", "fn a() { 1 }\nfn new_one() { 2 }\n", &base_nodes);
        let a_new = a_side[1].nid;
        let b_new = b_side[1].nid;
        assert_ne!(a_new, b_new);
        // The merged index was matched against side A, so it carries A's ID.
        let mut merged = a_side.clone();
        let aliases = reconcile_aliases(
            &mut merged,
            Some(&index(base_nodes.clone())),
            &index(b_side.clone()),
        );
        let survivor = merged[1].nid;
        let loser = if survivor == a_new { b_new } else { a_new };
        assert_eq!(
            survivor,
            a_new.min(b_new),
            "the lexically lower ID survives"
        );
        assert_eq!(aliases.len(), 1);
        assert_eq!(
            aliases.get(&ByteBuf::from(loser.0.to_vec())),
            Some(&survivor)
        );
        assert_eq!(
            merged[0].nid, base_nodes[0].nid,
            "units from the base are untouched"
        );
    }

    #[test]
    fn covering_tests_follow_deps_across_files() {
        let lib = nodes(
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn unused() {}\n",
            &[],
        );
        let mut all = lib.clone();
        // A test in another file that names `add`; the root-wide pass in
        // build_nodes resolves it, modeled here by setting deps directly.
        let mut t = nodes(
            "tests/t.rs",
            "#[test]\nfn add_works() { assert_eq!(add(1, 2), 3); }\n",
            &[],
        );
        assert_eq!(t[0].kind, "test");
        t[0].deps = Some(vec![lib[0].nid]);
        all.extend(t);
        let idx = index(all);
        let cov = covering_tests(&idx);
        assert_eq!(cov.get(&lib[0].nid).map(|v| v.len()), Some(1));
        assert!(!cov.contains_key(&lib[1].nid));
    }
}
