//! The view: mutable repository state at one op. Effect application and
//! three-way merge, per `spec/03-operations.md`.

use std::collections::{BTreeMap, BTreeSet};

use ciborium::value::Value;
use tessra_core::object::{Effect, LineState, PauseState, View};
use tessra_core::store::ObjectStore;
use tessra_core::trie::{MergePolicy, Trie, TrieValue};
use tessra_core::{EntityId, ObjectId};

use crate::{Error, Result};

/// A resolved pointer value from a view map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pointer {
    Id(ObjectId),
    Conflict(Vec<ObjectId>),
}

impl Pointer {
    pub fn to_value(&self) -> Value {
        match self {
            Pointer::Id(id) => Value::Bytes(id.0.to_vec()),
            Pointer::Conflict(v) => Value::Map(vec![(
                Value::Text("conflict".into()),
                Value::Array(v.iter().map(|i| Value::Bytes(i.0.to_vec())).collect()),
            )]),
        }
    }

    pub fn from_value(v: &Value) -> Option<Pointer> {
        match v {
            Value::Bytes(b) => ObjectId::from_slice(b).ok().map(Pointer::Id),
            Value::Map(m) if m.len() == 1 => match &m[0] {
                (Value::Text(k), Value::Array(items)) if k == "conflict" => {
                    let mut ids = Vec::new();
                    for it in items {
                        if let Value::Bytes(b) = it {
                            ids.push(ObjectId::from_slice(b).ok()?);
                        } else {
                            return None;
                        }
                    }
                    Some(Pointer::Conflict(ids))
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub fn from_trie(v: &TrieValue) -> Option<Pointer> {
        match v {
            TrieValue::Id(i) => Some(Pointer::Id(*i)),
            TrieValue::Conflict(c) => Some(Pointer::Conflict(c.clone())),
            TrieValue::Set => None,
        }
    }

    pub fn to_trie(&self) -> TrieValue {
        match self {
            Pointer::Id(i) => TrieValue::Id(*i),
            Pointer::Conflict(c) => TrieValue::Conflict(c.clone()),
        }
    }

    pub fn ids(&self) -> Vec<ObjectId> {
        match self {
            Pointer::Id(i) => vec![*i],
            Pointer::Conflict(c) => c.clone(),
        }
    }

    /// The single ID, or an error if this is a conflict.
    pub fn single(&self) -> Result<ObjectId> {
        match self {
            Pointer::Id(i) => Ok(*i),
            Pointer::Conflict(c) => Err(Error::Other(format!("pointer is in conflict: {c:?}"))),
        }
    }

    fn union(a: &Pointer, b: &Pointer) -> Pointer {
        let s: BTreeSet<ObjectId> = a.ids().into_iter().chain(b.ids()).collect();
        let v: Vec<ObjectId> = s.into_iter().collect();
        if v.len() == 1 {
            Pointer::Id(v[0])
        } else {
            Pointer::Conflict(v)
        }
    }
}

/// Operations over views in a store.
pub struct ViewState<'a, S: ObjectStore> {
    store: &'a S,
}

impl<'a, S: ObjectStore> ViewState<'a, S> {
    pub fn new(store: &'a S) -> Self {
        ViewState { store }
    }

    fn trie(&self) -> Trie<'a, S> {
        Trie::new(self.store)
    }

    /// The view before init: every map empty.
    pub fn empty(&self) -> Result<View> {
        let e = self.trie().empty_root()?;
        Ok(View {
            entities: e,
            proposed: e,
            claims: e,
            caps: e,
            revoked: e,
            tombstoned: e,
            lines: BTreeMap::new(),
            roots: Vec::new(),
            owners: Vec::new(),
            deployed: BTreeMap::new(),
            paused: None,
        })
    }

    pub fn store_view(&self, view: &View) -> Result<ObjectId> {
        Ok(self.store.put(view)?)
    }

    pub fn load_view(&self, id: &ObjectId) -> Result<View> {
        Ok(self.store.get(id)?)
    }

    /// Current pointer for an entity.
    pub fn entity(&self, view: &View, id: &EntityId) -> Result<Option<Pointer>> {
        Ok(self
            .trie()
            .get(&view.entities, id.as_bytes())?
            .and_then(|v| Pointer::from_trie(&v)))
    }

    pub fn claim(&self, view: &View, id: &EntityId) -> Result<Option<Pointer>> {
        Ok(self
            .trie()
            .get(&view.claims, id.as_bytes())?
            .and_then(|v| Pointer::from_trie(&v)))
    }

    pub fn in_set(&self, root: &ObjectId, id: &ObjectId) -> Result<bool> {
        Ok(self.trie().get(root, id.as_bytes())?.is_some())
    }

    pub fn principal_revoked(&self, view: &View, id: &EntityId) -> Result<bool> {
        Ok(self.trie().get(&view.revoked, id.as_bytes())?.is_some())
    }

    pub fn entities(&self, view: &View) -> Result<Vec<(EntityId, Pointer)>> {
        let mut out = Vec::new();
        for (k, v) in self.trie().entries(&view.entities)? {
            if let (Ok(id), Some(p)) = (EntityId::from_slice(&k), Pointer::from_trie(&v)) {
                out.push((id, p));
            }
        }
        Ok(out)
    }

    pub fn set_members(&self, root: &ObjectId) -> Result<Vec<ObjectId>> {
        Ok(self
            .trie()
            .entries(root)?
            .into_iter()
            .filter_map(|(k, _)| ObjectId::from_slice(&k).ok())
            .collect())
    }

    /// Apply effects to a view, producing the next view. Does not validate
    /// `from` fields; verification does that first.
    pub fn apply(
        &self,
        view: &View,
        effects: &[Effect],
        parent_op: Option<ObjectId>,
    ) -> Result<View> {
        let t = self.trie();
        let mut v = view.clone();
        for e in effects {
            match e {
                Effect::Put { .. } => {}
                Effect::Point { entity, to, .. } => {
                    v.entities = t.insert(&v.entities, entity.as_bytes(), TrieValue::Id(*to))?;
                }
                Effect::Unpoint { entity } => {
                    v.entities = t.remove(&v.entities, entity.as_bytes())?;
                }
                Effect::Head {
                    line, to, seq, id, ..
                } => {
                    let head = Pointer::Id(*to).to_value();
                    match v.lines.get_mut(line) {
                        Some(ls) => {
                            ls.head = head;
                            ls.seq = *seq;
                        }
                        None => {
                            let lid = id.ok_or_else(|| {
                                Error::Other(format!("head on unknown line {line} without id"))
                            })?;
                            v.lines.insert(
                                line.clone(),
                                LineState {
                                    id: lid,
                                    head,
                                    seq: *seq,
                                    coordinator: v.owners.first().copied().unwrap_or(lid),
                                },
                            );
                        }
                    }
                }
                Effect::Propose { rev } => {
                    v.proposed = t.insert(&v.proposed, rev.as_bytes(), TrieValue::Set)?;
                }
                Effect::Unpropose { rev } => {
                    v.proposed = t.remove(&v.proposed, rev.as_bytes())?;
                }
                Effect::Claim { claim, to } => {
                    v.claims = t.insert(&v.claims, claim.as_bytes(), TrieValue::Id(*to))?;
                }
                Effect::Unclaim { claim } => {
                    v.claims = t.remove(&v.claims, claim.as_bytes())?;
                }
                Effect::Cap { cap } => {
                    v.caps = t.insert(&v.caps, cap.as_bytes(), TrieValue::Set)?;
                }
                Effect::Uncap { cap } => {
                    v.caps = t.remove(&v.caps, cap.as_bytes())?;
                }
                Effect::Revoke { principal } => {
                    v.revoked = t.insert(&v.revoked, principal.as_bytes(), TrieValue::Set)?;
                }
                Effect::Tomb { of, .. } => {
                    v.tombstoned = t.insert(&v.tombstoned, of.as_bytes(), TrieValue::Set)?;
                }
                Effect::Deploy { target, to } => {
                    v.deployed.insert(*target, *to);
                }
                Effect::Pause { scope } => {
                    let by = v.owners.first().copied().unwrap_or(EntityId([0; 16]));
                    let ps = PauseState {
                        scope: scope.clone(),
                        by,
                        op: parent_op.unwrap_or(ObjectId([0; 32])),
                    };
                    let list = v.paused.get_or_insert_with(Vec::new);
                    if !list.iter().any(|p| p.scope == ps.scope) {
                        list.push(ps);
                    }
                }
                Effect::Resume { scope } => {
                    if let Some(list) = v.paused.as_mut() {
                        list.retain(|p| &p.scope != scope);
                        if list.is_empty() {
                            v.paused = None;
                        }
                    }
                }
                Effect::Owner { principal } => {
                    if !v.owners.contains(principal) {
                        v.owners.push(*principal);
                        v.owners.sort();
                    }
                }
                Effect::Unowner { principal } => {
                    v.owners.retain(|p| p != principal);
                }
                Effect::Root { name } => {
                    if !v.roots.contains(name) {
                        v.roots.push(name.clone());
                        v.roots.sort();
                    }
                }
                Effect::Unroot { name } => {
                    v.roots.retain(|r| r != name);
                }
                Effect::Coordinator { line, to } => {
                    if let Some(ls) = v.lines.get_mut(line) {
                        ls.coordinator = *to;
                    }
                }
            }
        }
        Ok(v)
    }

    /// Three-way merge of two views against a base.
    pub fn merge(&self, base: &View, a: &View, b: &View) -> Result<View> {
        let t = self.trie();
        let entities = t.merge3(
            &base.entities,
            &a.entities,
            &b.entities,
            MergePolicy::Pointers,
        )?;
        let claims = t.merge3(&base.claims, &a.claims, &b.claims, MergePolicy::Pointers)?;
        let proposed = t.merge3(&base.proposed, &a.proposed, &b.proposed, MergePolicy::Set)?;
        let caps = t.merge3(&base.caps, &a.caps, &b.caps, MergePolicy::Set)?;
        let revoked = self.union_sets(&[base.revoked, a.revoked, b.revoked])?;
        let tombstoned = self.union_sets(&[base.tombstoned, a.tombstoned, b.tombstoned])?;

        let mut lines = BTreeMap::new();
        let names: BTreeSet<&String> = base
            .lines
            .keys()
            .chain(a.lines.keys())
            .chain(b.lines.keys())
            .collect();
        for name in names {
            let lb = base.lines.get(name);
            let la = a.lines.get(name);
            let lb2 = b.lines.get(name);
            let merged = match (lb, la, lb2) {
                (_, Some(x), Some(y)) if x == y => x.clone(),
                (Some(bs), Some(x), Some(y)) => {
                    if x == bs {
                        y.clone()
                    } else if y == bs {
                        x.clone()
                    } else {
                        merge_line(bs, x, y)
                    }
                }
                (None, Some(x), None) | (None, None, Some(x)) => x.clone(),
                (None, Some(x), Some(y)) => merge_line(x, x, y),
                (Some(_), None, Some(x)) | (Some(_), Some(x), None) => x.clone(),
                (Some(_), None, None) | (None, None, None) => continue,
            };
            lines.insert(name.clone(), merged);
        }

        let roots = merge_set_vec(&base.roots, &a.roots, &b.roots);
        let owners = merge_set_vec(&base.owners, &a.owners, &b.owners);

        let mut deployed = BTreeMap::new();
        let keys: BTreeSet<&EntityId> = base
            .deployed
            .keys()
            .chain(a.deployed.keys())
            .chain(b.deployed.keys())
            .collect();
        for k in keys {
            let (vb, va, vb2) = (base.deployed.get(k), a.deployed.get(k), b.deployed.get(k));
            let out = if va == vb {
                vb2.copied()
            } else if vb2 == vb || va == vb2 {
                va.copied()
            } else {
                // Both changed differently: lower ID wins, deterministic. Rare and M6 work.
                match (va, vb2) {
                    (Some(x), Some(y)) => Some(*x.min(y)),
                    (Some(x), None) | (None, Some(x)) => Some(*x),
                    (None, None) => None,
                }
            };
            if let Some(v) = out {
                deployed.insert(*k, v);
            }
        }

        let mut paused: Vec<PauseState> = Vec::new();
        for p in a.paused.iter().flatten().chain(b.paused.iter().flatten()) {
            if !paused.iter().any(|q| q.scope == p.scope) {
                paused.push(p.clone());
            }
        }
        paused.sort_by(|x, y| x.scope.cmp(&y.scope));

        Ok(View {
            entities,
            proposed,
            claims,
            caps,
            revoked,
            tombstoned,
            lines,
            roots,
            owners,
            deployed,
            paused: if paused.is_empty() {
                None
            } else {
                Some(paused)
            },
        })
    }

    fn union_sets(&self, roots: &[ObjectId]) -> Result<ObjectId> {
        let t = self.trie();
        let mut all: BTreeSet<Vec<u8>> = BTreeSet::new();
        for r in roots {
            for (k, _) in t.entries(r)? {
                all.insert(k);
            }
        }
        let mut out = t.empty_root()?;
        for k in all {
            out = t.insert(&out, &k, TrieValue::Set)?;
        }
        Ok(out)
    }
}

fn merge_line(base: &LineState, a: &LineState, b: &LineState) -> LineState {
    let head = if a.head == base.head {
        b.head.clone()
    } else if b.head == base.head || a.head == b.head {
        a.head.clone()
    } else {
        match (Pointer::from_value(&a.head), Pointer::from_value(&b.head)) {
            (Some(x), Some(y)) => Pointer::union(&x, &y).to_value(),
            _ => a.head.clone(),
        }
    };
    let coordinator = if a.coordinator == base.coordinator {
        b.coordinator
    } else if b.coordinator == base.coordinator || a.coordinator == b.coordinator {
        a.coordinator
    } else {
        a.coordinator.min(b.coordinator)
    };
    LineState {
        id: a.id,
        head,
        seq: a.seq.max(b.seq),
        coordinator,
    }
}

fn merge_set_vec<T: Clone + Ord>(base: &[T], a: &[T], b: &[T]) -> Vec<T> {
    let all: BTreeSet<&T> = base.iter().chain(a).chain(b).collect();
    let mut out = Vec::new();
    for x in all {
        let in_b = base.contains(x);
        let in_a = a.contains(x);
        let in_b2 = b.contains(x);
        let present = (in_a && in_b2) || (in_a && !in_b) || (in_b2 && !in_b);
        if present {
            out.push(x.clone());
        }
    }
    out
}

/// Read a line's head as a pointer.
pub fn line_head(ls: &LineState) -> Option<Pointer> {
    Pointer::from_value(&ls.head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_core::store::MemoryStore;

    fn oid(n: u8) -> ObjectId {
        ObjectId([n; 32])
    }
    fn eid(n: u8) -> EntityId {
        EntityId([n; 16])
    }

    #[test]
    fn apply_and_merge_entities_and_sets() {
        let s = MemoryStore::new();
        let vs = ViewState::new(&s);
        let base = vs.empty().unwrap();
        let base = vs
            .apply(
                &base,
                &[
                    Effect::Owner { principal: eid(1) },
                    Effect::Root { name: "".into() },
                    Effect::Point {
                        entity: eid(2),
                        to: oid(2),
                        from: None,
                    },
                    Effect::Point {
                        entity: eid(3),
                        to: oid(3),
                        from: None,
                    },
                ],
                None,
            )
            .unwrap();
        // a rewrites entity 2 and proposes rev 10; b rewrites entity 3 and revokes 9.
        let a = vs
            .apply(
                &base,
                &[
                    Effect::Point {
                        entity: eid(2),
                        to: oid(22),
                        from: Some(Pointer::Id(oid(2)).to_value()),
                    },
                    Effect::Propose { rev: oid(10) },
                ],
                None,
            )
            .unwrap();
        let b = vs
            .apply(
                &base,
                &[
                    Effect::Point {
                        entity: eid(3),
                        to: oid(33),
                        from: Some(Pointer::Id(oid(3)).to_value()),
                    },
                    Effect::Revoke { principal: eid(9) },
                    Effect::Point {
                        entity: eid(2),
                        to: oid(23),
                        from: Some(Pointer::Id(oid(2)).to_value()),
                    },
                ],
                None,
            )
            .unwrap();
        let m = vs.merge(&base, &a, &b).unwrap();
        assert_eq!(vs.entity(&m, &eid(3)).unwrap(), Some(Pointer::Id(oid(33))));
        assert_eq!(
            vs.entity(&m, &eid(2)).unwrap(),
            Some(Pointer::Conflict(vec![oid(22), oid(23)]))
        );
        assert!(vs.in_set(&m.proposed, &oid(10)).unwrap());
        assert!(vs.principal_revoked(&m, &eid(9)).unwrap());
        assert_eq!(m.owners, vec![eid(1)]);
        assert_eq!(m.roots, vec!["".to_string()]);

        // Resolving the conflict by pointing from the conflict value.
        let resolved = vs
            .apply(
                &m,
                &[Effect::Point {
                    entity: eid(2),
                    to: oid(24),
                    from: Some(Pointer::Conflict(vec![oid(22), oid(23)]).to_value()),
                }],
                None,
            )
            .unwrap();
        assert_eq!(
            vs.entity(&resolved, &eid(2)).unwrap(),
            Some(Pointer::Id(oid(24)))
        );

        // Views are content-addressed: same effects, same ID.
        let id1 = vs.store_view(&m).unwrap();
        let m2 = vs.merge(&base, &b, &a).unwrap();
        let id2 = vs.store_view(&m2).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn lines_merge_heads_and_seq() {
        let s = MemoryStore::new();
        let vs = ViewState::new(&s);
        let base = vs.empty().unwrap();
        let base = vs
            .apply(
                &base,
                &[
                    Effect::Owner { principal: eid(1) },
                    Effect::Head {
                        line: "trunk".into(),
                        to: oid(1),
                        from: None,
                        seq: 0,
                        attests: vec![],
                        id: Some(eid(7)),
                    },
                ],
                None,
            )
            .unwrap();
        assert_eq!(base.lines["trunk"].coordinator, eid(1));
        let a = vs
            .apply(
                &base,
                &[Effect::Head {
                    line: "trunk".into(),
                    to: oid(2),
                    from: Some(Pointer::Id(oid(1)).to_value()),
                    seq: 1,
                    attests: vec![],
                    id: None,
                }],
                None,
            )
            .unwrap();
        let b = vs
            .apply(
                &base,
                &[Effect::Head {
                    line: "trunk".into(),
                    to: oid(3),
                    from: Some(Pointer::Id(oid(1)).to_value()),
                    seq: 1,
                    attests: vec![],
                    id: None,
                }],
                None,
            )
            .unwrap();
        let m = vs.merge(&base, &a, &b).unwrap();
        assert_eq!(
            line_head(&m.lines["trunk"]),
            Some(Pointer::Conflict(vec![oid(2), oid(3)]))
        );
        assert_eq!(m.lines["trunk"].seq, 1);
        let m2 = vs.merge(&base, &a, &base).unwrap();
        assert_eq!(line_head(&m2.lines["trunk"]), Some(Pointer::Id(oid(2))));
    }

    #[test]
    fn removed_owner_stays_removed_when_other_side_unchanged() {
        let s = MemoryStore::new();
        let vs = ViewState::new(&s);
        let base = vs
            .apply(
                &vs.empty().unwrap(),
                &[
                    Effect::Owner { principal: eid(1) },
                    Effect::Owner { principal: eid(2) },
                ],
                None,
            )
            .unwrap();
        let a = vs
            .apply(&base, &[Effect::Unowner { principal: eid(2) }], None)
            .unwrap();
        let m = vs.merge(&base, &a, &base).unwrap();
        assert_eq!(m.owners, vec![eid(1)]);
        let b = vs
            .apply(&base, &[Effect::Owner { principal: eid(3) }], None)
            .unwrap();
        let m = vs.merge(&base, &a, &b).unwrap();
        assert_eq!(m.owners, vec![eid(1), eid(3)]);
    }
}
