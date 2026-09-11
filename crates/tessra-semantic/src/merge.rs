//! Node-level three-way merge of one file, per `spec/03-operations.md`
//! landing step 2: different units merge cleanly, set-like regions union,
//! a unit changed on one side is taken, a unit changed on both sides
//! composes when one side's change is a rename, recurses into its children
//! when it is a container, and conflicts otherwise. Formatting-only changes
//! are invisible because identity is by body hash, so an edit beats a
//! reformat of the same unit and a reformat elsewhere is kept.
//!
//! Renames are the first semantic operation. One side's renames, recorded
//! by `edit` or inferred from a unit keeping its identity under a new name,
//! are applied to whatever the other side contributes, so a new call to the
//! old name in a concurrent change comes out calling the new name. Each such
//! application is reported as a resolved semantic conflict.

use std::collections::{HashMap, HashSet};

use tessra_core::object::Node;
use tessra_core::{EntityId, ObjectId};

use crate::matching::inferred_renames;
use crate::rename::{apply_all, Rename};

/// One version of a file with its nodes, which must be in document order
/// with parents before children.
#[derive(Clone, Copy)]
pub struct FileNodes<'a> {
    pub text: &'a [u8],
    pub nodes: &'a [Node],
}

/// The merged text, the units that could not be merged, and the semantic
/// conflicts that were resolved by applying a rename.
#[derive(Debug, Default)]
pub struct Merged {
    pub text: Vec<u8>,
    /// "kind name" per unmerged unit, with the reason in parentheses when it
    /// is not a plain both-changed edit.
    pub conflicts: Vec<String>,
    /// "kind name: old renamed to new" per unit the other side's rename was applied to.
    pub resolved: Vec<String>,
}

type Key = (String, String, usize);

#[derive(Clone, Debug)]
struct Unit {
    /// `(kind, name as known to the base, ordinal)`: a renamed unit keys by
    /// its old name so it lines up with the base and the other side.
    key: Key,
    kind: String,
    name: String,
    span: (usize, usize),
    body: ObjectId,
    /// Text between the previous sibling's end (or the container's start) and this unit.
    gap_start: usize,
    children: Vec<Unit>,
}

fn build_units(nodes: &[Node], match_names: &HashMap<EntityId, String>) -> Vec<Unit> {
    let mut by_parent: HashMap<Option<EntityId>, Vec<&Node>> = HashMap::new();
    for n in nodes {
        by_parent.entry(n.parent).or_default().push(n);
    }
    fn make(
        list: &[&Node],
        by_parent: &HashMap<Option<EntityId>, Vec<&Node>>,
        match_names: &HashMap<EntityId, String>,
        region_start: usize,
    ) -> Vec<Unit> {
        let mut counts: HashMap<(String, String), usize> = HashMap::new();
        let mut out = Vec::new();
        let mut prev_end = region_start;
        for n in list {
            let key_name = match_names.get(&n.nid).unwrap_or(&n.name).clone();
            let ord = *counts
                .entry((n.kind.clone(), key_name.clone()))
                .and_modify(|c| *c += 1)
                .or_insert(0);
            let span = (n.span.0 as usize, n.span.1 as usize);
            let kids = by_parent
                .get(&Some(n.nid))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let first_kid_start = kids.first().map(|k| k.span.0 as usize).unwrap_or(span.1);
            out.push(Unit {
                key: (n.kind.clone(), key_name, ord),
                kind: n.kind.clone(),
                name: n.name.clone(),
                span,
                body: n.body,
                gap_start: prev_end,
                children: make(kids, by_parent, match_names, first_kid_start),
            });
            prev_end = span.1;
        }
        out
    }
    let top = by_parent.get(&None).map(|v| v.as_slice()).unwrap_or(&[]);
    make(top, &by_parent, match_names, 0)
}

