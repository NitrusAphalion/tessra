//! Unit extraction per language, and the chunk fallback.

use tree_sitter::{Node as TsNode, Parser};

use crate::{body_hash, Error, Language, RawNode, Result};

const COMMENT_KINDS: &[&str] = &["line_comment", "block_comment", "comment"];

/// What a syntax node contributes: a unit, a prefix to attach to the next
/// unit (attributes, decorators, doc comments), or nothing.
enum Class<'t> {
    Unit {
        kind: &'static str,
        name: String,
        setlike: bool,
        /// The node whose children are sub-units, for containers.
        body: Option<TsNode<'t>>,
        /// Whether the container's children are a set-like region.
        children_setlike: bool,
    },
    Prefix,
    Skip,
}

pub fn with_grammar(lang: Language, source: &[u8]) -> Result<Vec<RawNode>> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.grammar())
        .map_err(|e| Error::Parse(e.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::Parse("parser returned nothing".into()))?;
    let root = tree.root_node();
    let mut out = Vec::new();
    collect(lang, root, source, None, false, &mut out);
    Ok(out)
}

fn text<'a>(node: TsNode<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}

fn field_text(node: TsNode<'_>, field: &str, source: &[u8]) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| text(n, source).to_string())
}

fn first_ident(node: TsNode<'_>, source: &[u8]) -> String {
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if matches!(
            child.kind(),
            "identifier"
                | "type_identifier"
                | "property_identifier"
                | "private_property_identifier"
        ) {
            return text(child, source).to_string();
        }
    }
    String::new()
}

