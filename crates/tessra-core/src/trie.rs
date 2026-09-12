//! The persistent hash array mapped trie that the view's large maps are built from.
//!
//! Sixteen-way branching, one nibble of the key per level, canonical form:
//! a subtree holding exactly one key is stored as a leaf in its parent, never
//! as a node. Two tries with the same contents have the same root ID.
//! Merge is structural: a subtree equal to the base on one side takes the
//! other side wholesale, so cost is proportional to the differences.

use std::collections::BTreeSet;

use ciborium::value::Value;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::cbor::TessraObject;
use crate::store::ObjectStore;
use crate::{Error, ObjectId, Result};

/// A value stored at a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrieValue {
    /// A pointer to an object, for entity maps.
    Id(ObjectId),
    /// Concurrent writers disagreed. Sorted, deduplicated.
    Conflict(Vec<ObjectId>),
    /// Set membership.
    Set,
}

impl TrieValue {
    fn ids(&self) -> Vec<ObjectId> {
        match self {
            TrieValue::Id(i) => vec![*i],
            TrieValue::Conflict(v) => v.clone(),
            TrieValue::Set => vec![],
        }
    }
    fn union(a: &TrieValue, b: &TrieValue) -> TrieValue {
        let mut s: BTreeSet<ObjectId> = a.ids().into_iter().collect();
        s.extend(b.ids());
        let v: Vec<ObjectId> = s.into_iter().collect();
        if v.len() == 1 {
            TrieValue::Id(v[0])
        } else {
            TrieValue::Conflict(v)
        }
    }
}

impl Serialize for TrieValue {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            TrieValue::Id(id) => id.serialize(s),
            TrieValue::Conflict(v) => {
                #[derive(Serialize)]
                struct C<'a> {
                    conflict: &'a Vec<ObjectId>,
                }
                C { conflict: v }.serialize(s)
            }
            TrieValue::Set => s.serialize_bool(true),
        }
    }
}

impl<'de> Deserialize<'de> for TrieValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        match v {
            Value::Bytes(b) => ObjectId::from_slice(&b)
                .map(TrieValue::Id)
                .map_err(serde::de::Error::custom),
            Value::Bool(true) => Ok(TrieValue::Set),
            Value::Map(_) => {
                #[derive(Deserialize)]
                struct C {
                    conflict: Vec<ObjectId>,
                }
                let c: C = v.deserialized().map_err(serde::de::Error::custom)?;
                Ok(TrieValue::Conflict(c.conflict))
            }
            other => Err(serde::de::Error::custom(format!(
                "bad trie value {other:?}"
            ))),
        }
    }
}

/// One slot of a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    Leaf { key: Vec<u8>, value: TrieValue },
    Node(ObjectId),
}

impl Serialize for Slot {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Slot::Leaf { key, value } => {
                #[derive(Serialize)]
                struct L<'a> {
                    leaf: (&'a serde_bytes::Bytes, &'a TrieValue),
                }
                L {
                    leaf: (serde_bytes::Bytes::new(key), value),
                }
                .serialize(s)
            }
            Slot::Node(id) => {
                #[derive(Serialize)]
                struct N<'a> {
                    node: &'a ObjectId,
                }
                N { node: id }.serialize(s)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Slot {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let entries = match &v {
            Value::Map(m) => m,
            _ => return Err(serde::de::Error::custom("slot must be a map")),
        };
        if entries.len() != 1 {
            return Err(serde::de::Error::custom("slot must have one key"));
        }
        let (k, val) = &entries[0];
        match (k, val) {
            (Value::Text(t), Value::Array(pair)) if t == "leaf" && pair.len() == 2 => {
                let key = match &pair[0] {
                    Value::Bytes(b) => b.clone(),
                    _ => return Err(serde::de::Error::custom("leaf key must be bytes")),
                };
                let value: TrieValue = pair[1]
                    .clone()
                    .deserialized()
                    .map_err(serde::de::Error::custom)?;
                Ok(Slot::Leaf { key, value })
            }
            (Value::Text(t), Value::Bytes(b)) if t == "node" => ObjectId::from_slice(b)
                .map(Slot::Node)
                .map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom("bad slot")),
        }
    }
}