/// Merge one file. `base` is None when both sides added the file. `ops_a`
/// and `ops_b` are the renames each side recorded; renames inferred from
/// unit identity are added to them.
pub fn merge_file(
    base: Option<FileNodes<'_>>,
    a: FileNodes<'_>,
    b: FileNodes<'_>,
    ops_a: &[Rename],
    ops_b: &[Rename],
) -> Merged {
    let base_nodes: &[Node] = base.map(|f| f.nodes).unwrap_or(&[]);
    let inferred_a = inferred_renames("", base_nodes, a.nodes);
    let inferred_b = inferred_renames("", base_nodes, b.nodes);
    let match_names = |inf: &[(EntityId, Rename)]| -> HashMap<EntityId, String> {
        inf.iter().map(|(nid, r)| (*nid, r.from.clone())).collect()
    };
    let renames = |inf: Vec<(EntityId, Rename)>, ops: &[Rename]| -> Vec<Rename> {
        let mut out: Vec<Rename> = inf.into_iter().map(|(_, r)| r).collect();
        for r in ops {
            if !out.iter().any(|x| x.from == r.from && x.to == r.to) {
                out.push(r.clone());
            }
        }
        out
    };
    let mut names_a = match_names(&inferred_a);
    let mut names_b = match_names(&inferred_b);
    // A recorded rename lines up its unit with the base even when the body
    // changed too and identity could not follow it.
    for (ops, side_nodes, names) in [
        (ops_a, a.nodes, &mut names_a),
        (ops_b, b.nodes, &mut names_b),
    ] {
        for r in ops {
            let Some(old) = base_nodes.iter().find(|n| n.name == r.from) else {
                continue;
            };
            if side_nodes
                .iter()
                .any(|n| n.name == r.from && n.kind == old.kind)
            {
                continue;
            }
            if let Some(new) = side_nodes.iter().find(|n| {
                n.name == r.to && n.kind == old.kind && n.parent.is_none() == old.parent.is_none()
            }) {
                names.entry(new.nid).or_insert_with(|| r.from.clone());
            }
        }
    }
    let bu = base.map(|f| build_units(f.nodes, &HashMap::new()));
    let au = build_units(a.nodes, &names_a);
    let bbu = build_units(b.nodes, &names_b);
    let ctx = Ctx {
        base: base.map(|f| f.text),
        a: a.text,
        b: b.text,
        ren_a: renames(inferred_a, ops_a),
        ren_b: renames(inferred_b, ops_b),
    };
    let mut m = Merged::default();
    let mut out = Vec::new();
    merge_level(&ctx, bu.as_deref(), &au, &bbu, &mut out, &mut m);
    // Trailing text after the last unit: prefer the side that changed it.
    let tail = |text: &[u8], units: &[Unit]| -> Vec<u8> {
        let end = units.last().map(|u| u.span.1).unwrap_or(0);
        text[end.min(text.len())..].to_vec()
    };
    let a_tail = tail(a.text, &au);
    let b_tail = tail(b.text, &bbu);
    let base_tail = base.map(|f| tail(f.text, bu.as_deref().unwrap_or(&[])));
    let chosen = match &base_tail {
        Some(bt) if a_tail == *bt => b_tail,
        _ => a_tail,
    };
    out.extend(chosen);
    m.text = out;
    m
}

struct Ctx<'a> {
    base: Option<&'a [u8]>,
    a: &'a [u8],
    b: &'a [u8],
    ren_a: Vec<Rename>,
    ren_b: Vec<Rename>,
}

impl Ctx<'_> {
    fn text(&self, side: Side) -> &[u8] {
        match side {
            Side::A => self.a,
            Side::B => self.b,
        }
    }

    fn renames(&self, side: Side) -> &[Rename] {
        match side {
            Side::A => &self.ren_a,
            Side::B => &self.ren_b,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    A,
    B,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }
}

/// Document order for the merged level: the base order, with each side's
/// additions placed after the unit that preceded them on their side.
/// Additions both sides made after the same unit keep landing order: the
/// first side's run stays ahead of the second's.
fn ordered_keys(base: Option<&[Unit]>, a: &[Unit], b: &[Unit]) -> Vec<Key> {
    let mut result: Vec<Key> = base
        .map(|u| u.iter().map(|x| x.key.clone()).collect())
        .unwrap_or_default();
    let in_base: HashSet<Key> = result.iter().cloned().collect();
    let mut present = in_base.clone();
    for side in [a, b] {
        let mut prev_in_result: Option<Key> = None;
        for u in side {
            if present.contains(&u.key) {
                prev_in_result = Some(u.key.clone());
                continue;
            }
            let mut pos = match &prev_in_result {
                Some(p) => result
                    .iter()
                    .position(|k| k == p)
                    .map(|i| i + 1)
                    .unwrap_or(result.len()),
                None => 0,
            };
            while pos < result.len() && !in_base.contains(&result[pos]) {
                pos += 1;
            }
            result.insert(pos, u.key.clone());
            present.insert(u.key.clone());
            prev_in_result = Some(u.key.clone());
        }
    }
    result
}

