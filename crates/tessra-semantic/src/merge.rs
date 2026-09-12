//! Node-level three-way merge of one file, per `spec/03-operations.md`
//! landing step 2: different units merge cleanly, set-like regions union,
//! a unit changed on one side is taken, a unit changed on both sides
//! composes when one side's change is a rename, recurses into its children
//! when it is a container, and conflicts otherwise. Formatting-only changes
//! are invisible because identity is by body hash, so an edit beats a
//! reformat of the same unit and a reformat elsewhere is kept.
//!
//! Units line up across the three versions by node ID: each side's index
//! was matched against the history it shares with the base, so a unit
//! carries the base's ID through edits, renames, and reorders, and an
//! overload inserted above its siblings shifts nothing. A recorded rename
//! lines its unit up when identity could not follow it. Only what carries
//! no shared ID, as when a side's index was built without history, falls
//! back to `(kind, name, ordinal)`; what still lines up with nothing is an
//! addition.
//!
//! Renames are the first semantic operation. One side's renames, recorded
//! by `edit` or inferred from a unit keeping its identity under a new name,
//! are applied to whatever the other side contributes, so a new call to the
//! old name in a concurrent change comes out calling the new name. Each such
//! application is reported as a resolved semantic conflict. A rename onto
//! a name the base or the other side already gives a unit of the same kind
//! at the same level is a conflict, not a second definition.

use std::collections::{HashMap, HashSet};

use tessra_core::object::Node;
use tessra_core::{EntityId, ObjectId};

use crate::matching::inferred_renames;
use crate::rename::{apply_edits, is_ident, rename_edits, Edit, Rename};
use crate::Language;

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

#[derive(Clone, Debug)]
struct Unit {
    nid: EntityId,
    kind: String,
    name: String,
    span: (usize, usize),
    body: ObjectId,
    /// Text between the previous sibling's end (or the container's start) and this unit.
    gap_start: usize,
    children: Vec<Unit>,
}

/// How a unit lines up across the three versions of one level: as the base
/// unit whose ID it carries or stands in for, or as an addition, keyed by
/// kind, name, and ordinal among the side's additions of that kind and
/// name, so the same addition made on both sides lands once.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Id(EntityId),
    New(String, String, usize),
}

fn build_units(nodes: &[Node]) -> Vec<Unit> {
    let mut by_parent: HashMap<Option<EntityId>, Vec<&Node>> = HashMap::new();
    for n in nodes {
        by_parent.entry(n.parent).or_default().push(n);
    }
    fn make(
        list: &[&Node],
        by_parent: &HashMap<Option<EntityId>, Vec<&Node>>,
        region_start: usize,
    ) -> Vec<Unit> {
        let mut out = Vec::new();
        let mut prev_end = region_start;
        for n in list {
            let span = (n.span.0 as usize, n.span.1 as usize);
            let kids = by_parent
                .get(&Some(n.nid))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let first_kid_start = kids.first().map(|k| k.span.0 as usize).unwrap_or(span.1);
            out.push(Unit {
                nid: n.nid,
                kind: n.kind.clone(),
                name: n.name.clone(),
                span,
                body: n.body,
                gap_start: prev_end,
                children: make(kids, by_parent, first_kid_start),
            });
            prev_end = span.1;
        }
        out
    }
    let top = by_parent.get(&None).map(|v| v.as_slice()).unwrap_or(&[]);
    make(top, &by_parent, 0)
}

