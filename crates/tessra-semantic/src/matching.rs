//! History-aware node identity: match a file's units against the previous
//! version of the same file so a unit keeps its ID across edits, and give
//! fresh IDs to what is new.
//!
//! Order of attempts, per `spec/02-objects.md`: among the siblings sharing
//! `(parent, kind, name)`, the same body hash first and then the same
//! position between those, so an overload inserted above does not shift
//! the identity of every one below it; then the same body hash under the
//! same parent (a rename); then, for a unit that vanished and one that
//! appeared under the same parent, enough structural similarity (a rename
//! that came with an edit); then a fresh ID.
//!
//! The same pass resolves each unit's references to the units of its file,
//! which become `Node.deps`; what does not resolve in the file is returned
//! for the caller to resolve against the rest of the root.

use std::collections::{HashMap, HashSet};

use tessra_core::object::Node;
use tessra_core::{EntityId, ObjectId};

use crate::rename::{is_ident, Rename};
use crate::RawNode;

/// The Jaccard overlap of token bigrams at or above which a unit that
/// appeared is the unit that vanished under the same parent, renamed and
/// edited in one step.
pub const SIMILARITY: f64 = 0.6;

/// Assign IDs to the units of `path`, matching against `previous`, which is
/// the nodes recorded for the same path in the parent index.
pub fn assign_ids(path: &str, raw: &[RawNode], previous: &[Node]) -> Vec<Node> {
    assign_ids_with_refs(path, raw, previous, None).0
}

