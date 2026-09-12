//! Persistent object store on redb, plus the GVPK pack format.
//!
//! One database file holds three tables: objects by ID, metadata (heads,
//! repository ID), and the idempotency index. Each object record is
//! `[tag_len u8][tag][bytes]` so the store can re-verify hashes on read.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use tessra_core::hash;
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};

pub mod pack;

const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("objects");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const IDEM: TableDefinition<&[u8], &[u8]> = TableDefinition::new("idem");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] tessra_core::Error),
    #[error("redb: {0}")]
    Db(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pack: {0}")]
    Pack(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn db_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Db(e.to_string())
}

/// The puts an open batch has collected, by object ID; `None` while no batch is open.
type PendingBatch = Option<HashMap<ObjectId, Vec<u8>>>;

/// A redb-backed object store. Cheap to clone; clones share the database.
///
/// A batch, when open, collects puts in memory and writes them in one
/// transaction on `end_batch`, which is what makes an import or a large
/// snapshot fast. Reads see pending puts.
#[derive(Clone)]
pub struct RedbStore {
    db: Arc<Database>,
    pending: Arc<Mutex<PendingBatch>>,
}

impl RedbStore {
    /// Open or create the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Another process may hold the store; wait a little for it.
        let started = Instant::now();
        let db = loop {
            match Database::create(path) {
                Ok(db) => break db,
                Err(e) if started.elapsed() < Duration::from_secs(10) => {
                    let msg = e.to_string();
                    if msg.contains("lock")
                        || msg.contains("already open")
                        || msg.contains("in use")
                    {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    return Err(db_err(e));
                }
                Err(e) => return Err(db_err(e)),
            }
        };
        {
            let w = db.begin_write().map_err(db_err)?;
            w.open_table(OBJECTS).map_err(db_err)?;
            w.open_table(META).map_err(db_err)?;
            w.open_table(IDEM).map_err(db_err)?;
            w.commit().map_err(db_err)?;
        }
        Ok(RedbStore {
            db: Arc::new(db),
            pending: Arc::new(Mutex::new(None)),
        })
    }

    /// Start collecting puts in memory.
    pub fn begin_batch(&self) {
        let mut p = self.pending.lock().unwrap();
        if p.is_none() {
            *p = Some(HashMap::new());
        }
    }

    /// Write every pending put in one transaction.
    pub fn end_batch(&self) -> Result<usize> {
        let taken = self.pending.lock().unwrap().take();
        let Some(map) = taken else { return Ok(0) };
        if map.is_empty() {
            return Ok(0);
        }
        let w = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = w.open_table(OBJECTS).map_err(db_err)?;
            for (id, rec) in &map {
                if t.get(id.0.as_slice()).map_err(db_err)?.is_none() {
                    t.insert(id.0.as_slice(), rec.as_slice()).map_err(db_err)?;
                }
            }
        }
        w.commit().map_err(db_err)?;
        Ok(map.len())
    }

    pub fn heads(&self) -> Result<Vec<ObjectId>> {
        let r = self.db.begin_read().map_err(db_err)?;
        let t = r.open_table(META).map_err(db_err)?;
        let Some(v) = t.get("heads").map_err(db_err)? else {
            return Ok(Vec::new());
        };
        let bytes = v.value();
        Ok(bytes
            .as_chunks::<32>()
            .0
            .iter()
            .filter_map(|c| ObjectId::from_slice(c).ok())
            .collect())
    }

    pub fn set_heads(&self, heads: &[ObjectId]) -> Result<()> {
        let mut buf = Vec::with_capacity(heads.len() * 32);
        for h in heads {
            buf.extend_from_slice(&h.0);
        }
        let w = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = w.open_table(META).map_err(db_err)?;
            t.insert("heads", buf.as_slice()).map_err(db_err)?;
        }
        w.commit().map_err(db_err)
    }

    pub fn meta(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let r = self.db.begin_read().map_err(db_err)?;
        let t = r.open_table(META).map_err(db_err)?;
        Ok(t.get(key).map_err(db_err)?.map(|v| v.value().to_vec()))
    }

    pub fn set_meta(&self, key: &str, value: &[u8]) -> Result<()> {
        let w = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = w.open_table(META).map_err(db_err)?;
            t.insert(key, value).map_err(db_err)?;
        }
        w.commit().map_err(db_err)
    }

    pub fn idem_get(&self, author: &EntityId, idem: &[u8; 16]) -> Result<Option<ObjectId>> {
        let mut k = Vec::with_capacity(32);
        k.extend_from_slice(&author.0);
        k.extend_from_slice(idem);
        let r = self.db.begin_read().map_err(db_err)?;
        let t = r.open_table(IDEM).map_err(db_err)?;
        Ok(t.get(k.as_slice())
            .map_err(db_err)?
            .and_then(|v| ObjectId::from_slice(v.value()).ok()))
    }

    pub fn idem_put(&self, author: &EntityId, idem: &[u8; 16], op: &ObjectId) -> Result<()> {
        let mut k = Vec::with_capacity(32);
        k.extend_from_slice(&author.0);
        k.extend_from_slice(idem);
        let w = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = w.open_table(IDEM).map_err(db_err)?;
            t.insert(k.as_slice(), op.0.as_slice()).map_err(db_err)?;
        }
        w.commit().map_err(db_err)
    }

    /// The type tag and bytes of an object.
    pub fn get_tagged(&self, id: &ObjectId) -> Result<Option<(String, Vec<u8>)>> {
        let r = self.db.begin_read().map_err(db_err)?;
        let t = r.open_table(OBJECTS).map_err(db_err)?;
        let Some(v) = t.get(id.0.as_slice()).map_err(db_err)? else {
            return Ok(None);
        };
        Ok(Some(split_record(v.value())))
    }

    /// Number of objects.
    pub fn len(&self) -> Result<u64> {
        let r = self.db.begin_read().map_err(db_err)?;
        let t = r.open_table(OBJECTS).map_err(db_err)?;
        t.len().map_err(db_err)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Every object ID, for packing and garbage collection.
    pub fn ids(&self) -> Result<Vec<ObjectId>> {
        let r = self.db.begin_read().map_err(db_err)?;
        let t = r.open_table(OBJECTS).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (k, _) = item.map_err(db_err)?;
            if let Ok(id) = ObjectId::from_slice(k.value()) {
                out.push(id);
            }
        }
        Ok(out)
    }
}

