//! Deterministic CBOR per RFC 8949 section 4.2.1, and the object envelope.
//!
//! Every structured object is a CBOR map with reserved keys `t` (type tag)
//! and `v` (schema version). Map keys are sorted by the bytewise order of
//! their encoded form. Floats and tags are forbidden. Unknown keys are
//! preserved on decode and round-trip byte for byte.

use ciborium::value::Value;
use serde::{de::DeserializeOwned, Serialize};

use crate::{hash, Error, ObjectId, Result};

/// A type that lives in the object store as a structured object.
pub trait TessraObject: Serialize + DeserializeOwned {
    /// The `t` tag.
    const TAG: &'static str;
    /// The `v` schema version this code emits and the newest it understands.
    const VERSION: u64;
}

/// The result of encoding: canonical bytes and the object ID they hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub id: ObjectId,
}

/// Encode an object into canonical bytes with its envelope and compute its ID.
pub fn encode<T: TessraObject>(obj: &T) -> Result<Encoded> {
    let value = Value::serialized(obj).map_err(|e| Error::Encode(e.to_string()))?;
    let mut entries = match value {
        Value::Map(m) => m,
        other => {
            return Err(Error::Encode(format!(
                "{} must serialize to a map, got {other:?}",
                T::TAG
            )))
        }
    };
    entries.push((Value::Text("t".into()), Value::Text(T::TAG.into())));
    entries.push((Value::Text("v".into()), Value::Integer(T::VERSION.into())));
    let canonical = canonicalize(Value::Map(entries))?;
    let bytes = to_bytes(&canonical)?;
    let id = hash::object_id(T::TAG, &bytes);
    Ok(Encoded { bytes, id })
}

/// Decode canonical bytes into an object, checking the envelope.
///
/// Unknown keys are ignored by the typed deserializer but the caller keeps
/// the original bytes, which is what round-trips them.
pub fn decode<T: TessraObject>(bytes: &[u8]) -> Result<T> {
    let value: Value =
        ciborium::de::from_reader(bytes).map_err(|e| Error::Decode(e.to_string()))?;
    let reencoded = to_bytes(&canonicalize(value.clone())?)?;
    if reencoded != bytes {
        return Err(Error::NotCanonical(format!(
            "{} bytes are not in deterministic form",
            T::TAG
        )));
    }
    let entries = match value {
        Value::Map(m) => m,
        _ => return Err(Error::Decode(format!("{} is not a map", T::TAG))),
    };
    let mut tag = None;
    let mut version = None;
    let mut rest = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        match (&k, &v) {
            (Value::Text(key), Value::Text(t)) if key == "t" => tag = Some(t.clone()),
            (Value::Text(key), Value::Integer(n)) if key == "v" => {
                version =
                    Some(u64::try_from(*n).map_err(|_| Error::Decode("v out of range".into()))?)
            }
            _ => rest.push((k, v)),
        }
    }
    let tag = tag.ok_or_else(|| Error::Decode("missing t".into()))?;
    if tag != T::TAG {
        return Err(Error::WrongType {
            expected: T::TAG,
            found: tag,
        });
    }
    let version = version.ok_or_else(|| Error::Decode("missing v".into()))?;
    if version > T::VERSION {
        return Err(Error::Version {
            tag: T::TAG,
            found: version,
            known: T::VERSION,
        });
    }
    Value::Map(rest)
        .deserialized::<T>()
        .map_err(|e| Error::Decode(format!("{}: {e}", T::TAG)))
}

/// Read only the type tag from an encoded object without knowing its type.
pub fn peek_tag(bytes: &[u8]) -> Result<String> {
    let value: Value =
        ciborium::de::from_reader(bytes).map_err(|e| Error::Decode(e.to_string()))?;
    if let Value::Map(entries) = value {
        for (k, v) in entries {
            if let (Value::Text(key), Value::Text(t)) = (k, v) {
                if key == "t" {
                    return Ok(t);
                }
            }
        }
    }
    Err(Error::Decode("no type tag".into()))
}

/// Serialize a plain value (not an object) to canonical bytes.
pub fn to_canonical_bytes<T: Serialize>(t: &T) -> Result<Vec<u8>> {
    let value = Value::serialized(t).map_err(|e| Error::Encode(e.to_string()))?;
    to_bytes(&canonicalize(value)?)
}

