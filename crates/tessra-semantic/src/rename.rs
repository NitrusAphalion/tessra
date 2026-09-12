//! Identifier renames as data: the semantic operation `edit` records, the
//! rename the matcher infers when a unit keeps its identity under a new
//! name, and the application of one to source text.
//!
//! A rename touches identifier tokens of the parse and nothing else: never
//! a string, a comment, or a character literal. Among identifiers it skips
//! what the tree says cannot be the renamed unit: a field or property
//! declared, accessed, or initialized, unless it is called; a keyword
//! argument's name; a lifetime or label; and every use inside a function
//! that binds the name itself as a parameter or a local.

use tree_sitter::{Node as TsNode, Parser};

use crate::Language;

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

/// Replace whole-word occurrences of `from` with `to`, textually, in text
/// with no grammar. Returns `None` when nothing matched, so callers can
/// tell an applied rename from a no-op. Source is renamed through its
/// parse instead: see `rename_edits`.
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

/// One identifier token to replace: `text[start..end]` becomes `to`, by
/// the renames in `applied`, in order.
#[derive(Clone, Debug)]
pub struct Edit<'r> {
    pub start: usize,
    pub end: usize,
    pub to: String,
    pub applied: Vec<&'r Rename>,
}

/// Where `renames` apply in `text` parsed as `lang`: one edit per
/// identifier token that names a renamed unit, in text order. Renames
/// chain in order, so `a` to `b` followed by `b` to `c` turns `a` into
/// `c`. Text that does not parse as anything yields no edits.
pub fn rename_edits<'r>(lang: Language, text: &[u8], renames: &'r [Rename]) -> Vec<Edit<'r>> {
    if renames.is_empty() || !renames.iter().any(|r| contains_ident(text, &r.from)) {
        return Vec::new();
    }
    let mut parser = Parser::new();
    if parser.set_language(&lang.grammar()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut scopes: Vec<Vec<Vec<u8>>> = Vec::new();
    walk(lang, tree.root_node(), text, renames, &mut scopes, &mut out);
    out
}

/// Apply the edits inside `text[from..to]` to that slice. Returns the
/// slice with the edits applied and the renames that changed something.
pub fn apply_edits<'r>(
    text: &[u8],
    from: usize,
    to: usize,
    edits: &[Edit<'r>],
) -> (Vec<u8>, Vec<&'r Rename>) {
    let to = to.min(text.len());
    let from = from.min(to);
    let mut out = Vec::with_capacity(to - from + 16);
    let mut applied: Vec<&'r Rename> = Vec::new();
    let mut last = from;
    for e in edits {
        if e.start < from || e.end > to || e.start < last {
            continue;
        }
        out.extend_from_slice(&text[last..e.start]);
        out.extend_from_slice(e.to.as_bytes());
        last = e.end;
        for r in &e.applied {
            if !applied.iter().any(|a| std::ptr::eq(*a, *r)) {
                applied.push(r);
            }
        }
    }
    out.extend_from_slice(&text[last..to]);
    (out, applied)
}

/// `text` parsed as `lang` with `renames` applied, and the renames that
/// changed something.
pub fn rename_source<'r>(
    lang: Language,
    text: &[u8],
    renames: &'r [Rename],
) -> (Vec<u8>, Vec<&'r Rename>) {
    let edits = rename_edits(lang, text, renames);
    apply_edits(text, 0, text.len(), &edits)
}

const STRING_KINDS: &[&str] = &[
    "string_literal",
    "raw_string_literal",
    "char_literal",
    "string",
    "concatenated_string",
    "template_string",
    "regex",
    "line_comment",
    "block_comment",
    "comment",
];

/// Whether a node opens a scope that can bind a name locally.
fn is_function(lang: Language, kind: &str) -> bool {
    match lang {
        Language::Rust => matches!(kind, "function_item" | "closure_expression"),
        Language::Python => matches!(kind, "function_definition" | "lambda"),
        _ => matches!(
            kind,
            "function_declaration"
                | "function_expression"
                | "generator_function"
                | "generator_function_declaration"
                | "arrow_function"
                | "method_definition"
        ),
    }
}

