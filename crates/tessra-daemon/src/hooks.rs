//! Hooks: declarative subscriptions on events, per HOOKS.md and
//! `spec/02-objects.md` hook. Standards gate; hooks react. A hook is an
//! entity with an event pattern, an optional `where` predicate over the
//! event, and actions that run as the hook's principal after the op that
//! produced the event has been committed. A failing action is reported and
//! never blocks the stage transition.
//!
//! Actions in this milestone: `webhook` (HTTP POST of the event, signed by
//! the daemon), `run` (a local command with the event on stdin), `verify`
//! (run the verifiers for the change now), `attest` (record an attestation
//! under the hook's principal), and `remember` (open a memory, a task by
//! default).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::Duration;

use ciborium::value::Value as Cbor;
use serde_json::{json, Value as Json};
use tessra_core::cbor;
use tessra_core::object::{Effect, Hook, HookAction, Memory, MemoryScope, Predicate, Revision};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build;
use tessra_oplog::glob;
use tessra_oplog::view::ViewState;

use crate::tree;
use crate::{now, Error, Repo, Result};

/// What a hook sees.
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: String,
    pub change: EntityId,
    pub revision: ObjectId,
    pub snapshot: ObjectId,
    pub title: String,
    pub author: EntityId,
    /// Paths changed against the revision's first parent.
    pub paths: Vec<String>,
    pub extra: Json,
}

impl Event {
    pub fn new(
        repo: &Repo,
        kind: &str,
        rev_id: ObjectId,
        rev: &Revision,
        extra: Json,
    ) -> Result<Event> {
        let snapshot = rev
            .snapshots
            .get("")
            .copied()
            .ok_or_else(|| Error::verb("ROOT", "revision has no root snapshot"))?;
        Ok(Event {
            kind: kind.into(),
            change: rev.id,
            revision: rev_id,
            snapshot,
            title: rev.title.clone(),
            author: rev.author,
            paths: changed_paths(repo, rev)?,
            extra,
        })
    }

    pub fn to_json(&self, hook: &str) -> Json {
        json!({
            "event": self.kind,
            "hook": hook,
            "change": self.change.to_letters(),
            "revision": self.revision.to_hex(),
            "snapshot": self.snapshot.to_hex(),
            "title": self.title,
            "author": self.author.to_letters(),
            "paths": self.paths,
            "extra": self.extra,
        })
    }
}

/// Paths whose content differs between a revision and its first parent.
pub fn changed_paths(repo: &Repo, rev: &Revision) -> Result<Vec<String>> {
    let Some(snap_id) = rev.snapshots.get("") else {
        return Ok(Vec::new());
    };
    let snap: tessra_core::object::Snapshot = repo.store().get(snap_id)?;
    let cur = tree::flatten(repo.store(), &snap.root)?;
    let parent = match rev.parents.first() {
        Some(p) => {
            let pr: Revision = repo.store().get(p)?;
            match pr.snapshots.get("") {
                Some(s) => {
                    let ps: tessra_core::object::Snapshot = repo.store().get(s)?;
                    tree::flatten(repo.store(), &ps.root)?
                }
                None => tree::Flat::new(),
            }
        }
        None => tree::Flat::new(),
    };
    let mut out: Vec<String> = cur
        .iter()
        .filter(|(p, l)| parent.get(*p) != Some(*l))
        .map(|(p, _)| p.clone())
        .collect();
    out.extend(parent.keys().filter(|p| !cur.contains_key(*p)).cloned());
    out.sort();
    out.dedup();
    Ok(out)
}

/// Every hook in the view: entity ID, current version ID, and the hook.
pub fn all_hooks(repo: &Repo) -> Result<Vec<(EntityId, ObjectId, Hook)>> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let mut out = Vec::new();
    for (id, ptr) in vs.entities(&view)? {
        let Ok(oid) = ptr.single() else { continue };
        let Some(bytes) = repo.store().get_bytes(&oid)? else {
            continue;
        };
        if cbor::peek_tag(&bytes).ok().as_deref() == Some("hook") {
            if let Ok(h) = cbor::decode::<Hook>(&bytes) {
                out.push((id, oid, h));
            }
        }
    }
    out.sort_by(|a, b| {
        a.2.order
            .unwrap_or(0)
            .cmp(&b.2.order.unwrap_or(0))
            .then(a.2.name.cmp(&b.2.name))
    });
    Ok(out)
}