/// Like `assign_ids`, also returning per node the references that did not
/// resolve to a unit of this file, for resolution against the root.
/// `previous_source` is the text `previous` was extracted from; with it a
/// unit that vanished can be followed into a similar one that appeared.
pub fn assign_ids_with_refs(
    path: &str,
    raw: &[RawNode],
    previous: &[Node],
    previous_source: Option<&[u8]>,
) -> (Vec<Node>, Vec<Vec<String>>) {
    let mut nodes = assign_only(path, raw, previous, previous_source);
    let mut by_name: HashMap<&str, Vec<EntityId>> = HashMap::new();
    for n in &nodes {
        // A test is never what code refers to, and a test titled after
        // what it tests must not capture that name.
        if is_ident(&n.name) && n.kind != "test" && n.kind != "test.skipped" {
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

/// The Jaccard overlap of two sorted, deduplicated shingle sets.
fn jaccard(a: &[u64], b: &[u64]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut both) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    both as f64 / (a.len() + b.len() - both) as f64
}

/// Within one group of siblings sharing `(parent, kind, name)`, which
/// previous unit each current one is: the same body first, then the
/// position between the units matched around it. Returns per current
/// member the index into `prevs` it takes.
fn match_group(
    raw: &[RawNode],
    members: &[usize],
    previous: &[Node],
    prevs: &[usize],
) -> Vec<Option<usize>> {
    let mut taken = vec![false; prevs.len()];
    let mut slot: Vec<Option<usize>> = vec![None; members.len()];
    for (m, &j) in members.iter().enumerate() {
        if let Some(q) =
            (0..prevs.len()).find(|&q| !taken[q] && previous[prevs[q]].body == raw[j].body)
        {
            taken[q] = true;
            slot[m] = Some(q);
        }
    }
    for m in 0..members.len() {
        if slot[m].is_some() {
            continue;
        }
        let lo = (0..m)
            .rev()
            .find_map(|k| slot[k])
            .map(|q| q + 1)
            .unwrap_or(0);
        let hi = (m + 1..members.len())
            .find_map(|k| slot[k])
            .unwrap_or(prevs.len());
        if let Some(q) = (lo..hi).find(|&q| !taken[q]) {
            taken[q] = true;
            slot[m] = Some(q);
        }
    }
    slot
}

fn assign_only(
    path: &str,
    raw: &[RawNode],
    previous: &[Node],
    previous_source: Option<&[u8]>,
) -> Vec<Node> {
    // Previous units grouped by (parent, kind, name), in document order.
    let mut prev_groups: HashMap<(Option<EntityId>, String, String), Vec<usize>> = HashMap::new();
    for (p, n) in previous.iter().enumerate() {
        if !n.name.is_empty() {
            prev_groups
                .entry((n.parent, n.kind.clone(), n.name.clone()))
                .or_default()
                .push(p);
        }
    }
    let mut used: HashSet<EntityId> = HashSet::new();

    let mut assigned: Vec<Option<EntityId>> = vec![None; raw.len()];
    // Pass 1: the same (parent, kind, name), for named units. Within a
    // group of siblings sharing the key, the same body matches first, then
    // what is left matches by position between those: an overload added
    // above its siblings does not take the identity of the one below it.
    // A nameless unit, such as a chunk of a file without a grammar, has
    // only its position for a key, so it is matched by body first (pass 2)
    // and then by position between the units matched around it (pass 2b):
    // an inserted paragraph does not shift the identity of every paragraph
    // after it.
    let mut grouped: HashSet<(Option<usize>, String, String)> = HashSet::new();
    for (i, r) in raw.iter().enumerate() {
        if r.name.is_empty() {
            continue;
        }
        if !grouped.insert((r.parent, r.kind.clone(), r.name.clone())) {
            continue;
        }
        let members: Vec<usize> = (i..raw.len())
            .filter(|&j| {
                raw[j].parent == r.parent && raw[j].kind == r.kind && raw[j].name == r.name
            })
            .collect();
        let parent_nid = r.parent.and_then(|p| assigned[p]);
        let Some(prevs) = prev_groups.get(&(parent_nid, r.kind.clone(), r.name.clone())) else {
            continue;
        };
        let slots = match_group(raw, &members, previous, prevs);
        for (m, &j) in members.iter().enumerate() {
            if let Some(q) = slots[m] {
                let nid = previous[prevs[q]].nid;
                if used.insert(nid) {
                    assigned[j] = Some(nid);
                }
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
    // Pass 2c: a named unit still unmatched takes the unmatched named unit
    // of the same kind under the same parent whose body it most resembles,
    // when the resemblance is enough: a rename that came with an edit in
    // the same step, which neither the name nor the body hash can follow.
    // The previous version's tokens are recovered by extracting it again.
    let wants_similarity = raw
        .iter()
        .enumerate()
        .any(|(i, r)| assigned[i].is_none() && !r.name.is_empty() && !r.shingles.is_empty())
        && previous
            .iter()
            .any(|n| !used.contains(&n.nid) && !n.name.is_empty());
    let prev_shingles: HashMap<EntityId, Vec<u64>> = match previous_source {
        Some(src) if wants_similarity => {
            let by_span: HashMap<(u64, u64), Vec<u64>> = crate::extract(path, src)
                .unwrap_or_default()
                .into_iter()
                .map(|r| ((r.span.0 as u64, r.span.1 as u64), r.shingles))
                .collect();
            previous
                .iter()
                .filter_map(|n| by_span.get(&n.span).map(|s| (n.nid, s.clone())))
                .collect()
        }
        _ => HashMap::new(),
    };
    if !prev_shingles.is_empty() {
        for (i, r) in raw.iter().enumerate() {
            if assigned[i].is_some() || r.name.is_empty() || r.shingles.is_empty() {
                continue;
            }
            let parent_nid = r.parent.and_then(|p| assigned[p]);
            let best = previous
                .iter()
                .filter(|n| {
                    !used.contains(&n.nid)
                        && !n.name.is_empty()
                        && n.kind == r.kind
                        && n.parent == parent_nid
                })
                .filter_map(|n| {
                    prev_shingles
                        .get(&n.nid)
                        .map(|s| (n.nid, jaccard(s, &r.shingles)))
                })
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((nid, score)) = best {
                if score >= SIMILARITY && used.insert(nid) {
                    assigned[i] = Some(nid);
                }
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
    fn same_named_siblings_match_by_body_before_position() {
        // Three overloads of m; a fourth is inserted above them while the
        // implementation below is edited. Each keeps its identity.
        let v1 = with_grammar(
            Language::TypeScript,
            b"class K {\n  m(a: string): void;\n  m(a: number): void;\n  m(a: any) { return a; }\n}\n",
        )
        .unwrap();
        let n1 = assign_ids("k.ts", &v1, &[]);
        let v2 = with_grammar(
            Language::TypeScript,
            b"class K {\n  m(a: boolean): void;\n  m(a: string): void;\n  m(a: number): void;\n  m(a: any) { return a + 1; }\n}\n",
        )
        .unwrap();
        let n2 = assign_ids("k.ts", &v2, &n1);
        let ids = |list: &[Node]| -> Vec<EntityId> {
            list.iter()
                .filter(|n| n.kind == "method")
                .map(|n| n.nid)
                .collect()
        };
        let (old, new) = (ids(&n1), ids(&n2));
        assert_eq!(new.len(), 4);
        assert!(!old.contains(&new[0]), "the inserted overload is fresh");
        assert_eq!(new[1], old[0], "an unchanged overload keeps its id by body");
        assert_eq!(new[2], old[1]);
        assert_eq!(
            new[3], old[2],
            "the edited implementation keeps its id by position after the matched ones"
        );
        // The same in Rust, where same-named units under one parent arise
        // from cfg-gated definitions.
        let r1 = with_grammar(
            Language::Rust,
            b"#[cfg(unix)]\nfn f() { 1 }\n#[cfg(windows)]\nfn f() { 2 }\n",
        )
        .unwrap();
        let m1 = assign_ids("x.rs", &r1, &[]);
        let r2 = with_grammar(
            Language::Rust,
            b"#[cfg(wasm)]\nfn f() { 0 }\n#[cfg(unix)]\nfn f() { 1 }\n#[cfg(windows)]\nfn f() { 22 }\n",
        )
        .unwrap();
        let m2 = assign_ids("x.rs", &r2, &m1);
        assert_eq!(m2[1].nid, m1[0].nid);
        assert_eq!(m2[2].nid, m1[1].nid);
        assert_ne!(m2[0].nid, m1[0].nid);
        assert_ne!(m2[0].nid, m1[1].nid);
    }

    #[test]
    fn a_rename_with_an_edit_keeps_its_id_by_similarity() {
        let base = "fn a() { 1 }\n\nfn parse(input: &str) -> Vec<u32> {\n    let mut out = Vec::new();\n    for part in input.split(',') {\n        if let Ok(n) = part.trim().parse::<u32>() {\n            out.push(n);\n        }\n    }\n    out\n}\n";
        let v1 = with_grammar(Language::Rust, base.as_bytes()).unwrap();
        let n1 = assign_ids("x.rs", &v1, &[]);
        // Renamed, one line added: neither the name nor the body hash follows it.
        let edited = base
            .replace("fn parse(", "fn parse_numbers(")
            .replace("    out\n}", "    out.sort();\n    out\n}");
        let v2 = with_grammar(Language::Rust, edited.as_bytes()).unwrap();
        let n2 = assign_ids_with_refs("x.rs", &v2, &n1, Some(base.as_bytes())).0;
        assert_eq!(n2[1].name, "parse_numbers");
        assert_eq!(n2[1].nid, n1[1].nid, "identity follows the similar body");
        assert_eq!(n2[0].nid, n1[0].nid);
        let renames = inferred_renames("x.rs", &n1, &n2);
        assert_eq!(renames.len(), 1);
        assert_eq!(
            (renames[0].1.from.as_str(), renames[0].1.to.as_str()),
            ("parse", "parse_numbers")
        );
        // Without the previous text there is nothing to compare: fresh.
        let n3 = assign_ids("x.rs", &v2, &n1);
        assert_ne!(n3[1].nid, n1[1].nid);
        // A unit that merely replaced another of the same kind is fresh:
        // small bodies with different contents do not resemble each other.
        let other = base.replace("fn a() { 1 }", "fn z(x: i32) -> i32 { x * x + x }");
        let v4 = with_grammar(Language::Rust, other.as_bytes()).unwrap();
        let n4 = assign_ids_with_refs("x.rs", &v4, &n1, Some(base.as_bytes())).0;
        assert_ne!(n4[0].nid, n1[0].nid);
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
