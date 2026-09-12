//! A minimal MCP server over stdio: newline-delimited JSON-RPC 2.0 with
//! `initialize`, `tools/list`, `tools/call`, and `ping`. Each tool is one
//! verb named `tessra_<verb>`; arguments pass straight to the verb. The
//! agent manual is the server's `instructions`, and the `next` suggestions
//! every verb returns are rewritten from the CLI's syntax into tool calls.

use std::io::{BufRead, Write};

use serde_json::{json, Map, Value};

/// The agent manual, served as the MCP `instructions` so a session that
/// connects has read it before its first call.
const MANUAL: &str = include_str!("../../../spec/06-manual.md");

const VERBS: &[(&str, &str)] = &[
    ("status", "Where am I: my assignment from the planner (units, paths, budget), claims, open questions, my change's standard status, and what happened since a cursor, newest first and cut to the budget (since_omitted says how many more; cursor is what to pass next time). Call first every session."),
    ("context", "A context pack within a token budget. path: the file's content, its units, memories, claims. unit (name or path:name): the unit's text, what it depends on, what depends on it, the tests that cover it, memories, claims, and who last changed it. Without a workspace it describes trunk."),
    ("query", "Read the graph. kind: memory (scope.ref, kinds), revision (change), since (since cursor), object (id prefix; a blob such as a verifier's evidence is paged: offset, or tail for its end), blame (path: who last changed each unit), tests (path: the tests covering each unit), diff (change?, path?: units added, removed, changed, renamed against the parent), trusted (change: the most attested trunk snapshot containing it), bisect (test: the first trunk revision where a test in your revision fails, using cached results), activity (window like 1h, altitude summary|changes|ops: what happened), exceptions (open requests for a human with the judges' reasoning). Every kind is cut to the budget and says what it left out."),
    ("workspace", "action: create | list | drop | rewind. Create materializes trunk or a revision into your own workspace (with_state restores its build state); rewind puts your workspace back at a revision with everything it carried."),
    ("edit", "Write a file (path, content), replace text (path, old, new, all?), or rename an identifier everywhere (rename, to, path?) as a recorded semantic operation the merge carries into concurrent changes. Plain file edits through other tools also count."),
    ("snapshot", "Record the workspace's state as a revision. Never blocks; flags tell you what will block promotion. then: verify | promote (proposes) | promote landed (the owner lands). A retry with the same idem reports the revision the first call recorded."),
    ("claim", "action: claim (paths, expires_s?, exclusive?, note?) | release (id?). Advisory; the response lists other claims that overlap yours, which means a merge is coming."),
    ("remember", "Record a memory. kind: fact|decision|convention|gotcha|preference|task|question|summary|resolution. scope: {kind: path|unit|intent|repo, ref}; a session records path, unit (ref path:name), and intent memories, and repo, the default, is the owner's. body. confidence 0..1. A unit or path memory is anchored to that content and recalled as stale once it changes. supersedes: an earlier memory this one replaces. retire: take a memory of yours back."),
    ("verify", "Run the verifiers the standard still needs (cached when the snapshot was verified before, selected by covering tests when possible) and report the standard clause by clause with what would satisfy each unmet one. full: run everything."),
    ("try", "Speculate: candidates [{path, content} | {path, old, new}, title?] are each materialized on your revision, verified, scored, and ranked; keep: <candidate> writes the winner into your workspace."),
    ("promote", "to: proposed | landed | <target> (with slice: canary | all). Refused with code STANDARD_UNMET and the unmet clauses, each with a fix, if the standard does not hold."),
    ("revert", "Public: revert a landed change (change, reason?) with its edits undone at unit granularity; a task is opened. Landed at once by the owner, proposed by anyone else."),
    ("undo", "Take back your most recent op, or op: <hex>. Private; never a landing."),
];

fn prop(kind: &str, description: &str) -> Value {
    json!({ "type": kind, "description": description })
}

fn enumerated(values: &[&str], description: &str) -> Value {
    json!({ "type": "string", "enum": values, "description": description })
}