/// Whether an event pattern matches: exact, or a `prefix.*` family.
fn on_matches(pattern: &str, kind: &str) -> bool {
    pattern == kind
        || pattern == "*"
        || pattern
            .strip_suffix(".*")
            .is_some_and(|p| kind == p || kind.starts_with(&format!("{p}.")))
}

/// The `where` grammar over an event: `scope(pattern)` matches a changed
/// path, `author(id)` the author, `title(text)` a substring; `all`, `any`,
/// and `not` nest.
fn where_matches(p: &Predicate, event: &Event) -> bool {
    let name = p.name.as_deref().unwrap_or("");
    match p.kind.as_str() {
        "scope" => event.paths.iter().any(|path| glob::matches(name, path)),
        "author" => event.author.to_letters().starts_with(name),
        "title" => event.title.contains(name),
        "event" => on_matches(name, &event.kind),
        "all" | "any" | "not" => {
            let preds: Vec<Predicate> = p
                .args
                .as_ref()
                .and_then(|a| a.get("preds"))
                .and_then(|v| v.clone().deserialized::<Vec<Predicate>>().ok())
                .unwrap_or_default();
            match p.kind.as_str() {
                "all" => preds.iter().all(|q| where_matches(q, event)),
                "any" => preds.iter().any(|q| where_matches(q, event)),
                _ => !preds.first().is_some_and(|q| where_matches(q, event)),
            }
        }
        _ => false,
    }
}

/// Run every enabled hook that subscribes to the event. Returns one entry
/// per action run, with its outcome.
pub fn fire(repo: &mut Repo, event: &Event) -> Result<Vec<Json>> {
    let hooks = all_hooks(repo)?;
    let mut out = Vec::new();
    for (_, _, h) in hooks {
        if !h.enabled || !on_matches(&h.on, &event.kind) {
            continue;
        }
        if let Some(w) = &h.r#where {
            if !where_matches(w, event) {
                continue;
            }
        }
        for action in &h.r#do {
            let result = run_action(repo, &h, action, event);
            out.push(match result {
                Ok(j) => json!({ "hook": h.name, "action": action.kind, "ok": true, "result": j }),
                Err(e) => json!({ "hook": h.name, "action": action.kind, "ok": false, "error": e.to_string() }),
            });
        }
    }
    Ok(out)
}

