//! The semantic index: `spec/02-objects.md` nodeindex, built from tree-sitter.
//!
//! A file is a sequence of units: functions, types, imports, and the
//! equivalent per language, with containers such as impl blocks and classes
//! holding child units. Each unit has a format-insensitive body hash over its
//! token stream, and a stable identity assigned by matching against the
//! previous version of the file. Files without a grammar fall back to chunks.

pub mod extract;
pub mod matching;
pub mod merge;
pub mod rename;

use tessra_core::ObjectId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("parse: {0}")]
    Parse(String),
    #[error(transparent)]
    Core(#[from] tessra_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A unit found in a file, before identity is assigned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawNode {
    pub kind: String,
    pub name: String,
    /// Byte range in the file, end exclusive. Includes leading attributes and doc comments.
    pub span: (usize, usize),
    pub body: ObjectId,
    /// Index into the containing `Vec<RawNode>` of the enclosing container.
    pub parent: Option<usize>,
    /// Members of a set-like region: imports, enum variants, struct fields, class members.
    pub setlike: bool,
    /// Distinct identifiers the unit mentions, its own name excluded, sorted.
    /// Resolved to `Node.deps` against the units of the same file and root.
    pub refs: Vec<String>,
}

/// The languages with a grammar. Everything else is chunked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
}

impl Language {
    pub fn from_path(path: &str) -> Option<Language> {
        let ext = path.rsplit('.').next()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => Language::Rust,
            "py" | "pyi" => Language::Python,
            "js" | "mjs" | "cjs" | "jsx" => Language::JavaScript,
            "ts" | "mts" | "cts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
        }
    }

    pub(crate) fn grammar(&self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }
}

/// Identifier of the grammar set and matcher, recorded on every nodeindex.
pub const GRAMMARS: &str =
    "tessra-semantic/1 tree-sitter-0.27 rust-0.24 python-0.25 javascript-0.25 typescript-0.23";

/// Extract the units of a file. Binary content yields no nodes. Files without
/// a grammar yield chunk nodes at blank-line boundaries.
pub fn extract(path: &str, source: &[u8]) -> Result<Vec<RawNode>> {
    if source.iter().take(8192).any(|&b| b == 0) {
        return Ok(Vec::new());
    }
    match Language::from_path(path) {
        Some(lang) => extract::with_grammar(lang, source),
        None => Ok(extract::chunks(source)),
    }
}

/// Body hash: BLAKE3 with domain `tessra:nodebody` over tokens joined by 0x1F.
pub fn body_hash<'a>(tokens: impl Iterator<Item = &'a [u8]>) -> ObjectId {
    let mut h = tessra_core::hash::DomainHasher::new("nodebody");
    let mut first = true;
    for t in tokens {
        if !first {
            h.update(&[0x1f]);
        }
        first = false;
        h.update(t);
    }
    h.finalize()
}