fn find<'u>(units: &'u [Unit], key: &Key) -> Option<&'u Unit> {
    units.iter().find(|u| &u.key == key)
}

fn slice(text: &[u8], from: usize, to: usize) -> &[u8] {
    let to = to.min(text.len());
    let from = from.min(to);
    &text[from..to]
}

/// Emit a unit's gap and text from one side, with the other side's renames
/// applied to it. Every rename that changed something is reported.
fn emit_leaf(ctx: &Ctx<'_>, side: Side, u: &Unit, out: &mut Vec<u8>, m: &mut Merged) {
    let text = ctx.text(side);
    push_gap(out, slice(text, u.gap_start, u.span.0));
    let body = slice(text, u.span.0, u.span.1);
    let (fixed, applied) = apply_all(body, ctx.renames(side.other()));
    for r in applied {
        m.resolved.push(format!(
            "{} {}: {} renamed to {}",
            u.kind, u.name, r.from, r.to
        ));
    }
    out.extend_from_slice(&fixed);
}

/// A unit's gap from its own side, or a line break when the unit was first
/// on its side but is not first in the merged output.
fn push_gap(out: &mut Vec<u8>, gap: &[u8]) {
    if gap.is_empty() && !out.is_empty() && out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out.extend_from_slice(gap);
}

fn header<'a>(text: &'a [u8], u: &Unit) -> &'a [u8] {
    let end = u.children.first().map(|c| c.span.0).unwrap_or(u.span.1);
    slice(text, u.span.0, end)
}

fn footer<'a>(text: &'a [u8], u: &Unit) -> &'a [u8] {
    let start = u.children.last().map(|c| c.span.1).unwrap_or(u.span.1);
    slice(text, start, u.span.1)
}

/// Whether a side's change to a unit is exactly its renames applied to the
/// base text, so the other side's version can be taken with the renames.
fn change_is_renames(ctx: &Ctx<'_>, side: Side, base_unit: &Unit, side_unit: &Unit) -> bool {
    let Some(base_text) = ctx.base else {
        return false;
    };
    let renames = ctx.renames(side);
    if renames.is_empty() {
        return false;
    }
    let base_slice = slice(base_text, base_unit.span.0, base_unit.span.1);
    let (renamed, applied) = apply_all(base_slice, renames);
    !applied.is_empty() && renamed == slice(ctx.text(side), side_unit.span.0, side_unit.span.1)
}