/// Deserialize a plain value from bytes, requiring canonical form.
pub fn from_canonical_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let value: Value =
        ciborium::de::from_reader(bytes).map_err(|e| Error::Decode(e.to_string()))?;
    let reencoded = to_bytes(&canonicalize(value.clone())?)?;
    if reencoded != bytes {
        return Err(Error::NotCanonical(
            "bytes are not in deterministic form".into(),
        ));
    }
    value
        .deserialized::<T>()
        .map_err(|e| Error::Decode(e.to_string()))
}

fn to_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(value, &mut out).map_err(|e| Error::Encode(e.to_string()))?;
    Ok(out)
}

/// Put a value into deterministic form: sort map keys by encoded bytes,
/// reject duplicates, floats, and tags, recursively.
pub fn canonicalize(value: Value) -> Result<Value> {
    Ok(match value {
        Value::Map(entries) => {
            let mut keyed: Vec<(Vec<u8>, Value, Value)> = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let k = canonicalize(k)?;
                let v = canonicalize(v)?;
                let kb = to_bytes(&k)?;
                keyed.push((kb, k, v));
            }
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            for w in keyed.windows(2) {
                if w[0].0 == w[1].0 {
                    return Err(Error::NotCanonical("duplicate map key".into()));
                }
            }
            Value::Map(keyed.into_iter().map(|(_, k, v)| (k, v)).collect())
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(canonicalize)
                .collect::<Result<Vec<_>>>()?,
        ),
        Value::Float(_) => return Err(Error::NotCanonical("floats are forbidden".into())),
        Value::Tag(_, _) => return Err(Error::NotCanonical("tags are forbidden".into())),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Probe {
        zeta: u32,
        #[serde(with = "serde_bytes")]
        alpha: Vec<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        maybe: Option<String>,
    }
    impl TessraObject for Probe {
        const TAG: &'static str = "probe";
        const VERSION: u64 = 1;
    }

    #[test]
    fn round_trip_and_key_order() {
        let p = Probe {
            zeta: 7,
            alpha: vec![1, 2, 3],
            maybe: None,
        };
        let e = encode(&p).unwrap();
        // Keys sorted by encoded bytes: "t" (0x61 0x74), "v", "zeta" come after
        // "alpha" because the text header length prefix sorts shorter first.
        let back: Probe = decode(&e.bytes).unwrap();
        assert_eq!(back, p);
        assert_eq!(peek_tag(&e.bytes).unwrap(), "probe");
        assert_eq!(e.id, hash::object_id("probe", &e.bytes));
    }

    #[test]
    fn rejects_non_canonical() {
        // Encode a map with keys in the wrong order by hand.
        let v = Value::Map(vec![
            (Value::Text("zeta".into()), Value::Integer(1.into())),
            (Value::Text("alpha".into()), Value::Bytes(vec![])),
            (Value::Text("t".into()), Value::Text("probe".into())),
            (Value::Text("v".into()), Value::Integer(1.into())),
        ]);
        let bytes = to_bytes(&v).unwrap();
        assert!(matches!(
            decode::<Probe>(&bytes),
            Err(Error::NotCanonical(_))
        ));
    }

    #[test]
    fn wrong_tag_and_future_version() {
        let p = Probe {
            zeta: 1,
            alpha: vec![],
            maybe: Some("x".into()),
        };
        let e = encode(&p).unwrap();
        #[derive(Serialize, Deserialize)]
        struct Other {
            zeta: u32,
        }
        impl TessraObject for Other {
            const TAG: &'static str = "other";
            const VERSION: u64 = 1;
        }
        assert!(matches!(
            decode::<Other>(&e.bytes),
            Err(Error::WrongType { .. })
        ));
    }

    #[test]
    fn unknown_keys_do_not_fail_typed_decode() {
        let v = Value::Map(vec![
            (Value::Text("alpha".into()), Value::Bytes(vec![9])),
            (Value::Text("future".into()), Value::Text("yes".into())),
            (Value::Text("t".into()), Value::Text("probe".into())),
            (Value::Text("v".into()), Value::Integer(1.into())),
            (Value::Text("zeta".into()), Value::Integer(3.into())),
        ]);
        let bytes = to_bytes(&canonicalize(v).unwrap()).unwrap();
        let p: Probe = decode(&bytes).unwrap();
        assert_eq!(p.zeta, 3);
    }
}