/// A trie node. Object type `trie`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrieNode {
    pub bitmap: u16,
    pub slots: Vec<Slot>,
}

impl TessraObject for TrieNode {
    const TAG: &'static str = "trie";
    const VERSION: u64 = 1;
}

impl TrieNode {
    pub fn empty() -> Self {
        TrieNode {
            bitmap: 0,
            slots: Vec::new(),
        }
    }

    fn slot_index(&self, nibble: u8) -> Option<usize> {
        if self.bitmap & (1 << nibble) == 0 {
            None
        } else {
            Some((self.bitmap & ((1u32 << nibble) as u16).wrapping_sub(1)).count_ones() as usize)
        }
    }

    fn get_slot(&self, nibble: u8) -> Option<&Slot> {
        self.slot_index(nibble).map(|i| &self.slots[i])
    }

    fn set_slot(&mut self, nibble: u8, slot: Option<Slot>) {
        match (self.slot_index(nibble), slot) {
            (Some(i), Some(s)) => self.slots[i] = s,
            (Some(i), None) => {
                self.slots.remove(i);
                self.bitmap &= !(1 << nibble);
            }
            (None, Some(s)) => {
                self.bitmap |= 1 << nibble;
                let i =
                    (self.bitmap & ((1u32 << nibble) as u16).wrapping_sub(1)).count_ones() as usize;
                self.slots.insert(i, s);
            }
            (None, None) => {}
        }
    }
}

fn nibble_at(key: &[u8], depth: usize) -> u8 {
    let b = key[depth / 2];
    if depth.is_multiple_of(2) {
        b >> 4
    } else {
        b & 0x0f
    }
}

/// How removed-versus-changed is resolved in a three-way merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergePolicy {
    /// Entity pointer maps: a removal (containment, redaction) wins over a concurrent change.
    Pointers,
    /// Sets: add wins. A member present on either side and not removed relative to the base stays.
    Set,
}

/// A trie rooted at a node in a store.
pub struct Trie<'a, S: ObjectStore> {
    store: &'a S,
}

/// A slot viewed abstractly during merge.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Sub {
    Empty,
    Leaf(Vec<u8>, TrieValue),
    Node(ObjectId),
}

impl From<Option<&Slot>> for Sub {
    fn from(s: Option<&Slot>) -> Self {
        match s {
            None => Sub::Empty,
            Some(Slot::Leaf { key, value }) => Sub::Leaf(key.clone(), value.clone()),
            Some(Slot::Node(id)) => Sub::Node(*id),
        }
    }
}

impl<'a, S: ObjectStore> Trie<'a, S> {
    pub fn new(store: &'a S) -> Self {
        Trie { store }
    }

    /// The ID of the empty trie, storing it if needed.
    pub fn empty_root(&self) -> Result<ObjectId> {
        self.store.put(&TrieNode::empty())
    }

    fn load(&self, id: &ObjectId) -> Result<TrieNode> {
        self.store.get(id)
    }

    fn save(&self, node: &TrieNode) -> Result<ObjectId> {
        self.store.put(node)
    }

    pub fn get(&self, root: &ObjectId, key: &[u8]) -> Result<Option<TrieValue>> {
        let mut node = self.load(root)?;
        let mut depth = 0;
        loop {
            if depth >= key.len() * 2 {
                return Ok(None);
            }
            match node.get_slot(nibble_at(key, depth)) {
                None => return Ok(None),
                Some(Slot::Leaf { key: k, value }) => {
                    return Ok(if k == key { Some(value.clone()) } else { None })
                }
                Some(Slot::Node(id)) => {
                    node = self.load(id)?;
                    depth += 1;
                }
            }
        }
    }

    pub fn insert(&self, root: &ObjectId, key: &[u8], value: TrieValue) -> Result<ObjectId> {
        let node = self.load(root)?;
        let node = self.insert_in(node, 0, key, value)?;
        self.save(&node)
    }

