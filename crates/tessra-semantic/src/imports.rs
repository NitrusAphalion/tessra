//! Which files a file's import statements name, so that a reference that
//! does not resolve within its own file is resolved through its imports and
//! never against a same-named unit elsewhere in the repository: a class that
//! extends the global `Error` does not depend on a Storybook story that
//! happens to define one, and `describe` from `vitest` depends on nothing in
//! the tree.
//!
//! Resolution is textual, over the one-line import statements the extractor
//! records as `import` units, and asks the caller whether a candidate path
//! exists in the tree. JavaScript and TypeScript resolve relative specifiers
//! and the `$lib/`, `@/`, and `~/` aliases; Python resolves relative and
//! absolute modules from the file's directory and its ancestors; Rust
//! resolves `crate::`, `self::`, `super::`, and workspace crate paths. A
//! bare package name resolves to nothing, since the package is not in the
//! tree.

use crate::Language;

/// The paths in the tree that `statements`, the import units of the file at
/// `path`, name. Sorted, without duplicates, and never `path` itself.
pub fn imported_paths(
    path: &str,
    statements: &[&str],
    exists: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    match Language::from_path(path) {
        Some(Language::Rust) => {
            for s in statements {
                rust(path, s, exists, &mut out);
            }
        }
        Some(Language::Python) => {
            for s in statements {
                python(path, s, exists, &mut out);
            }
        }
        Some(_) => {
            for s in statements {
                javascript(path, s, exists, &mut out);
            }
        }
        None => {}
    }
    out.retain(|p| p != path);
    out.sort();
    out.dedup();
    out
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if name.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `dir` joined with a relative path, `.` and `..` resolved; `None` when
/// `..` would leave the tree.
fn relative(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// The directory and each of its ancestors, nearest first, ending with the
/// root `""`.
fn ancestors(dir: &str) -> Vec<String> {
    let mut out = vec![dir.to_string()];
    let mut d = dir;
    while !d.is_empty() {
        d = dir_of(d);
        out.push(d.to_string());
    }
    out
}

// ------------------------------------------------------ JavaScript, TypeScript

const JS_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs", "d.ts", "svelte", "vue", "json",
];

/// The quoted strings of a statement: its module specifiers.
fn quoted(stmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = stmt;
    while let Some(i) = rest.find(['\'', '"', '`']) {
        let quote = rest[i..].chars().next().unwrap_or('\'');
        let after = &rest[i + quote.len_utf8()..];
        let Some(j) = after.find(quote) else { break };
        out.push(after[..j].to_string());
        rest = &after[j + quote.len_utf8()..];
    }
    out
}

/// The file a specifier's resolved base names: as written, with each
/// extension, as a directory index, and with a `.js`-style extension
/// swapped for the TypeScript one that compiles to it.
fn js_file(base: &str, exists: &dyn Fn(&str) -> bool) -> Option<String> {
    if JS_EXTENSIONS
        .iter()
        .any(|e| base.ends_with(&format!(".{e}")))
        && exists(base)
    {
        return Some(base.to_string());
    }
    for ext in JS_EXTENSIONS {
        let p = format!("{base}.{ext}");
        if exists(&p) {
            return Some(p);
        }
    }
    for ext in JS_EXTENSIONS {
        let p = format!("{base}/index.{ext}");
        if exists(&p) {
            return Some(p);
        }
    }
    for (from, to) in [
        (".js", &["ts", "tsx"][..]),
        (".jsx", &["tsx"][..]),
        (".mjs", &["mts"][..]),
        (".cjs", &["cts"][..]),
    ] {
        if let Some(stem) = base.strip_suffix(from) {
            for ext in to {
                let p = format!("{stem}.{ext}");
                if exists(&p) {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn javascript(path: &str, stmt: &str, exists: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    let dir = dir_of(path);
    for spec in quoted(stmt) {
        let bases: Vec<String> =
            if spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../") {
                relative(dir, &spec).into_iter().collect()
            } else if let Some(rest) = spec.strip_prefix("$lib/") {
                ancestors(dir)
                    .iter()
                    .map(|a| join(&join(a, "src/lib"), rest))
                    .collect()
            } else if let Some(rest) = spec.strip_prefix("@/").or_else(|| spec.strip_prefix("~/")) {
                ancestors(dir)
                    .iter()
                    .map(|a| join(&join(a, "src"), rest))
                    .collect()
            } else {
                Vec::new()
            };
        if let Some(found) = bases.iter().find_map(|b| js_file(b, exists)) {
            out.push(found);
        }
    }
}

// ------------------------------------------------------------------- Python

/// The files a module path names, from `dir` upward for an absolute path,
/// from the package `dots` deep for a relative one. `names` are the items a
/// `from` statement imports, which may be submodules.
fn python_module(
    dir: &str,
    module: &str,
    names: &[&str],
    exists: &dyn Fn(&str) -> bool,
    out: &mut Vec<String>,
) {
    let dots = module.chars().take_while(|c| *c == '.').count();
    let rel = module[dots..].replace('.', "/");
    let bases: Vec<String> = if dots > 0 {
        let mut d = dir.to_string();
        for _ in 1..dots {
            d = dir_of(&d).to_string();
        }
        vec![join(&d, &rel)]
    } else {
        ancestors(dir).iter().map(|a| join(a, &rel)).collect()
    };
    for base in bases {
        let mut hit = false;
        if !rel.is_empty() || dots > 0 {
            let file = format!("{base}.py");
            if !base.is_empty() && exists(&file) {
                out.push(file);
                hit = true;
            }
            let init = join(&base, "__init__.py");
            if exists(&init) {
                out.push(init);
                hit = true;
            }
        }
        for name in names {
            let sub = join(&base, &format!("{name}.py"));
            if exists(&sub) {
                out.push(sub);
                hit = true;
            }
            let pkg = join(&join(&base, name), "__init__.py");
            if exists(&pkg) {
                out.push(pkg);
                hit = true;
            }
        }
        if hit {
            break;
        }
    }
}

fn python(path: &str, stmt: &str, exists: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    let dir = dir_of(path);
    let stmt = stmt.trim().trim_end_matches(';').trim();
    if let Some(rest) = stmt.strip_prefix("from ") {
        let mut words = rest.split_whitespace();
        let Some(module) = words.next() else { return };
        let names: Vec<&str> = match rest.split_once(" import ") {
            Some((_, list)) => list
                .trim_matches(|c| c == '(' || c == ')')
                .split(',')
                .filter_map(|n| n.split_whitespace().next())
                .filter(|n| *n != "*")
                .collect(),
            None => Vec::new(),
        };
        python_module(dir, module, &names, exists, out);
    } else if let Some(rest) = stmt.strip_prefix("import ") {
        for m in rest.split(',') {
            if let Some(module) = m.split_whitespace().next() {
                python_module(dir, module, &[], exists, out);
            }
        }
    }
}

// --------------------------------------------------------------------- Rust

/// `use a::{b, c::{d, e}}` as the paths `a::b`, `a::c::d`, `a::c::e`;
/// `as` renames dropped.
fn expand_braces(tree: &str) -> Vec<String> {
    let tree = tree.trim();
    let Some(open) = tree.find('{') else {
        let one = tree.split(" as ").next().unwrap_or(tree).trim();
        return if one.is_empty() {
            Vec::new()
        } else {
            vec![one.to_string()]
        };
    };
    let prefix = tree[..open].trim().trim_end_matches("::");
    let Some(close) = tree.rfind('}') else {
        return Vec::new();
    };
    let inner = &tree[open + 1..close];
    // Split the group on the commas at depth zero.
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&inner[start..]);
    let mut out = Vec::new();
    for item in items {
        for sub in expand_braces(item) {
            out.push(if prefix.is_empty() {
                sub
            } else {
                format!("{prefix}::{sub}")
            });
        }
    }
    out
}

/// The directory a file's module owns its children in: the file's own
/// directory for `mod.rs`, `lib.rs`, and `main.rs`, else the directory
/// named after the file.
fn module_dir(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if matches!(name, "mod.rs" | "lib.rs" | "main.rs") {
        dir_of(path).to_string()
    } else {
        path.strip_suffix(".rs").unwrap_or(path).to_string()
    }
}

/// The directory of the crate's root module: the nearest ancestor holding
/// `lib.rs` or `main.rs`, else the nearest named `src`, else the file's own.
fn crate_src(path: &str, exists: &dyn Fn(&str) -> bool) -> String {
    let dir = dir_of(path);
    for a in ancestors(dir) {
        if exists(&join(&a, "lib.rs")) || exists(&join(&a, "main.rs")) {
            return a;
        }
    }
    for a in ancestors(dir) {
        if a == "src" || a.ends_with("/src") {
            return a;
        }
    }
    dir.to_string()
}

/// Where a crate named in a `use` path may live: `crates/<name>/src` or
/// `<name>/src` with `_` and `-` interchangeable, and the file's own crate
/// when it names it, as an integration test under `tests/` does.
fn crate_dirs_named(path: &str, name: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let spellings = [name.to_string(), name.replace('_', "-")];
    for n in &spellings {
        for base in [format!("crates/{n}/src"), format!("{n}/src")] {
            if exists(&join(&base, "lib.rs")) {
                out.push(base);
            }
        }
    }
    let own = crate_src(path, exists);
    let crate_dir = dir_of(&own);
    let crate_name = crate_dir.rsplit('/').next().unwrap_or(crate_dir);
    let in_tests = path.starts_with("tests/") || path.contains("/tests/");
    if (spellings.iter().any(|s| s == crate_name) || in_tests) && !out.contains(&own) {
        out.push(own);
    }
    out
}

/// The module files along a `use` path under `base`: the root module,
/// then `s.rs` or `s/mod.rs` for each segment in turn.
fn rust_walk(base: &str, segments: &[&str], exists: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    for root in ["lib.rs", "main.rs", "mod.rs"] {
        let p = join(base, root);
        if exists(&p) {
            out.push(p);
        }
    }
    let own = format!("{base}.rs");
    if !base.is_empty() && exists(&own) {
        out.push(own);
    }
    let mut dir = base.to_string();
    for s in segments {
        if matches!(*s, "*" | "self") {
            break;
        }
        let file = join(&dir, &format!("{s}.rs"));
        if exists(&file) {
            out.push(file);
        }
        let module = join(&join(&dir, s), "mod.rs");
        if exists(&module) {
            out.push(module);
        }
        dir = join(&dir, s);
    }
}

fn rust(path: &str, stmt: &str, exists: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    let stmt = stmt.trim();
    let Some(at) = stmt.find("use ") else { return };
    let body = stmt[at + 4..].trim().trim_end_matches(';').trim();
    for p in expand_braces(body) {
        let segments: Vec<&str> = p.split("::").map(str::trim).collect();
        let Some(first) = segments.first() else {
            continue;
        };
        let (bases, rest): (Vec<String>, &[&str]) = match *first {
            "crate" => (vec![crate_src(path, exists)], &segments[1..]),
            "self" => (vec![module_dir(path)], &segments[1..]),
            "super" => {
                let mut dir = dir_of(&module_dir(path)).to_string();
                let mut i = 1;
                while segments.get(i) == Some(&"super") {
                    dir = dir_of(&dir).to_string();
                    i += 1;
                }
                (vec![dir], &segments[i..])
            }
            "std" | "core" | "alloc" => continue,
            name => (crate_dirs_named(path, name, exists), &segments[1..]),
        };
        for base in bases {
            rust_walk(&base, rest, exists, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree<'a>(paths: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |p: &str| paths.contains(&p)
    }

    #[test]
    fn typescript_resolves_relative_specifiers_and_aliases_and_drops_packages() {
        let files = tree(&[
            "platform/src/lib/adapters/kalshi/signing.ts",
            "platform/src/lib/adapters/kalshi/signing.test.ts",
            "platform/src/lib/util/index.ts",
            "platform/src/routes/trade/dispatch.ts",
            "basecomponents/src/lib/Badge/Badge.stories.js",
        ]);
        let test = "platform/src/lib/adapters/kalshi/signing.test.ts";
        let got = imported_paths(
            test,
            &[
                "import { describe, it, expect } from 'vitest';",
                "import { sign } from './signing';",
                "import { clamp } from \"../../util/index.js\";",
            ],
            &files,
        );
        assert_eq!(
            got,
            vec![
                "platform/src/lib/adapters/kalshi/signing.ts".to_string(),
                "platform/src/lib/util/index.ts".to_string(),
            ]
        );
        let got = imported_paths(
            "platform/src/routes/trade/dispatch.ts",
            &[
                "import { sign } from '$lib/adapters/kalshi/signing';",
                "import { clamp } from '$lib/util';",
                "import Badge from 'basecomponents';",
            ],
            &files,
        );
        assert_eq!(
            got,
            vec![
                "platform/src/lib/adapters/kalshi/signing.ts".to_string(),
                "platform/src/lib/util/index.ts".to_string(),
            ]
        );
    }

    #[test]
    fn python_resolves_relative_and_absolute_modules_from_the_source_roots() {
        let files = tree(&[
            "pkg/__init__.py",
            "pkg/helpers.py",
            "pkg/sub/__init__.py",
            "pkg/sub/deep.py",
            "pkg/mod.py",
            "tests/test_mod.py",
        ]);
        let got = imported_paths(
            "pkg/mod.py",
            &[
                "from .helpers import one, two",
                "from . import sub",
                "from .sub.deep import three",
                "import os, sys",
            ],
            &files,
        );
        assert_eq!(
            got,
            vec![
                "pkg/__init__.py".to_string(),
                "pkg/helpers.py".to_string(),
                "pkg/sub/__init__.py".to_string(),
                "pkg/sub/deep.py".to_string(),
            ]
        );
        let got = imported_paths(
            "tests/test_mod.py",
            &["from pkg.mod import run", "import pkg.helpers"],
            &files,
        );
        assert_eq!(
            got,
            vec!["pkg/helpers.py".to_string(), "pkg/mod.py".to_string()]
        );
    }

    #[test]
    fn rust_resolves_crate_self_super_and_workspace_crates() {
        let files = tree(&[
            "crates/tessra-core/src/lib.rs",
            "crates/tessra-core/src/id.rs",
            "crates/tessra-daemon/src/lib.rs",
            "crates/tessra-daemon/src/fs.rs",
            "crates/tessra-daemon/src/tree.rs",
            "crates/tessra-daemon/src/sem/mod.rs",
            "crates/tessra-daemon/src/sem/merge.rs",
            "crates/tessra-daemon/tests/end_to_end.rs",
        ]);
        let got = imported_paths(
            "crates/tessra-daemon/src/fs.rs",
            &[
                "use std::collections::BTreeMap;",
                "use tessra_core::{ObjectId, id::EntityId};",
                "use crate::tree::{self, Flat, Leaf};",
                "pub use crate::sem::merge::merge_file;",
            ],
            &files,
        );
        assert_eq!(
            got,
            vec![
                "crates/tessra-core/src/id.rs".to_string(),
                "crates/tessra-core/src/lib.rs".to_string(),
                "crates/tessra-daemon/src/lib.rs".to_string(),
                "crates/tessra-daemon/src/sem/merge.rs".to_string(),
                "crates/tessra-daemon/src/sem/mod.rs".to_string(),
                "crates/tessra-daemon/src/tree.rs".to_string(),
            ]
        );
        // `super` from a module file names the parent module.
        let got = imported_paths(
            "crates/tessra-daemon/src/sem/merge.rs",
            &["use super::*;"],
            &files,
        );
        assert_eq!(got, vec!["crates/tessra-daemon/src/sem/mod.rs".to_string()]);
        // An integration test names its own crate.
        let got = imported_paths(
            "crates/tessra-daemon/tests/end_to_end.rs",
            &["use tessra_daemon::fs;"],
            &files,
        );
        assert_eq!(
            got,
            vec![
                "crates/tessra-daemon/src/fs.rs".to_string(),
                "crates/tessra-daemon/src/lib.rs".to_string(),
            ]
        );
    }

    #[test]
    fn braces_expand_to_every_path() {
        let mut got = expand_braces("a::{b, c::{d, e as f}, self}");
        got.sort();
        assert_eq!(got, vec!["a::b", "a::c::d", "a::c::e", "a::self"]);
    }
}
