//! BLAKE3 hashing with per-type domain separation, and the object ID type.

use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{Error, Result};

/// A 32-byte content hash. The identity of every content-addressed object.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    pub const LEN: usize = 32;
    /// Minimum display prefix in hex characters.
    pub const MIN_PREFIX: usize = 8;

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let v = hex::decode(s).map_err(|e| Error::Id(format!("object id hex: {e}")))?;
        Self::from_slice(&v)
    }

    pub fn from_slice(v: &[u8]) -> Result<Self> {
        if v.len() != 32 {
            return Err(Error::Id(format!(
                "object id must be 32 bytes, got {}",
                v.len()
            )));
        }
        let mut a = [0u8; 32];
        a.copy_from_slice(v);
        Ok(ObjectId(a))
    }

    /// True if this ID starts with the given hex prefix (case-insensitive).
    pub fn matches_prefix(&self, prefix: &str) -> bool {
        let h = self.to_hex();
        h.starts_with(&prefix.to_ascii_lowercase())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", &self.to_hex()[..12])
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> de::Visitor<'de> for V {
            type Value = ObjectId;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("32 bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> std::result::Result<ObjectId, E> {
                ObjectId::from_slice(v).map_err(E::custom)
            }
            fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> std::result::Result<ObjectId, E> {
                ObjectId::from_slice(&v).map_err(E::custom)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<ObjectId, A::Error> {
                let mut v = Vec::with_capacity(32);
                while let Some(b) = seq.next_element::<u8>()? {
                    v.push(b);
                }
                ObjectId::from_slice(&v).map_err(de::Error::custom)
            }
        }
        d.deserialize_bytes(V)
    }
}

/// Hash canonical object bytes under the type's domain: `BLAKE3("tessra:" || t || "\n" || bytes)`.
pub fn object_id(tag: &str, bytes: &[u8]) -> ObjectId {
    let mut h = blake3::Hasher::new();
    h.update(b"tessra:");
    h.update(tag.as_bytes());
    h.update(b"\n");
    h.update(bytes);
    ObjectId(*h.finalize().as_bytes())
}

/// Hash raw blob bytes.
pub fn blob_id(bytes: &[u8]) -> ObjectId {
    object_id("blob", bytes)
}

/// Hash an artifact chunk.
pub fn chunk_id(bytes: &[u8]) -> ObjectId {
    object_id("chunk", bytes)
}

/// Hash full artifact content.
pub fn artifact_content_hash(bytes: &[u8]) -> ObjectId {
    object_id("artifact-content", bytes)
}

/// Streaming hasher for large content under a domain.
pub struct DomainHasher(blake3::Hasher);

impl DomainHasher {
    pub fn new(domain: &str) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"tessra:");
        h.update(domain.as_bytes());
        h.update(b"\n");
        DomainHasher(h)
    }
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    pub fn finalize(self) -> ObjectId {
        ObjectId(*self.0.finalize().as_bytes())
    }
}

/// The shard slot for a tree entry name: first byte of `BLAKE3("tessra:shard" || name)`.
pub fn shard_slot(name: &str) -> u8 {
    let mut h = blake3::Hasher::new();
    h.update(b"tessra:shard");
    h.update(name.as_bytes());
    h.finalize().as_bytes()[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_separate() {
        let a = object_id("blob", b"x");
        let b = object_id("tree", b"x");
        assert_ne!(a, b);
        assert_eq!(a, blob_id(b"x"));
    }

    #[test]
    fn hex_round_trip_and_prefix() {
        let id = blob_id(b"hello");
        let h = id.to_hex();
        assert_eq!(ObjectId::from_hex(&h).unwrap(), id);
        assert!(id.matches_prefix(&h[..8].to_uppercase()));
        assert!(!id.matches_prefix("zzzz"));
    }

    #[test]
    fn serde_as_bytes() {
        let id = blob_id(b"q");
        let bytes = crate::cbor::to_canonical_bytes(&id).unwrap();
        assert_eq!(bytes[0], 0x58); // bytes, 1-byte length follows
        assert_eq!(bytes[1], 32);
        let back: ObjectId = crate::cbor::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(back, id);
    }
}