fn budget(default: u64) -> Value {
    json!({ "type": "integer", "description": format!("Token budget for the response; the result is cut to it and says what was left out. Default {default}."), "default": default })
}

fn idem() -> Value {
    prop(
        "string",
        "Idempotency key: reuse it on a retry and nothing happens twice.",
    )
}

fn tool_schema(verb: &str) -> Value {
    let (props, required): (Value, &[&str]) = match verb {
        "status" => (
            json!({
                "since": prop("string", "The cursor a previous status or query returned; only what happened after it is listed."),
                "budget": budget(4000),
            }),
            &[],
        ),
        "context" => (
            json!({
                "path": prop("string", "A file or directory to pack: its content or listing, its units, memories, claims."),
                "unit": prop("string", "A unit to pack, as name or path:name."),
                "budget": budget(4000),
            }),
            &[],
        ),
        "try" => (
            json!({
                "candidates": { "type": "array", "description": "Candidate edits, each {path, content} or {path, old, new} with an optional title.", "items": { "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" }, "old": { "type": "string" }, "new": { "type": "string" }, "title": { "type": "string" } }, "required": ["path"] } },
                "keep": prop("integer", "The rank of the candidate to write into the workspace, 1 being the best."),
            }),
            &["candidates"],
        ),
        "query" => (
            json!({
                "kind": enumerated(&["memory", "revision", "since", "object", "blame", "tests", "diff", "trusted", "bisect", "activity", "exceptions"], "What to read."),
                "scope": { "type": "object", "description": "For memory: {kind, ref} to narrow by scope.", "properties": { "kind": { "type": "string" }, "ref": { "type": "string" } } },
                "kinds": { "type": "array", "items": { "type": "string" }, "description": "For memory: the memory kinds to list." },
                "change": prop("string", "For revision, diff, trusted: the change, by ID prefix."),
                "since": prop("string", "For since: the cursor to start after."),
                "id": prop("string", "For object: the object, by ID prefix."),
                "path": prop("string", "For blame, tests, diff: the path to answer unit by unit."),
                "test": prop("string", "For bisect: the test unit in your revision."),
                "window": prop("string", "For activity: how far back, such as 30m, 1h, 2d."),
                "altitude": enumerated(&["summary", "changes", "ops"], "For activity: how much detail."),
                "offset": prop("integer", "For object: read a blob's text from this byte offset."),
                "tail": prop("boolean", "For object: the end of a blob within the budget, where a verifier's summary and errors are."),
                "budget": budget(4000),
            }),
            &["kind"],
        ),
        "workspace" => (
            json!({
                "action": enumerated(&["create", "list", "drop", "rewind"], "What to do."),
                "from": prop("string", "For create: trunk, or a revision by prefix."),
                "path": prop("string", "For create: a fresh directory to materialize into instead of one of Tessra's own."),
                "id": prop("string", "For drop: the workspace, by ID prefix."),
                "with_state": prop("boolean", "For create: restore the build state the revision's snapshot carried."),
                "to": prop("string", "For rewind: the revision to go back to."),
            }),
            &["action"],
        ),
        "edit" => (
            json!({
                "path": prop("string", "The file to write or edit. Optional with rename, which then covers every tracked file."),
                "content": prop("string", "The whole new content of the file."),
                "old": prop("string", "Text to replace, with new."),
                "new": prop("string", "The replacement for old."),
                "all": prop("boolean", "Replace every occurrence of old, not only the first."),
                "rename": prop("string", "An identifier to rename everywhere, recorded as a semantic operation."),
                "to": prop("string", "The new name for rename."),
                "delete": prop("boolean", "Delete the file instead of writing it."),
                "resolve": prop("boolean", "Mark a conflicted file resolved after writing it."),
            }),
            &[],
        ),
        "snapshot" => (
            json!({
                "title": prop("string", "What the change does, in one line."),
                "then": enumerated(&["verify", "promote", "promote landed"], "A follow-on in the same call."),
                "with_state": prop("boolean", "Also record the toolchain, lockfile hashes, and the state paths as trees."),
                "idem": idem(),
            }),
            &[],
        ),
        "claim" => (
            json!({
                "action": enumerated(&["claim", "release"], "What to do."),
                "paths": { "type": "array", "items": { "type": "string" }, "description": "The paths to claim, as globs." },
                "id": prop("string", "For release: the claim, by ID prefix."),
                "expires_s": prop("integer", "How long the claim holds, in seconds."),
                "exclusive": prop("boolean", "Ask others to stay off these paths."),
                "note": prop("string", "What you are doing there, for whoever overlaps."),
                "idem": idem(),
            }),
            &["action"],
        ),
        "remember" => (
            json!({
                "kind": enumerated(&["fact", "decision", "convention", "gotcha", "preference", "task", "question", "summary", "resolution"], "What kind of memory."),
                "body": prop("string", "The memory, short."),
                "scope": { "type": "object", "description": "What it is about: {kind: path|unit|intent|repo, ref}.", "properties": { "kind": { "type": "string", "enum": ["path", "unit", "intent", "repo"] }, "ref": { "type": "string" } } },
                "confidence": prop("number", "How sure you are, 0 to 1."),
                "visibility": enumerated(&["shared", "private"], "Who may recall it."),
                "supersedes": prop("string", "An earlier memory this one replaces, by ID prefix; it is retired in the same op."),
                "retire": prop("string", "Take a memory back by ID prefix instead of recording one; its author or the owner may."),
                "idem": idem(),
            }),
            &["kind"],
        ),
        "verify" => (
            json!({
                "kinds": { "type": "array", "items": { "type": "string" }, "description": "Only these attestation kinds." },
                "full": prop("boolean", "Run the whole suite even when a cached or selected result would do."),
            }),
            &[],
        ),
        "promote" => (
            json!({
                "to": prop("string", "proposed, landed, or a target's name."),
                "slice": enumerated(&["canary", "all"], "For a target: which slice."),
                "idem": idem(),
            }),
            &["to"],
        ),
        "undo" => (
            json!({ "op": prop("string", "The op to take back, by hex; default your most recent."), "idem": idem() }),
            &[],
        ),
        "revert" => (
            json!({
                "change": prop("string", "The landed change to revert, by ID prefix."),
                "reason": prop("string", "Why, for the task the revert opens."),
                "idem": idem(),
            }),
            &["change"],
        ),
        _ => (json!({}), &[]),
    };
    json!({ "type": "object", "properties": props, "required": required })
}