fn name_of(node: TsNode<'_>, source: &[u8]) -> String {
    field_text(node, "name", source).unwrap_or_else(|| first_ident(node, source))
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn classify<'t>(lang: Language, node: TsNode<'t>, source: &[u8]) -> Class<'t> {
    let k = node.kind();
    match lang {
        Language::Rust => match k {
            "function_item" | "function_signature_item" => Class::Unit {
                kind: "function",
                name: name_of(node, source),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "impl_item" => {
                let ty = field_text(node, "type", source).unwrap_or_default();
                let name = match field_text(node, "trait", source) {
                    Some(t) => format!("{t} for {ty}"),
                    None => ty,
                };
                Class::Unit {
                    kind: "impl",
                    name,
                    setlike: false,
                    body: node.child_by_field_name("body"),
                    children_setlike: true,
                }
            }
            "trait_item" => Class::Unit {
                kind: "trait",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: true,
            },
            "mod_item" => Class::Unit {
                kind: "module",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: false,
            },
            "struct_item" | "union_item" => Class::Unit {
                kind: "struct",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: true,
            },
            "enum_item" => Class::Unit {
                kind: "enum",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: true,
            },
            "field_declaration" => Class::Unit {
                kind: "field",
                name: name_of(node, source),
                setlike: true,
                body: None,
                children_setlike: false,
            },
            "enum_variant" => Class::Unit {
                kind: "variant",
                name: name_of(node, source),
                setlike: true,
                body: None,
                children_setlike: false,
            },
            "use_declaration" | "extern_crate_declaration" => Class::Unit {
                kind: "import",
                name: one_line(text(node, source)),
                setlike: true,
                body: None,
                children_setlike: false,
            },
            "const_item" => Class::Unit {
                kind: "const",
                name: name_of(node, source),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "static_item" => Class::Unit {
                kind: "static",
                name: name_of(node, source),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "type_item" => Class::Unit {
                kind: "type",
                name: name_of(node, source),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "macro_definition" => Class::Unit {
                kind: "macro",
                name: name_of(node, source),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "foreign_mod_item" => Class::Unit {
                kind: "extern",
                name: String::new(),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "attribute_item" | "inner_attribute_item" => Class::Prefix,
            "line_comment" | "block_comment" => {
                let t = text(node, source);
                if t.starts_with("///") || t.starts_with("//!") || t.starts_with("/**") {
                    Class::Prefix
                } else {
                    Class::Skip
                }
            }
            "empty_statement" => Class::Skip,
            _ => Class::Unit {
                kind: "statement",
                name: String::new(),
                setlike: false,
                body: None,
                children_setlike: false,
            },
        },
        Language::Python => match k {
            "function_definition" => Class::Unit {
                kind: "function",
                name: name_of(node, source),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "class_definition" => Class::Unit {
                kind: "class",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: true,
            },
            "decorated_definition" => {
                let inner = node.child_by_field_name("definition");
                match inner {
                    Some(d) => match classify(lang, d, source) {
                        Class::Unit {
                            kind,
                            name,
                            setlike,
                            body,
                            children_setlike,
                        } => Class::Unit {
                            kind,
                            name,
                            setlike,
                            body,
                            children_setlike,
                        },
                        other => other,
                    },
                    None => Class::Unit {
                        kind: "statement",
                        name: String::new(),
                        setlike: false,
                        body: None,
                        children_setlike: false,
                    },
                }
            }
            "import_statement" | "import_from_statement" | "future_import_statement" => {
                Class::Unit {
                    kind: "import",
                    name: one_line(text(node, source)),
                    setlike: true,
                    body: None,
                    children_setlike: false,
                }
            }
            "expression_statement" => {
                let first = node.named_child(0);
                match first {
                    Some(a) if a.kind() == "assignment" || a.kind() == "augmented_assignment" => {
                        Class::Unit {
                            kind: "assignment",
                            name: field_text(a, "left", source)
                                .map(|s| one_line(&s))
                                .unwrap_or_default(),
                            setlike: false,
                            body: None,
                            children_setlike: false,
                        }
                    }
                    _ => Class::Unit {
                        kind: "statement",
                        name: String::new(),
                        setlike: false,
                        body: None,
                        children_setlike: false,
                    },
                }
            }
            "comment" => Class::Prefix,
            _ => Class::Unit {
                kind: "statement",
                name: String::new(),
                setlike: false,
                body: None,
                children_setlike: false,
            },
        },
        Language::JavaScript | Language::TypeScript | Language::Tsx => match k {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                Class::Unit {
                    kind: "function",
                    name: name_of(node, source),
                    setlike: false,
                    body: None,
                    children_setlike: false,
                }
            }
            "class_declaration" | "abstract_class_declaration" | "class" => Class::Unit {
                kind: "class",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: true,
            },
            "method_definition" | "method_signature" | "abstract_method_signature" => Class::Unit {
                kind: "method",
                name: name_of(node, source),
                setlike: true,
                body: None,
                children_setlike: false,
            },
            "public_field_definition" | "field_definition" | "property_signature" => Class::Unit {
                kind: "field",
                name: name_of(node, source),
                setlike: true,
                body: None,
                children_setlike: false,
            },
            "lexical_declaration" | "variable_declaration" => {
                let name = node
                    .named_child(0)
                    .filter(|d| d.kind() == "variable_declarator")
                    .and_then(|d| field_text(d, "name", source))
                    .map(|s| one_line(&s))
                    .unwrap_or_default();
                Class::Unit {
                    kind: "variable",
                    name,
                    setlike: false,
                    body: None,
                    children_setlike: false,
                }
            }
            "import_statement" => Class::Unit {
                kind: "import",
                name: one_line(text(node, source)),
                setlike: true,
                body: None,
                children_setlike: false,
            },
            "export_statement" => {
                let inner = node.child_by_field_name("declaration").or_else(|| {
                    node.named_child(0)
                        .filter(|c| !matches!(c.kind(), "export_clause" | "string"))
                });
                match inner {
                    Some(d) if d.kind() != "identifier" => match classify(lang, d, source) {
                        Class::Unit {
                            kind,
                            name,
                            setlike,
                            body,
                            children_setlike,
                        } => Class::Unit {
                            kind,
                            name,
                            setlike,
                            body,
                            children_setlike,
                        },
                        _ => Class::Unit {
                            kind: "export",
                            name: one_line(text(node, source)),
                            setlike: true,
                            body: None,
                            children_setlike: false,
                        },
                    },
                    _ => Class::Unit {
                        kind: "export",
                        name: one_line(text(node, source)),
                        setlike: true,
                        body: None,
                        children_setlike: false,
                    },
                }
            }
            "interface_declaration" => Class::Unit {
                kind: "interface",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: true,
            },
            "type_alias_declaration" => Class::Unit {
                kind: "type",
                name: name_of(node, source),
                setlike: false,
                body: None,
                children_setlike: false,
            },
            "enum_declaration" => Class::Unit {
                kind: "enum",
                name: name_of(node, source),
                setlike: false,
                body: node.child_by_field_name("body"),
                children_setlike: true,
            },
            "enum_assignment" | "property_identifier" => Class::Unit {
                kind: "variant",
                name: match k {
                    "enum_assignment" => name_of(node, source),
                    _ => text(node, source).to_string(),
                },
                setlike: true,
                body: None,
                children_setlike: false,
            },
            "decorator" => Class::Prefix,
            "comment" => {
                if text(node, source).starts_with("/**") {
                    Class::Prefix
                } else {
                    Class::Skip
                }
            }
            "empty_statement" => Class::Skip,
            _ => Class::Unit {
                kind: "statement",
                name: String::new(),
                setlike: false,
                body: None,
                children_setlike: false,
            },
        },
    }
}

/// Leaf kinds that name something: what a unit references.
const IDENT_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "property_identifier",
    "shorthand_property_identifier",
    "shorthand_property_identifier_pattern",
];

/// Tokens of a node: every leaf, comments excluded. Identifier leaves are
/// also collected into `refs`.
fn tokens<'a>(
    node: TsNode<'_>,
    source: &'a [u8],
    out: &mut Vec<&'a [u8]>,
    refs: &mut Vec<&'a [u8]>,
) {
    if COMMENT_KINDS.contains(&node.kind()) {
        return;
    }
    if node.child_count() == 0 {
        if !node.byte_range().is_empty() {
            let t = &source[node.byte_range()];
            out.push(t);
            if IDENT_KINDS.contains(&node.kind()) {
                refs.push(t);
            }
        }
        return;
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        tokens(child, source, out, refs);
    }
}

/// A function is a test when its language marks it so: a `#[test]` or
/// `#[tokio::test]`-style attribute in Rust, a `test_` prefix in Python, and
/// a `test`/`it` name in JavaScript and TypeScript.
fn is_test(lang: Language, prefix: &str, name: &str) -> bool {
    match lang {
        Language::Rust => {
            prefix.contains("#[test]") || prefix.contains("::test]") || prefix.contains("#[test(")
        }
        Language::Python => name.starts_with("test_") || name.starts_with("test"),
        _ => name == "test" || name == "it" || name.starts_with("test"),
    }
}

/// A test its language marks as skipped or ignored: `#[ignore]` in Rust,
/// a skip decorator in Python, an `x`-prefixed name in JavaScript.
fn is_skipped(lang: Language, prefix: &str, name: &str) -> bool {
    match lang {
        Language::Rust => prefix.contains("#[ignore"),
        Language::Python => prefix.contains("mark.skip") || prefix.contains("unittest.skip"),
        _ => name == "xit" || name == "xtest" || name.starts_with("xdescribe"),
    }
}

/// Two normalizations before hashing. A separator directly before a closing
/// bracket is dropped, so a formatter's trailing comma is not an edit. The
/// unit's own name is dropped once, so a pure rename keeps its body hash and
/// the matcher can recognize it; identity by name is a separate step.
fn normalize_tokens(toks: &mut Vec<&[u8]>, name: &[u8]) {
    let mut i = 0;
    while i + 1 < toks.len() {
        if toks[i] == b"," && matches!(toks[i + 1], b")" | b"]" | b"}" | b">") {
            toks.remove(i);
        } else {
            i += 1;
        }
    }
    if !name.is_empty() {
        if let Some(pos) = toks.iter().position(|t| *t == name) {
            toks.remove(pos);
        }
    }
}

fn collect(
    lang: Language,
    container: TsNode<'_>,
    source: &[u8],
    parent: Option<usize>,
    children_setlike: bool,
    out: &mut Vec<RawNode>,
) {
    let mut prefix_start: Option<usize> = None;
    let mut cursor = container.walk();
    let children: Vec<TsNode<'_>> = container.children(&mut cursor).collect();
    for child in children {
        if !child.is_named() {
            continue;
        }
        match classify(lang, child, source) {
            Class::Prefix => {
                if prefix_start.is_none() {
                    prefix_start = Some(child.start_byte());
                }
            }
            Class::Skip => {}
            Class::Unit {
                kind,
                name,
                setlike,
                body,
                children_setlike: kids_setlike,
            } => {
                let start = prefix_start.take().unwrap_or(child.start_byte());
                let mut toks = Vec::new();
                let mut raw_refs = Vec::new();
                tokens(child, source, &mut toks, &mut raw_refs);
                normalize_tokens(&mut toks, name.as_bytes());
                let mut refs: Vec<String> = raw_refs
                    .into_iter()
                    .filter_map(|r| std::str::from_utf8(r).ok())
                    .filter(|r| *r != name && crate::rename::is_ident(r))
                    .map(str::to_string)
                    .collect();
                refs.sort();
                refs.dedup();
                let idx = out.len();
                // Tests are units of their own kind, so the covering-tests
                // relation and test-change guards can find them.
                let prefix = std::str::from_utf8(&source[start..child.start_byte()]).unwrap_or("");
                let kind = if kind == "function" && is_test(lang, prefix, &name) {
                    if is_skipped(lang, prefix, &name) {
                        "test.skipped"
                    } else {
                        "test"
                    }
                } else {
                    kind
                };
                out.push(RawNode {
                    kind: kind.into(),
                    name,
                    span: (start, child.end_byte()),
                    body: body_hash(toks.into_iter()),
                    parent,
                    setlike: setlike || children_setlike,
                    refs,
                });
                if let Some(b) = body {
                    collect(lang, b, source, Some(idx), kids_setlike, out);
                }
            }
        }
    }
}

/// Chunk fallback: one node per run of lines between blank lines.
pub fn chunks(source: &[u8]) -> Vec<RawNode> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut pos = 0;
    let mut lines: Vec<&[u8]> = Vec::new();
    let flush =
        |start: &mut Option<usize>, end: usize, lines: &mut Vec<&[u8]>, out: &mut Vec<RawNode>| {
            if let Some(s) = start.take() {
                let body = body_hash(lines.iter().map(|l| trim_end(l)));
                out.push(RawNode {
                    kind: "chunk".into(),
                    name: String::new(),
                    span: (s, end),
                    body,
                    parent: None,
                    setlike: false,
                    refs: Vec::new(),
                });
                lines.clear();
            }
        };
    for line in source.split_inclusive(|&b| b == b'\n') {
        let content = trim_end(line);
        if content.is_empty() {
            let end = pos;
            flush(&mut start, end, &mut lines, &mut out);
        } else {
            if start.is_none() {
                start = Some(pos);
            }
            lines.push(line);
        }
        pos += line.len();
    }
    flush(&mut start, pos, &mut lines, &mut out);
    out
}

fn trim_end(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = r#"use std::fmt;
use std::io;

/// Adds.
#[inline]
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub struct Point { x: i32, y: i32 }

pub enum Color {
    Red,
    Blue,
}

impl Point {
    pub fn new() -> Self { Point { x: 0, y: 0 } }
    fn len(&self) -> i32 { self.x + self.y }
}
"#;

    #[test]
    fn rust_units_and_children() {
        let nodes = with_grammar(Language::Rust, RUST.as_bytes()).unwrap();
        let names: Vec<(String, String)> = nodes
            .iter()
            .map(|n| (n.kind.clone(), n.name.clone()))
            .collect();
        assert_eq!(names[0], ("import".into(), "use std::fmt;".into()));
        assert_eq!(names[1], ("import".into(), "use std::io;".into()));
        assert_eq!(names[2], ("function".into(), "add".into()));
        // The doc comment and attribute are part of add's span.
        assert!(RUST[nodes[2].span.0..nodes[2].span.1].starts_with("/// Adds."));
        let tests = with_grammar(
            Language::Rust,
            b"#[test]\nfn works() {}\n\n#[tokio::test]\nasync fn also() {}\n\nfn plain() {}\n",
        )
        .unwrap();
        assert_eq!(
            tests.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>(),
            vec!["test", "test", "function"]
        );
        let skipped = with_grammar(Language::Rust, b"#[test]\n#[ignore]\nfn later() {}\n").unwrap();
        assert_eq!(skipped[0].kind, "test.skipped");
        let point = nodes
            .iter()
            .position(|n| n.kind == "struct" && n.name == "Point")
            .unwrap();
        let fields: Vec<&RawNode> = nodes.iter().filter(|n| n.parent == Some(point)).collect();
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["x", "y"]
        );
        assert!(fields[0].setlike);
        let color = nodes.iter().position(|n| n.kind == "enum").unwrap();
        let variants: Vec<&RawNode> = nodes.iter().filter(|n| n.parent == Some(color)).collect();
        assert_eq!(
            variants.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            vec!["Red", "Blue"]
        );
        let imp = nodes
            .iter()
            .position(|n| n.kind == "impl" && n.name == "Point")
            .unwrap();
        let methods: Vec<&RawNode> = nodes.iter().filter(|n| n.parent == Some(imp)).collect();
        assert_eq!(
            methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["new", "len"]
        );
    }

    #[test]
    fn body_hash_ignores_formatting_and_comments() {
        let a = with_grammar(Language::Rust, b"fn f(a: i32) -> i32 { a + 1 }").unwrap();
        let b = with_grammar(
            Language::Rust,
            b"fn f(\n    a: i32,\n) -> i32 {\n    // comment\n    a + 1\n}\n",
        )
        .unwrap();
        assert_eq!(a[0].body, b[0].body);
        let c = with_grammar(Language::Rust, b"fn f(a: i32) -> i32 { a + 2 }").unwrap();
        assert_ne!(a[0].body, c[0].body);
    }

    #[test]
    fn python_units() {
        let src = b"import os\nfrom x import y\n\n@dec\ndef f(a):\n    return a\n\nclass C:\n    def m(self):\n        pass\n\nX = 1\n";
        let nodes = with_grammar(Language::Python, src).unwrap();
        let names: Vec<(String, String)> = nodes
            .iter()
            .map(|n| (n.kind.clone(), n.name.clone()))
            .collect();
        assert_eq!(names[0], ("import".into(), "import os".into()));
        assert_eq!(names[2], ("function".into(), "f".into()));
        assert!(std::str::from_utf8(&src[nodes[2].span.0..nodes[2].span.1])
            .unwrap()
            .starts_with("@dec"));
        assert_eq!(names[3], ("class".into(), "C".into()));
        assert_eq!(names[4], ("function".into(), "m".into()));
        assert_eq!(nodes[4].parent, Some(3));
        assert_eq!(names[5], ("assignment".into(), "X".into()));
    }

    #[test]
    fn typescript_units() {
        let src = b"import { a } from './a';\nexport function f() { return 1; }\nexport class K { m() {} }\nconst x = 2;\nexport enum E { A, B }\n";
        let nodes = with_grammar(Language::TypeScript, src).unwrap();
        let names: Vec<(String, String)> = nodes
            .iter()
            .map(|n| (n.kind.clone(), n.name.clone()))
            .collect();
        assert_eq!(names[0].0, "import");
        assert_eq!(names[1], ("function".into(), "f".into()));
        assert_eq!(names[2], ("class".into(), "K".into()));
        assert_eq!(names[3], ("method".into(), "m".into()));
        assert_eq!(names[4], ("variable".into(), "x".into()));
        assert_eq!(names[5], ("enum".into(), "E".into()));
        assert_eq!(names[6], ("variant".into(), "A".into()));
    }

    #[test]
    fn chunk_fallback() {
        let nodes = chunks(b"line one\nline two\n\n\nthird block  \nmore\n");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].span, (0, 18));
        assert_eq!(nodes[1].kind, "chunk");
        let again = chunks(b"line one\nline two\n\nthird block\nmore\n");
        assert_eq!(again[1].body, nodes[1].body);
    }
}