    fn insert_in(
        &self,
        mut node: TrieNode,
        depth: usize,
        key: &[u8],
        value: TrieValue,
    ) -> Result<TrieNode> {
        let n = nibble_at(key, depth);
        let new_slot = match node.get_slot(n).cloned() {
            None => Slot::Leaf {
                key: key.to_vec(),
                value,
            },
            Some(Slot::Leaf { key: k, value: v }) => {
                if k == key {
                    Slot::Leaf { key: k, value }
                } else {
                    if k.len() != key.len() {
                        return Err(Error::Invariant("trie keys must have one length".into()));
                    }
                    let child = self.pair_node(depth + 1, k, v, key.to_vec(), value)?;
                    Slot::Node(self.save(&child)?)
                }
            }
            Some(Slot::Node(id)) => {
                let child = self.load(&id)?;
                let child = self.insert_in(child, depth + 1, key, value)?;
                Slot::Node(self.save(&child)?)
            }
        };
        node.set_slot(n, Some(new_slot));
        Ok(node)
    }

    fn pair_node(
        &self,
        depth: usize,
        k1: Vec<u8>,
        v1: TrieValue,
        k2: Vec<u8>,
        v2: TrieValue,
    ) -> Result<TrieNode> {
        if depth >= k1.len() * 2 {
            return Err(Error::Invariant(
                "trie keys collide beyond their length".into(),
            ));
        }
        let n1 = nibble_at(&k1, depth);
        let n2 = nibble_at(&k2, depth);
        let mut node = TrieNode::empty();
        if n1 == n2 {
            let child = self.pair_node(depth + 1, k1, v1, k2, v2)?;
            node.set_slot(n1, Some(Slot::Node(self.save(&child)?)));
        } else {
            node.set_slot(n1, Some(Slot::Leaf { key: k1, value: v1 }));
            node.set_slot(n2, Some(Slot::Leaf { key: k2, value: v2 }));
        }
        Ok(node)
    }

    pub fn remove(&self, root: &ObjectId, key: &[u8]) -> Result<ObjectId> {
        let node = self.load(root)?;
        let node = match self.remove_in(node, 0, key)? {
            Sub::Empty => TrieNode::empty(),
            Sub::Leaf(k, v) => {
                let mut n = TrieNode::empty();
                n.set_slot(nibble_at(&k, 0), Some(Slot::Leaf { key: k, value: v }));
                n
            }
            Sub::Node(id) => return Ok(id),
        };
        self.save(&node)
    }

    /// Returns the normalized form of the node after removal.
    fn remove_in(&self, mut node: TrieNode, depth: usize, key: &[u8]) -> Result<Sub> {
        let n = nibble_at(key, depth);
        match node.get_slot(n).cloned() {
            None => return Ok(Sub::Node(self.save(&node)?)),
            Some(Slot::Leaf { key: k, .. }) => {
                if k != key {
                    return Ok(Sub::Node(self.save(&node)?));
                }
                node.set_slot(n, None);
            }
            Some(Slot::Node(id)) => {
                let child = self.load(&id)?;
                match self.remove_in(child, depth + 1, key)? {
                    Sub::Empty => node.set_slot(n, None),
                    Sub::Leaf(k, v) => node.set_slot(n, Some(Slot::Leaf { key: k, value: v })),
                    Sub::Node(cid) => node.set_slot(n, Some(Slot::Node(cid))),
                }
            }
        }
        self.normalize(node)
    }

    /// Canonical form: empty stays empty, a single leaf collapses upward, otherwise a node.
    fn normalize(&self, node: TrieNode) -> Result<Sub> {
        match node.slots.len() {
            0 => Ok(Sub::Empty),
            1 => match &node.slots[0] {
                Slot::Leaf { key, value } => Ok(Sub::Leaf(key.clone(), value.clone())),
                Slot::Node(_) => Ok(Sub::Node(self.save(&node)?)),
            },
            _ => Ok(Sub::Node(self.save(&node)?)),
        }
    }

