//! The object store contract and an in-memory implementation.
//!
//! A store maps object IDs to bytes. It never interprets them beyond the
//! type tag. Persistent implementations live in `tessra-store`.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::cbor::{self, TessraObject};
use crate::{hash, Error, ObjectId, Result};

/// Content-addressed storage of encoded objects.
pub trait ObjectStore: Send + Sync {
    /// Store already-canonical bytes of type `tag`. Returns the ID. Idempotent.
    fn put_bytes(&self, tag: &str, bytes: &[u8]) -> Result<ObjectId>;

    /// Fetch the bytes for an ID, if present.
    fn get_bytes(&self, id: &ObjectId) -> Result<Option<Vec<u8>>>;

    fn has(&self, id: &ObjectId) -> Result<bool> {
        Ok(self.get_bytes(id)?.is_some())
    }

    /// Machine-local metadata beside the objects, such as caches and
    /// indexes. Not content; never synced. A store without it reads nothing.
    fn meta(&self, _key: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Encode and store a structured object.
    fn put<T: TessraObject>(&self, obj: &T) -> Result<ObjectId>
    where
        Self: Sized,
    {
        let e = cbor::encode(obj)?;
        let id = self.put_bytes(T::TAG, &e.bytes)?;
        debug_assert_eq!(id, e.id);
        Ok(id)
    }

    /// Fetch and decode a structured object, failing if absent.
    fn get<T: TessraObject>(&self, id: &ObjectId) -> Result<T>
    where
        Self: Sized,
    {
        let bytes = self.get_bytes(id)?.ok_or(Error::NotFound(*id))?;
        let expected = hash::object_id(T::TAG, &bytes);
        if expected != *id {
            return Err(Error::Store(format!(
                "bytes for {id} do not hash to it as {}",
                T::TAG
            )));
        }
        cbor::decode(&bytes)
    }

    /// Store a raw blob.
    fn put_blob(&self, bytes: &[u8]) -> Result<ObjectId> {
        self.put_bytes("blob", bytes)
    }
}

/// An in-memory store for tests and for ephemeral computation.
#[derive(Default)]
pub struct MemoryStore {
    inner: RwLock<HashMap<ObjectId, Vec<u8>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ObjectStore for MemoryStore {
    fn put_bytes(&self, tag: &str, bytes: &[u8]) -> Result<ObjectId> {
        let id = hash::object_id(tag, bytes);
        self.inner
            .write()
            .unwrap()
            .entry(id)
            .or_insert_with(|| bytes.to_vec());
        Ok(id)
    }

    fn get_bytes(&self, id: &ObjectId) -> Result<Option<Vec<u8>>> {
        Ok(self.inner.read().unwrap().get(id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_round_trip() {
        let s = MemoryStore::new();
        let id = s.put_blob(b"hello").unwrap();
        assert_eq!(id, hash::blob_id(b"hello"));
        assert_eq!(s.get_bytes(&id).unwrap().unwrap(), b"hello");
        assert!(s.has(&id).unwrap());
        assert!(!s.has(&hash::blob_id(b"nope")).unwrap());
    }
}