/// A `next` suggestion in the CLI's syntax, as the tool call it is over MCP:
/// `verify --stage landed` becomes `tessra_verify {"stage":"landed"}`.
/// Placeholders such as `<paths>` stay as they are.
pub fn tool_call_text(cli: &str) -> String {
    let tokens = tokenize(cli);
    let Some((verb, rest)) = tokens.split_first() else {
        return cli.to_string();
    };
    let mut args = Map::new();
    let mut i = 0;
    while i < rest.len() {
        let t = &rest[i];
        if let Some(key) = t.strip_prefix("--") {
            let key = key.replace('-', "_");
            let value = rest.get(i + 1).filter(|v| !v.starts_with("--"));
            match value {
                Some(v) => {
                    let parsed = match v.as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        s => match s.parse::<i64>() {
                            Ok(n) => json!(n),
                            Err(_) => Value::String(s.to_string()),
                        },
                    };
                    // A repeated flag collects into a list.
                    match args.remove(&key) {
                        Some(Value::Array(mut a)) => {
                            a.push(parsed);
                            args.insert(key, Value::Array(a));
                        }
                        Some(prev) => {
                            args.insert(key, Value::Array(vec![prev, parsed]));
                        }
                        None => {
                            args.insert(key, parsed);
                        }
                    }
                    i += 2;
                }
                None => {
                    args.insert(key, Value::Bool(true));
                    i += 1;
                }
            }
        } else {
            // A bare word after the verb, as in `promote landed`.
            let key = match verb.as_str() {
                "promote" => "to",
                _ => "arg",
            };
            args.insert(key.to_string(), Value::String(t.clone()));
            i += 1;
        }
    }
    if verb == "snapshot" {
        if let Some(Value::String(t)) = args.get("then") {
            if t == "promote" && args.get("arg").is_some() {
                let stage = args
                    .remove("arg")
                    .and_then(|v| v.as_str().map(str::to_string));
                if let Some(s) = stage {
                    args.insert("then".into(), Value::String(format!("promote {s}")));
                }
            }
        }
    }
    format!("tessra_{verb} {}", Value::Object(args))
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'') | (None, '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn serve(call: &mut dyn FnMut(&str, &Value) -> Value) {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_msg(
                    &stdout,
                    &json!({ "jsonrpc": "2.0", "id": Value::Null, "error": { "code": -32700, "message": format!("parse error: {e}") } }),
                );
                continue;
            }
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let response = match method {
            "initialize" => Some(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2024-11-05")),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "tessra", "version": env!("CARGO_PKG_VERSION") },
                "instructions": MANUAL
            })),
            "notifications/initialized" | "notifications/cancelled" => None,
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({
                "tools": VERBS.iter().map(|(v, d)| json!({ "name": format!("tessra_{v}"), "description": d, "inputSchema": tool_schema(v) })).collect::<Vec<_>>()
            })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let verb = name.strip_prefix("tessra_").unwrap_or(name);
                let mut out = call(verb, &args);
                // The verbs suggest their next calls in the CLI's words; here
                // the caller's words are tool calls.
                if let Some(next) = out.get_mut("next").and_then(Value::as_array_mut) {
                    for n in next.iter_mut() {
                        if let Some(s) = n.as_str() {
                            *n = Value::String(tool_call_text(s));
                        }
                    }
                }
                let is_error = out.get("ok") != Some(&Value::Bool(true));
                Some(json!({
                    "content": [{ "type": "text", "text": serde_json::to_string(&out).unwrap_or_default() }],
                    "isError": is_error
                }))
            }
            _ => {
                if id.is_some() {
                    write_msg(
                        &stdout,
                        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {method}") } }),
                    );
                }
                continue;
            }
        };
        if let (Some(id), Some(result)) = (id, response) {
            write_msg(
                &stdout,
                &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            );
        }
    }
}

