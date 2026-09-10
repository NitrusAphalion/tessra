//! Entity IDs: 16 random bytes, rendered in the `klmnopqrstuvwxyz` alphabet
//! so they are visibly not hashes.

use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{Error, Result};

const ALPHABET: &[u8; 16] = b"klmnopqrstuvwxyz";

/// The identity of an entity: a change, intent, memory, principal, and so on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(pub [u8; 16]);

impl EntityId {
    pub const LEN: usize = 16;
    /// Minimum display prefix in letters.
    pub const MIN_PREFIX: usize = 4;

    /// A fresh random ID from the OS entropy source.
    pub fn random() -> Self {
        let mut b = [0u8; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
        EntityId(b)
    }

    /// Deterministic derivation, used for legacy change IDs from git commits.
    pub fn derive(domain: &str, input: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"tessra:");
        h.update(domain.as_bytes());
        h.update(input);
        let out = h.finalize();
        let mut b = [0u8; 16];
        b.copy_from_slice(&out.as_bytes()[..16]);
        EntityId(b)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn from_slice(v: &[u8]) -> Result<Self> {
        if v.len() != 16 {
            return Err(Error::Id(format!(
                "entity id must be 16 bytes, got {}",
                v.len()
            )));
        }
        let mut a = [0u8; 16];
        a.copy_from_slice(v);
        Ok(EntityId(a))
    }

    /// Thirty-two letters, one per nibble, high nibble first.
    pub fn to_letters(&self) -> String {
        let mut s = String::with_capacity(32);
        for b in self.0 {
            s.push(ALPHABET[(b >> 4) as usize] as char);
            s.push(ALPHABET[(b & 0x0f) as usize] as char);
        }
        s
    }

    pub fn from_letters(s: &str) -> Result<Self> {
        if s.len() != 32 {
            return Err(Error::Id(format!(
                "entity id must be 32 letters, got {}",
                s.len()
            )));
        }
        let mut b = [0u8; 16];
        let bytes = s.as_bytes();
        for i in 0..16 {
            let hi = nibble(bytes[2 * i])?;
            let lo = nibble(bytes[2 * i + 1])?;
            b[i] = (hi << 4) | lo;
        }
        Ok(EntityId(b))
    }

    pub fn matches_prefix(&self, prefix: &str) -> bool {
        self.to_letters().starts_with(prefix)
    }
}

fn nibble(c: u8) -> Result<u8> {
    ALPHABET
        .iter()
        .position(|&a| a == c)
        .map(|p| p as u8)
        .ok_or_else(|| Error::Id(format!("invalid entity id letter {:?}", c as char)))
}

impl fmt::Debug for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntityId({})", &self.to_letters()[..8])
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_letters())
    }
}

impl Serialize for EntityId {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for EntityId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> de::Visitor<'de> for V {
            type Value = EntityId;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("16 bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> std::result::Result<EntityId, E> {
                EntityId::from_slice(v).map_err(E::custom)
            }
            fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> std::result::Result<EntityId, E> {
                EntityId::from_slice(&v).map_err(E::custom)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<EntityId, A::Error> {
                let mut v = Vec::with_capacity(16);
                while let Some(b) = seq.next_element::<u8>()? {
                    v.push(b);
                }
                EntityId::from_slice(&v).map_err(de::Error::custom)
            }
        }
        d.deserialize_bytes(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_round_trip() {
        let id = EntityId::random();
        let s = id.to_letters();
        assert_eq!(s.len(), 32);
        assert!(s.bytes().all(|c| ALPHABET.contains(&c)));
        assert_eq!(EntityId::from_letters(&s).unwrap(), id);
    }

    #[test]
    fn known_encoding() {
        let id = EntityId([0x00, 0x1f, 0xf0, 0xab, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let s = id.to_letters();
        assert!(s.starts_with("kklzzkuv"));
        assert!(EntityId::from_letters("k").is_err());
        assert!(EntityId::from_letters(&"a".repeat(32)).is_err());
    }

    #[test]
    fn derive_is_deterministic() {
        let a = EntityId::derive("legacy-cid", b"abc");
        let b = EntityId::derive("legacy-cid", b"abc");
        assert_eq!(a, b);
        assert_ne!(a, EntityId::derive("legacy-cid", b"abd"));
    }
}
