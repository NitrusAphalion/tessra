//! Deterministic evaluation of a standard against a revision and a set of
//! cited attestations, per `spec/02-objects.md` standard and
//! `spec/03-operations.md` landing steps 6 and 7.
//!
//! What a clause may read: the view, the revision, its snapshots and their
//! indexes, the first parent's index, and the attestations cited. Never a
//! clock. Attestations count only when signed by a principal the standard
//! trusts for the kind: a verifier, the daemon as runner, a hook, an
//! external system, or a human. A session's or agent's own attestation
//! never satisfies an `attest` clause unless the clause says `self`.
//!
//! Judged, approval, and reputation clauses arrive in M5 and evaluate as
//! unmet until then, so a standard that uses them cannot be satisfied by
//! accident.

use std::collections::{BTreeMap, HashMap, HashSet};

use ciborium::value::Value;
use tessra_core::object::{
    Attestation, Clause, Node, NodeIndex, Predicate, Principal, Revision, Snapshot, Standard,
};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};

use crate::view::ViewState;
use crate::{Error, Result};

/// One clause that did not hold, with a reason an agent can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmet {
    pub clause: String,
    pub reason: String,
}

/// The default standard created by init: no flags on the revision.
pub fn default_trunk_standard(id: EntityId, name: &str) -> Standard {
    Standard {
        id,
        prev: None,
        name: name.into(),
        extends: None,
        clauses: vec![Clause {
            op: "require".into(),
            pred: Predicate {
                kind: "structural".into(),
                name: Some("flags.none".into()),
                args: None,
            },
            unless: None,
        }],
        scopes: None,
        when: None,
        audit_permille: None,
    }
}

/// Principal kinds whose attestations satisfy `attest` clauses.
pub const TRUSTED_ATTESTERS: &[&str] = &[
    "verifier", "daemon", "hook", "external", "human", "observer", "deployer",
];

/// Kinds that are not code a test could cover.
const UNCOVERABLE_KINDS: &[&str] = &[
    "import",
    "chunk",
    "test",
    "test.skipped",
    "impl",
    "module",
    "variant",
    "field",
];

/// Load the standard chain, most general first.
pub fn chain<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    view: &tessra_core::object::View,
    standard: &EntityId,
) -> Result<Vec<Standard>> {
    let mut chain: Vec<Standard> = Vec::new();
    let mut cur = Some(*standard);
    let mut guard = 0;
    while let Some(id) = cur {
        guard += 1;
        if guard > 32 {
            return Err(Error::Other("standard extends chain too deep".into()));
        }
        let ptr = vs
            .entity(view, &id)?
            .ok_or_else(|| Error::Other(format!("standard {id} not in view")))?;
        let s: Standard = store.get(&ptr.single()?)?;
        cur = s.extends;
        chain.push(s);
    }
    chain.reverse();
    Ok(chain)
}