/// One key per unit of a side at one level. A unit is the base unit whose
/// ID it carries; failing that, the base unit a recorded rename says it
/// was, when the body changed too and identity could not follow; failing
/// that, the base unit of its kind and name it stands in for, by body and
/// then by position, which is all that lines up when a side's index was
/// built without history. What is left is an addition.
fn keys(base: Option<&[Unit]>, side: &[Unit], recorded: &[Rename]) -> Vec<Key> {
    let base = base.unwrap_or(&[]);
    let mut out: Vec<Option<Key>> = vec![None; side.len()];
    let mut taken = vec![false; base.len()];
    let by_nid: HashMap<EntityId, usize> =
        base.iter().enumerate().map(|(i, u)| (u.nid, i)).collect();
    for (i, u) in side.iter().enumerate() {
        if let Some(&b) = by_nid.get(&u.nid) {
            if !taken[b] {
                taken[b] = true;
                out[i] = Some(Key::Id(u.nid));
            }
        }
    }
    for r in recorded {
        let Some(b) = base
            .iter()
            .position(|u| u.name == r.from && !taken[by_nid[&u.nid]])
        else {
            continue;
        };
        // The rename happened at this level only if nothing here still
        // carries the old name.
        if side
            .iter()
            .any(|u| u.name == r.from && u.kind == base[b].kind)
        {
            continue;
        }
        let found = side
            .iter()
            .enumerate()
            .find(|(i, u)| out[*i].is_none() && u.name == r.to && u.kind == base[b].kind)
            .map(|(i, _)| i);
        if let Some(i) = found {
            taken[b] = true;
            out[i] = Some(Key::Id(base[b].nid));
        }
    }
    // By kind and name among what is left, the same body first.
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (i, u) in side.iter().enumerate() {
        if out[i].is_some() || !seen.insert((u.kind.clone(), u.name.clone())) {
            continue;
        }
        let members: Vec<usize> = (i..side.len())
            .filter(|&j| out[j].is_none() && side[j].kind == u.kind && side[j].name == u.name)
            .collect();
        let mut candidates: Vec<usize> = (0..base.len())
            .filter(|&b| !taken[b] && base[b].kind == u.kind && base[b].name == u.name)
            .collect();
        for &j in &members {
            if let Some(pos) = candidates
                .iter()
                .position(|&b| base[b].body == side[j].body)
            {
                let b = candidates.remove(pos);
                taken[b] = true;
                out[j] = Some(Key::Id(base[b].nid));
            }
        }
        let mut rest = candidates.into_iter();
        for &j in &members {
            if out[j].is_none() {
                if let Some(b) = rest.next() {
                    taken[b] = true;
                    out[j] = Some(Key::Id(base[b].nid));
                }
            }
        }
    }
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for (i, u) in side.iter().enumerate() {
        if out[i].is_none() {
            let k = *counts
                .entry((u.kind.clone(), u.name.clone()))
                .and_modify(|c| *c += 1)
                .or_insert(0);
            out[i] = Some(Key::New(u.kind.clone(), u.name.clone(), k));
        }
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// Merge one file. `lang` is the file's grammar; without one, renames are
/// not applied, since only identifier tokens of a parse are renamed.
/// `base` is None when both sides added the file. `ops_a` and `ops_b` are
/// the renames each side recorded; renames inferred from unit identity are
/// added to them.
pub fn merge_file(
    lang: Option<Language>,
    base: Option<FileNodes<'_>>,
    a: FileNodes<'_>,
    b: FileNodes<'_>,
    ops_a: &[Rename],
    ops_b: &[Rename],
) -> Merged {
    let base_nodes: &[Node] = base.map(|f| f.nodes).unwrap_or(&[]);
    let inferred_a = inferred_renames("", base_nodes, a.nodes);
    let inferred_b = inferred_renames("", base_nodes, b.nodes);
    let renames = |inf: Vec<(EntityId, Rename)>, ops: &[Rename]| -> Vec<Rename> {
        let mut out: Vec<Rename> = inf.into_iter().map(|(_, r)| r).collect();
        for r in ops {
            if !out.iter().any(|x| x.from == r.from && x.to == r.to) {
                out.push(r.clone());
            }
        }
        out
    };
    let ren_a = renames(inferred_a, ops_a);
    let ren_b = renames(inferred_b, ops_b);
    let bu = base.map(|f| build_units(f.nodes));
    let au = build_units(a.nodes);
    let bbu = build_units(b.nodes);
    let ctx = Ctx {
        base: base.map(|f| f.text),
        a: a.text,
        b: b.text,
        ren_a: &ren_a,
        ren_b: &ren_b,
        ops_a,
        ops_b,
        edits_a: edits_in(lang, Some(a.text), &ren_b),
        edits_b: edits_in(lang, Some(b.text), &ren_a),
        base_edits_a: edits_in(lang, base.map(|f| f.text), &ren_a),
        base_edits_b: edits_in(lang, base.map(|f| f.text), &ren_b),
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

/// Where `renames` land in `text`, when there is a grammar to find
/// identifier tokens with.
fn edits_in<'r>(
    lang: Option<Language>,
    text: Option<&[u8]>,
    renames: &'r [Rename],
) -> Vec<Edit<'r>> {
    match (lang, text) {
        (Some(l), Some(t)) => rename_edits(l, t, renames),
        _ => Vec::new(),
    }
}

struct Ctx<'a> {
    base: Option<&'a [u8]>,
    a: &'a [u8],
    b: &'a [u8],
    /// Every rename a side made, recorded or inferred: what is applied to
    /// the other side's text.
    ren_a: &'a [Rename],
    ren_b: &'a [Rename],
    /// The renames a side recorded: what lines a unit up with the base
    /// when identity could not.
    ops_a: &'a [Rename],
    ops_b: &'a [Rename],
    /// Where the other side's renames land in a side's text, and where a
    /// side's own renames land in the base text.
    edits_a: Vec<Edit<'a>>,
    edits_b: Vec<Edit<'a>>,
    base_edits_a: Vec<Edit<'a>>,
    base_edits_b: Vec<Edit<'a>>,
}

impl<'a> Ctx<'a> {
    fn text(&self, side: Side) -> &'a [u8] {
        match side {
            Side::A => self.a,
            Side::B => self.b,
        }
    }

    fn renames(&self, side: Side) -> &'a [Rename] {
        match side {
            Side::A => self.ren_a,
            Side::B => self.ren_b,
        }
    }

    fn recorded(&self, side: Side) -> &'a [Rename] {
        match side {
            Side::A => self.ops_a,
            Side::B => self.ops_b,
        }
    }

    /// The other side's renames, located in this side's text.
    fn edits(&self, side: Side) -> &[Edit<'a>] {
        match side {
            Side::A => &self.edits_a,
            Side::B => &self.edits_b,
        }
    }

    /// This side's renames, located in the base text.
    fn base_edits(&self, side: Side) -> &[Edit<'a>] {
        match side {
            Side::A => &self.base_edits_a,
            Side::B => &self.base_edits_b,
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
fn ordered_keys(base: &[Key], a: &[Key], b: &[Key]) -> Vec<Key> {
    let mut result: Vec<Key> = base.to_vec();
    let in_base: HashSet<Key> = result.iter().cloned().collect();
    let mut present = in_base.clone();
    for side in [a, b] {
        let mut prev_in_result: Option<Key> = None;
        for key in side {
            if present.contains(key) {
                prev_in_result = Some(key.clone());
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
            result.insert(pos, key.clone());
            present.insert(key.clone());
            prev_in_result = Some(key.clone());
        }
    }
    result
}

/// One level of one version: its units and their keys, aligned.
#[derive(Clone, Copy)]
struct Level<'u> {
    units: &'u [Unit],
    keys: &'u [Key],
}

impl<'u> Level<'u> {
    fn find(&self, key: &Key) -> Option<&'u Unit> {
        self.keys
            .iter()
            .position(|k| k == key)
            .map(|i| &self.units[i])
    }
}

fn slice(text: &[u8], from: usize, to: usize) -> &[u8] {
    let to = to.min(text.len());
    let from = from.min(to);
    &text[from..to]
}

/// The gap before a unit in the merged output: the text before it on a
/// side where it follows the unit it follows in the output, `prefer`
/// first, so an insertion above the first member of a container brings
/// the separator between them and the member keeps its own lead-in; the
/// preferred side's own gap when neither side lines up.
fn gap_before<'c>(
    ctx: &'c Ctx<'_>,
    level_a: Level<'_>,
    level_b: Level<'_>,
    key: &Key,
    prefer: Side,
    last: Option<&Key>,
) -> &'c [u8] {
    let level_of = |side: Side| match side {
        Side::A => level_a,
        Side::B => level_b,
    };
    for side in [prefer, prefer.other()] {
        let level = level_of(side);
        let Some(i) = level.keys.iter().position(|k| k == key) else {
            continue;
        };
        let pred = if i == 0 {
            None
        } else {
            Some(&level.keys[i - 1])
        };
        if pred == last {
            let u = &level.units[i];
            return slice(ctx.text(side), u.gap_start, u.span.0);
        }
    }
    let u = level_of(prefer)
        .find(key)
        .expect("a unit is emitted from a side that holds it");
    slice(ctx.text(prefer), u.gap_start, u.span.0)
}