fn merge_level(
    ctx: &Ctx<'_>,
    base: Option<&[Unit]>,
    a: &[Unit],
    b: &[Unit],
    out: &mut Vec<u8>,
    m: &mut Merged,
) {
    // A unit added on one side under a name the other side renamed something
    // to would collide with the renamed unit.
    for (side, units) in [(Side::A, a), (Side::B, b)] {
        for u in units {
            let added = base.map(|bu| find(bu, &u.key).is_none()).unwrap_or(true);
            if added && u.key.1 == u.name {
                if let Some(r) = ctx.renames(side.other()).iter().find(|r| r.to == u.name) {
                    m.conflicts.push(format!(
                        "{} {} (added on one side, {} renamed to it on the other)",
                        u.kind, u.name, r.from
                    ));
                }
            }
        }
    }
    for key in ordered_keys(base, a, b) {
        let bu = base.and_then(|u| find(u, &key));
        let au = find(a, &key);
        let bbu = find(b, &key);
        match (bu, au, bbu) {
            (_, None, None) => {}
            (None, Some(x), None) => emit_leaf(ctx, Side::A, x, out, m),
            (None, None, Some(y)) => emit_leaf(ctx, Side::B, y, out, m),
            (None, Some(x), Some(y)) => {
                if x.body == y.body {
                    emit_leaf(ctx, Side::A, x, out, m);
                } else if !x.children.is_empty() && !y.children.is_empty() {
                    merge_container(ctx, None, x, y, out, m);
                } else {
                    m.conflicts.push(format!("{} {}", x.kind, x.name));
                    emit_leaf(ctx, Side::A, x, out, m);
                }
            }
            (Some(b0), None, Some(y)) => {
                if y.body != b0.body || y.name != b0.name {
                    m.conflicts.push(format!(
                        "{} {} (deleted on one side, changed on the other)",
                        y.kind, y.name
                    ));
                    emit_leaf(ctx, Side::B, y, out, m);
                }
            }
            (Some(b0), Some(x), None) => {
                if x.body != b0.body || x.name != b0.name {
                    m.conflicts.push(format!(
                        "{} {} (deleted on one side, changed on the other)",
                        x.kind, x.name
                    ));
                    emit_leaf(ctx, Side::A, x, out, m);
                }
            }
            (Some(b0), Some(x), Some(y)) => {
                let a_changed = x.body != b0.body || x.name != b0.name;
                let b_changed = y.body != b0.body || y.name != b0.name;
                match (a_changed, b_changed) {
                    (false, false) => {
                        // Formatting only, if anything. Prefer the side whose text differs from base.
                        let base_text = ctx
                            .base
                            .map(|t| slice(t, b0.span.0, b0.span.1))
                            .unwrap_or(&[]);
                        if slice(ctx.a, x.span.0, x.span.1) != base_text {
                            emit_leaf(ctx, Side::A, x, out, m);
                        } else {
                            emit_leaf(ctx, Side::B, y, out, m);
                        }
                    }
                    (true, false) => emit_leaf(ctx, Side::A, x, out, m),
                    (false, true) => emit_leaf(ctx, Side::B, y, out, m),
                    (true, true) => {
                        if x.body == y.body && x.name == y.name {
                            emit_leaf(ctx, Side::A, x, out, m);
                        } else if x.name != b0.name && y.name != b0.name && x.name != y.name {
                            m.conflicts.push(format!(
                                "{} {} (renamed to {} on one side and {} on the other)",
                                b0.kind, b0.name, x.name, y.name
                            ));
                            emit_leaf(ctx, Side::A, x, out, m);
                        } else if change_is_renames(ctx, Side::A, b0, x) {
                            emit_leaf(ctx, Side::B, y, out, m);
                        } else if change_is_renames(ctx, Side::B, b0, y) {
                            emit_leaf(ctx, Side::A, x, out, m);
                        } else if !x.children.is_empty()
                            && !y.children.is_empty()
                            && !b0.children.is_empty()
                        {
                            merge_container(ctx, Some(b0), x, y, out, m);
                        } else {
                            let reason = if x.name != b0.name || y.name != b0.name {
                                " (renamed on one side, changed on the other)"
                            } else {
                                ""
                            };
                            m.conflicts
                                .push(format!("{} {}{}", b0.kind, b0.name, reason));
                            emit_leaf(ctx, Side::A, x, out, m);
                        }
                    }
                }
            }
        }
    }
}

