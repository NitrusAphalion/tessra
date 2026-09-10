//! The op DAG: heads, ancestry, latest common ancestors, merged views,
//! acceptance, and the idempotency index.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use tessra_core::object::{Op, View};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};

use crate::verify;
use crate::view::ViewState;
use crate::{Error, Result};

/// An operation log over a store.
pub struct OpLog<S: ObjectStore> {
    store: S,
    heads: BTreeSet<ObjectId>,
    verified: HashSet<ObjectId>,
    views: HashMap<ObjectId, ObjectId>,
    idem: HashMap<(EntityId, [u8; 16]), ObjectId>,
}

impl<S: ObjectStore> OpLog<S> {
    /// A log with no ops. Call `build::init` next.
    pub fn new(store: S) -> Self {
        OpLog {
            store,
            heads: BTreeSet::new(),
            verified: HashSet::new(),
            views: HashMap::new(),
            idem: HashMap::new(),
        }
    }

    /// Open a log whose heads are known and whose ops were verified when stored.
    pub fn open(store: S, heads: Vec<ObjectId>) -> Result<Self> {
        let mut log = OpLog::new(store);
        let mut queue: VecDeque<ObjectId> = heads.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            if log.verified.contains(&id) {
                continue;
            }
            let op: Op = log.store.get(&id)?;
            log.views.insert(id, op.view);
            if let Some(k) = idem_key(&op) {
                log.idem.insert(k, id);
            }
            log.verified.insert(id);
            queue.extend(op.parents.iter().copied());
        }
        log.heads = heads.into_iter().collect();
        Ok(log)
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn into_store(self) -> S {
        self.store
    }

    pub fn heads(&self) -> Vec<ObjectId> {
        self.heads.iter().copied().collect()
    }

    pub fn is_verified(&self, op: &ObjectId) -> bool {
        self.verified.contains(op)
    }

    pub fn get_op(&self, id: &ObjectId) -> Result<Op> {
        Ok(self.store.get(id)?)
    }

    /// The view an op produced.
    pub fn view_of(&self, op: &ObjectId) -> Result<View> {
        let vid = match self.views.get(op) {
            Some(v) => *v,
            None => self.get_op(op)?.view,
        };
        Ok(self.store.get(&vid)?)
    }

    /// Every ancestor of `op`, including itself.
    pub fn ancestors(&self, op: &ObjectId) -> Result<HashSet<ObjectId>> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([*op]);
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            let o = self.get_op(&id)?;
            queue.extend(o.parents.iter().copied());
        }
        Ok(seen)
    }

    /// The latest common ancestor of a set of ops: a common ancestor that is
    /// not an ancestor of another common ancestor. Ties break on the lowest ID.
    pub fn lca(&self, ops: &[ObjectId]) -> Result<Option<ObjectId>> {
        if ops.is_empty() {
            return Ok(None);
        }
        let mut common: Option<HashSet<ObjectId>> = None;
        for o in ops {
            let a = self.ancestors(o)?;
            common = Some(match common {
                None => a,
                Some(c) => c.intersection(&a).copied().collect(),
            });
        }
        let common = common.unwrap_or_default();
        if common.is_empty() {
            return Ok(None);
        }
        // Remove anything that is a proper ancestor of another common element.
        let mut latest: BTreeSet<ObjectId> = common.iter().copied().collect();
        for c in &common {
            let anc = self.ancestors(c)?;
            for d in anc {
                if d != *c {
                    latest.remove(&d);
                }
            }
        }
        Ok(latest.into_iter().next())
    }

    /// The merged view of a set of ops, folded pairwise in ascending ID.
    pub fn merged_view(&self, ops: &[ObjectId]) -> Result<View> {
        let vs = ViewState::new(&self.store);
        let mut sorted: Vec<ObjectId> = ops.to_vec();
        sorted.sort();
        sorted.dedup();
        match sorted.len() {
            0 => vs.empty(),
            1 => self.view_of(&sorted[0]),
            _ => {
                let mut acc = self.view_of(&sorted[0])?;
                let mut folded = vec![sorted[0]];
                for next in &sorted[1..] {
                    let mut set = folded.clone();
                    set.push(*next);
                    let base = match self.lca(&set)? {
                        Some(b) => self.view_of(&b)?,
                        None => vs.empty()?,
                    };
                    let nv = self.view_of(next)?;
                    acc = vs.merge(&base, &acc, &nv)?;
                    folded.push(*next);
                }
                Ok(acc)
            }
        }
    }

    /// The current merged view of the heads.
    pub fn current_view(&self) -> Result<View> {
        let heads = self.heads();
        self.merged_view(&heads)
    }

    /// The op previously produced for this author and idempotency key, if any.
    pub fn lookup_idem(&self, author: &EntityId, idem: &[u8; 16]) -> Option<ObjectId> {
        self.idem.get(&(*author, *idem)).copied()
    }

    /// Verify an op and, if it passes, store it and advance the heads.
    /// Returns the op's ID. A repeated idempotency key returns the earlier op.
    pub fn accept(&mut self, op: &Op) -> Result<ObjectId> {
        if let Some(k) = idem_key(op) {
            if let Some(existing) = self.idem.get(&k) {
                return Ok(*existing);
            }
        }
        for p in &op.parents {
            if !self.verified.contains(p) {
                return Err(Error::UnknownParent(*p));
            }
        }
        let verified = verify::verify_op(self, op)?;
        let id = self.store.put(op)?;
        self.store.put(&verified.view)?;
        for p in &op.parents {
            self.heads.remove(p);
        }
        self.heads.insert(id);
        self.verified.insert(id);
        self.views.insert(id, op.view);
        if let Some(k) = idem_key(op) {
            self.idem.insert(k, id);
        }
        Ok(id)
    }
}

fn idem_key(op: &Op) -> Option<(EntityId, [u8; 16])> {
    match op.args.get("idem") {
        Some(ciborium::value::Value::Bytes(b)) if b.len() == 16 => {
            let mut k = [0u8; 16];
            k.copy_from_slice(b);
            Some((op.author, k))
        }
        _ => None,
    }
}
