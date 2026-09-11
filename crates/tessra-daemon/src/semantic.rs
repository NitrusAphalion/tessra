//! The semantic index in the daemon: building indexes at snapshot time
//! against the parent's index, finding or computing an index for any
//! snapshot, node-level merge at landing with renames carried across the
//! merge, node ID reconciliation into aliases, the covering-tests relation,
//! and blame by node.

use std::collections::{BTreeMap, HashMap, HashSet};

use ciborium::value::Value as Cbor;
use tessra_core::object::{EntryKind, Node, NodeIndex, Revision, SemOp, Snapshot};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_semantic::matching::{assign_ids_with_refs, inferred_renames};
use tessra_semantic::merge::{merge_file, FileNodes};
use tessra_semantic::rename::{contains_ident, is_ident, replace_ident, Rename};
use tessra_semantic::{extract, Language, GRAMMARS};
use tessra_store::RedbStore;
use serde_bytes::ByteBuf;

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
    // file, resolved below against the top-level units of the whole root.
    let mut pending: Vec<(usize, Vec<String>)> = Vec::new();
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
        let (nodes, unresolved) = assign_ids_with_refs(path, &raw, &previous);
        for (i, left) in unresolved.into_iter().enumerate() {
            if !left.is_empty() {
                pending.push((out.len() + i, left));
            }
        }
        out.extend(nodes);
    }
    if !pending.is_empty() {
        // Top-level units by name across the root; only unambiguous names resolve.
        let mut by_name: HashMap<&str, Option<EntityId>> = HashMap::new();
        for n in &out {
            if n.parent.is_none() && is_ident(&n.name) {
                by_name
                    .entry(n.name.as_str())
                    .and_modify(|e| *e = None)
                    .or_insert(Some(n.nid));
            }
        }
        let resolved: Vec<(usize, Vec<EntityId>)> = pending
            .iter()
            .map(|(i, names)| {
                let ids: Vec<EntityId> = names
                    .iter()
                    .filter_map(|n| by_name.get(n.as_str()).copied().flatten())
                    .filter(|id| *id != out[*i].nid)
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

/// The renames a side made relative to the base, inferred from unit
/// identity across the whole index, with the path each was seen in.
pub fn renames_between(base: &NodeIndex, side: &NodeIndex) -> Vec<Rename> {
    let mut by_path_base: HashMap<&str, Vec<Node>> = HashMap::new();
    for n in &base.nodes {
        by_path_base.entry(n.path.as_str()).or_default().push(n.clone());
    }
    let mut by_path_side: HashMap<&str, Vec<Node>> = HashMap::new();
    for n in &side.nodes {
        by_path_side.entry(n.path.as_str()).or_default().push(n.clone());
    }
    let mut out = Vec::new();
    for (path, after) in &by_path_side {
        let Some(before) = by_path_base.get(path) else { continue };
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
            .find(|m| m.kind == o.kind && m.name == o.name && m.parent.is_some() == o.parent.is_some())
            .or_else(|| {
                merged
                    .iter()
                    .filter(|m| m.path == o.path && !base_ids.contains(&m.nid) && !taken.contains(&m.nid))
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
            from: prev.get(&n.nid).map(|p| p.name.clone()).filter(|_| change == "renamed"),
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
/// demand without a parent and cached against the snapshot ID.
pub fn index_for_snapshot(store: &RedbStore, snap_id: &ObjectId) -> Result<(ObjectId, NodeIndex)> {
    let snap: Snapshot = store.get(snap_id)?;
    if let Some(id) = snap.index {
        return Ok((id, store.get(&id)?));
    }
    let key = format!("idx:{}", snap_id.to_hex());
    if let Some(bytes) = store.meta(&key)? {
        if let Ok(id) = ObjectId::from_slice(&bytes) {
            if let Ok(idx) = store.get::<NodeIndex>(&id) {
                return Ok((id, idx));
            }
        }
    }
    let flat = tree::flatten(store, &snap.root)?;
    store.begin_batch();
    let nodes = build_nodes(store, &flat, None)?;
    let id = put_index(store, snap.root, Vec::new(), nodes)?;
    store.end_batch()?;
    store.set_meta(&key, &id.0)?;
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
    // Files only one side changed: carry the other side's renames into them.
    let mut carried: Vec<(String, ObjectId, Vec<u8>)> = Vec::new();
    for (path, leaf) in &merged {
        if leaf.kind != EntryKind::File || Language::from_path(path).is_none() {
            continue;
        }
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
        let applicable: Vec<&Rename> = renames
            .iter()
            .filter(|r| r.applies_to(path))
            .collect();
        if applicable.is_empty() {
            continue;
        }
        let Some(text) = store.get_bytes(&blob)? else { continue };
        if !applicable.iter().any(|r| contains_ident(&text, &r.from)) {
            continue;
        }
        let mut cur = text;
        for r in applicable {
            if let Some(next) = replace_ident(&cur, &r.from, &r.to) {
                cur = next;
                resolved.push(format!("{path}: {} renamed to {}", r.from, r.to));
            }
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
        let na = nodes_for_path(a_idx, &path);
        let nb = nodes_for_path(b_idx, &path);
        let base_leaf = base.get(&path).filter(|l| l.kind == EntryKind::File);
        let base_text = match base_leaf.and_then(|l| l.r#ref) {
            Some(r) => store.get_bytes(&r)?,
            None => None,
        };
        let base_nodes = base_idx
            .map(|i| nodes_for_path(i, &path))
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
            let snap = rev_of(store, revs, id)?.snapshots.get("").copied();
            let nodes = match snap {
                Some(s) => {
                    let (_, idx) = index_for_snapshot(store, &s)?;
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
    let start_nodes = nodes_of(store, &mut revs, &mut nodes_cache, &mut alias_cache, rev_id, path)?.clone();
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
                let pn = nodes_of(store, &mut revs, &mut nodes_cache, &mut alias_cache, p, path)?;
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
    fn renames_are_inferred_across_the_index_and_scoped_by_level() {
        let base_lib = nodes("lib.rs", "fn a() { 1 }\nimpl S { fn m(&self) { 2 } }\n", &[]);
        let base = index(base_lib.clone());
        let side = index(nodes("lib.rs", "fn b() { 1 }\nimpl S { fn n(&self) { 2 } }\n", &base_lib));
        let mut r = renames_between(&base, &side);
        r.sort_by(|x, y| x.from.cmp(&y.from));
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!((r[0].from.as_str(), r[0].to.as_str(), r[0].path.as_deref()), ("a", "b", None));
        assert_eq!((r[1].from.as_str(), r[1].to.as_str(), r[1].path.as_deref()), ("m", "n", Some("lib.rs")));
        // The functions are renames; the impl that holds one of them changed,
        // since its body covers its children.
        let d = diff_indexes(&base, &side);
        let mut changes: Vec<(&str, &str, Option<&str>)> =
            d.iter().map(|c| (c.kind.as_str(), c.change, c.from.as_deref())).collect();
        changes.sort();
        assert_eq!(
            changes,
            vec![("function", "renamed", Some("a")), ("function", "renamed", Some("m")), ("impl", "changed", None)]
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
        let aliases = reconcile_aliases(&mut merged, Some(&index(base_nodes.clone())), &index(b_side.clone()));
        let survivor = merged[1].nid;
        let loser = if survivor == a_new { b_new } else { a_new };
        assert_eq!(survivor, a_new.min(b_new), "the lexically lower ID survives");
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases.get(&ByteBuf::from(loser.0.to_vec())), Some(&survivor));
        assert_eq!(merged[0].nid, base_nodes[0].nid, "units from the base are untouched");
    }

    #[test]
    fn covering_tests_follow_deps_across_files() {
        let lib = nodes("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn unused() {}\n", &[]);
        let mut all = lib.clone();
        // A test in another file that names `add`; the root-wide pass in
        // build_nodes resolves it, modeled here by setting deps directly.
        let mut t = nodes("tests/t.rs", "#[test]\nfn add_works() { assert_eq!(add(1, 2), 3); }\n", &[]);
        assert_eq!(t[0].kind, "test");
        t[0].deps = Some(vec![lib[0].nid]);
        all.extend(t);
        let idx = index(all);
        let cov = covering_tests(&idx);
        assert_eq!(cov.get(&lib[0].nid).map(|v| v.len()), Some(1));
        assert!(!cov.contains_key(&lib[1].nid));
    }
}