    /// All entries in key order.
    pub fn entries(&self, root: &ObjectId) -> Result<Vec<(Vec<u8>, TrieValue)>> {
        let mut out = Vec::new();
        self.walk(root, &mut out)?;
        Ok(out)
    }

    fn walk(&self, id: &ObjectId, out: &mut Vec<(Vec<u8>, TrieValue)>) -> Result<()> {
        let node = self.load(id)?;
        for slot in &node.slots {
            match slot {
                Slot::Leaf { key, value } => out.push((key.clone(), value.clone())),
                Slot::Node(cid) => self.walk(cid, out)?,
            }
        }
        Ok(())
    }

    pub fn len(&self, root: &ObjectId) -> Result<usize> {
        Ok(self.entries(root)?.len())
    }

    /// Three-way merge of two tries against a common base.
    pub fn merge3(
        &self,
        base: &ObjectId,
        a: &ObjectId,
        b: &ObjectId,
        policy: MergePolicy,
    ) -> Result<ObjectId> {
        let result = self.merge_sub(0, Sub::Node(*base), Sub::Node(*a), Sub::Node(*b), policy)?;
        match result {
            Sub::Empty => self.empty_root(),
            Sub::Leaf(k, v) => {
                let mut n = TrieNode::empty();
                n.set_slot(nibble_at(&k, 0), Some(Slot::Leaf { key: k, value: v }));
                self.save(&n)
            }
            Sub::Node(id) => Ok(id),
        }
    }

    fn expand(&self, sub: &Sub, depth: usize) -> Result<TrieNode> {
        Ok(match sub {
            Sub::Empty => TrieNode::empty(),
            Sub::Leaf(k, v) => {
                let mut n = TrieNode::empty();
                n.set_slot(
                    nibble_at(k, depth),
                    Some(Slot::Leaf {
                        key: k.clone(),
                        value: v.clone(),
                    }),
                );
                n
            }
            Sub::Node(id) => self.load(id)?,
        })
    }

    fn is_flat(sub: &Sub) -> bool {
        !matches!(sub, Sub::Node(_))
    }

    fn leaf_parts(sub: &Sub) -> (Option<&Vec<u8>>, Option<&TrieValue>) {
        match sub {
            Sub::Empty => (None, None),
            Sub::Leaf(k, v) => (Some(k), Some(v)),
            Sub::Node(_) => (None, None),
        }
    }

