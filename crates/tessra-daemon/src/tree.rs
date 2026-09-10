//! Trees as flat path maps: flatten, build with sharding, and a file-level
//! three-way merge. Node-level merge arrives with the semantic index in M2;
//! this is the fallback the spec describes for content without a grammar.

use std::collections::BTreeMap;

use tessra_core::hash;
use tessra_core::object::{ConflictTerm, EntryKind, Tree, TreeEntry};
use tessra_core::store::ObjectStore;
use tessra_core::ObjectId;

use crate::Result;

/// A leaf entry keyed by its root-relative path.
#[derive(Clone, Debug, PartialEq)]
pub struct Leaf {
    pub kind: EntryKind,
    pub mode: Option<u8>,
    pub r#ref: Option<ObjectId>,
    pub terms: Option<Vec<ConflictTerm>>,
}

impl Leaf {
    pub fn file(id: ObjectId, executable: bool) -> Self {
        Leaf {
            kind: EntryKind::File,
            mode: Some(if executable { 1 } else { 0 }),
            r#ref: Some(id),
            terms: None,
        }
    }
    pub fn is_conflict(&self) -> bool {
        self.kind == EntryKind::Conflict
    }
}

pub type Flat = BTreeMap<String, Leaf>;

/// Every leaf under a tree, keyed by path.
pub fn flatten<S: ObjectStore>(store: &S, tree: &ObjectId) -> Result<Flat> {
    let mut out = Flat::new();
    walk(store, tree, "", &mut out)?;
    Ok(out)
}

fn entries<S: ObjectStore>(store: &S, id: &ObjectId) -> Result<Vec<TreeEntry>> {
    let t: Tree = store.get(id)?;
    if let Some(e) = t.entries {
        return Ok(e);
    }
    let mut all = Vec::new();
    for shard in t.shards.unwrap_or_default().into_iter().flatten() {
        let st: Tree = store.get(&shard)?;
        all.extend(st.entries.unwrap_or_default());
    }
    all.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    Ok(all)
}

fn walk<S: ObjectStore>(store: &S, id: &ObjectId, prefix: &str, out: &mut Flat) -> Result<()> {
    for e in entries(store, id)? {
        let path = if prefix.is_empty() {
            e.name.clone()
        } else {
            format!("{prefix}/{}", e.name)
        };
        if e.kind == EntryKind::Dir {
            if let Some(r) = e.r#ref {
                walk(store, &r, &path, out)?;
            }
        } else {
            out.insert(
                path,
                Leaf {
                    kind: e.kind,
                    mode: e.mode,
                    r#ref: e.r#ref,
                    terms: e.terms,
                },
            );
        }
    }
    Ok(())
}

/// Build nested tree objects from a flat map. Directories with more than
/// the shard threshold are sharded by name hash.
pub fn build<S: ObjectStore>(store: &S, flat: &Flat) -> Result<ObjectId> {
    #[derive(Default)]
    struct Dir {
        files: Vec<TreeEntry>,
        dirs: BTreeMap<String, Dir>,
    }
    let mut root = Dir::default();
    for (path, leaf) in flat {
        let parts: Vec<&str> = path.split('/').collect();
        let mut cur = &mut root;
        for p in &parts[..parts.len() - 1] {
            cur = cur.dirs.entry((*p).to_string()).or_default();
        }
        cur.files.push(TreeEntry {
            name: parts[parts.len() - 1].to_string(),
            kind: leaf.kind,
            mode: leaf.mode,
            r#ref: leaf.r#ref,
            terms: leaf.terms.clone(),
        });
    }
    fn emit<S: ObjectStore>(store: &S, d: Dir) -> Result<ObjectId> {
        let mut entries = d.files;
        for (name, sub) in d.dirs {
            let id = emit(store, sub)?;
            entries.push(TreeEntry {
                name,
                kind: EntryKind::Dir,
                mode: None,
                r#ref: Some(id),
                terms: None,
            });
        }
        if entries.len() > Tree::SHARD_THRESHOLD {
            let mut buckets: Vec<Vec<TreeEntry>> = (0..256).map(|_| Vec::new()).collect();
            for e in entries {
                buckets[hash::shard_slot(&e.name) as usize].push(e);
            }
            let mut shards = Vec::with_capacity(256);
            for b in buckets {
                if b.is_empty() {
                    shards.push(None);
                } else {
                    shards.push(Some(store.put(&Tree::flat(b))?));
                }
            }
            return Ok(store.put(&Tree {
                entries: None,
                shards: Some(shards),
            })?);
        }
        Ok(store.put(&Tree::flat(entries))?)
    }
    emit(store, root)
}