/// Both sides changed a container differently: keep the header and footer
/// from whichever side changed them, and merge the children.
fn merge_container(
    ctx: &Ctx<'_>,
    base: Option<&Unit>,
    x: &Unit,
    y: &Unit,
    out: &mut Vec<u8>,
    m: &mut Merged,
) {
    let base_header = base.and_then(|b| ctx.base.map(|t| header(t, b)));
    let a_header = header(ctx.a, x);
    let b_header = header(ctx.b, y);
    let (gap_side, head) = match base_header {
        Some(bh) if a_header == bh => (Side::B, b_header),
        _ => (Side::A, a_header),
    };
    let gap_text = match gap_side {
        Side::A => slice(ctx.a, x.gap_start, x.span.0),
        Side::B => slice(ctx.b, y.gap_start, y.span.0),
    };
    push_gap(out, gap_text);
    let (head, applied) = apply_all(head, ctx.renames(gap_side.other()));
    for r in applied {
        m.resolved.push(format!(
            "{} {}: {} renamed to {}",
            x.kind, x.name, r.from, r.to
        ));
    }
    out.extend_from_slice(&head);
    merge_level(
        ctx,
        base.map(|b| b.children.as_slice()),
        &x.children,
        &y.children,
        out,
        m,
    );
    let base_footer = base.and_then(|b| ctx.base.map(|t| footer(t, b)));
    let foot = match base_footer {
        Some(bf) if footer(ctx.a, x) == bf => footer(ctx.b, y),
        _ => footer(ctx.a, x),
    };
    out.extend_from_slice(foot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::with_grammar;
    use crate::matching::assign_ids;
    use crate::Language;

    fn nodes(src: &str, prev: &[Node]) -> Vec<Node> {
        let raw = with_grammar(Language::Rust, src.as_bytes()).unwrap();
        assign_ids("f.rs", &raw, prev)
    }

    fn merge_ops(base: &str, a: &str, b: &str, ops_a: &[Rename], ops_b: &[Rename]) -> Merged {
        let bn = nodes(base, &[]);
        let an = nodes(a, &bn);
        let bbn = nodes(b, &bn);
        merge_file(
            Some(FileNodes {
                text: base.as_bytes(),
                nodes: &bn,
            }),
            FileNodes {
                text: a.as_bytes(),
                nodes: &an,
            },
            FileNodes {
                text: b.as_bytes(),
                nodes: &bbn,
            },
            ops_a,
            ops_b,
        )
    }

    fn merge(base: &str, a: &str, b: &str) -> Merged {
        merge_ops(base, a, b, &[], &[])
    }

    const BASE: &str =
        "use std::fmt;\n\nfn a() {\n    1\n}\n\nfn b() {\n    2\n}\n\nfn c() {\n    3\n}\n";

    #[test]
    fn different_functions_merge_cleanly() {
        let a = BASE.replace("    1\n", "    10\n");
        let b = BASE.replace("    3\n", "    30\n");
        let m = merge(BASE, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert_eq!(
            t,
            "use std::fmt;\n\nfn a() {\n    10\n}\n\nfn b() {\n    2\n}\n\nfn c() {\n    30\n}\n"
        );
    }

    #[test]
    fn imports_union_and_new_functions_from_both_sides() {
        let a = format!("use std::io;\n{BASE}fn d() {{ 4 }}\n");
        let b = format!("use std::env;\n{BASE}fn e() {{ 5 }}\n");
        let m = merge(BASE, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("use std::io;"));
        assert!(t.contains("use std::env;"));
        assert!(t.contains("use std::fmt;"));
        assert!(t.contains("fn d() { 4 }"));
        assert!(t.contains("fn e() { 5 }"));
        assert!(t.contains("fn a() {\n    1\n}"));
    }

    #[test]
    fn reformat_loses_to_edit_but_survives_elsewhere() {
        let reformatted = "use std::fmt;\n\nfn a() { 1 }\n\nfn b() { 2 }\n\nfn c() { 3 }\n";
        let edited = BASE.replace("    2\n", "    20\n");
        let m = merge(BASE, reformatted, &edited);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(
            t.contains("fn a() { 1 }"),
            "reformat kept where nothing else changed"
        );
        assert!(
            t.contains("fn b() {\n    20\n}"),
            "the edit wins over the reformat of the same unit"
        );
        assert!(t.contains("fn c() { 3 }"));
    }

    #[test]
    fn same_function_changed_differently_conflicts() {
        let a = BASE.replace("    2\n", "    20\n");
        let b = BASE.replace("    2\n", "    22\n");
        let m = merge(BASE, &a, &b);
        assert_eq!(m.conflicts, vec!["function b".to_string()]);
    }

    #[test]
    fn methods_added_to_the_same_impl_from_both_sides() {
        let base = "struct S;\n\nimpl S {\n    fn one(&self) {}\n}\n";
        let a = "struct S;\n\nimpl S {\n    fn one(&self) {}\n    fn two(&self) {}\n}\n";
        let b = "struct S;\n\nimpl S {\n    fn zero(&self) {}\n    fn one(&self) {}\n}\n";
        let m = merge(base, a, b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("fn zero(&self) {}"));
        assert!(t.contains("fn one(&self) {}"));
        assert!(t.contains("fn two(&self) {}"));
        assert!(t.trim_end().ends_with('}'));
    }

    #[test]
    fn enum_variants_from_both_sides() {
        let base = "enum E {\n    A,\n}\n";
        let a = "enum E {\n    A,\n    B,\n}\n";
        let b = "enum E {\n    A,\n    C,\n}\n";
        let m = merge(base, a, b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(
            t.contains("A,") && t.contains("B,") && t.contains("C,"),
            "{t}"
        );
        let reparsed = with_grammar(Language::Rust, t.as_bytes()).unwrap();
        assert_eq!(reparsed.iter().filter(|n| n.kind == "variant").count(), 3);
    }

    #[test]
    fn deleted_versus_changed_conflicts_and_deleted_versus_untouched_deletes() {
        let a = BASE.replace("fn c() {\n    3\n}\n", "");
        let b = BASE.replace("    3\n", "    33\n");
        let m = merge(BASE, &a, &b);
        assert_eq!(m.conflicts.len(), 1);
        let m2 = merge(BASE, &a, BASE);
        assert!(m2.conflicts.is_empty());
        assert!(!String::from_utf8(m2.text).unwrap().contains("fn c"));
    }

    #[test]
    fn rename_plus_new_call_to_the_old_name_is_resolved() {
        // A renames b to bee (definition and callers); B adds a caller of b.
        let a = BASE.replace("fn b()", "fn bee()");
        let b = format!("{BASE}\nfn d() -> i32 {{ b() + 1 }}\n");
        let m = merge(BASE, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("fn bee() {"), "{t}");
        assert!(!t.contains("fn b()"), "{t}");
        assert!(t.contains("fn d() -> i32 { bee() + 1 }"), "{t}");
        assert_eq!(m.resolved, vec!["function d: b renamed to bee".to_string()]);
        // The other landing order gives the same file.
        let m2 = merge(BASE, &b, &a);
        assert!(m2.conflicts.is_empty(), "{:?}", m2.conflicts);
        assert_eq!(String::from_utf8(m2.text).unwrap(), t);
    }

    #[test]
    fn rename_composes_with_an_edit_of_the_same_unit() {
        let a = BASE.replace("fn b()", "fn bee()");
        let b = BASE.replace("    2\n", "    20\n");
        let m = merge(BASE, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("fn bee() {\n    20\n}"), "{t}");
        assert!(!t.contains("fn b()"), "{t}");
    }

    #[test]
    fn rename_composes_with_an_edit_of_a_caller() {
        let base = "fn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    b()\n}\n";
        let a = base.replace("b()", "bee()");
        let b = base.replace("    b()\n", "    b() + 1\n");
        let m = merge(base, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert_eq!(
            t,
            "fn bee() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    bee() + 1\n}\n"
        );
        assert_eq!(m.resolved, vec!["function c: b renamed to bee".to_string()]);
        // A rename plus a real body change on the same unit still conflicts.
        let a2 = base.replace("fn b() -> i32 {\n    2\n}", "fn bee() -> i32 {\n    22\n}");
        let b2 = base.replace("    2\n", "    20\n");
        let m2 = merge_ops(base, &a2, &b2, &[Rename::new("b", "bee")], &[]);
        assert_eq!(m2.conflicts.len(), 1, "{:?}", m2.conflicts);
        assert!(
            m2.conflicts[0].contains("renamed on one side"),
            "{:?}",
            m2.conflicts
        );
    }

    #[test]
    fn renamed_differently_on_both_sides_conflicts() {
        let a = BASE.replace("fn b()", "fn bee()");
        let b = BASE.replace("fn b()", "fn bea()");
        let m = merge(BASE, &a, &b);
        assert_eq!(m.conflicts.len(), 1, "{:?}", m.conflicts);
        assert!(
            m.conflicts[0].contains("renamed to bee on one side and bea"),
            "{:?}",
            m.conflicts
        );
    }

    #[test]
    fn recorded_rename_applies_when_identity_cannot_infer_it() {
        // A renamed b to bee and changed its body, so the matcher sees a new
        // unit; the recorded op still carries the rename to B's new caller.
        let a = BASE.replace("fn b() {\n    2\n}", "fn bee() {\n    2 + 2\n}");
        let b = format!("{BASE}\nfn d() -> i32 {{ b() }}\n");
        let m = merge_ops(BASE, &a, &b, &[Rename::new("b", "bee")], &[]);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("fn d() -> i32 { bee() }"), "{t}");
        assert!(t.contains("fn bee() {\n    2 + 2\n}"), "{t}");
        assert!(!t.contains("fn b()"), "{t}");
    }

    #[test]
    fn rename_target_added_on_the_other_side_conflicts() {
        let a = BASE.replace("fn b()", "fn bee()");
        let b = format!("{BASE}\nfn bee() {{ 9 }}\n");
        let m = merge(BASE, &a, &b);
        assert_eq!(m.conflicts.len(), 1, "{:?}", m.conflicts);
        assert!(
            m.conflicts[0].contains("added on one side"),
            "{:?}",
            m.conflicts
        );
    }

    #[test]
    fn concurrent_additions_after_the_same_unit_keep_landing_order() {
        let a = BASE.replace("fn b()", "fn x() { 0 }\n\nfn b()");
        let b = BASE.replace("fn b()", "fn y() { 0 }\n\nfn b()");
        let m = merge(BASE, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        let pos = |s: &str| t.find(s).unwrap();
        assert!(pos("fn a()") < pos("fn x()"), "{t}");
        assert!(pos("fn x()") < pos("fn y()"), "{t}");
        assert!(pos("fn y()") < pos("fn b()"), "{t}");
        let reparsed = with_grammar(Language::Rust, t.as_bytes()).unwrap();
        assert_eq!(reparsed.iter().filter(|n| n.kind == "function").count(), 5);
    }
}