fn write_msg(stdout: &std::io::Stdout, v: &Value) {
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", serde_json::to_string(v).unwrap_or_default());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_suggestions_become_tool_calls() {
        assert_eq!(tool_call_text("status"), "tessra_status {}");
        assert_eq!(
            tool_call_text("verify --stage landed"),
            r#"tessra_verify {"stage":"landed"}"#
        );
        assert_eq!(
            tool_call_text("context --path src/lib.rs --budget 8000"),
            r#"tessra_context {"budget":8000,"path":"src/lib.rs"}"#
        );
        assert_eq!(
            tool_call_text("claim --action claim --paths <paths>"),
            r#"tessra_claim {"action":"claim","paths":"<paths>"}"#
        );
        assert_eq!(
            tool_call_text("promote --to landed --all"),
            r#"tessra_promote {"all":true,"to":"landed"}"#
        );
        assert_eq!(
            tool_call_text("remember --kind gotcha --body 'two words'"),
            r#"tessra_remember {"body":"two words","kind":"gotcha"}"#
        );
        assert_eq!(
            tool_call_text("snapshot --then promote landed"),
            r#"tessra_snapshot {"then":"promote landed"}"#
        );
    }

    #[test]
    fn every_tool_has_a_schema_with_descriptions() {
        for (v, _) in VERBS {
            let s = tool_schema(v);
            assert_eq!(s["type"], json!("object"), "{v}");
            assert!(s["required"].is_array(), "{v}");
            for (_, p) in s["properties"].as_object().unwrap() {
                assert!(
                    p.get("description").is_some() || p.get("type").is_some(),
                    "{v}"
                );
            }
        }
        assert!(MANUAL.contains("thirteen verbs"));
    }
}
