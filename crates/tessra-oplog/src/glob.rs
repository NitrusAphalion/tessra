//! Path patterns for scopes and tracking rules, gitignore-flavoured.
//!
//! `**` matches zero or more path segments, `*` matches within a segment,
//! `?` matches one character. A pattern without a slash matches a path
//! whose basename matches, at any depth.

/// Does `path` match `pattern`?
pub fn matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches('/');
    let path = path.trim_start_matches('/');
    if !pattern.contains('/') {
        return match_segments(&["**", pattern], &path.split('/').collect::<Vec<_>>());
    }
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => {
            // Try consuming zero or more segments.
            (0..=segs.len()).any(|i| match_segments(rest, &segs[i..]))
        }
        Some((p, rest)) => match segs.split_first() {
            None => false,
            Some((s, srest)) => match_segment(p, s) && match_segments(rest, srest),
        },
    }
}

fn match_segment(p: &str, s: &str) -> bool {
    let p: Vec<char> = p.chars().collect();
    let s: Vec<char> = s.chars().collect();
    match_chars(&p, &s)
}

fn match_chars(p: &[char], s: &[char]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some(('*', rest)) => (0..=s.len()).any(|i| match_chars(rest, &s[i..])),
        Some(('?', rest)) => !s.is_empty() && match_chars(rest, &s[1..]),
        Some((c, rest)) => !s.is_empty() && s[0] == *c && match_chars(rest, &s[1..]),
    }
}

/// Does every path that `child` can match also match `parent`? Conservative:
/// true when `child` equals `parent`, or `parent` matches `child` read as a
/// literal path, or `parent` ends in `**` and is a prefix of `child`.
pub fn contained(child: &str, parent: &str) -> bool {
    if child == parent {
        return true;
    }
    if let Some(prefix) = parent.strip_suffix("**") {
        if child.starts_with(prefix) {
            return true;
        }
    }
    if !child.contains(['*', '?']) && matches(parent, child) {
        return true;
    }
    false
}

/// Is `path` inside any of the patterns?
pub fn any_match(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| matches(p, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_patterns() {
        assert!(matches("src/**", "src/a/b.rs"));
        assert!(matches("src/**", "src/x"));
        assert!(matches("src/**/*.rs", "src/a/b/c.rs"));
        assert!(matches("src/**/*.rs", "src/c.rs"));
        assert!(!matches("src/**/*.rs", "lib/c.rs"));
        assert!(matches("*.md", "docs/README.md"));
        assert!(matches("*.md", "README.md"));
        assert!(!matches("*.md", "README.txt"));
        assert!(matches("a/?/c", "a/b/c"));
        assert!(!matches("a/?/c", "a/bb/c"));
        assert!(matches("**", "anything/at/all"));
        assert!(matches("**", ""));
    }

    #[test]
    fn containment() {
        assert!(contained("src/auth/**", "src/**"));
        assert!(contained("src/a.rs", "src/**"));
        assert!(contained("src/**", "src/**"));
        assert!(!contained("src/**", "src/auth/**"));
        assert!(!contained("lib/**", "src/**"));
    }
}
