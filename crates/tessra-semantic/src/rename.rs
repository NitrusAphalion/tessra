//! Identifier renames as data: the semantic operation `edit` records, the
//! rename the matcher infers when a unit keeps its identity under a new
//! name, and the whole-word replacement that applies one to text.

/// A rename of one identifier to another. `path` scopes a recorded op to
/// one file; `None` means the whole root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
    pub path: Option<String>,
}

impl Rename {
    pub fn new(from: &str, to: &str) -> Rename {
        Rename {
            from: from.into(),
            to: to.into(),
            path: None,
        }
    }

    pub fn applies_to(&self, path: &str) -> bool {
        self.path.as_deref().is_none_or(|p| p == path)
    }
}

/// An identifier in every supported grammar: ASCII letters, digits, and
/// underscores, not starting with a digit. Unit names that are not
/// identifiers, such as `Trait for Type` or a use path, never rename.
pub fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `ident` occurs in `text` as a whole word.
pub fn contains_ident(text: &[u8], ident: &str) -> bool {
    find_ident(text, ident.as_bytes(), 0).is_some()
}

fn find_ident(text: &[u8], ident: &[u8], from: usize) -> Option<usize> {
    if ident.is_empty() || text.len() < ident.len() {
        return None;
    }
    let mut i = from;
    while i + ident.len() <= text.len() {
        if &text[i..i + ident.len()] == ident
            && (i == 0 || !ident_byte(text[i - 1]))
            && (i + ident.len() == text.len() || !ident_byte(text[i + ident.len()]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Replace whole-word occurrences of `from` with `to`. Returns `None` when
/// nothing matched, so callers can tell an applied rename from a no-op.
pub fn replace_ident(text: &[u8], from: &str, to: &str) -> Option<Vec<u8>> {
    let f = from.as_bytes();
    let mut pos = find_ident(text, f, 0)?;
    let mut out = Vec::with_capacity(text.len() + 16);
    let mut last = 0;
    loop {
        out.extend_from_slice(&text[last..pos]);
        out.extend_from_slice(to.as_bytes());
        last = pos + f.len();
        match find_ident(text, f, last) {
            Some(p) => pos = p,
            None => break,
        }
    }
    out.extend_from_slice(&text[last..]);
    Some(out)
}

/// Apply every rename in order. Returns the text and the renames that
/// changed something.
pub fn apply_all<'r>(text: &[u8], renames: &'r [Rename]) -> (Vec<u8>, Vec<&'r Rename>) {
    let mut cur = text.to_vec();
    let mut applied = Vec::new();
    for r in renames {
        if let Some(next) = replace_ident(&cur, &r.from, &r.to) {
            cur = next;
            applied.push(r);
        }
    }
    (cur, applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_only() {
        assert!(is_ident("f05") && is_ident("_x") && !is_ident("5f") && !is_ident("A for B"));
        let t = b"f05() + f051 + xf05 + crate::f05 + f05";
        let out = replace_ident(t, "f05", "compute05").unwrap();
        assert_eq!(
            out,
            b"compute05() + f051 + xf05 + crate::compute05 + compute05"
        );
        assert!(replace_ident(b"nothing here", "f05", "x").is_none());
        assert!(contains_ident(b"a.f05(", "f05"));
        assert!(!contains_ident(b"a.f050(", "f05"));
    }
}