/// File-level three-way merge. Returns the merged map and the conflicted paths.
pub fn merge3(base: &Flat, a: &Flat, b: &Flat) -> (Flat, Vec<String>) {
    let mut out = Flat::new();
    let mut conflicts = Vec::new();
    let mut paths: Vec<&String> = base.keys().chain(a.keys()).chain(b.keys()).collect();
    paths.sort();
    paths.dedup();
    for p in paths {
        let (lb, la, lb2) = (base.get(p), a.get(p), b.get(p));
        let pick = if la == lb {
            lb2.cloned()
        } else if lb2 == lb {
            la.cloned()
        } else if la == lb2 {
            la.cloned()
        } else {
            conflicts.push(p.clone());
            let mut terms = Vec::new();
            if let Some(x) = lb {
                terms.push(ConflictTerm {
                    sign: -1,
                    kind: x.kind,
                    mode: x.mode,
                    r#ref: x.r#ref.unwrap_or(ObjectId([0; 32])),
                });
            }
            for side in [la, lb2].into_iter().flatten() {
                terms.push(ConflictTerm {
                    sign: 1,
                    kind: side.kind,
                    mode: side.mode,
                    r#ref: side.r#ref.unwrap_or(ObjectId([0; 32])),
                });
            }
            // Positive terms must outnumber negative by one; pad with an empty-blob add when a side deleted.
            while terms.iter().filter(|t| t.sign > 0).count()
                < terms.iter().filter(|t| t.sign < 0).count() + 1
            {
                terms.push(ConflictTerm {
                    sign: 1,
                    kind: EntryKind::File,
                    mode: Some(0),
                    r#ref: hash::blob_id(b""),
                });
            }
            Some(Leaf {
                kind: EntryKind::Conflict,
                mode: None,
                r#ref: None,
                terms: Some(terms),
            })
        };
        if let Some(l) = pick {
            out.insert(p.clone(), l);
        }
    }
    (out, conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_core::store::MemoryStore;

    fn leaf(s: &MemoryStore, content: &[u8]) -> Leaf {
        Leaf::file(s.put_blob(content).unwrap(), false)
    }

    #[test]
    fn build_and_flatten_round_trip() {
        let s = MemoryStore::new();
        let mut flat = Flat::new();
        flat.insert("a.txt".into(), leaf(&s, b"a"));
        flat.insert("src/main.rs".into(), leaf(&s, b"fn main(){}"));
        flat.insert("src/lib/mod.rs".into(), leaf(&s, b"mod x;"));
        let id = build(&s, &flat).unwrap();
        let back = flatten(&s, &id).unwrap();
        assert_eq!(back, flat);
        assert_eq!(build(&s, &back).unwrap(), id);
    }

    #[test]
    fn sharded_directory_round_trips() {
        let s = MemoryStore::new();
        let mut flat = Flat::new();
        for i in 0..5000 {
            flat.insert(format!("big/f{i}"), leaf(&s, format!("{i}").as_bytes()));
        }
        let id = build(&s, &flat).unwrap();
        let back = flatten(&s, &id).unwrap();
        assert_eq!(back.len(), 5000);
        assert_eq!(back, flat);
    }

    #[test]
    fn merge_disjoint_and_conflicting() {
        let s = MemoryStore::new();
        let mut base = Flat::new();
        base.insert("a".into(), leaf(&s, b"1"));
        base.insert("b".into(), leaf(&s, b"1"));
        base.insert("c".into(), leaf(&s, b"1"));
        let mut x = base.clone();
        x.insert("a".into(), leaf(&s, b"x"));
        x.insert("new".into(), leaf(&s, b"n"));
        let mut y = base.clone();
        y.insert("b".into(), leaf(&s, b"y"));
        y.remove("c");
        let (m, conflicts) = merge3(&base, &x, &y);
        assert!(conflicts.is_empty());
        assert_eq!(m["a"], leaf(&s, b"x"));
        assert_eq!(m["b"], leaf(&s, b"y"));
        assert!(!m.contains_key("c"));
        assert!(m.contains_key("new"));
        let mut z = base.clone();
        z.insert("a".into(), leaf(&s, b"z"));
        let (m, conflicts) = merge3(&base, &x, &z);
        assert_eq!(conflicts, vec!["a".to_string()]);
        assert!(m["a"].is_conflict());
        let terms = m["a"].terms.as_ref().unwrap();
        assert_eq!(terms.iter().filter(|t| t.sign > 0).count(), 2);
        assert_eq!(terms.iter().filter(|t| t.sign < 0).count(), 1);
    }
}