    fn merge_sub(
        &self,
        depth: usize,
        base: Sub,
        a: Sub,
        b: Sub,
        policy: MergePolicy,
    ) -> Result<Sub> {
        if a == base {
            return Ok(b);
        }
        if b == base {
            return Ok(a);
        }
        if a == b {
            return Ok(a);
        }
        // Value-level rule applies when everything is a leaf or empty and all leaves share one key.
        if Self::is_flat(&base) && Self::is_flat(&a) && Self::is_flat(&b) {
            let (kb, vb) = Self::leaf_parts(&base);
            let (ka, va) = Self::leaf_parts(&a);
            let (kb2, vb2) = Self::leaf_parts(&b);
            let keys: Vec<&Vec<u8>> = [kb, ka, kb2].into_iter().flatten().collect();
            if keys.windows(2).all(|w| w[0] == w[1]) {
                let key = keys[0].clone();
                // a != base and b != base and a != b here.
                let merged: Option<TrieValue> = match (vb, va, vb2) {
                    (_, Some(x), Some(y)) => Some(TrieValue::union(x, y)),
                    (Some(_), None, Some(y)) | (Some(_), Some(y), None) => match policy {
                        MergePolicy::Pointers => None,
                        MergePolicy::Set => Some(y.clone()),
                    },
                    (None, None, None) | (Some(_), None, None) => None,
                    (None, None, Some(y)) | (None, Some(y), None) => Some(y.clone()),
                };
                return Ok(match merged {
                    None => Sub::Empty,
                    Some(v) => Sub::Leaf(key, v),
                });
            }
        }
        // Structural: expand all three to nodes at this depth and merge slot by slot.
        let nb = self.expand(&base, depth)?;
        let na = self.expand(&a, depth)?;
        let nb2 = self.expand(&b, depth)?;
        let mut out = TrieNode::empty();
        for nib in 0u8..16 {
            let sb: Sub = nb.get_slot(nib).into();
            let sa: Sub = na.get_slot(nib).into();
            let sb2: Sub = nb2.get_slot(nib).into();
            let merged = self.merge_sub(depth + 1, sb, sa, sb2, policy)?;
            let slot = match merged {
                Sub::Empty => None,
                Sub::Leaf(k, v) => Some(Slot::Leaf { key: k, value: v }),
                Sub::Node(id) => Some(Slot::Node(id)),
            };
            out.set_slot(nib, slot);
        }
        self.normalize(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn key(n: u32) -> Vec<u8> {
        let mut k = [0u8; 16];
        k[..4].copy_from_slice(&n.to_be_bytes());
        // Spread bits so tries branch early.
        let h = blake3::hash(&k);
        h.as_bytes()[..16].to_vec()
    }
    fn oid(n: u8) -> ObjectId {
        ObjectId([n; 32])
    }

    #[test]
    fn insert_get_remove_and_canonical_form() {
        let s = MemoryStore::new();
        let t = Trie::new(&s);
        let mut root = t.empty_root().unwrap();
        for i in 0..200u32 {
            root = t
                .insert(&root, &key(i), TrieValue::Id(oid((i % 250) as u8)))
                .unwrap();
        }
        for i in 0..200u32 {
            assert_eq!(
                t.get(&root, &key(i)).unwrap(),
                Some(TrieValue::Id(oid((i % 250) as u8)))
            );
        }
        assert_eq!(t.get(&root, &key(999)).unwrap(), None);
        assert_eq!(t.len(&root).unwrap(), 200);

        // Same contents inserted in another order yield the same root.
        let mut root2 = t.empty_root().unwrap();
        for i in (0..200u32).rev() {
            root2 = t
                .insert(&root2, &key(i), TrieValue::Id(oid((i % 250) as u8)))
                .unwrap();
        }
        assert_eq!(root, root2);

        // Remove everything but one; the result equals a fresh single insert.
        for i in 1..200u32 {
            root = t.remove(&root, &key(i)).unwrap();
        }
        let single = t
            .insert(&t.empty_root().unwrap(), &key(0), TrieValue::Id(oid(0)))
            .unwrap();
        assert_eq!(root, single);
        root = t.remove(&root, &key(0)).unwrap();
        assert_eq!(root, t.empty_root().unwrap());
    }

    #[test]
    fn entries_are_in_key_order() {
        let s = MemoryStore::new();
        let t = Trie::new(&s);
        let mut root = t.empty_root().unwrap();
        for i in 0..50u32 {
            root = t.insert(&root, &key(i), TrieValue::Set).unwrap();
        }
        let e = t.entries(&root).unwrap();
        let mut keys: Vec<Vec<u8>> = e.iter().map(|(k, _)| k.clone()).collect();
        let sorted = {
            let mut s = keys.clone();
            s.sort();
            s
        };
        assert_eq!(keys, sorted);
        keys.dedup();
        assert_eq!(keys.len(), 50);
    }

    #[test]
    fn node_round_trips_through_cbor() {
        let mut n = TrieNode::empty();
        n.set_slot(
            3,
            Some(Slot::Leaf {
                key: key(1),
                value: TrieValue::Conflict(vec![oid(1), oid(2)]),
            }),
        );
        n.set_slot(9, Some(Slot::Node(oid(7))));
        n.set_slot(
            0,
            Some(Slot::Leaf {
                key: key(2),
                value: TrieValue::Set,
            }),
        );
        let e = crate::cbor::encode(&n).unwrap();
        let back: TrieNode = crate::cbor::decode(&e.bytes).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn merge_independent_and_conflicting() {
        let s = MemoryStore::new();
        let t = Trie::new(&s);
        let mut base = t.empty_root().unwrap();
        for i in 0..20u32 {
            base = t.insert(&base, &key(i), TrieValue::Id(oid(1))).unwrap();
        }
        // a changes key 3, adds key 100; b changes key 7, adds key 200; both change key 5 differently.
        let a = t.insert(&base, &key(3), TrieValue::Id(oid(2))).unwrap();
        let a = t.insert(&a, &key(100), TrieValue::Id(oid(3))).unwrap();
        let a = t.insert(&a, &key(5), TrieValue::Id(oid(50))).unwrap();
        let b = t.insert(&base, &key(7), TrieValue::Id(oid(4))).unwrap();
        let b = t.insert(&b, &key(200), TrieValue::Id(oid(5))).unwrap();
        let b = t.insert(&b, &key(5), TrieValue::Id(oid(51))).unwrap();

        let m = t.merge3(&base, &a, &b, MergePolicy::Pointers).unwrap();
        assert_eq!(t.get(&m, &key(3)).unwrap(), Some(TrieValue::Id(oid(2))));
        assert_eq!(t.get(&m, &key(7)).unwrap(), Some(TrieValue::Id(oid(4))));
        assert_eq!(t.get(&m, &key(100)).unwrap(), Some(TrieValue::Id(oid(3))));
        assert_eq!(t.get(&m, &key(200)).unwrap(), Some(TrieValue::Id(oid(5))));
        assert_eq!(
            t.get(&m, &key(5)).unwrap(),
            Some(TrieValue::Conflict(vec![oid(50), oid(51)]))
        );
        assert_eq!(t.get(&m, &key(1)).unwrap(), Some(TrieValue::Id(oid(1))));
        assert_eq!(t.len(&m).unwrap(), 22);

        // Symmetric.
        let m2 = t.merge3(&base, &b, &a, MergePolicy::Pointers).unwrap();
        assert_eq!(m, m2);

        // Merging identical sides is the side.
        assert_eq!(t.merge3(&base, &a, &a, MergePolicy::Pointers).unwrap(), a);
        assert_eq!(
            t.merge3(&base, &base, &b, MergePolicy::Pointers).unwrap(),
            b
        );
    }

    #[test]
    fn merge_removed_versus_changed_by_policy() {
        let s = MemoryStore::new();
        let t = Trie::new(&s);
        let mut base = t.empty_root().unwrap();
        for i in 0..8u32 {
            base = t.insert(&base, &key(i), TrieValue::Id(oid(1))).unwrap();
        }
        let a = t.remove(&base, &key(2)).unwrap();
        let b = t.insert(&base, &key(2), TrieValue::Id(oid(9))).unwrap();
        let m = t.merge3(&base, &a, &b, MergePolicy::Pointers).unwrap();
        assert_eq!(t.get(&m, &key(2)).unwrap(), None);
        let m = t.merge3(&base, &a, &b, MergePolicy::Set).unwrap();
        assert_eq!(t.get(&m, &key(2)).unwrap(), Some(TrieValue::Id(oid(9))));
    }

    #[test]
    fn merge_set_add_wins() {
        let s = MemoryStore::new();
        let t = Trie::new(&s);
        let base = t.empty_root().unwrap();
        let a = t.insert(&base, &key(1), TrieValue::Set).unwrap();
        let b = t.insert(&base, &key(2), TrieValue::Set).unwrap();
        let m = t.merge3(&base, &a, &b, MergePolicy::Set).unwrap();
        assert_eq!(t.len(&m).unwrap(), 2);
        // Both add the same member: no conflict, one member.
        let a2 = t.insert(&base, &key(3), TrieValue::Set).unwrap();
        let b2 = t.insert(&base, &key(3), TrieValue::Set).unwrap();
        let m = t.merge3(&base, &a2, &b2, MergePolicy::Set).unwrap();
        assert_eq!(t.get(&m, &key(3)).unwrap(), Some(TrieValue::Set));
        assert_eq!(m, a2);
    }
}