fn arg_text(a: &HookAction, key: &str) -> Option<String> {
    match a.args.get(key) {
        Some(Cbor::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

fn run_action(repo: &mut Repo, hook: &Hook, action: &HookAction, event: &Event) -> Result<Json> {
    let payload = event.to_json(&hook.name);
    match action.kind.as_str() {
        "webhook" => {
            let url = arg_text(action, "url")
                .or_else(|| arg_text(action, "name"))
                .ok_or_else(|| Error::verb("HOOK", "webhook needs url"))?;
            let body = serde_json::to_vec(&payload).unwrap_or_default();
            let sig = repo.daemon_signer().key.sign("hook", &body).0;
            let headers = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Tessra-Event".to_string(), event.kind.clone()),
                ("X-Tessra-Hook".to_string(), hook.name.clone()),
                ("X-Tessra-Principal".to_string(), repo.daemon.to_letters()),
                (
                    "X-Tessra-Signature".to_string(),
                    format!("ed25519:{}", hex::encode(sig)),
                ),
            ];
            let (status, resp) = http_post(&url, &headers, &body, Duration::from_secs(10))?;
            Ok(
                json!({ "url": url, "status": status, "response": String::from_utf8_lossy(&resp[..resp.len().min(400)]) }),
            )
        }
        "run" => {
            let cmd = arg_text(action, "cmd")
                .or_else(|| arg_text(action, "name"))
                .ok_or_else(|| Error::verb("HOOK", "run needs cmd"))?;
            let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
            if argv.is_empty() {
                return Err(Error::verb("HOOK", "run needs a command"));
            }
            let mut c = crate::quiet(Command::new(&argv[0]));
            c.args(&argv[1..]);
            c.current_dir(&repo.root);
            c.env(
                "TESSRA_EVENT",
                serde_json::to_string(&payload).unwrap_or_default(),
            );
            c.env("TESSRA_REPO", repo.root.display().to_string());
            c.stdin(Stdio::piped());
            c.stdout(Stdio::piped());
            c.stderr(Stdio::piped());
            let mut child = c
                .spawn()
                .map_err(|e| Error::verb("HOOK", format!("cannot run {}: {e}", argv[0])))?;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&serde_json::to_vec(&payload).unwrap_or_default());
            }
            let started = std::time::Instant::now();
            let status = loop {
                if let Some(s) = child.try_wait()? {
                    break s;
                }
                if started.elapsed() > Duration::from_secs(60) {
                    let _ = child.kill();
                    return Err(Error::verb("HOOK", "command timed out after 60 s"));
                }
                std::thread::sleep(Duration::from_millis(25));
            };
            let mut out = String::new();
            if let Some(mut s) = child.stdout.take() {
                let _ = s.read_to_string(&mut out);
            }
            Ok(
                json!({ "exit": status.code(), "output": out.chars().take(400).collect::<String>() }),
            )
        }
        "land" => {
            // The continuous frontier: land the event's revision now if the
            // standard holds. Runs as the coordinator.
            let rev: Revision = repo.store().get(&event.revision)?;
            let Some(ws) = repo
                .workspaces
                .iter()
                .find(|w| w.current == Some(event.revision))
                .cloned()
            else {
                return Err(Error::verb(
                    "HOOK",
                    "no workspace holds the revision to land",
                ));
            };
            let mut actor = repo.daemon_actor();
            match crate::verbs::land_revision(repo, &mut actor, &ws, event.revision, &rev) {
                Ok(o) => Ok(json!({
                    "landed": o.result.get("stage").and_then(Json::as_str) == Some("landed"),
                    "seq": o.result.get("seq"), "conflicts": o.result.get("conflicts"),
                    "revision": o.result.get("revision"),
                })),
                Err(e) => Ok(json!({ "landed": false, "code": e.code(), "why": e.to_string() })),
            }
        }
        "agent" => {
            // Spawn an agent with an intent: the daemon records the intent and
            // an assignment scoped to the event's paths, then starts the
            // configured agent runner detached, with the event in its
            // environment. The runner speaks to the daemon like any agent.
            let name = arg_text(action, "name")
                .ok_or_else(|| Error::verb("HOOK", "agent needs a name"))?;
            let intent_title = arg_text(action, "intent")
                .unwrap_or_else(|| format!("{} {}", event.kind, event.title));
            let budget: u64 = action
                .args
                .get("budget")
                .and_then(|v| match v {
                    Cbor::Integer(i) => Some(i128::from(*i) as u64),
                    Cbor::Text(t) => t.parse().ok(),
                    _ => None,
                })
                .unwrap_or(500);
            let agent_id = repo.agent_id(&name)?;
            let t = now();
            let intent = tessra_core::object::Intent {
                id: EntityId::random(),
                prev: None,
                title: intent_title.clone(),
                body: Some(serde_json::to_string(&payload).unwrap_or_default()),
                spec: None,
                evals: None,
                parent: None,
                depends: None,
                priority: 0,
                status: "open".into(),
                assignee: Some(agent_id),
                created_by: hook.r#as,
                time: t,
                legacy: None,
            };
            let intent_oid = repo.store().put(&intent)?;
            let task = Memory {
                id: EntityId::random(),
                prev: None,
                kind: "task".into(),
                scope: MemoryScope {
                    kind: "intent".into(),
                    r#ref: Cbor::Bytes(intent.id.0.to_vec()),
                },
                body: format!(
                    "{name}: {intent_title} (from {} on change {})",
                    event.kind,
                    event.change.to_letters()
                ),
                anchor: None,
                confidence: 1000,
                author: hook.r#as,
                time: t,
                expires: None,
                proposed: None,
                status: "active".into(),
                links: Some(vec![event.revision]),
                visibility: "shared".into(),
            };
            let task_oid = repo.store().put(&task)?;
            let signer = repo.daemon_signer();
            let op = build::build_op(
                &repo.log,
                &signer,
                None,
                "plan",
                build::args_with_idem(EntityId::random().0, BTreeMap::new()),
                vec![
                    Effect::Put { id: intent_oid },
                    Effect::Point {
                        entity: intent.id,
                        to: intent_oid,
                        from: None,
                    },
                    Effect::Put { id: task_oid },
                    Effect::Point {
                        entity: task.id,
                        to: task_oid,
                        from: None,
                    },
                ],
                t,
            )?;
            repo.commit_op(&op)?;
            crate::swarm::save_assignment(
                repo,
                &crate::swarm::Assignment {
                    agent: agent_id,
                    intent: intent.id,
                    title: intent_title.clone(),
                    paths: if event.paths.is_empty() {
                        vec!["**".into()]
                    } else {
                        event.paths.clone()
                    },
                    units: vec![],
                    ops: budget,
                },
            )?;
            let runner = match repo.config().get("agent_runner") {
                Some(Cbor::Text(r)) if !r.trim().is_empty() => r.clone(),
                _ => {
                    return Ok(
                        json!({ "agent": name, "intent": intent.id.to_letters(), "spawned": false, "why": "no agent_runner configured; set it with tessra config --set agent_runner=<command>" }),
                    )
                }
            };
            let argv: Vec<String> = runner.split_whitespace().map(str::to_string).collect();
            let log_dir = crate::paths::workspaces_dir_for(&repo.repo_id).join("agents");
            std::fs::create_dir_all(&log_dir)?;
            let log = std::fs::File::create(log_dir.join(format!("{name}-{t}.log")))?;
            let err = log.try_clone()?;
            let mut c = crate::quiet(Command::new(&argv[0]));
            c.args(&argv[1..]);
            c.current_dir(&repo.root);
            c.env("TESSRA_AGENT", &name);
            c.env("TESSRA_INTENT", intent.id.to_letters());
            c.env("TESSRA_INTENT_TITLE", &intent_title);
            c.env(
                "TESSRA_EVENT",
                serde_json::to_string(&payload).unwrap_or_default(),
            );
            c.env("TESSRA_REPO", repo.root.display().to_string());
            if let Ok(exe) = std::env::current_exe() {
                c.env("TESSRA_EXE", exe.display().to_string());
            }
            c.stdin(Stdio::null());
            c.stdout(Stdio::from(log));
            c.stderr(Stdio::from(err));
            let child = c.spawn().map_err(|e| {
                Error::verb(
                    "HOOK",
                    format!("cannot start agent runner {}: {e}", argv[0]),
                )
            })?;
            Ok(
                json!({ "agent": name, "intent": intent.id.to_letters(), "task": task.id.to_letters(), "spawned": true, "pid": child.id(), "log": log_dir.display().to_string() }),
            )
        }
        "verify" => {
            let rev: Revision = repo.store().get(&event.revision)?;
            let ran = crate::verbs::run_verifiers_for(repo, &event.revision, &rev, false, &[])?;
            Ok(json!({ "ran": ran }))
        }
        "attest" => {
            let kind = arg_text(action, "kind")
                .or_else(|| arg_text(action, "name"))
                .ok_or_else(|| Error::verb("HOOK", "attest needs kind"))?;
            let result = match action.args.get("result") {
                Some(v) => v.clone(),
                None => Cbor::Bool(true),
            };
            let id = crate::verifiers::attest_as_runner(
                repo,
                &kind,
                Some((event.revision.as_bytes().as_slice(), "revision")),
                None,
                Some(BTreeMap::from([(
                    "hook".to_string(),
                    Cbor::Text(hook.name.clone()),
                )])),
                result,
                None,
                hook.r#as,
                None,
            )?;
            Ok(json!({ "attestation": id.to_hex() }))
        }
        "notify" => {
            let channel = arg_text(action, "channel").or_else(|| arg_text(action, "name"));
            let text = arg_text(action, "text")
                .unwrap_or_else(|| format!("{}: {}", event.kind, event.title));
            let mut message = payload.clone();
            message["text"] = json!(text);
            let delivered = crate::verbs::deliver_to_channels(repo, &message, channel.as_deref())?;
            Ok(json!({ "delivered": delivered }))
        }
        "remember" | "task" => {
            let body = arg_text(action, "body")
                .or_else(|| arg_text(action, "name"))
                .unwrap_or_else(|| {
                    format!(
                        "{} {}: {}",
                        event.kind,
                        event.change.to_letters(),
                        event.title
                    )
                });
            let kind = arg_text(action, "kind").unwrap_or_else(|| "task".into());
            let m = Memory {
                id: EntityId::random(),
                prev: None,
                kind,
                scope: MemoryScope {
                    kind: "repo".into(),
                    r#ref: Cbor::Text(String::new()),
                },
                body,
                anchor: None,
                confidence: 1000,
                author: hook.r#as,
                time: now(),
                expires: None,
                proposed: None,
                status: "active".into(),
                links: Some(vec![event.revision]),
                visibility: "shared".into(),
            };
            let mid = repo.store().put(&m)?;
            let signer = repo.daemon_signer();
            let op = build::build_op(
                &repo.log,
                &signer,
                None,
                "remember",
                build::args_with_idem(EntityId::random().0, BTreeMap::new()),
                vec![
                    Effect::Put { id: mid },
                    Effect::Point {
                        entity: m.id,
                        to: mid,
                        from: None,
                    },
                ],
                now(),
            )?;
            repo.commit_op(&op)?;
            Ok(json!({ "memory": m.id.to_letters() }))
        }
        other => Err(Error::verb(
            "HOOK",
            format!("action {other} is not available yet"),
        )),
    }
}