/// The identifiers a function binds as parameters or locals, not entering
/// the functions nested in it, which bind for themselves.
fn bindings(lang: Language, node: TsNode<'_>, text: &[u8], out: &mut Vec<Vec<u8>>) {
    let binder = match lang {
        Language::Rust => matches!(
            node.kind(),
            "parameter" | "closure_parameters" | "let_declaration" | "for_expression"
        ),
        Language::Python => matches!(
            node.kind(),
            "parameters"
                | "lambda_parameters"
                | "assignment"
                | "for_statement"
                | "named_expression"
        ),
        _ => matches!(node.kind(), "formal_parameters" | "variable_declarator"),
    };
    if binder {
        let target = match lang {
            Language::Rust => match node.kind() {
                "parameter" | "let_declaration" | "for_expression" => {
                    node.child_by_field_name("pattern")
                }
                _ => Some(node),
            },
            Language::Python => match node.kind() {
                "assignment" | "for_statement" | "named_expression" => node
                    .child_by_field_name("left")
                    .or_else(|| node.child_by_field_name("name")),
                _ => Some(node),
            },
            _ => match node.kind() {
                "variable_declarator" => node.child_by_field_name("name"),
                _ => Some(node),
            },
        };
        if let Some(t) = target {
            identifiers(t, text, out);
        }
        if node.kind() != "for_expression" && node.kind() != "for_statement" {
            return;
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if is_function(lang, child.kind()) {
            continue;
        }
        bindings(lang, child, text, out);
    }
}

/// Every identifier leaf under a node, for a binding pattern. A default
/// value or a type annotation in there is not a binding, but a name that
/// only appears there is at worst skipped in its own function.
fn identifiers(node: TsNode<'_>, text: &[u8], out: &mut Vec<Vec<u8>>) {
    if node.child_count() == 0 {
        if node.kind() == "identifier" {
            out.push(text[node.byte_range()].to_vec());
        }
        return;
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        identifiers(child, text, out);
    }
}

/// Whether an identifier leaf can name the renamed unit, from where it
/// sits in the tree.
fn renamable(lang: Language, leaf: TsNode<'_>) -> bool {
    let Some(parent) = leaf.parent() else {
        return true;
    };
    let is_field = |field: &str| {
        parent
            .child_by_field_name(field)
            .is_some_and(|n| n.id() == leaf.id())
    };
    // A member is the unit only when it is called or defined as a method.
    let called = || {
        parent.parent().is_some_and(|call| {
            matches!(call.kind(), "call_expression" | "call")
                && call
                    .child_by_field_name("function")
                    .is_some_and(|f| f.id() == parent.id())
        })
    };
    match lang {
        Language::Rust => match leaf.kind() {
            "identifier" => !matches!(
                parent.kind(),
                "lifetime" | "label" | "shorthand_field_initializer"
            ),
            "type_identifier" => true,
            "field_identifier" => parent.kind() == "field_expression" && called(),
            _ => false,
        },
        Language::Python => match leaf.kind() {
            "identifier" => {
                if parent.kind() == "attribute" && is_field("attribute") {
                    called()
                } else {
                    !(parent.kind() == "keyword_argument" && is_field("name"))
                }
            }
            _ => false,
        },
        _ => match leaf.kind() {
            "identifier" | "type_identifier" | "shorthand_property_identifier" => true,
            "property_identifier" | "private_property_identifier" => match parent.kind() {
                "member_expression" => called(),
                "method_definition" | "method_signature" | "abstract_method_signature" => {
                    is_field("name")
                }
                _ => false,
            },
            _ => false,
        },
    }
}

fn walk<'r>(
    lang: Language,
    node: TsNode<'_>,
    text: &[u8],
    renames: &'r [Rename],
    scopes: &mut Vec<Vec<Vec<u8>>>,
    out: &mut Vec<Edit<'r>>,
) {
    if STRING_KINDS.contains(&node.kind()) {
        // Code embedded in a string, such as a template substitution or an
        // f-string interpolation, is still code.
        let mut c = node.walk();
        for child in node.children(&mut c) {
            if child.is_named() && child.child_count() > 0 {
                walk(lang, child, text, renames, scopes, out);
            }
        }
        return;
    }
    if node.child_count() == 0 {
        if !renamable(lang, node) {
            return;
        }
        let ident = &text[node.byte_range()];
        if scopes.iter().any(|s| s.iter().any(|b| b == ident)) {
            return;
        }
        let mut name = ident.to_vec();
        let mut applied = Vec::new();
        for r in renames {
            if name == r.from.as_bytes() {
                name = r.to.as_bytes().to_vec();
                applied.push(r);
            }
        }
        if !applied.is_empty() {
            out.push(Edit {
                start: node.start_byte(),
                end: node.end_byte(),
                to: String::from_utf8(name).unwrap_or_default(),
                applied,
            });
        }
        return;
    }
    let scoped = is_function(lang, node.kind());
    if scoped {
        let mut bound = Vec::new();
        bindings(lang, node, text, &mut bound);
        bound.retain(|b| renames.iter().any(|r| r.from.as_bytes() == b.as_slice()));
        scopes.push(bound);
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk(lang, child, text, renames, scopes, out);
    }
    if scoped {
        scopes.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(lang: Language, text: &str, from: &str, to: &str) -> String {
        let r = [Rename::new(from, to)];
        String::from_utf8(rename_source(lang, text.as_bytes(), &r).0).unwrap()
    }

    #[test]
    fn a_rename_touches_identifiers_and_nothing_else() {
        let src = "/// Calls b.\nfn a() -> i32 {\n    let s = \"b\"; // b\n    let c = 'b';\n    b() + s.len() as i32 + c as i32\n}\n\nfn b() -> i32 { 1 }\n";
        let out = rename(Language::Rust, src, "b", "bee");
        assert_eq!(
            out,
            "/// Calls b.\nfn a() -> i32 {\n    let s = \"b\"; // b\n    let c = 'b';\n    bee() + s.len() as i32 + c as i32\n}\n\nfn bee() -> i32 { 1 }\n"
        );
        let py =
            "def a():\n    print(\"b\")  # b\n    return b()\n\ndef b():\n    return f\"{b()}\"\n";
        assert_eq!(
            rename(Language::Python, py, "b", "bee"),
            "def a():\n    print(\"b\")  # b\n    return bee()\n\ndef bee():\n    return f\"{bee()}\"\n"
        );
        let ts = "// b\nconst s = `b ${b()}`;\nfunction b() { return 'b'; }\n";
        assert_eq!(
            rename(Language::TypeScript, ts, "b", "bee"),
            "// b\nconst s = `b ${bee()}`;\nfunction bee() { return 'b'; }\n"
        );
    }

    #[test]
    fn a_rename_skips_fields_locals_and_lifetimes() {
        let rs = "struct S { b: i32 }\nfn f<'b>(x: &'b S, s: S) -> i32 {\n    let t = S { b: 1 };\n    x.b + s.b + t.b + b()\n}\nfn g(b: i32) -> i32 { b + 1 }\nfn h() -> i32 { let b = 2; b }\nfn k(s: &S) -> i32 { s.b() }\nfn b() -> i32 { 0 }\n";
        assert_eq!(
            rename(Language::Rust, rs, "b", "bee"),
            "struct S { b: i32 }\nfn f<'b>(x: &'b S, s: S) -> i32 {\n    let t = S { b: 1 };\n    x.b + s.b + t.b + bee()\n}\nfn g(b: i32) -> i32 { b + 1 }\nfn h() -> i32 { let b = 2; b }\nfn k(s: &S) -> i32 { s.bee() }\nfn bee() -> i32 { 0 }\n"
        );
        let py = "class C:\n    def m(self):\n        return self.b + self.b()\ndef f(b):\n    return b\ndef g():\n    b = 1\n    return b\ndef h(x):\n    return x(b=1) + b()\n";
        assert_eq!(
            rename(Language::Python, py, "b", "bee"),
            "class C:\n    def m(self):\n        return self.b + self.bee()\ndef f(b):\n    return b\ndef g():\n    b = 1\n    return b\ndef h(x):\n    return x(b=1) + bee()\n"
        );
        let ts = "class K {\n  b = 1;\n  m() { return this.b + this.b() + b(); }\n  b() { return 0; }\n}\nconst o = { b: 1, k: b };\nfunction f(b: number) { return b; }\nfunction g() { const b = 1; return b; }\n";
        assert_eq!(
            rename(Language::TypeScript, ts, "b", "bee"),
            "class K {\n  b = 1;\n  m() { return this.b + this.bee() + bee(); }\n  bee() { return 0; }\n}\nconst o = { b: 1, k: bee };\nfunction f(b: number) { return b; }\nfunction g() { const b = 1; return b; }\n"
        );
    }

    #[test]
    fn renames_chain_in_order_and_report_what_applied() {
        let r = [Rename::new("a", "b"), Rename::new("b", "c")];
        let (out, applied) = rename_source(Language::Rust, b"fn a() { a() }", &r);
        assert_eq!(out, b"fn c() { c() }");
        assert_eq!(applied.len(), 2);
        let (same, none) = rename_source(Language::Rust, b"fn x() {}", &r);
        assert_eq!(same, b"fn x() {}");
        assert!(none.is_empty());
        // Edits inside a slice apply to that slice alone.
        let text = b"fn a() {}\nfn z() { a() }\n";
        let edits = rename_edits(Language::Rust, text, &r[..1]);
        let (unit, applied) = apply_edits(text, 10, text.len(), &edits);
        assert_eq!(unit, b"fn z() { b() }\n");
        assert_eq!(applied.len(), 1);
    }

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
