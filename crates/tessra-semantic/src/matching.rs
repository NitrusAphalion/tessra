//! History-aware node identity: match a file's units against the previous
//! version of the same file so a unit keeps its ID across edits, and give
//! fresh IDs to what is new.
//!
//! Order of attempts, per `spec/02-objects.md`: the same `(parent, kind,
//! name)` with the same ordinal among siblings, then the same body hash under
//! the same parent (a rename), then a fresh ID.
//!
//! The same pass resolves each unit's references to the units of its file,
//! which become `Node.deps`; what does not resolve in the file is returned
//! for the caller to resolve against the rest of the root.

use std::collections::{HashMap, HashSet};

use tessra_core::object::Node;
use tessra_core::{EntityId, ObjectId};

use crate::rename::{is_ident, Rename};
use crate::RawNode;

type Key = (Option<EntityId>, String, String, usize);

/// Assign IDs to the units of `path`, matching against `previous`, which is
/// the nodes recorded for the same path in the parent index.
pub fn assign_ids(path: &str, raw: &[RawNode], previous: &[Node]) -> Vec<Node> {
    assign_ids_with_refs(path, raw, previous).0
}

/// Like `assign_ids`, also returning per node the references that did not
/// resolve to a unit of this file, for resolution against the root.
pub fn assign_ids_with_refs(
    path: &str,
    raw: &[RawNode],
    previous: &[Node],
) -> (Vec<Node>, Vec<Vec<String>>) {
    let mut nodes = assign_only(path, raw, previous);
    let mut by_name: HashMap<&str, Vec<EntityId>> = HashMap::new();
    for n in &nodes {
        if is_ident(&n.name) {
            by_name.entry(n.name.as_str()).or_default().push(n.nid);
        }
    }
    let mut unresolved = Vec::with_capacity(raw.len());
    let resolved: Vec<(Vec<EntityId>, Vec<String>)> = raw
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut deps = Vec::new();
            let mut left = Vec::new();
            for name in &r.refs {
                match by_name.get(name.as_str()) {
                    Some(ids) => deps.extend(ids.iter().filter(|id| **id != nodes[i].nid)),
                    None => left.push(name.clone()),
                }
            }
            deps.sort();
            deps.dedup();
            (deps, left)
        })
        .collect();
    for (n, (deps, left)) in nodes.iter_mut().zip(resolved) {
        n.deps = if deps.is_empty() { None } else { Some(deps) };
        unresolved.push(left);
    }
    (nodes, unresolved)
}

/// Renames implied by identity: a unit in `after` that carries the ID of a
/// unit in `before` under a different name was renamed. Only identifier
/// names count. Each rename carries the path of the file it was seen in.
pub fn inferred_renames(path: &str, before: &[Node], after: &[Node]) -> Vec<(EntityId, Rename)> {
    let by_nid: HashMap<EntityId, &Node> = before.iter().map(|n| (n.nid, n)).collect();
    let mut out = Vec::new();
    for n in after {
        if let Some(b) = by_nid.get(&n.nid) {
            if b.name != n.name && is_ident(&b.name) && is_ident(&n.name) {
                out.push((
                    n.nid,
                    Rename {
                        from: b.name.clone(),
                        to: n.name.clone(),
                        path: Some(path.to_string()),
                    },
                ));
            }
        }
    }
    out
}