/// A minimal HTTP/1.1 POST over plain TCP. `https` is not supported here;
/// a `run` action with curl covers it until a TLS client is wired in.
pub fn http_post(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    timeout: Duration,
) -> Result<(u16, Vec<u8>)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| Error::verb("HOOK", "webhook url must start with http://"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr_text = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let addr = addr_text
        .to_socket_addrs()
        .map_err(|e| Error::verb("HOOK", format!("resolve {hostport}: {e}")))?
        .next()
        .ok_or_else(|| Error::verb("HOOK", format!("resolve {hostport}: no address")))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| Error::verb("HOOK", format!("connect {hostport}: {e}")))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.write_all(body)?;
    let mut resp = Vec::new();
    let _ = stream.read_to_end(&mut resp);
    let text = String::from_utf8_lossy(&resp);
    let status: u16 = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body_start = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(resp.len());
    Ok((status, resp[body_start..].to_vec()))
}

/// Build a hook action from its text form `kind(arg, key=value)`: the
/// positional argument becomes `name`.
pub fn parse_action(text: &str) -> std::result::Result<HookAction, String> {
    let p = tessra_oplog::standard::parse_predicate(text)?;
    let mut args = p.args.unwrap_or_default();
    if let Some(n) = p.name {
        args.insert("name".into(), Cbor::Text(n));
    }
    Ok(HookAction { kind: p.kind, args })
}

