# 01. Conventions

Draft 2. Changes from draft 1: unknown keys are preserved, not rejected (B10); signature verification is strict (S17); no verification step compares timestamps (B6); large trees are sharded deterministically (S5).

## Encoding

Every structured object is a CBOR map encoded in the deterministic form of RFC 8949 section 4.2.1: shortest-form integers and lengths, definite lengths only, map keys sorted by their encoded bytes, no duplicate keys, no indefinite-length items, no tags except where this spec names one. Floating point is not used anywhere in the object model. Ratios such as confidence are stored as integers in a stated unit.

Two keys are reserved in every structured object:

| Key | Type | Meaning |
|---|---|---|
| `t` | text | The type tag, one of the tags in 02-objects.md |
| `v` | uint | The schema version of that type. Starts at 1 |

Blobs and artifact chunks are raw bytes with no envelope.

Text fields are UTF-8. Names that appear on disk, meaning tree entry names and root names, are NFC-normalized before encoding and MUST NOT contain `/`, `\`, NUL, or be empty, `.`, or `..`. Names that Windows cannot materialize are legal in a tree and are flagged at snapshot time, see 02-objects.md trackingrules.

**Unknown keys.** A reader MUST preserve keys it does not know, byte for byte, and MUST NOT reject an object for containing them. A validator checks the keys it knows. This is what lets a newer writer add an optional field without breaking older readers, and the hash covers the bytes regardless, so integrity is unaffected. A reader that encounters a `v` newer than it knows treats the object as opaque: it stores, hashes, and forwards it, and does not interpret it.

## Hashing

The hash function is BLAKE3 with a 32-byte output.

The object ID of a structured object is BLAKE3 over a domain string followed by the object's canonical CBOR bytes:

```
id = BLAKE3( "tessra:" || t || "\n" || bytes )
```

The object ID of a blob is BLAKE3 over `"tessra:blob\n"` followed by its raw bytes. Artifact chunks use domain `tessra:chunk`. The domain prefix means an object of one type can never collide with an object of another type or with raw content that shares its bytes.

Two other hashes are defined where they are used: the artifact content hash and the node body hash, both in 02-objects.md.

## Identifiers

Three kinds of identifier, never mixed.

**Object IDs** are content hashes, 32 bytes. Everything content-addressed is referenced by object ID. API rendering is lowercase hex, 64 characters. Human rendering is the shortest unique prefix, minimum 8 characters. Lookup by object ID accepts a unique prefix.

**Entity IDs** identify things that have versions: changes, intents, memories, standards, hooks, principals, channels, lines, targets, claims, nodes, workspaces. They are 16 bytes from a cryptographically secure random source, generated once at creation. API rendering is 32 characters from the alphabet `klmnopqrstuvwxyz`, one letter per nibble in order, so an entity ID is visibly not a hash. Human rendering is the shortest unique prefix, minimum 4 characters. Lookup accepts a unique prefix. This is the same alphabet Jujutsu uses for change IDs, which is confusable in a repository that also runs Jujutsu; that is accepted (A4).

**Principal keys** are Ed25519 public keys, 32 bytes. A principal is an entity with an entity ID; its key is a field that changes on rotation. Ops reference principals by entity ID.

Type prefixes such as `rev:` or `mem:` MAY appear in human rendering and are never part of a canonical identifier.

## Time

Timestamps are signed 64-bit integers of nanoseconds since the Unix epoch, UTC. They are informational. Ordering of anything that matters is by the operation DAG. No verification step anywhere in this spec compares a timestamp to another timestamp or to a clock. Expiry of keys, capabilities, and claims is enforced by the daemon that holds the relevant key or hosts the relevant state, never by a replica verifying an op (04-security.md).

## Signatures

Signatures are Ed25519 per RFC 8032. A signed object carries a `sig` field of 64 bytes computed over the object without its `sig` field:

```
payload = canonical CBOR of the object with `sig` removed
sig     = Ed25519.sign( sk, BLAKE3( "tessra:sig:" || t || "\n" || payload ) )
```

Verification MUST be strict: reject signatures whose S component is not in canonical reduced form, and reject public keys or R points of small order. Ed25519 is deterministic, so a given key and payload produce exactly one canonical signature and exactly one object ID.

The object ID of a signed object is the hash of the full object including `sig`. Signed types: op, attestation, capability, principal, release, anchor, and manifest.

Human approvals produced through a passkey carry a WebAuthn assertion instead of a raw Ed25519 signature. The attestation type has a `sigkind` field for this; 04-security.md defines the challenge binding.

## Schema versions

`v` is bumped only on an incompatible change: a required field added, a field's meaning changed, an invariant changed. Adding an optional field never bumps `v`. There is no migration of stored objects: old versions stay readable forever, and a writer emits the newest version it knows.

## Sizes

| Limit | Default | Note |
|---|---|---|
| Blob size above which content MUST be an artifact | 8 MiB | Configurable per repository in tracking rules |
| Artifact chunking | FastCDC, min 256 KiB, average 1 MiB, max 4 MiB | `OPEN:` parameters. Default as stated |
| Tree sharding threshold | 4,096 entries | A tree with more entries MUST be sharded, and one with fewer MUST NOT be, so the encoding of a directory is unique. See 02-objects.md |
| Maximum structured object size | 16 MiB | Sharding keeps trees under it; a nodeindex over it is split per path prefix the same way |
| Entity ID display minimum | 4 letters | |
| Object ID display minimum | 8 hex characters | |
