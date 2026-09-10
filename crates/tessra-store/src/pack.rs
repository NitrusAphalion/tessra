//! The GVPK pack format from `spec/05-layout.md`: an append-only file of
//! `(id, type, len, bytes)` records with a BLAKE3 trailer. Compression is
//! not applied in M1; the `len` field is the stored length and the format
//! reserves zstd for later without changing the layout.

use std::io::{Read, Write};

use tessra_core::hash;
use tessra_core::store::ObjectStore;
use tessra_core::ObjectId;

use crate::{Error, Result};

const MAGIC: &[u8; 4] = b"GVPK";
const VERSION: u8 = 1;

/// A record ready to be written.
pub struct Record {
    pub id: ObjectId,
    pub tag: String,
    pub bytes: Vec<u8>,
}

fn write_varint<W: Write>(w: &mut W, mut n: u64) -> std::io::Result<()> {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            w.write_all(&[b])?;
            return Ok(());
        }
        w.write_all(&[b | 0x80])?;
    }
}

fn read_varint<R: Read>(r: &mut R) -> std::io::Result<u64> {
    let mut n = 0u64;
    let mut shift = 0;
    loop {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        n |= ((b[0] & 0x7f) as u64) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(n);
        }
        shift += 7;
        if shift > 63 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "varint too long",
            ));
        }
    }
}

/// Write a pack of records.
pub fn write<W: Write>(mut w: W, records: &[Record]) -> Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(MAGIC);
    body.push(VERSION);
    for r in records {
        body.extend_from_slice(&r.id.0);
        write_varint(&mut body, r.tag.len() as u64)?;
        body.extend_from_slice(r.tag.as_bytes());
        write_varint(&mut body, r.bytes.len() as u64)?;
        body.extend_from_slice(&r.bytes);
    }
    let trailer = blake3::hash(&body);
    w.write_all(&body)?;
    w.write_all(trailer.as_bytes())?;
    Ok(())
}

/// Read and verify a pack: trailer, then every record's hash under its tag.
pub fn read(bytes: &[u8]) -> Result<Vec<Record>> {
    if bytes.len() < 4 + 1 + 32 {
        return Err(Error::Pack("too short".into()));
    }
    let (body, trailer) = bytes.split_at(bytes.len() - 32);
    if blake3::hash(body).as_bytes() != trailer {
        return Err(Error::Pack("trailer mismatch".into()));
    }
    if &body[..4] != MAGIC || body[4] != VERSION {
        return Err(Error::Pack("bad magic or version".into()));
    }
    let mut cur = &body[5..];
    let mut out = Vec::new();
    while !cur.is_empty() {
        let mut idb = [0u8; 32];
        cur.read_exact(&mut idb)?;
        let tag_len = read_varint(&mut cur)? as usize;
        let mut tag = vec![0u8; tag_len];
        cur.read_exact(&mut tag)?;
        let tag = String::from_utf8(tag).map_err(|_| Error::Pack("tag not utf-8".into()))?;
        let len = read_varint(&mut cur)? as usize;
        let mut data = vec![0u8; len];
        cur.read_exact(&mut data)?;
        let id = ObjectId(idb);
        if hash::object_id(&tag, &data) != id {
            return Err(Error::Pack(format!("record {id} does not hash to its id")));
        }
        out.push(Record {
            id,
            tag,
            bytes: data,
        });
    }
    Ok(out)
}

/// Import every record of a pack into a store.
pub fn import<S: ObjectStore>(store: &S, bytes: &[u8]) -> Result<usize> {
    let records = read(bytes)?;
    for r in &records {
        store.put_bytes(&r.tag, &r.bytes)?;
    }
    Ok(records.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_core::store::MemoryStore;

    #[test]
    fn round_trip_and_tamper() {
        let recs = vec![
            Record {
                id: hash::blob_id(b"a"),
                tag: "blob".into(),
                bytes: b"a".to_vec(),
            },
            Record {
                id: hash::object_id("tree", b"\xa0"),
                tag: "tree".into(),
                bytes: b"\xa0".to_vec(),
            },
        ];
        let mut buf = Vec::new();
        write(&mut buf, &recs).unwrap();
        let back = read(&buf).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].id, recs[0].id);
        let s = MemoryStore::new();
        assert_eq!(import(&s, &buf).unwrap(), 2);
        assert_eq!(s.get_bytes(&recs[0].id).unwrap().unwrap(), b"a");
        let mut bad = buf.clone();
        bad[10] ^= 1;
        assert!(read(&bad).is_err());
    }
}