/// Emit a unit's gap and text from one side, with the other side's renames
/// applied to it. Every rename that changed something is reported.
fn emit_leaf(ctx: &Ctx<'_>, side: Side, u: &Unit, gap: &[u8], out: &mut Vec<u8>, m: &mut Merged) {
    push_gap(out, gap);
    let (fixed, applied) = apply_edits(ctx.text(side), u.span.0, u.span.1, ctx.edits(side));
    for r in applied {
        m.resolved.push(format!(
            "{} {}: {} renamed to {}",
            u.kind, u.name, r.from, r.to
        ));
    }
    out.extend_from_slice(&fixed);
}

/// A unit's gap from its own side, or a line break when the unit was first
/// on its side but is not first in the merged output. A line holding only
/// indentation is a container header's lead-in to this very unit, not a
/// line to break: breaking there de-indents the container's first item.
fn push_gap(out: &mut Vec<u8>, gap: &[u8]) {
    if gap.is_empty() && !out.is_empty() {
        let line_start = out
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        if out[line_start..].iter().any(|b| !b.is_ascii_whitespace()) {
            out.push(b'\n');
        }
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
    let (renamed, applied) = apply_edits(
        base_text,
        base_unit.span.0,
        base_unit.span.1,
        ctx.base_edits(side),
    );
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
    let base_keys: Vec<Key> = base
        .map(|u| u.iter().map(|x| Key::Id(x.nid)).collect())
        .unwrap_or_default();
    let keys_a = keys(base, a, ctx.recorded(Side::A));
    let keys_b = keys(base, b, ctx.recorded(Side::B));
    let base_level = base.map(|units| Level {
        units,
        keys: &base_keys,
    });
    let level_a = Level {
        units: a,
        keys: &keys_a,
    };
    let level_b = Level {
        units: b,
        keys: &keys_b,
    };
    for (side, level) in [(Side::A, level_a), (Side::B, level_b)] {
        let other = match side {
            Side::A => level_b,
            Side::B => level_a,
        };
        for (u, key) in level.units.iter().zip(level.keys) {
            match key {
                // A unit added on one side under a name the other side
                // renamed something to would collide with the renamed unit.
                Key::New(..) => {
                    if let Some(r) = ctx.renames(side.other()).iter().find(|r| r.to == u.name) {
                        m.conflicts.push(format!(
                            "{} {} (added on one side, {} renamed to it on the other)",
                            u.kind, u.name, r.from
                        ));
                    }
                }
                // A unit renamed onto a name the base already gives another
                // unit of its kind, or that the other side renamed a
                // different unit to, would be a second definition.
                Key::Id(nid) => {
                    let Some(b0) = base_level.and_then(|l| l.find(key)) else {
                        continue;
                    };
                    if u.name == b0.name || !is_ident(&u.name) {
                        continue;
                    }
                    let in_base = base_level.and_then(|l| {
                        l.units
                            .iter()
                            .find(|x| x.nid != *nid && x.kind == u.kind && x.name == u.name)
                    });
                    if in_base.is_some() {
                        m.conflicts.push(format!(
                            "{} {} (renamed to {}, which the base already defines)",
                            u.kind, b0.name, u.name
                        ));
                        continue;
                    }
                    let elsewhere = other.units.iter().zip(other.keys).find(|(x, k)| {
                        x.kind == u.kind && x.name == u.name && matches!(k, Key::Id(o) if o != nid)
                    });
                    if let Some((_, Key::Id(o))) = elsewhere {
                        let from = base_level
                            .and_then(|l| l.find(&Key::Id(*o)))
                            .map(|x| x.name.clone())
                            .unwrap_or_default();
                        m.conflicts.push(format!(
                            "{} {} (renamed to {} on one side, {} renamed to it on the other)",
                            u.kind, b0.name, u.name, from
                        ));
                    }
                }
            }
        }
    }
    // The key last emitted at this level, for choosing each unit's gap.
    let mut last: Option<Key> = None;
    for key in ordered_keys(&base_keys, &keys_a, &keys_b) {
        let bu = base_level.and_then(|l| l.find(&key));
        let au = level_a.find(&key);
        let bbu = level_b.find(&key);
        let gap = |prefer: Side| gap_before(ctx, level_a, level_b, &key, prefer, last.as_ref());
        let before = out.len();
        match (bu, au, bbu) {
            (_, None, None) => {}
            (None, Some(x), None) => emit_leaf(ctx, Side::A, x, gap(Side::A), out, m),
            (None, None, Some(y)) => emit_leaf(ctx, Side::B, y, gap(Side::B), out, m),
            (None, Some(x), Some(y)) => {
                if x.body == y.body {
                    emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
                } else if !x.children.is_empty() && !y.children.is_empty() {
                    merge_container(ctx, None, x, y, gap(Side::A), gap(Side::B), out, m);
                } else {
                    m.conflicts.push(format!("{} {}", x.kind, x.name));
                    emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
                }
            }
            (Some(b0), None, Some(y)) => {
                if y.body != b0.body || y.name != b0.name {
                    m.conflicts.push(format!(
                        "{} {} (deleted on one side, changed on the other)",
                        y.kind, y.name
                    ));
                    emit_leaf(ctx, Side::B, y, gap(Side::B), out, m);
                }
            }
            (Some(b0), Some(x), None) => {
                if x.body != b0.body || x.name != b0.name {
                    m.conflicts.push(format!(
                        "{} {} (deleted on one side, changed on the other)",
                        x.kind, x.name
                    ));
                    emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
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
                            emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
                        } else {
                            emit_leaf(ctx, Side::B, y, gap(Side::B), out, m);
                        }
                    }
                    (true, false) => emit_leaf(ctx, Side::A, x, gap(Side::A), out, m),
                    (false, true) => emit_leaf(ctx, Side::B, y, gap(Side::B), out, m),
                    (true, true) => {
                        if x.body == y.body && x.name == y.name {
                            emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
                        } else if x.name != b0.name && y.name != b0.name && x.name != y.name {
                            m.conflicts.push(format!(
                                "{} {} (renamed to {} on one side and {} on the other)",
                                b0.kind, b0.name, x.name, y.name
                            ));
                            emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
                        } else if change_is_renames(ctx, Side::A, b0, x) {
                            emit_leaf(ctx, Side::B, y, gap(Side::B), out, m);
                        } else if change_is_renames(ctx, Side::B, b0, y) {
                            emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
                        } else if !x.children.is_empty()
                            && !y.children.is_empty()
                            && !b0.children.is_empty()
                        {
                            merge_container(
                                ctx,
                                Some(b0),
                                x,
                                y,
                                gap(Side::A),
                                gap(Side::B),
                                out,
                                m,
                            );
                        } else {
                            let reason = if x.name != b0.name || y.name != b0.name {
                                " (renamed on one side, changed on the other)"
                            } else {
                                ""
                            };
                            m.conflicts
                                .push(format!("{} {}{}", b0.kind, b0.name, reason));
                            emit_leaf(ctx, Side::A, x, gap(Side::A), out, m);
                        }
                    }
                }
            }
        }
        if out.len() != before {
            last = Some(key);
        }
    }
}

