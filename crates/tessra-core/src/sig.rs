//! Ed25519 keys and signatures with strict verification and domain separation.
//!
//! `sig = Ed25519.sign(sk, BLAKE3("tessra:sig:" || t || "\n" || payload))`
//! where `payload` is the canonical encoding of the object without its `sig`.

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{cbor::TessraObject, Error, Result};

/// An Ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey(pub [u8; 32]);

/// A 64-byte Ed25519 signature.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Sig(pub [u8; 64]);

/// A private key. Never serialized by this crate.
pub struct SecretKey(SigningKey);

impl SecretKey {
    pub fn generate() -> Self {
        SecretKey(SigningKey::generate(&mut rand_core::OsRng))
    }

    pub fn from_bytes(b: &[u8; 32]) -> Self {
        SecretKey(SigningKey::from_bytes(b))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key().to_bytes())
    }

    /// Sign a payload for an object of type `tag`.
    pub fn sign(&self, tag: &str, payload: &[u8]) -> Sig {
        let msg = signing_message(tag, payload);
        Sig(self.0.sign(&msg).to_bytes())
    }
}

impl PublicKey {
    /// Strict verification: rejects non-canonical signatures and small-order points.
    pub fn verify(&self, tag: &str, payload: &[u8], sig: &Sig) -> Result<()> {
        let vk = VerifyingKey::from_bytes(&self.0)
            .map_err(|e| Error::Signature(format!("bad public key: {e}")))?;
        if vk.is_weak() {
            return Err(Error::Signature("weak public key".into()));
        }
        let signature = Signature::from_bytes(&sig.0);
        let msg = signing_message(tag, payload);
        vk.verify_strict(&msg, &signature)
            .map_err(|_| Error::Signature(format!("invalid signature on {tag}")))
    }

    pub fn from_slice(v: &[u8]) -> Result<Self> {
        if v.len() != 32 {
            return Err(Error::Id(format!(
                "public key must be 32 bytes, got {}",
                v.len()
            )));
        }
        let mut a = [0u8; 32];
        a.copy_from_slice(v);
        Ok(PublicKey(a))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

fn signing_message(tag: &str, payload: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"tessra:sig:");
    h.update(tag.as_bytes());
    h.update(b"\n");
    h.update(payload);
    *h.finalize().as_bytes()
}

/// A signed object: one that carries an optional `sig` field.
pub trait SignedObject: TessraObject + Clone {
    fn sig(&self) -> Option<&Sig>;
    fn set_sig(&mut self, sig: Option<Sig>);

    /// The canonical bytes of this object with `sig` removed.
    fn payload(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.set_sig(None);
        Ok(crate::cbor::encode(&unsigned)?.bytes)
    }

    /// Sign in place.
    fn sign_with(&mut self, key: &SecretKey) -> Result<()> {
        let payload = self.payload()?;
        self.set_sig(Some(key.sign(Self::TAG, &payload)));
        Ok(())
    }

    /// Verify against a public key.
    fn verify_with(&self, key: &PublicKey) -> Result<()> {
        let sig = self
            .sig()
            .ok_or_else(|| Error::Signature(format!("{} is unsigned", Self::TAG)))?;
        let payload = self.payload()?;
        key.verify(Self::TAG, &payload, sig)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", &self.to_hex()[..12])
    }
}

impl fmt::Debug for Sig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sig({})", hex::encode(&self.0[..6]))
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let b = serde_bytes::ByteBuf::deserialize(d)?;
        PublicKey::from_slice(&b).map_err(de::Error::custom)
    }
}

impl Serialize for Sig {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sig {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let b = serde_bytes::ByteBuf::deserialize(d)?;
        if b.len() != 64 {
            return Err(de::Error::custom(format!(
                "signature must be 64 bytes, got {}",
                b.len()
            )));
        }
        let mut a = [0u8; 64];
        a.copy_from_slice(&b);
        Ok(Sig(a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
    struct Note {
        body: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sig: Option<Sig>,
    }
    impl TessraObject for Note {
        const TAG: &'static str = "note";
        const VERSION: u64 = 1;
    }
    impl SignedObject for Note {
        fn sig(&self) -> Option<&Sig> {
            self.sig.as_ref()
        }
        fn set_sig(&mut self, sig: Option<Sig>) {
            self.sig = sig;
        }
    }

    #[test]
    fn sign_and_verify() {
        let k = SecretKey::generate();
        let mut n = Note {
            body: "hi".into(),
            sig: None,
        };
        n.sign_with(&k).unwrap();
        n.verify_with(&k.public()).unwrap();
        let other = SecretKey::generate();
        assert!(n.verify_with(&other.public()).is_err());
        let mut tampered = n.clone();
        tampered.body = "bye".into();
        assert!(tampered.verify_with(&k.public()).is_err());
    }

    #[test]
    fn signature_is_deterministic_and_round_trips() {
        let k = SecretKey::from_bytes(&[7u8; 32]);
        let mut a = Note {
            body: "x".into(),
            sig: None,
        };
        let mut b = a.clone();
        a.sign_with(&k).unwrap();
        b.sign_with(&k).unwrap();
        assert_eq!(a.sig, b.sig);
        let e = crate::cbor::encode(&a).unwrap();
        let back: Note = crate::cbor::decode(&e.bytes).unwrap();
        assert_eq!(back, a);
        back.verify_with(&k.public()).unwrap();
    }
}