fn split_record(rec: &[u8]) -> (String, Vec<u8>) {
    let n = rec[0] as usize;
    let tag = String::from_utf8_lossy(&rec[1..1 + n]).into_owned();
    (tag, rec[1 + n..].to_vec())
}

impl ObjectStore for RedbStore {
    fn meta(&self, key: &str) -> tessra_core::Result<Option<Vec<u8>>> {
        RedbStore::meta(self, key).map_err(|e| tessra_core::Error::Store(e.to_string()))
    }

    fn put_bytes(&self, tag: &str, bytes: &[u8]) -> tessra_core::Result<ObjectId> {
        let id = hash::object_id(tag, bytes);
        if let Some(map) = self.pending.lock().unwrap().as_mut() {
            map.entry(id).or_insert_with(|| {
                let mut rec = Vec::with_capacity(1 + tag.len() + bytes.len());
                rec.push(tag.len() as u8);
                rec.extend_from_slice(tag.as_bytes());
                rec.extend_from_slice(bytes);
                rec
            });
            return Ok(id);
        }
        let w = self
            .db
            .begin_write()
            .map_err(|e| tessra_core::Error::Store(e.to_string()))?;
        {
            let mut t = w
                .open_table(OBJECTS)
                .map_err(|e| tessra_core::Error::Store(e.to_string()))?;
            let exists = t
                .get(id.0.as_slice())
                .map_err(|e| tessra_core::Error::Store(e.to_string()))?
                .is_some();
            if !exists {
                let mut rec = Vec::with_capacity(1 + tag.len() + bytes.len());
                rec.push(tag.len() as u8);
                rec.extend_from_slice(tag.as_bytes());
                rec.extend_from_slice(bytes);
                t.insert(id.0.as_slice(), rec.as_slice())
                    .map_err(|e| tessra_core::Error::Store(e.to_string()))?;
            }
        }
        w.commit()
            .map_err(|e| tessra_core::Error::Store(e.to_string()))?;
        Ok(id)
    }