/// Both sides changed a container differently: keep the header and footer
/// from whichever side changed them, and merge the children. `gap_a` and
/// `gap_b` are the container's gap when its header comes from A or B.
#[allow(clippy::too_many_arguments)]
fn merge_container(
    ctx: &Ctx<'_>,
    base: Option<&Unit>,
    x: &Unit,
    y: &Unit,
    gap_a: &[u8],
    gap_b: &[u8],
    out: &mut Vec<u8>,
    m: &mut Merged,
) {
    let base_header = base.and_then(|b| ctx.base.map(|t| header(t, b)));
    let (gap_side, head_unit, gap_text) = match base_header {
        Some(bh) if header(ctx.a, x) == bh => (Side::B, y, gap_b),
        _ => (Side::A, x, gap_a),
    };
    push_gap(out, gap_text);
    let head_end = head_unit
        .children
        .first()
        .map(|c| c.span.0)
        .unwrap_or(head_unit.span.1);
    let (head, applied) = apply_edits(
        ctx.text(gap_side),
        head_unit.span.0,
        head_end,
        ctx.edits(gap_side),
    );
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
    use crate::matching::assign_ids_with_refs;
    use crate::Language;

    /// Nodes of one version, matched against the previous version's nodes
    /// and text as the daemon matches them at snapshot time.
    fn nodes_in(lang: Language, src: &str, prev: &[Node], prev_src: Option<&str>) -> Vec<Node> {
        let path = match lang {
            Language::Rust => "f.rs",
            Language::Python => "f.py",
            Language::JavaScript => "f.js",
            Language::TypeScript => "f.ts",
            Language::Tsx => "f.tsx",
        };
        let raw = with_grammar(lang, src.as_bytes()).unwrap();
        assign_ids_with_refs(path, &raw, prev, prev_src.map(str::as_bytes)).0
    }

    fn nodes(src: &str, prev: &[Node]) -> Vec<Node> {
        nodes_in(Language::Rust, src, prev, None)
    }

    fn merge_nodes(
        lang: Language,
        base: Option<(&str, &[Node])>,
        a: (&str, &[Node]),
        b: (&str, &[Node]),
        ops_a: &[Rename],
        ops_b: &[Rename],
    ) -> Merged {
        merge_file(
            Some(lang),
            base.map(|(text, nodes)| FileNodes {
                text: text.as_bytes(),
                nodes,
            }),
            FileNodes {
                text: a.0.as_bytes(),
                nodes: a.1,
            },
            FileNodes {
                text: b.0.as_bytes(),
                nodes: b.1,
            },
            ops_a,
            ops_b,
        )
    }

    fn merge_in(
        lang: Language,
        base: &str,
        a: &str,
        b: &str,
        ops_a: &[Rename],
        ops_b: &[Rename],
    ) -> Merged {
        let bn = nodes_in(lang, base, &[], None);
        let an = nodes_in(lang, a, &bn, Some(base));
        let bbn = nodes_in(lang, b, &bn, Some(base));
        merge_nodes(lang, Some((base, &bn)), (a, &an), (b, &bbn), ops_a, ops_b)
    }

    fn merge_ops(base: &str, a: &str, b: &str, ops_a: &[Rename], ops_b: &[Rename]) -> Merged {
        merge_in(Language::Rust, base, a, b, ops_a, ops_b)
    }

    fn merge(base: &str, a: &str, b: &str) -> Merged {
        merge_ops(base, a, b, &[], &[])
    }

    #[test]
    fn same_named_siblings_do_not_drift() {
        // Three overloads of m; one side inserts a fourth above them, the
        // other edits the implementation below. Ordinals shifted, IDs did
        // not: no conflict, and both changes land.
        let base = "class K {\n  m(a: string): void;\n  m(a: number): void;\n  m(a: any) { return a; }\n}\n";
        let a = base.replace("  m(a: string)", "  m(a: boolean): void;\n  m(a: string)");
        let b = base.replace("return a;", "return a + 1;");
        let m = merge_in(Language::TypeScript, base, &a, &b, &[], &[]);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert_eq!(
            t,
            "class K {\n  m(a: boolean): void;\n  m(a: string): void;\n  m(a: number): void;\n  m(a: any) { return a + 1; }\n}\n"
        );
        // The other landing order gives the same file.
        let m2 = merge_in(Language::TypeScript, base, &b, &a, &[], &[]);
        assert!(m2.conflicts.is_empty(), "{:?}", m2.conflicts);
        assert_eq!(String::from_utf8(m2.text).unwrap(), t);
    }

    #[test]
    fn a_member_inserted_above_the_first_keeps_the_separator() {
        // A field's comma is not part of its span; it lives in the gap
        // before the next field, which the inserting side supplies.
        let base = "struct S {\n    a: i32,\n}\n";
        let a = "struct S {\n    z: u8,\n    a: i32,\n}\n";
        let b = "struct S {\n    a: i64,\n}\n";
        let m = merge(base, a, b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        assert_eq!(
            String::from_utf8(m.text).unwrap(),
            "struct S {\n    z: u8,\n    a: i64,\n}\n"
        );
    }

    #[test]
    fn units_line_up_by_id_and_fall_back_to_name_without_one() {
        // A reorders and edits a; B edits c. By ID nothing drifts.
        let a =
            "use std::fmt;\n\nfn c() {\n    3\n}\n\nfn a() {\n    10\n}\n\nfn b() {\n    2\n}\n";
        let b = BASE.replace("    3\n", "    30\n");
        let m = merge(BASE, a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("fn a() {\n    10\n}"), "{t}");
        assert!(t.contains("fn c() {\n    30\n}"), "{t}");
        // B's index was built without history, so it shares no ID with the
        // base: its units still line up by kind and name.
        let bn = nodes(BASE, &[]);
        let an = nodes(a, &bn);
        let bbn = nodes(&b, &[]);
        assert!(bbn.iter().all(|n| bn.iter().all(|p| p.nid != n.nid)));
        let m2 = merge_nodes(
            Language::Rust,
            Some((BASE, &bn)),
            (a, &an),
            (&b, &bbn),
            &[],
            &[],
        );
        assert!(m2.conflicts.is_empty(), "{:?}", m2.conflicts);
        assert_eq!(String::from_utf8(m2.text).unwrap(), t);
        // And on that side a changed unit is still a change, not a
        // deletion plus an addition.
        let b3 = BASE.replace("    1\n", "    11\n");
        let b3n = nodes(&b3, &[]);
        let m3 = merge_nodes(
            Language::Rust,
            Some((BASE, &bn)),
            (a, &an),
            (&b3, &b3n),
            &[],
            &[],
        );
        assert_eq!(m3.conflicts, vec!["function a".to_string()]);
    }

    #[test]
    fn a_rename_with_an_edit_carries_to_the_other_sides_caller() {
        // A renames parse to parse_numbers and adds a line in the same
        // step, with no recorded op; B adds a caller of parse. Identity
        // follows by similarity, so the caller comes out renamed.
        let base = "fn a() { 1 }\n\nfn parse(input: &str) -> Vec<u32> {\n    let mut out = Vec::new();\n    for part in input.split(',') {\n        if let Ok(n) = part.trim().parse::<u32>() {\n            out.push(n);\n        }\n    }\n    out\n}\n";
        let a = base
            .replace("fn parse(", "fn parse_numbers(")
            .replace("    out\n}", "    out.sort();\n    out\n}");
        let b = format!("{base}\nfn count(s: &str) -> usize {{ parse(s).len() }}\n");
        let m = merge(base, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("fn parse_numbers(input"), "{t}");
        assert!(t.contains("    out.sort();\n    out\n}"), "{t}");
        assert!(t.contains("{ parse_numbers(s).len() }"), "{t}");
        assert!(!t.contains("fn parse("), "{t}");
        assert_eq!(
            m.resolved,
            vec!["function count: parse renamed to parse_numbers".to_string()]
        );
    }

    #[test]
    fn a_rename_carried_across_the_merge_leaves_strings_comments_and_locals_alone() {
        // A renames b to bee; B adds a unit that mentions b in a string, a
        // comment, a local binding, a field, and a call. Only the call and
        // the definition change.
        let a = BASE.replace("fn b()", "fn bee()");
        let b = format!(
            "{BASE}\n/// Uses b.\nfn d(s: S) -> i32 {{\n    let name = \"b\"; // b\n    let b = s.b;\n    b + s.b() + {}()\n}}\n",
            "b"
        );
        let m = merge(BASE, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.contains("fn bee() {"), "{t}");
        assert!(
            t.contains("/// Uses b.\nfn d(s: S) -> i32 {\n    let name = \"b\"; // b\n    let b = s.b;\n    b + s.b() + b()\n}"),
            "a function that binds b itself keeps every b: {t}"
        );
        // The same caller without the local binding gets the call renamed
        // and nothing else.
        let b2 = format!(
            "{BASE}\n/// Uses b.\nfn d(s: S) -> i32 {{\n    let name = \"b\"; // b\n    s.b + s.b() + b()\n}}\n"
        );
        let m2 = merge(BASE, &a, &b2);
        assert!(m2.conflicts.is_empty(), "{:?}", m2.conflicts);
        let t2 = String::from_utf8(m2.text).unwrap();
        assert!(
            t2.contains("/// Uses b.\nfn d(s: S) -> i32 {\n    let name = \"b\"; // b\n    s.b + s.bee() + bee()\n}"),
            "{t2}"
        );
        assert_eq!(
            m2.resolved,
            vec!["function d: b renamed to bee".to_string()]
        );
    }

    #[test]
    fn twenty_sides_each_editing_a_different_function_fold_cleanly() {
        // Twenty functions; twenty changes against the same base, each
        // editing one of them, landed one after another: each merge is
        // three-way between the base, the head so far, and the next side,
        // with the head's index matched against the one before it as the
        // daemon does after a landing.
        let base: String = (0..20)
            .map(|i| {
                format!("/// Function {i}.\npub fn f{i:02}(x: i32) -> i32 {{\n    x + {i}\n}}\n\n")
            })
            .collect();
        let edit = |i: usize| match i % 4 {
            0 => base.replace(&format!("    x + {i}\n"), &format!("    x * {i} + 1000\n")),
            1 => base.replace(
                &format!("pub fn f{i:02}(x: i32)"),
                &format!("#[inline]\npub fn f{i:02}(x: i32)"),
            ),
            2 => base.replace(
                &format!("/// Function {i}.\n"),
                &format!("/// Function {i}, documented.\n"),
            ),
            _ => base.replace(
                &format!("    x + {i}\n}}"),
                &format!("    let y = x + {i};\n    y * 2\n}}"),
            ),
        };
        let bn = nodes(&base, &[]);
        let mut head = base.clone();
        let mut head_nodes = bn.clone();
        for i in 0..20 {
            let side = edit(i);
            assert_ne!(side, base, "edit {i} changes something");
            let sn = nodes_in(Language::Rust, &side, &bn, Some(&base));
            let m = merge_nodes(
                Language::Rust,
                Some((&base, &bn)),
                (&head, &head_nodes),
                (&side, &sn),
                &[],
                &[],
            );
            assert!(m.conflicts.is_empty(), "side {i}: {:?}", m.conflicts);
            let next = String::from_utf8(m.text).unwrap();
            head_nodes = nodes_in(Language::Rust, &next, &head_nodes, Some(&head));
            head = next;
        }
        for i in 0..20 {
            let expected = match i % 4 {
                0 => format!("    x * {i} + 1000\n"),
                1 => format!("#[inline]\npub fn f{i:02}(x: i32)"),
                2 => format!("/// Function {i}, documented.\n"),
                _ => format!("    let y = x + {i};\n    y * 2\n}}"),
            };
            assert!(head.contains(&expected), "edit {i} is present:\n{head}");
        }
        let reparsed = with_grammar(Language::Rust, head.as_bytes()).unwrap();
        assert_eq!(
            reparsed.iter().filter(|n| n.kind == "function").count(),
            20,
            "{head}"
        );
        assert_eq!(head_nodes.len(), 20);
        assert!(
            head_nodes.iter().all(|n| bn.iter().any(|b| b.nid == n.nid)),
            "every function keeps its identity through twenty landings"
        );
    }

    #[test]
    fn a_rename_onto_an_existing_name_conflicts() {
        // The base already defines bee; A renames b to bee.
        let base = format!("{BASE}\nfn bee() {{ 9 }}\n");
        let a = base.replace("fn b()", "fn bee()");
        let b = base.replace("    3\n", "    30\n");
        let m = merge(&base, &a, &b);
        assert_eq!(m.conflicts.len(), 1, "{:?}", m.conflicts);
        assert!(
            m.conflicts[0].contains("renamed to bee, which the base already defines"),
            "{:?}",
            m.conflicts
        );
        // Both sides renamed different units to the same name.
        let a2 = BASE.replace("fn b()", "fn x()");
        let b2 = BASE.replace("fn c()", "fn x()");
        let m2 = merge(BASE, &a2, &b2);
        assert_eq!(m2.conflicts.len(), 2, "{:?}", m2.conflicts);
        assert!(
            m2.conflicts
                .iter()
                .all(|c| c.contains("renamed to x on one side")),
            "{:?}",
            m2.conflicts
        );
        // The same unit renamed the same way on both sides is not a collision.
        let m3 = merge(BASE, &a2, &a2);
        assert!(m3.conflicts.is_empty(), "{:?}", m3.conflicts);
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
    fn insertions_into_the_same_module_keep_its_first_item_indented() {
        let base = "mod tests {\n    use super::*;\n\n    #[test]\n    fn a() {}\n}\n";
        let a = base.replace(
            "    #[test]\n    fn a() {}",
            "    #[test]\n    fn b() {}\n\n    #[test]\n    fn a() {}",
        );
        let b = base.replace(
            "    #[test]\n    fn a() {}",
            "    #[test]\n    fn c() {}\n\n    #[test]\n    fn a() {}",
        );
        let m = merge(base, &a, &b);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let t = String::from_utf8(m.text).unwrap();
        assert!(t.starts_with("mod tests {\n    use super::*;\n"), "{t}");
        for f in ["fn b()", "fn c()", "fn a()"] {
            assert!(t.contains(f), "{t}");
        }
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