/// Every `attest` predicate a standard chain asks for: `(kind, for)`.
pub fn required_attestations(chain: &[Standard]) -> Vec<(String, Option<String>)> {
    fn walk(p: &Predicate, out: &mut Vec<(String, Option<String>)>) {
        match p.kind.as_str() {
            "attest" => {
                if let Some(n) = &p.name {
                    let f = p
                        .args
                        .as_ref()
                        .and_then(|a| a.get("for"))
                        .and_then(|v| v.as_text().map(str::to_string));
                    if !out.iter().any(|(k, ff)| k == n && *ff == f) {
                        out.push((n.clone(), f));
                    }
                }
            }
            "all" | "any" | "not" => {
                for q in sub_preds(p) {
                    walk(&q, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in chain {
        for c in &s.clauses {
            walk(&c.pred, &mut out);
            if let Some(u) = &c.unless {
                walk(u, &mut out);
            }
        }
    }
    out
}

fn sub_preds(p: &Predicate) -> Vec<Predicate> {
    p.args
        .as_ref()
        .and_then(|a| a.get("preds"))
        .and_then(|v| v.clone().deserialized::<Vec<Predicate>>().ok())
        .unwrap_or_default()
}

/// The index of a snapshot: the one it names, or the one a daemon computed
/// and cached against the snapshot ID.
pub fn index_of<S: ObjectStore>(store: &S, snap_id: &ObjectId) -> Result<Option<NodeIndex>> {
    let snap: Snapshot = store.get(snap_id)?;
    if let Some(id) = snap.index {
        return Ok(Some(store.get(&id)?));
    }
    if let Some(bytes) = store.meta(&format!("idx:{}", snap_id.to_hex()))? {
        if let Ok(id) = ObjectId::from_slice(&bytes) {
            return Ok(Some(store.get(&id)?));
        }
    }
    Ok(None)
}

/// Evaluate the standard entity `standard` for `revision` with the cited attestations.
/// Returns the unmet clauses; empty means satisfied.
pub fn evaluate<S: ObjectStore>(
    store: &S,
    vs: &ViewState<'_, S>,
    view: &tessra_core::object::View,
    standard: &EntityId,
    revision_id: &ObjectId,
    revision: &Revision,
    attests: &[ObjectId],
) -> Result<Vec<Unmet>> {
    let chain = chain(store, vs, view, standard)?;
    let mut loaded: Vec<Loaded> = Vec::with_capacity(attests.len());
    let principal_of = |id: &EntityId| -> Result<Option<Principal>> {
        Ok(match vs.entity(view, id)? {
            Some(p) => store.get::<Principal>(&p.single()?).ok(),
            None => None,
        })
    };
    for a in attests {
        let att: Attestation = store.get(a)?;
        let signer = att.runner.unwrap_or(att.verifier);
        let p = principal_of(&signer)?;
        loaded.push(Loaded {
            att,
            signer_kind: p.as_ref().map(|p| p.kind.clone()),
            signer_parent: p.as_ref().and_then(|p| p.parent),
            signer_model: p.as_ref().and_then(|p| p.model.clone()),
            signer_name: p.as_ref().map(|p| p.name.clone()),
        });
    }
    let author_parent = principal_of(&revision.author)?.and_then(|p| p.parent);
    // The revision's index and its first parent's, for node-scoped checks.
    let snap_ids: Vec<ObjectId> = revision.snapshots.values().copied().collect();
    let index = match revision.snapshots.get("") {
        Some(s) => index_of(store, s)?,
        None => None,
    };
    let parent_index = match revision.parents.first() {
        Some(p) => {
            let pr: Revision = store.get(p)?;
            match pr.snapshots.get("") {
                Some(s) => index_of(store, s)?,
                None => None,
            }
        }
        None => Some(NodeIndex {
            root: ObjectId([0; 32]),
            grammars: String::new(),
            parents: vec![],
            nodes: vec![],
            aliases: None,
        }),
    };
    let mut subjects: Vec<Vec<u8>> = vec![revision_id.as_bytes().to_vec()];
    // A revision inherits the attestations on the revision it supersedes
    // only when the system derived it from that revision by merging it onto
    // a new base: a landing, a restack, or a conflict merge, whose second
    // parent names the revision merged. An author's re-snapshot names its
    // predecessor as prev too, but its content is the author's again, so an
    // approval, a judge's verdict, or a CI result on the predecessor does
    // not carry to it.
    if let Some(p) = revision.prev {
        if revision.parents.get(1) == Some(&p) {
            subjects.push(p.as_bytes().to_vec());
        }
    }
    subjects.extend(snap_ids.iter().map(|s| s.as_bytes().to_vec()));
    let ctx = Ctx {
        revision,
        attests: &loaded,
        subjects,
        index: index.as_ref(),
        parent_index: parent_index.as_ref(),
        author_parent,
    };
    let mut unmet = Vec::new();
    for s in &chain {
        for c in &s.clauses {
            if let Some(u) = check_clause(&ctx, c)? {
                unmet.push(u);
            }
        }
        // Clauses that apply from a risk level up, read from the latest
        // risk.change attestation on the revision.
        for w in s.when.iter().flatten() {
            match ctx.risk() {
                None => unmet.push(Unmet {
                    clause: format!("when risk>={}", w.risk_at_least),
                    reason: "no risk.change attestation on this revision; run verify".into(),
                }),
                Some((score, level, factors)) => {
                    if level_rank(&level) >= level_rank(&w.risk_at_least) {
                        for c in &w.clauses {
                            if let Some(mut u) = check_clause(&ctx, c)? {
                                let top: Vec<String> = factors
                                    .iter()
                                    .take(3)
                                    .map(|(n, p, l)| format!("{n} +{p} (lower it: {l})"))
                                    .collect();
                                u.clause = format!("when risk>={}: {}", w.risk_at_least, u.clause);
                                u.reason = format!(
                                    "risk is {level} ({score}); {}; top factors: {}",
                                    u.reason,
                                    top.join("; ")
                                );
                                unmet.push(u);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(unmet)
}

/// Score, level, and factors as `(name, points, lower)`.
type RiskSummary = (u32, String, Vec<(String, u32, String)>);

/// Rank of a risk level name.
pub fn level_rank(level: &str) -> u8 {
    match level {
        "low" => 0,
        "medium" => 1,
        "high" => 2,
        "critical" => 3,
        _ => 0,
    }
}

struct Loaded {
    att: Attestation,
    signer_kind: Option<String>,
    signer_parent: Option<EntityId>,
    signer_model: Option<String>,
    signer_name: Option<String>,
}

struct Ctx<'a> {
    revision: &'a Revision,
    attests: &'a [Loaded],
    /// Object IDs an attestation's `subject` may name to apply to this revision.
    subjects: Vec<Vec<u8>>,
    index: Option<&'a NodeIndex>,
    parent_index: Option<&'a NodeIndex>,
    /// The author's durable agent, which no judge may share.
    author_parent: Option<EntityId>,
}

impl<'a> Ctx<'a> {
    /// Units changed against the first parent: added, changed, or renamed,
    /// by identity. `None` when an index is missing.
    fn changed_units(&self) -> Option<Vec<&'a Node>> {
        let idx = self.index?;
        let parent = self.parent_index?;
        let prev: HashMap<EntityId, &Node> = parent.nodes.iter().map(|n| (n.nid, n)).collect();
        Some(
            idx.nodes
                .iter()
                .filter(|n| match prev.get(&n.nid) {
                    None => true,
                    Some(p) => p.body != n.body || p.name != n.name,
                })
                .collect(),
        )
    }

    fn deleted_units(&self) -> Option<Vec<&'a Node>> {
        let idx = self.index?;
        let parent = self.parent_index?;
        let now: HashSet<EntityId> = idx.nodes.iter().map(|n| n.nid).collect();
        Some(
            parent
                .nodes
                .iter()
                .filter(|n| !now.contains(&n.nid))
                .collect(),
        )
    }

    fn new_tests(&self) -> Option<Vec<&'a Node>> {
        let parent = self.parent_index?;
        let prev: HashSet<EntityId> = parent.nodes.iter().map(|n| n.nid).collect();
        let prev_bodies: HashSet<ObjectId> = parent.nodes.iter().map(|n| n.body).collect();
        Some(
            self.changed_units()?
                .into_iter()
                .filter(|n| {
                    is_test_kind(&n.kind)
                        && !prev.contains(&n.nid)
                        && !prev_bodies.contains(&n.body)
                })
                .collect(),
        )
    }

    fn trusted(&self, l: &Loaded, allow_self: bool) -> bool {
        if allow_self {
            return true;
        }
        l.signer_kind
            .as_deref()
            .is_some_and(|k| TRUSTED_ATTESTERS.contains(&k))
    }

    /// The units among `units` that no true attestation of `kind` names in `bodies`.
    fn uncovered_by_bodies(
        &self,
        kind: &str,
        units: &[&'a Node],
        allow_self: bool,
    ) -> Vec<&'a Node> {
        let mut covered: HashSet<ObjectId> = HashSet::new();
        for l in self.attests {
            if l.att.kind == kind && truthy(&l.att.result) && self.trusted(l, allow_self) {
                if let Some(b) = &l.att.bodies {
                    covered.extend(b.iter().copied());
                }
            }
        }
        units
            .iter()
            .filter(|u| !covered.contains(&u.body))
            .copied()
            .collect()
    }

    fn subject_attested(&self, kind: &str, allow_self: bool) -> bool {
        self.attests.iter().any(|l| {
            l.att.kind == kind
                && truthy(&l.att.result)
                && self.trusted(l, allow_self)
                && l.att
                    .subject
                    .as_ref()
                    .is_some_and(|s| self.subjects.iter().any(|x| x == s.as_slice()))
        })
    }

    /// The latest trusted risk.change attestation on this revision: score,
    /// level, and factors as `(name, points, lower)`.
    fn risk(&self) -> Option<RiskSummary> {
        let mut best: Option<(i64, &Attestation)> = None;
        for l in self.attests {
            if l.att.kind != "risk.change" || !self.trusted(l, false) {
                continue;
            }
            let on_us = l
                .att
                .subject
                .as_ref()
                .is_some_and(|s| self.subjects.iter().any(|x| x == s.as_slice()));
            if on_us && best.map_or(true, |(t, _)| l.att.time > t) {
                best = Some((l.att.time, &l.att));
            }
        }
        let (_, att) = best?;
        let Value::Map(m) = &att.result else {
            return None;
        };
        let get = |k: &str| {
            m.iter()
                .find(|(key, _)| matches!(key, Value::Text(t) if t == k))
                .map(|(_, v)| v)
        };
        let score = match get("score") {
            Some(Value::Integer(i)) => i128::from(*i) as u32,
            _ => return None,
        };
        let level = match get("level") {
            Some(Value::Text(t)) => t.clone(),
            _ => return None,
        };
        let mut factors = Vec::new();
        if let Some(Value::Array(a)) = get("factors") {
            for f in a {
                if let Value::Map(fm) = f {
                    let text = |k: &str| {
                        fm.iter()
                            .find(|(key, _)| matches!(key, Value::Text(t) if t == k))
                            .and_then(|(_, v)| v.as_text().map(str::to_string))
                            .unwrap_or_default()
                    };
                    let points = fm
                        .iter()
                        .find(|(key, _)| matches!(key, Value::Text(t) if t == "points"))
                        .and_then(|(_, v)| match v {
                            Value::Integer(i) => Some(i128::from(*i) as u32),
                            _ => None,
                        })
                        .unwrap_or(0);
                    factors.push((text("name"), points, text("lower")));
                }
            }
        }
        Some((score, level, factors))
    }

    /// A false attestation of `kind` on this revision, by subject or by any
    /// changed unit's body, for a reason.
    fn negative_on_revision(&self, kind: &str) -> Option<&Attestation> {
        let changed: HashSet<ObjectId> = self
            .changed_units()
            .map(|u| u.iter().map(|n| n.body).collect())
            .unwrap_or_default();
        self.attests
            .iter()
            .filter(|l| l.att.kind == kind && !truthy(&l.att.result))
            .find(|l| {
                l.att
                    .subject
                    .as_ref()
                    .is_some_and(|s| self.subjects.iter().any(|x| x == s.as_slice()))
                    || l.att
                        .bodies
                        .as_ref()
                        .is_some_and(|b| b.iter().any(|x| changed.contains(x)))
            })
            .map(|l| &l.att)
    }

    /// A false attestation of `kind` about any of these bodies, for a reason.
    fn negative_for(&self, kind: &str, units: &[&Node]) -> Option<&Attestation> {
        let bodies: HashSet<ObjectId> = units.iter().map(|u| u.body).collect();
        self.attests
            .iter()
            .find(|l| {
                l.att.kind == kind
                    && !truthy(&l.att.result)
                    && l.att
                        .bodies
                        .as_ref()
                        .is_some_and(|b| b.iter().any(|x| bodies.contains(x)))
            })
            .map(|l| &l.att)
    }
}

fn is_test_kind(k: &str) -> bool {
    k == "test" || k == "test.skipped"
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Map(m) => m.iter().any(|(k, v)| {
            matches!(k, Value::Text(t) if t == "ok") && matches!(v, Value::Bool(true))
        }),
        Value::Integer(_) | Value::Text(_) | Value::Array(_) => true,
        _ => false,
    }
}

fn names(units: &[&Node]) -> String {
    let mut v: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
    v.sort();
    v.dedup();
    v.join(", ")
}

fn check_clause(ctx: &Ctx<'_>, c: &Clause) -> Result<Option<Unmet>> {
    if let Some(u) = &c.unless {
        if holds(ctx, u)? == Holds::Yes {
            return Ok(None);
        }
    }
    let h = holds(ctx, &c.pred)?;
    let mut label = describe(&c.pred);
    // The escape hatch is part of what is unmet: a person reading the refusal
    // sees what would satisfy it, and the daemon knows to ask for it.
    if let Some(u) = &c.unless {
        label = format!("{label} unless {}", describe(u));
    }
    Ok(match (c.op.as_str(), h) {
        ("require", Holds::Yes) => None,
        ("require", Holds::No(reason)) => Some(Unmet {
            clause: format!("require {label}"),
            reason,
        }),
        ("forbid", Holds::No(_)) => None,
        ("forbid", Holds::Yes) => Some(Unmet {
            clause: format!("forbid {label}"),
            reason: forbidden_reason(ctx, &c.pred),
        }),
        (other, _) => Some(Unmet {
            clause: format!("{other} {label}"),
            reason: format!("unknown clause op {other}"),
        }),
    })
}

/// Why a forbidden structural condition holds, naming the units.
fn forbidden_reason(ctx: &Ctx<'_>, p: &Predicate) -> String {
    if p.kind == "structural" {
        if let Some(units) = structural_units(ctx, p.name.as_deref().unwrap_or("")) {
            if !units.is_empty() {
                return format!("{}: {}", p.name.as_deref().unwrap_or(""), names(&units));
            }
        }
    }
    "the forbidden condition holds".into()
}

#[derive(Debug, PartialEq, Eq)]
enum Holds {
    Yes,
    No(String),
}

/// A predicate as text: `kind(name, key=value, ...)`.
pub fn describe(p: &Predicate) -> String {
    let mut inner = Vec::new();
    if let Some(n) = &p.name {
        inner.push(n.clone());
    }
    if let Some(args) = &p.args {
        for (k, v) in args {
            if k == "preds" {
                inner.push(
                    sub_preds(p)
                        .iter()
                        .map(describe)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            } else {
                inner.push(format!("{k}={}", value_text(v)));
            }
        }
    }
    format!("{}({})", p.kind, inner.join(", "))
}

/// Why a negative attestation leaves a clause unmet, with what the agent
/// can do about it: the failed tests when the runner reported them, the exit
/// code when it did not, and where the run's output is.
fn negative_reason(name: &str, neg: &Attestation) -> String {
    let scope = |k: &str| neg.scope.as_ref().and_then(|s| s.get(k)).map(value_text);
    let failed = scope("failed").unwrap_or_default();
    let mut reason = format!("{name} is false on this snapshot");
    if failed.is_empty() || failed == "[]" {
        match scope("exit") {
            Some(code) => reason.push_str(&format!(
                "; the runner exited {code} and reported no per-test results"
            )),
            None => reason.push_str("; no per-test results were reported"),
        }
    } else {
        reason.push_str(&format!("; failed: {failed}"));
    }
    if let Some(ev) = neg.evidence {
        reason.push_str(&format!(
            "; its output: query --kind object --id {} --tail",
            ev.to_hex()
        ));
    }
    reason.push_str("; fix it and run verify");
    reason
}

fn value_text(v: &Value) -> String {
    match v {
        Value::Text(t) => t.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Integer(i) => {
            let n: i128 = (*i).into();
            n.to_string()
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(value_text).collect::<Vec<_>>().join(" ")
        ),
        other => format!("{other:?}"),
    }
}

/// Parse `kind(name, key=value, ...)` into a predicate. Values: `true`,
/// `false`, integers, or text. `all`, `any`, and `not` take predicates as
/// their arguments.
pub fn parse_predicate(text: &str) -> std::result::Result<Predicate, String> {
    let text = text.trim();
    let open = text
        .find('(')
        .ok_or_else(|| format!("expected kind(name, ...), got {text}"))?;
    if !text.ends_with(')') {
        return Err(format!("expected a closing parenthesis in {text}"));
    }
    let kind = text[..open].trim().to_string();
    let inner = &text[open + 1..text.len() - 1];
    if kind == "all" || kind == "any" || kind == "not" {
        let parts = split_top(inner);
        let preds: std::result::Result<Vec<Predicate>, String> =
            parts.iter().map(|s| parse_predicate(s)).collect();
        let arr = preds?
            .into_iter()
            .map(|p| {
                let mut m = vec![(Value::Text("kind".into()), Value::Text(p.kind))];
                if let Some(n) = p.name {
                    m.push((Value::Text("name".into()), Value::Text(n)));
                }
                if let Some(a) = p.args {
                    m.push((
                        Value::Text("args".into()),
                        Value::Map(a.into_iter().map(|(k, v)| (Value::Text(k), v)).collect()),
                    ));
                }
                Value::Map(m)
            })
            .collect();
        return Ok(Predicate {
            kind,
            name: None,
            args: Some(BTreeMap::from([("preds".to_string(), Value::Array(arr))])),
        });
    }
    let mut name = None;
    let mut args: BTreeMap<String, Value> = BTreeMap::new();
    for (i, part) in split_top(inner).iter().enumerate() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((k, v)) => {
                args.insert(k.trim().to_string(), parse_value(v.trim()));
            }
            None if i == 0 => name = Some(part.to_string()),
            None => return Err(format!("expected key=value, got {part}")),
        }
    }
    Ok(Predicate {
        kind,
        name,
        args: if args.is_empty() { None } else { Some(args) },
    })
}

fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn parse_value(v: &str) -> Value {
    match v {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => match v.parse::<i64>() {
            Ok(n) => Value::Integer(n.into()),
            Err(_) => Value::Text(v.to_string()),
        },
    }
}

/// The units a structural condition names, for reasons. `None` when the
/// condition is not unit-based or the indexes are missing.
fn structural_units<'a>(ctx: &Ctx<'a>, name: &str) -> Option<Vec<&'a Node>> {
    match name {
        "test.modified" => {
            let parent = ctx.parent_index?;
            let prev: HashSet<EntityId> = parent.nodes.iter().map(|n| n.nid).collect();
            Some(
                ctx.changed_units()?
                    .into_iter()
                    .filter(|n| is_test_kind(&n.kind) && prev.contains(&n.nid))
                    .collect(),
            )
        }
        "test.deleted" => Some(
            ctx.deleted_units()?
                .into_iter()
                .filter(|n| is_test_kind(&n.kind))
                .collect(),
        ),
        "test.weakened" => {
            let mut v = structural_units(ctx, "test.modified")?;
            v.extend(structural_units(ctx, "test.deleted")?);
            Some(v)
        }
        "test.skipped" => Some(
            ctx.changed_units()?
                .into_iter()
                .filter(|n| n.kind == "test.skipped")
                .collect(),
        ),
        _ => None,
    }
}

fn holds(ctx: &Ctx<'_>, p: &Predicate) -> Result<Holds> {
    Ok(match p.kind.as_str() {
        "structural" => match p.name.as_deref() {
            Some("flags.none") => match &ctx.revision.flags {
                None => Holds::Yes,
                Some(f) if f.is_empty() => Holds::Yes,
                Some(f) => {
                    let mut parts = Vec::new();
                    if let Some(x) = &f.out_of_scope {
                        parts.push(format!("out_of_scope {:?}", x));
                    }
                    if let Some(x) = &f.secrets {
                        parts.push(format!(
                            "secrets in {:?}",
                            x.iter().map(|m| &m.path).collect::<Vec<_>>()
                        ));
                    }
                    if let Some(x) = &f.case_collisions {
                        parts.push(format!("case_collisions {:?}", x));
                    }
                    if let Some(x) = &f.unportable_names {
                        parts.push(format!("unportable_names {:?}", x));
                    }
                    if let Some(x) = &f.generated_drift {
                        parts.push(format!("generated_drift {:?}", x));
                    }
                    Holds::No(format!("revision carries flags: {}", parts.join("; ")))
                }
            },
            Some("intent.linked") => {
                if ctx.revision.intent.is_some() {
                    Holds::Yes
                } else {
                    Holds::No("revision has no intent".into())
                }
            }
            Some(n @ ("test.modified" | "test.deleted" | "test.weakened" | "test.skipped")) => {
                match structural_units(ctx, n) {
                    None => Holds::No("no semantic index for this revision or its parent".into()),
                    Some(units) if units.is_empty() => Holds::No(format!("no {n} units")),
                    Some(_) => Holds::Yes,
                }
            }
            Some("changed.covered") => {
                let Some(idx) = ctx.index else {
                    return Ok(Holds::No("no semantic index for this revision".into()));
                };
                let Some(changed) = ctx.changed_units() else {
                    return Ok(Holds::No(
                        "no semantic index for the parent revision".into(),
                    ));
                };
                let mut covered: HashSet<EntityId> = HashSet::new();
                for t in idx.nodes.iter().filter(|n| n.kind == "test") {
                    if let Some(d) = &t.deps {
                        covered.extend(d.iter().copied());
                    }
                }
                let uncovered: Vec<&Node> = changed
                    .into_iter()
                    .filter(|n| !UNCOVERABLE_KINDS.contains(&n.kind.as_str()) && is_ident(&n.name))
                    .filter(|n| !covered.contains(&n.nid))
                    .collect();
                if uncovered.is_empty() {
                    Holds::Yes
                } else {
                    Holds::No(format!(
                        "changed units with no covering test: {}; add a test that calls each",
                        names(&uncovered)
                    ))
                }
            }
            Some(other) => Holds::No(format!("structural check {other} is not available yet")),
            None => Holds::No("structural predicate without a name".into()),
        },
        "attest" => {
            let name = p.name.as_deref().unwrap_or("");
            let arg_text = |k: &str| {
                p.args
                    .as_ref()
                    .and_then(|a| a.get(k))
                    .and_then(|v| v.as_text().map(str::to_string))
            };
            let allow_self = p
                .args
                .as_ref()
                .and_then(|a| a.get("self"))
                .is_some_and(|v| matches!(v, Value::Bool(true)));
            match arg_text("for").as_deref() {
                None => {
                    if ctx.subject_attested(name, allow_self) {
                        Holds::Yes
                    } else {
                        // A node-scoped run counts when it covers every changed unit.
                        let by_bodies = match ctx.changed_units() {
                            Some(changed) if !changed.is_empty() => {
                                let coverable: Vec<&Node> = changed
                                    .into_iter()
                                    .filter(|n| {
                                        !UNCOVERABLE_KINDS.contains(&n.kind.as_str())
                                            || is_test_kind(&n.kind)
                                    })
                                    .collect();
                                !coverable.is_empty()
                                    && ctx
                                        .uncovered_by_bodies(name, &coverable, allow_self)
                                        .is_empty()
                            }
                            _ => false,
                        };
                        if by_bodies {
                            Holds::Yes
                        } else if let Some(neg) = ctx.negative_on_revision(name) {
                            Holds::No(negative_reason(name, neg))
                        } else {
                            Holds::No(format!(
                                "no {name} attestation on this snapshot; run verify"
                            ))
                        }
                    }
                }
                Some("new_tests") => match ctx.new_tests() {
                    None => Holds::No("no semantic index for this revision or its parent".into()),
                    Some(tests) if tests.is_empty() => Holds::Yes,
                    Some(tests) => {
                        let missing = ctx.uncovered_by_bodies(name, &tests, allow_self);
                        if missing.is_empty() {
                            Holds::Yes
                        } else if let Some(neg) = ctx.negative_for(name, &missing) {
                            let detail = neg
                                .scope
                                .as_ref()
                                .and_then(|s| s.get("passed_on_parent"))
                                .map(value_text)
                                .unwrap_or_default();
                            Holds::No(format!(
                                "new tests do not prove the change, they pass on the parent too: {detail}"
                            ))
                        } else {
                            Holds::No(format!(
                                "new tests without a {name} proof: {}; run verify",
                                names(&missing)
                            ))
                        }
                    }
                },
                Some("changed_nodes") => match ctx.changed_units() {
                    None => Holds::No("no semantic index for this revision or its parent".into()),
                    Some(changed) => {
                        let coverable: Vec<&Node> = changed
                            .into_iter()
                            .filter(|n| !UNCOVERABLE_KINDS.contains(&n.kind.as_str()))
                            .collect();
                        let missing = ctx.uncovered_by_bodies(name, &coverable, allow_self);
                        if missing.is_empty() {
                            Holds::Yes
                        } else {
                            Holds::No(format!(
                                "changed units without a {name} attestation: {}",
                                names(&missing)
                            ))
                        }
                    }
                },
                Some(other) => Holds::No(format!("unknown attest scope for={other}")),
            }
        }
        "all" | "any" | "not" => {
            let preds = sub_preds(p);
            let results: Vec<Holds> = preds.iter().map(|q| holds(ctx, q)).collect::<Result<_>>()?;
            match p.kind.as_str() {
                "all" => results
                    .into_iter()
                    .find(|r| matches!(r, Holds::No(_)))
                    .unwrap_or(Holds::Yes),
                "any" => {
                    if results.contains(&Holds::Yes) {
                        Holds::Yes
                    } else {
                        Holds::No("none of the alternatives hold".into())
                    }
                }
                _ => match results.first() {
                    Some(Holds::Yes) => Holds::No("negated predicate holds".into()),
                    Some(Holds::No(_)) => Holds::Yes,
                    None => Holds::No("not without a predicate".into()),
                },
            }
        }
        "approved" => {
            // A human's approval: an approval.human attestation on the
            // revision whose signer is a human principal. Key-signed means
            // no runner; a channel-attested one carries the channel as runner.
            let by = p.name.as_deref().unwrap_or("human");
            let keysigned = p
                .args
                .as_ref()
                .and_then(|a| a.get("keysigned"))
                .is_some_and(|v| matches!(v, Value::Bool(true)));
            if by != "human" {
                return Ok(Holds::No(format!(
                    "approved by {by} is not available yet; by=human is"
                )));
            }
            let found = ctx.attests.iter().any(|l| {
                l.att.kind == "approval.human"
                    && truthy(&l.att.result)
                    && l.signer_kind.as_deref() == Some("human")
                    && (!keysigned || l.att.runner.is_none())
                    && l.att
                        .subject
                        .as_ref()
                        .is_some_and(|s| self_subjects(&ctx.subjects, s))
            });
            if found {
                Holds::Yes
            } else if ctx
                .attests
                .iter()
                .any(|l| l.att.kind == "approval.human" && !truthy(&l.att.result))
            {
                Holds::No("a human declined this change".into())
            } else {
                Holds::No("needs a human's approval through a channel".into())
            }
        }
        "judge" => {
            // Judges are sessions of agents other than the author's, each
            // attesting judge.<rubric> with a verdict, a confidence, and its
            // reasoning. One negative verdict is an exception for a human.
            let rubric = p.name.as_deref().unwrap_or("");
            let kind = format!("judge.{rubric}");
            let arg_u = |k: &str, d: i64| -> i64 {
                p.args
                    .as_ref()
                    .and_then(|a| a.get(k))
                    .and_then(|v| match v {
                        Value::Integer(i) => Some(i128::from(*i) as i64),
                        Value::Text(t) => t.parse().ok(),
                        _ => None,
                    })
                    .unwrap_or(d)
            };
            let needed = arg_u("judges", 1).max(1) as usize;
            let min_conf = arg_u("min_confidence", 0);
            let distinct = p
                .args
                .as_ref()
                .and_then(|a| a.get("distinct_models"))
                .is_some_and(|v| matches!(v, Value::Bool(true)));
            let mut yes: Vec<(EntityId, Option<String>, String)> = Vec::new();
            let mut no: Vec<String> = Vec::new();
            let mut low: Vec<String> = Vec::new();
            let mut own: usize = 0;
            for l in self_judges(ctx, &kind) {
                if l.signer_parent.is_some() && l.signer_parent == ctx.author_parent {
                    own += 1;
                    continue;
                }
                let name = l.signer_name.clone().unwrap_or_else(|| "a judge".into());
                let (conf, reasoning) = judge_result(&l.att.result);
                let who = format!(
                    "{name}{}",
                    l.signer_model
                        .as_ref()
                        .map(|m| format!(" ({m})"))
                        .unwrap_or_default()
                );
                if conf < min_conf {
                    low.push(format!("{who}: confidence {conf}"));
                } else if truthy(&l.att.result) {
                    let key = l.signer_parent.unwrap_or(l.att.verifier);
                    if !yes.iter().any(|(k, _, _)| *k == key) {
                        yes.push((key, l.signer_model.clone(), who));
                    }
                } else {
                    no.push(format!("{who}: {reasoning}"));
                }
            }
            if !no.is_empty() {
                Holds::No(format!("a judge said no on {rubric}: {}", no.join(" | ")))
            } else if yes.len() < needed {
                let mut why = format!("needs {needed} judges on {rubric}, has {}", yes.len());
                if !yes.is_empty() {
                    why.push_str(&format!(
                        " ({})",
                        yes.iter()
                            .map(|(_, _, w)| w.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if !low.is_empty() {
                    why.push_str(&format!(
                        "; below min_confidence {min_conf}: {}",
                        low.join(", ")
                    ));
                }
                if own > 0 {
                    why.push_str(&format!("; {own} from the author's own agent do not count"));
                }
                Holds::No(why)
            } else if distinct {
                let mut models: Vec<&Option<String>> = yes.iter().map(|(_, m, _)| m).collect();
                models.sort();
                models.dedup();
                if models.len() < needed.min(yes.len()) {
                    Holds::No(format!(
                        "judges on {rubric} must use distinct models; got {}",
                        yes.iter()
                            .map(|(_, m, _)| m.clone().unwrap_or_else(|| "unknown".into()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                } else {
                    Holds::Yes
                }
            } else {
                Holds::Yes
            }
        }
        "observe" => {
            // The latest trusted observe.<signal> attestation on the revision,
            // in permille, against max and min bounds.
            let signal = p.name.as_deref().unwrap_or("");
            let kind = format!("observe.{signal}");
            let bound = |k: &str| -> Option<i64> {
                p.args
                    .as_ref()
                    .and_then(|a| a.get(k))
                    .and_then(|v| match v {
                        Value::Integer(i) => Some(i128::from(*i) as i64),
                        Value::Text(t) => t.parse().ok(),
                        _ => None,
                    })
            };
            let latest = ctx
                .attests
                .iter()
                .filter(|l| {
                    l.att.kind == kind
                        && l.att
                            .subject
                            .as_ref()
                            .is_some_and(|s| self_subjects(&ctx.subjects, s))
                        && self_trusted(ctx, l)
                })
                .max_by_key(|l| l.att.time);
            match latest {
                None => Holds::No(format!(
                    "no observe.{signal} attestation on this revision; observe the target first"
                )),
                Some(l) => {
                    let value = match &l.att.result {
                        Value::Integer(i) => i128::from(*i) as i64,
                        Value::Float(f) => (*f * 1000.0) as i64,
                        _ => 0,
                    };
                    if bound("max").is_some_and(|m| value > m) {
                        Holds::No(format!(
                            "{signal} is {value} permille, above {}",
                            bound("max").unwrap_or(0)
                        ))
                    } else if bound("min").is_some_and(|m| value < m) {
                        Holds::No(format!(
                            "{signal} is {value} permille, below {}",
                            bound("min").unwrap_or(0)
                        ))
                    } else {
                        Holds::Yes
                    }
                }
            }
        }
        "reputation" => Holds::No(format!("{} clauses are evaluated from M5 onward", p.kind)),
        other => Holds::No(format!("unknown predicate kind {other}")),
    })
}

fn self_subjects(subjects: &[Vec<u8>], s: &[u8]) -> bool {
    subjects.iter().any(|x| x == s)
}

fn self_trusted(ctx: &Ctx<'_>, l: &Loaded) -> bool {
    ctx.trusted(l, false)
}

/// The judge attestations of one kind on this revision.
fn self_judges<'a>(ctx: &'a Ctx<'a>, kind: &str) -> Vec<&'a Loaded> {
    ctx.attests
        .iter()
        .filter(|l| l.att.kind == kind)
        .filter(|l| {
            l.att
                .subject
                .as_ref()
                .is_some_and(|s| self_subjects(&ctx.subjects, s))
        })
        .collect()
}

/// A judge's confidence in permille and its reasoning, from the result map.
pub fn judge_result(v: &Value) -> (i64, String) {
    let mut conf = 1000;
    let mut reasoning = String::new();
    if let Value::Map(m) = v {
        for (k, val) in m {
            match (k, val) {
                (Value::Text(t), Value::Integer(i)) if t == "confidence" => {
                    conf = i128::from(*i) as i64
                }
                (Value::Text(t), Value::Float(f)) if t == "confidence" => {
                    conf = (*f * 1000.0) as i64
                }
                (Value::Text(t), Value::Text(r)) if t == "reasoning" => reasoning = r.clone(),
                _ => {}
            }
        }
    }
    (conf, reasoning)
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_negative_attestation_says_what_to_read() {
        let evidence = ObjectId::from_slice(&[7u8; 32]).unwrap();
        let mut scope = BTreeMap::new();
        scope.insert("failed".to_string(), Value::Array(vec![]));
        scope.insert("exit".to_string(), Value::Integer(1.into()));
        let mut att = Attestation {
            kind: "tests.pass".into(),
            subject: None,
            bodies: None,
            subject_type: None,
            scope: Some(scope),
            result: Value::Bool(false),
            env: None,
            verifier: EntityId::random(),
            runner: None,
            evidence: Some(evidence),
            time: 0,
            sigkind: "ed25519".into(),
            sig: None,
        };
        let reason = negative_reason("tests.pass", &att);
        assert!(reason.starts_with("tests.pass is false on this snapshot; the runner exited 1 and reported no per-test results"), "{reason}");
        assert!(
            reason.contains(&format!(
                "query --kind object --id {} --tail",
                evidence.to_hex()
            )),
            "{reason}"
        );
        assert!(reason.ends_with("fix it and run verify"), "{reason}");
        att.scope.as_mut().unwrap().insert(
            "failed".to_string(),
            Value::Array(vec![Value::Text("tests::sub_fails".into())]),
        );
        att.evidence = None;
        let reason = negative_reason("tests.pass", &att);
        assert!(reason.contains("failed: [tests::sub_fails]"), "{reason}");
        assert!(
            !reason.contains("exited") && !reason.contains("query"),
            "{reason}"
        );
    }

    #[test]
    fn predicates_round_trip_through_text() {
        let p = parse_predicate("attest(tests.fail_on_parent, for=new_tests)").unwrap();
        assert_eq!(p.kind, "attest");
        assert_eq!(p.name.as_deref(), Some("tests.fail_on_parent"));
        assert_eq!(describe(&p), "attest(tests.fail_on_parent, for=new_tests)");
        let s = parse_predicate("structural(test.weakened)").unwrap();
        assert_eq!(describe(&s), "structural(test.weakened)");
        let a = parse_predicate("any(attest(tests.pass), structural(flags.none))").unwrap();
        assert_eq!(a.kind, "any");
        assert_eq!(sub_preds(&a).len(), 2);
        assert!(parse_predicate("nonsense").is_err());
    }

    #[test]
    fn required_attestations_walk_the_chain() {
        let mut s = default_trunk_standard(EntityId::random(), "trunk");
        for text in [
            "attest(tests.pass)",
            "attest(tests.fail_on_parent, for=new_tests)",
            "any(attest(lint.clean), attest(tests.pass))",
        ] {
            s.clauses.push(Clause {
                op: "require".into(),
                pred: parse_predicate(text).unwrap(),
                unless: None,
            });
        }
        let req = required_attestations(std::slice::from_ref(&s));
        assert_eq!(
            req,
            vec![
                ("tests.pass".to_string(), None),
                (
                    "tests.fail_on_parent".to_string(),
                    Some("new_tests".to_string())
                ),
                ("lint.clean".to_string(), None),
            ]
        );
    }
}