fn assign_only(path: &str, raw: &[RawNode], previous: &[Node]) -> Vec<Node> {
    // Previous units by key, with ordinal among siblings sharing (parent, kind, name).
    let mut prev_by_key: HashMap<Key, (EntityId, ObjectId)> = HashMap::new();
    let mut counts: HashMap<(Option<EntityId>, String, String), usize> = HashMap::new();
    for n in previous {
        let base = (n.parent, n.kind.clone(), n.name.clone());
        let ord = *counts
            .entry(base.clone())
            .and_modify(|c| *c += 1)
            .or_insert(0);
        prev_by_key.insert((base.0, base.1, base.2, ord), (n.nid, n.body));
    }
    let mut used: HashSet<EntityId> = HashSet::new();

    let mut assigned: Vec<Option<EntityId>> = vec![None; raw.len()];
    let mut ordinals: HashMap<(Option<usize>, String, String), usize> = HashMap::new();
    // Pass 1: exact key, for named units. A nameless unit, such as a chunk
    // of a file without a grammar, has only its position for a key, so it
    // is matched by body first (pass 2) and then by position between the
    // units matched around it (pass 2b): an inserted paragraph does not
    // shift the identity of every paragraph after it.
    for (i, r) in raw.iter().enumerate() {
        if r.name.is_empty() {
            continue;
        }
        let parent_nid = r.parent.and_then(|p| assigned[p]);
        let ord_key = (r.parent, r.kind.clone(), r.name.clone());
        let ord = *ordinals.entry(ord_key).and_modify(|c| *c += 1).or_insert(0);
        let key: Key = (parent_nid, r.kind.clone(), r.name.clone(), ord);
        if let Some((nid, _)) = prev_by_key.get(&key) {
            if used.insert(*nid) {
                assigned[i] = Some(*nid);
            }
        }
    }
    // Pass 2: same body under the same parent, for renames.
    let mut prev_by_body: HashMap<(Option<EntityId>, String, ObjectId), Vec<EntityId>> =
        HashMap::new();
    for n in previous {
        if !used.contains(&n.nid) {
            prev_by_body
                .entry((n.parent, n.kind.clone(), n.body))
                .or_default()
                .push(n.nid);
        }
    }
    for (i, r) in raw.iter().enumerate() {
        if assigned[i].is_some() {
            continue;
        }
        let parent_nid = r.parent.and_then(|p| assigned[p]);
        if let Some(list) = prev_by_body.get_mut(&(parent_nid, r.kind.clone(), r.body)) {
            while let Some(nid) = list.first().copied() {
                list.remove(0);
                if used.insert(nid) {
                    assigned[i] = Some(nid);
                    break;
                }
            }
        }
    }
    // Pass 2b: nameless units still unmatched take, in order, the unmatched
    // nameless units that sit between the same matched neighbours in the
    // previous version, so an edited paragraph keeps its identity.
    let old_pos: HashMap<EntityId, usize> = previous
        .iter()
        .enumerate()
        .map(|(p, n)| (n.nid, p))
        .collect();
    let anchor = |i: usize| assigned[i].and_then(|nid| old_pos.get(&nid).copied());
    let mut before: Vec<Option<usize>> = Vec::with_capacity(raw.len());
    let mut last = None;
    for i in 0..raw.len() {
        before.push(last);
        last = anchor(i).or(last);
    }
    let mut after: Vec<Option<usize>> = vec![None; raw.len()];
    let mut next = None;
    for (i, slot) in after.iter_mut().enumerate().rev() {
        *slot = next;
        next = anchor(i).or(next);
    }
    let mut cursor = 0;
    for (i, r) in raw.iter().enumerate() {
        if assigned[i].is_some() || !r.name.is_empty() {
            continue;
        }
        let parent_nid = r.parent.and_then(|p| assigned[p]);
        let lo = before[i].map(|p| p + 1).unwrap_or(0).max(cursor);
        let hi = after[i].unwrap_or(previous.len());
        for (p, n) in previous.iter().enumerate().take(hi).skip(lo) {
            if n.name.is_empty() && n.kind == r.kind && n.parent == parent_nid && used.insert(n.nid)
            {
                assigned[i] = Some(n.nid);
                cursor = p + 1;
                break;
            }
        }
    }
    // Pass 3: fresh.
    for slot in assigned.iter_mut() {
        if slot.is_none() {
            *slot = Some(EntityId::random());
        }
    }
    raw.iter()
        .enumerate()
        .map(|(i, r)| Node {
            nid: assigned[i].unwrap(),
            path: path.to_string(),
            kind: r.kind.clone(),
            name: r.name.clone(),
            span: (r.span.0 as u64, r.span.1 as u64),
            body: r.body,
            parent: r.parent.and_then(|p| assigned[p]),
            deps: None,
            setlike: if r.setlike { Some(true) } else { None },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::with_grammar;
    use crate::Language;

    #[test]
    fn ids_survive_edits_renames_and_reorders() {
        let v1 = with_grammar(
            Language::Rust,
            b"fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n",
        )
        .unwrap();
        let n1 = assign_ids("x.rs", &v1, &[]);
        let ids: Vec<EntityId> = n1.iter().map(|n| n.nid).collect();
        // Edit b, reorder c before a, rename a to a2 with the same body.
        let v2 = with_grammar(
            Language::Rust,
            b"fn c() { 3 }\nfn a2() { 1 }\nfn b() { 22 }\n",
        )
        .unwrap();
        let n2 = assign_ids("x.rs", &v2, &n1);
        let by_name = |name: &str, list: &[Node]| list.iter().find(|n| n.name == name).unwrap().nid;
        assert_eq!(by_name("c", &n2), ids[2]);
        assert_eq!(by_name("b", &n2), ids[1]);
        assert_eq!(
            by_name("a2", &n2),
            ids[0],
            "rename with the same body keeps the id"
        );
        // A brand new function gets a fresh id.
        let v3 = with_grammar(
            Language::Rust,
            b"fn c() { 3 }\nfn a2() { 1 }\nfn b() { 22 }\nfn d() { 4 }\n",
        )
        .unwrap();
        let n3 = assign_ids("x.rs", &v3, &n2);
        let d = by_name("d", &n3);
        assert!(!ids.contains(&d));
        assert_eq!(by_name("c", &n3), ids[2]);
    }

    #[test]
    fn chunks_keep_their_ids_around_an_inserted_and_an_edited_paragraph() {
        let v1 = crate::extract::chunks(b"one\n\ntwo\n\nthree\n\nfour\n");
        let n1 = assign_ids("notes.md", &v1, &[]);
        let ids: Vec<EntityId> = n1.iter().map(|n| n.nid).collect();
        // A new first paragraph, the second edited, the rest untouched.
        let v2 = crate::extract::chunks(b"zero\n\none\n\ntwo, edited\n\nthree\n\nfour\n");
        let n2 = assign_ids("notes.md", &v2, &n1);
        assert!(!ids.contains(&n2[0].nid), "the new paragraph is fresh");
        assert_eq!(n2[1].nid, ids[0], "an unchanged paragraph keeps its id");
        assert_eq!(n2[2].nid, ids[1], "the edited paragraph keeps its id");
        assert_eq!(n2[3].nid, ids[2]);
        assert_eq!(n2[4].nid, ids[3]);
        // The same file again matches every chunk.
        let n3 = assign_ids("notes.md", &v2, &n2);
        let same: Vec<EntityId> = n3.iter().map(|n| n.nid).collect();
        let prev: Vec<EntityId> = n2.iter().map(|n| n.nid).collect();
        assert_eq!(same, prev);
    }

    #[test]
    fn children_match_under_their_parent() {
        let v1 = with_grammar(
            Language::Rust,
            b"impl A { fn m() {} }\nimpl B { fn m() {} }\n",
        )
        .unwrap();
        let n1 = assign_ids("x.rs", &v1, &[]);
        let v2 = with_grammar(
            Language::Rust,
            b"impl A { fn m() { 1 } }\nimpl B { fn m() {} }\n",
        )
        .unwrap();
        let n2 = assign_ids("x.rs", &v2, &n1);
        assert_eq!(n2[1].nid, n1[1].nid);
        assert_eq!(n2[3].nid, n1[3].nid);
        assert_ne!(n2[1].nid, n2[3].nid);
    }
}