/// A hook as text, for listings.
pub fn describe(h: &Hook) -> Json {
    let actions: Vec<String> = h
        .r#do
        .iter()
        .map(|a| {
            let inner: Vec<String> = a
                .args
                .iter()
                .map(|(k, v)| match v {
                    Cbor::Text(t) if k == "name" => t.clone(),
                    Cbor::Text(t) => format!("{k}={t}"),
                    Cbor::Integer(i) => format!("{k}={}", i128::from(*i)),
                    Cbor::Bool(b) => format!("{k}={b}"),
                    other => format!("{k}={other:?}"),
                })
                .collect();
            format!("{}({})", a.kind, inner.join(", "))
        })
        .collect();
    json!({
        "id": h.id.to_letters(),
        "name": h.name,
        "on": h.on,
        "where": h.r#where.as_ref().map(tessra_oplog::standard::describe),
        "do": actions,
        "as": h.r#as.to_letters(),
        "enabled": h.enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_and_where_clauses() {
        assert!(on_matches("proposed", "proposed"));
        assert!(on_matches("conflict.*", "conflict.opened"));
        assert!(!on_matches("landed", "proposed"));
        let ev = Event {
            kind: "proposed".into(),
            change: EntityId::random(),
            revision: ObjectId([1; 32]),
            snapshot: ObjectId([2; 32]),
            title: "fix payments rounding".into(),
            author: EntityId::random(),
            paths: vec!["services/payments/lib.rs".into()],
            extra: Json::Null,
        };
        let p = tessra_oplog::standard::parse_predicate("scope(services/**)").unwrap();
        assert!(where_matches(&p, &ev));
        let q = tessra_oplog::standard::parse_predicate("all(scope(docs/**), title(fix))").unwrap();
        assert!(!where_matches(&q, &ev));
        let a = parse_action("webhook(http://127.0.0.1:9/ci)").unwrap();
        assert_eq!(a.kind, "webhook");
        assert_eq!(
            arg_text(&a, "name").as_deref(),
            Some("http://127.0.0.1:9/ci")
        );
    }
}
