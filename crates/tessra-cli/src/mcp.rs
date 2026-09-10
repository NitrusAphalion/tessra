//! A minimal MCP server over stdio: newline-delimited JSON-RPC 2.0 with
//! `initialize`, `tools/list`, `tools/call`, and `ping`. Each tool is one
//! verb named `tessra_<verb>`; arguments pass straight to the verb.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const VERBS: &[(&str, &str)] = &[
    ("status", "Where am I: my assignment from the planner (units, paths, budget), claims, open questions, my change's standard status, what changed since a cursor. Call first every session."),
    ("context", "A context pack within a token budget. path: the file's content, its units, memories, claims. unit (name or path:name): the unit's text, what it depends on, what depends on it, the tests that cover it, memories, claims, and who last changed it."),
    ("query", "Read the graph. kind: memory (scope.ref, kinds), revision (change), since (since cursor), object (id prefix; a blob such as a verifier's evidence is paged: offset, or tail for its end), blame (path: who last changed each unit), tests (path: the tests covering each unit), diff (change?, path?: units added, removed, changed, renamed against the parent), trusted (change: the most attested trunk snapshot containing it), bisect (test: the first trunk revision where a test in your revision fails, using cached results), activity (window like 1h, altitude summary|changes|ops: what happened), exceptions (open requests for a human with the judges' reasoning). Budgeted."),
    ("workspace", "action: create | list | drop | rewind. Create materializes trunk or a revision into your own workspace (with_state restores its build state); rewind puts your workspace back at a revision with everything it carried."),
    ("edit", "Write a file (path, content), replace text (path, old, new, all?), or rename an identifier everywhere (rename, to, path?) as a recorded semantic operation the merge carries into concurrent changes. Plain file edits through other tools also count."),
    ("snapshot", "Record the workspace's state as a revision. Never blocks; flags tell you what will block promotion. then: verify | promote."),
    ("claim", "action: claim (paths, expires_s?, exclusive?, note?) | release (id?). Advisory; the response lists other claims that overlap yours, which means a merge is coming."),
    ("remember", "Record a memory. kind: fact|decision|convention|gotcha|preference|task|question|summary|resolution. scope: {kind, ref}. body. confidence 0..1."),
    ("verify", "Run the verifiers the standard still needs (cached when the snapshot was verified before, selected by covering tests when possible) and report the standard clause by clause with what would satisfy each unmet one. full: run everything."),
    ("try", "Speculate: candidates [{path, content} | {path, old, new}, title?] are each materialized on your revision, verified, scored, and ranked; keep: <candidate> writes the winner into your workspace."),
    ("promote", "to: proposed | landed | <target> (with slice: canary | all). Refused with the unmet clauses if the standard does not hold."),
    ("revert", "Public: revert a landed change (change, reason?) with its edits undone at unit granularity; a task is opened. Landed at once by the owner, proposed by anyone else."),
    ("undo", "Take back your most recent op, or op: <hex>. Private; never a landing."),
];

fn tool_schema(verb: &str) -> Value {
    let props = match verb {
        "status" => json!({ "since": { "type": "string" } }),
        "context" => json!({ "path": { "type": "string" }, "unit": { "type": "string" }, "budget": { "type": "integer" } }),
        "try" => json!({ "candidates": { "type": "array", "items": { "type": "object" } }, "keep": { "type": "integer" } }),
        "query" => {
            json!({ "kind": { "type": "string" }, "scope": { "type": "object" }, "kinds": { "type": "array", "items": { "type": "string" } }, "change": { "type": "string" }, "since": { "type": "string" }, "id": { "type": "string" }, "path": { "type": "string" }, "test": { "type": "string" }, "window": { "type": "string" }, "altitude": { "type": "string" }, "offset": { "type": "integer" }, "tail": { "type": "boolean" }, "budget": { "type": "integer" } })
        }
        "workspace" => {
            json!({ "action": { "type": "string" }, "from": { "type": "string" }, "path": { "type": "string" }, "id": { "type": "string" }, "with_state": { "type": "boolean" }, "to": { "type": "string" } })
        }
        "edit" => {
            json!({ "path": { "type": "string" }, "content": { "type": "string" }, "old": { "type": "string" }, "new": { "type": "string" }, "all": { "type": "boolean" }, "rename": { "type": "string" }, "to": { "type": "string" }, "delete": { "type": "boolean" }, "resolve": { "type": "boolean" } })
        }
        "snapshot" => {
            json!({ "title": { "type": "string" }, "then": { "type": "string" }, "with_state": { "type": "boolean" }, "idem": { "type": "string" } })
        }
        "claim" => {
            json!({ "action": { "type": "string" }, "paths": { "type": "array", "items": { "type": "string" } }, "id": { "type": "string" }, "expires_s": { "type": "integer" }, "exclusive": { "type": "boolean" }, "note": { "type": "string" }, "idem": { "type": "string" } })
        }
        "remember" => {
            json!({ "kind": { "type": "string" }, "body": { "type": "string" }, "scope": { "type": "object" }, "confidence": { "type": "number" }, "visibility": { "type": "string" }, "idem": { "type": "string" } })
        }
        "verify" => json!({ "kinds": { "type": "array", "items": { "type": "string" } }, "full": { "type": "boolean" } }),
        "promote" => json!({ "to": { "type": "string" }, "slice": { "type": "string" }, "idem": { "type": "string" } }),
        "undo" => json!({ "op": { "type": "string" }, "idem": { "type": "string" } }),
        "revert" => json!({ "change": { "type": "string" }, "reason": { "type": "string" }, "idem": { "type": "string" } }),
        _ => json!({}),
    };
    json!({ "type": "object", "properties": props })
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
                "instructions": "Start with tessra_status. Every read takes a budget. Snapshot never blocks; promote enforces. Record gotchas with tessra_remember."
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
                let out = call(verb, &args);
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