    fn get_bytes(&self, id: &ObjectId) -> tessra_core::Result<Option<Vec<u8>>> {
        if let Some(map) = self.pending.lock().unwrap().as_ref() {
            if let Some(rec) = map.get(id) {
                return Ok(Some(split_record(rec).1));
            }
        }
        let r = self
            .db
            .begin_read()
            .map_err(|e| tessra_core::Error::Store(e.to_string()))?;
        let t = r
            .open_table(OBJECTS)
            .map_err(|e| tessra_core::Error::Store(e.to_string()))?;
        let Some(v) = t
            .get(id.0.as_slice())
            .map_err(|e| tessra_core::Error::Store(e.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(split_record(v.value()).1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_core::object::Tree;

    #[test]
    fn persists_objects_heads_and_idem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("objects.redb");
        let tree_id;
        let blob_id;
        {
            let s = RedbStore::open(&path).unwrap();
            tree_id = s.put(&Tree::empty()).unwrap();
            blob_id = s.put_blob(b"hello").unwrap();
            s.set_heads(&[tree_id, blob_id]).unwrap();
            s.idem_put(&EntityId([1; 16]), &[2; 16], &tree_id).unwrap();
            assert_eq!(s.len().unwrap(), 2);
        }
        let s = RedbStore::open(&path).unwrap();
        let t: Tree = s.get(&tree_id).unwrap();
        assert_eq!(t, Tree::empty());
        assert_eq!(s.get_bytes(&blob_id).unwrap().unwrap(), b"hello");
        assert_eq!(s.get_tagged(&blob_id).unwrap().unwrap().0, "blob");
        assert_eq!(s.heads().unwrap(), vec![tree_id, blob_id]);
        assert_eq!(
            s.idem_get(&EntityId([1; 16]), &[2; 16]).unwrap(),
            Some(tree_id)
        );
        assert_eq!(s.idem_get(&EntityId([1; 16]), &[3; 16]).unwrap(), None);
        let mut ids = s.ids().unwrap();
        ids.sort();
        let mut want = vec![tree_id, blob_id];
        want.sort();
        assert_eq!(ids, want);
    }

    #[test]
    fn tampered_object_is_detected_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(&dir.path().join("o.redb")).unwrap();
        let id = s.put(&Tree::empty()).unwrap();
        // Overwrite the record bytes behind the store's back.
        {
            let w = s.db.begin_write().unwrap();
            {
                let mut t = w.open_table(OBJECTS).unwrap();
                let mut rec = Vec::new();
                rec.push(4u8);
                rec.extend_from_slice(b"tree");
                rec.extend_from_slice(b"\xa1\x61x\x01");
                t.insert(id.0.as_slice(), rec.as_slice()).unwrap();
            }
            w.commit().unwrap();
        }
        let err = s.get::<Tree>(&id).unwrap_err();
        assert!(err.to_string().contains("do not hash"), "{err}");
    }

    #[test]
    fn batch_writes_once_and_reads_see_pending() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(&dir.path().join("o.redb")).unwrap();
        s.begin_batch();
        let a = s.put_blob(b"a").unwrap();
        let b = s.put_blob(b"b").unwrap();
        assert_eq!(s.get_bytes(&a).unwrap().unwrap(), b"a");
        assert_eq!(s.len().unwrap(), 0);
        assert_eq!(s.end_batch().unwrap(), 2);
        assert_eq!(s.len().unwrap(), 2);
        assert_eq!(s.get_bytes(&b).unwrap().unwrap(), b"b");
    }
}
