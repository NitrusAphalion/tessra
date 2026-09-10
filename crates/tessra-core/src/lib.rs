//! Tessra core: the object model as specified in `spec/`.
//!
//! This crate is the durable contract. Everything here maps one to one onto
//! `spec/01-conventions.md` and `spec/02-objects.md`, plus the trie that the
//! view is built from. It has no I/O beyond an in-memory store used for tests;
//! persistent stores live in other crates.

pub mod cbor;
pub mod hash;
pub mod id;
pub mod object;
pub mod sig;
pub mod store;
pub mod trie;

pub use hash::ObjectId;
pub use id::EntityId;

/// Errors raised by the core crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("encoding: {0}")]
    Encode(String),
    #[error("decoding: {0}")]
    Decode(String),
    #[error("not canonical: {0}")]
    NotCanonical(String),
    #[error("wrong type tag: expected {expected}, found {found}")]
    WrongType {
        expected: &'static str,
        found: String,
    },
    #[error("unsupported schema version {found} for {tag} (known up to {known})")]
    Version {
        tag: &'static str,
        found: u64,
        known: u64,
    },
    #[error("invalid identifier: {0}")]
    Id(String),
    #[error("signature: {0}")]
    Signature(String),
    #[error("object not found: {0}")]
    NotFound(ObjectId),
    #[error("invariant violated: {0}")]
    Invariant(String),
    #[error("store: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, Error>;
