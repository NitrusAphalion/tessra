//! Delivery, per `spec/02-objects.md` group E and PIPELINE.md observe and
//! revert: targets with a deployer adapter and observers, releases signed
//! by whoever cuts them, deployments as effects on the view, progressive
//! delivery by slice, observation as attestations, and rollback, automatic
//! when the target's standard trips.
//!
//! Adapters in this milestone: a `dir` deployer copies the snapshot's tree
//! into `<path>/<slice>`; a `run` deployer or observer runs a command with
//! the deployment in its environment and, for observers, reads a JSON
//! object of signals from its stdout. Secrets named `vault:local:<name>`
//! come from machine-local configuration and reach only the command's
//! environment.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use ciborium::value::Value as Cbor;
use tessra_core::cbor;
use tessra_core::object::{
    Attestation, Deployment, Effect, Memory, MemoryScope, Release, Revision, Snapshot, Standard, Target,
};
use tessra_core::sig::SignedObject;
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build;
use tessra_oplog::standard;
use tessra_oplog::view::ViewState;
use serde_bytes::ByteBuf;
use serde_json::{json, Value as Json};

use crate::hooks::{self, Event};
use crate::principals::Actor;
use crate::{fs, now, Error, Repo, Result};

// ------------------------------------------------------------- targets

/// Every target in the view with its entity ID and current version.
pub fn all_targets(repo: &Repo) -> Result<Vec<(EntityId, ObjectId, Target)>> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let mut out = Vec::new();
    for (id, ptr) in vs.entities(&view)? {
        let Ok(oid) = ptr.single() else { continue };
        let Some(bytes) = repo.store().get_bytes(&oid)? else { continue };
        if cbor::peek_tag(&bytes).ok().as_deref() == Some("target") {
            if let Ok(t) = cbor::decode::<Target>(&bytes) {
                out.push((id, oid, t));
            }
        }
    }
    out.sort_by(|a, b| a.2.name.cmp(&b.2.name));
    Ok(out)
}

pub fn target_by_name(repo: &Repo, name: &str) -> Result<(EntityId, ObjectId, Target)> {
    all_targets(repo)?
        .into_iter()
        .find(|(_, _, t)| t.name == name)
        .ok_or_else(|| Error::verb("NOT_FOUND", format!("no target named {name}; the owner creates one with `tessra target --name {name} --deployer ...`")))
}

/// The current deployment of a target, if any.
pub fn deployed(repo: &Repo, target: &EntityId) -> Result<Option<(ObjectId, Deployment)>> {
    let view = repo.log.current_view()?;
    match view.deployed.get(target) {
        Some(id) => Ok(Some((*id, repo.store().get(id)?))),
        None => Ok(None),
    }
}

fn text(m: &BTreeMap<String, Cbor>, k: &str) -> Option<String> {
    match m.get(k) {
        Some(Cbor::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

/// Create a target: its own standard that extends trunk's, the deployer
/// and observer adapters as parsed from `kind(arg)` text, and the canary
/// percentage. Owner only.
pub fn create_target(
    repo: &mut Repo,
    actor: &Actor,
    name: &str,
    deployer: &str,
    observers: &[String],
    canary_percent: u64,
    secrets: &[String],
) -> Result<Json> {
    if all_targets(repo)?.iter().any(|(_, _, t)| t.name == name) {
        return Err(Error::verb("EXISTS", format!("target {name} exists")));
    }
    let adapter = |text: &str| -> Result<BTreeMap<String, Cbor>> {
        let p = standard::parse_predicate(text).map_err(|e| Error::verb("ARGS", e))?;
        let mut m = BTreeMap::from([("kind".to_string(), Cbor::Text(p.kind.clone()))]);
        if let Some(n) = p.name {
            m.insert(if p.kind == "dir" { "path".into() } else { "cmd".into() }, Cbor::Text(n));
        }
        for (k, v) in p.args.unwrap_or_default() {
            m.insert(k, v);
        }
        Ok(m)
    };
    let mut deployer_map = adapter(deployer)?;
    if !secrets.is_empty() {
        deployer_map.insert(
            "secrets".into(),
            Cbor::Array(secrets.iter().map(|s| Cbor::Text(s.clone())).collect()),
        );
    }
    deployer_map.insert("canary_percent".into(), Cbor::Integer(canary_percent.into()));
    let trunk_std = crate::verbs::trunk_standard_of(repo)?;
    let t = now();
    let std = Standard {
        id: EntityId::random(),
        prev: None,
        name: format!("release:{name}"),
        extends: Some(trunk_std),
        clauses: vec![],
        scopes: None,
        when: None,
        audit_permille: None,
    };
    let std_oid = repo.store().put(&std)?;
    let view = repo.log.current_view()?;
    let line = view.lines.get("trunk").map(|l| l.id);
    let target = Target {
        id: EntityId::random(),
        prev: None,
        name: name.into(),
        deployer: deployer_map,
        standard: std.id,
        line,
        observers: if observers.is_empty() {
            None
        } else {
            Some(observers.iter().map(|o| adapter(o)).collect::<Result<Vec<_>>>()?)
        },
        risk_budget: None,
        sensitivity: None,
    };
    let target_oid = repo.store().put(&target)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "target",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        vec![
            Effect::Put { id: std_oid },
            Effect::Point { entity: std.id, to: std_oid, from: None },
            Effect::Put { id: target_oid },
            Effect::Point { entity: target.id, to: target_oid, from: None },
        ],
        t,
    )?;
    repo.commit_op(&op)?;
    Ok(json!({ "target": target.id.to_letters(), "name": name, "standard": std.id.to_letters(), "deployer": deployer, "observers": observers, "canary_percent": canary_percent }))
}

pub fn describe_target(repo: &Repo, id: &EntityId, t: &Target) -> Result<Json> {
    let dep = deployed(repo, id)?;
    let (rev_title, slice) = match &dep {
        Some((_, d)) => {
            let title = repo.store().get::<Revision>(&d.revision).map(|r| r.title).unwrap_or_default();
            let slice = d.step.as_ref().and_then(|s| text(s, "of"));
            (Some(title), slice)
        }
        None => (None, None),
    };
    Ok(json!({
        "id": id.to_letters(), "name": t.name, "standard": t.standard.to_letters(),
        "deployer": t.deployer.get("kind").and_then(|v| v.as_text()), "observers": t.observers.as_ref().map(|o| o.len()).unwrap_or(0),
        "deployed": dep.as_ref().map(|(id, d)| json!({ "deployment": id.to_hex(), "revision": d.revision.to_hex(), "title": rev_title, "slice": slice, "time": d.time })),
    }))
}

// ------------------------------------------------------------- releases

/// Cut a release of the trunk head: signed by the cutter, with a changelog
/// of the titles since the last release and the attestations the landing
/// cited. Content-addressed; indexed in store metadata.
pub fn cut_release(repo: &mut Repo, actor: &Actor, name: &str) -> Result<Json> {
    let (head_id, _) = crate::verbs::trunk_head_of(repo)?;
    let head: Revision = repo.store().get(&head_id)?;
    let view = repo.log.current_view()?;
    let line = view.lines.get("trunk").map(|l| l.id).ok_or_else(|| Error::verb("LINE", "no trunk"))?;
    let previous = releases(repo)?.into_iter().next();
    let since = previous.as_ref().map(|(_, r)| r.revision);
    // Titles from the head back to the previous release's revision.
    let mut lines = Vec::new();
    let mut cur = Some(head_id);
    let mut steps = 0;
    while let Some(id) = cur {
        if Some(id) == since || steps > 500 {
            break;
        }
        steps += 1;
        let r: Revision = repo.store().get(&id)?;
        lines.push(format!("- {} ({})", r.title, repo.principal_name(&r.author).unwrap_or_default()));
        cur = r.parents.first().copied();
    }
    let changelog = repo.store().put_blob(format!("{name}\n\n{}\n", lines.join("\n")).as_bytes())?;
    let (snap_id, _) = crate::verbs::root_snapshot_of(repo, &head)?;
    let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
    let attestations: Vec<ObjectId> = crate::verifiers::collect_for(repo, &head_id, &head, Some(&idx))?
        .into_iter()
        .filter(|id| repo.store().get::<Attestation>(id).map(|a| crate::verifiers::trusted_signer(repo, &a)).unwrap_or(false))
        .collect();
    let mut release = Release {
        name: name.into(),
        line,
        revision: head_id,
        since,
        changelog,
        attestations: if attestations.is_empty() { None } else { Some(attestations.clone()) },
        signer: actor.principal(),
        time: now(),
        sig: None,
    };
    release.sign_with(&actor.signer.key)?;
    let rid = repo.store().put(&release)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "release",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        vec![Effect::Put { id: rid }],
        now(),
    )?;
    repo.commit_op(&op)?;
    let mut list = repo.store().meta("releases")?.unwrap_or_default();
    list.extend_from_slice(rid.as_bytes());
    repo.store().set_meta("releases", &list)?;
    let hooks_out = hooks::fire(repo, &Event::new(repo, "released", head_id, &head, json!({ "release": name }))?)?;
    Ok(json!({ "release": rid.to_hex(), "name": name, "revision": head_id.to_hex(), "title": head.title, "changes": lines.len(), "attestations": attestations.len(), "since": since.map(|s| s.to_hex()), "hooks": hooks_out }))
}

/// Releases, newest first.
pub fn releases(repo: &Repo) -> Result<Vec<(ObjectId, Release)>> {
    let mut out = Vec::new();
    for chunk in repo.store().meta("releases")?.unwrap_or_default().chunks(32) {
        if let Ok(id) = ObjectId::from_slice(chunk) {
            if let Ok(r) = repo.store().get::<Release>(&id) {
                out.push((id, r));
            }
        }
    }
    out.sort_by_key(|(_, r)| std::cmp::Reverse(r.time));
    Ok(out)
}

// ------------------------------------------------------------- deploy

fn secrets_env(repo: &Repo, deployer: &BTreeMap<String, Cbor>) -> Vec<(String, String)> {
    let config = repo.config();
    let mut out = Vec::new();
    if let Some(Cbor::Array(list)) = deployer.get("secrets") {
        for s in list {
            if let Cbor::Text(r) = s {
                // vault:local:NAME reads vault.NAME from machine-local configuration.
                if let Some(name) = r.strip_prefix("vault:local:") {
                    if let Some(Cbor::Text(v)) = config.get(&format!("vault.{name}")) {
                        out.push((name.to_string(), v.clone()));
                    }
                }
            }
        }
    }
    out
}

fn run_adapter(cmd: &str, env: &[(String, String)], cwd: Option<&PathBuf>, timeout: Duration) -> Result<(bool, String)> {
    let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        return Err(Error::verb("ADAPTER", "empty command"));
    }
    let mut c = crate::quiet(Command::new(&argv[0]));
    c.args(&argv[1..]);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(Stdio::null());
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| Error::verb("ADAPTER", format!("cannot run {}: {e}", argv[0])))?;
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            return Err(Error::verb("ADAPTER", "adapter timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut out);
    }
    Ok((status.success(), out))
}

/// Deploy a revision to a target's slice: the target's standard for the
/// `all` slice, trunk's for a canary; the adapter; a deployment effect.
pub fn deploy(repo: &mut Repo, actor: &Actor, target_name: &str, revision: Option<ObjectId>, slice: &str, step_kind: &str) -> Result<Json> {
    let (tid, _, target) = target_by_name(repo, target_name)?;
    let rev_id = match revision {
        Some(r) => r,
        None => crate::verbs::trunk_head_of(repo)?.0,
    };
    let rev: Revision = repo.store().get(&rev_id)?;
    let (snap_id, snap) = crate::verbs::root_snapshot_of(repo, &rev)?;
    // The standard: the target's own clauses gate everyone; a canary is
    // gated by what the target extends.
    let std_id = if slice == "canary" {
        let view = repo.log.current_view()?;
        let vs = ViewState::new(repo.store());
        let s: Standard = repo.store().get(&vs.entity(&view, &target.standard)?.ok_or_else(|| Error::verb("STANDARD", "target standard missing"))?.single()?)?;
        s.extends.unwrap_or(target.standard)
    } else {
        target.standard
    };
    if step_kind != "rollback" {
        let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
        let attests = crate::verifiers::collect_for(repo, &rev_id, &rev, Some(&idx))?;
        let view = repo.log.current_view()?;
        let vs = ViewState::new(repo.store());
        let unmet = standard::evaluate(repo.store(), &vs, &view, &std_id, &rev_id, &rev, &attests)?;
        if !unmet.is_empty() {
            let list: Vec<Json> = unmet.iter().map(|u| json!({ "clause": u.clause, "reason": u.reason })).collect();
            return Err(Error::verb("STANDARD_UNMET", serde_json::to_string(&list).unwrap_or_default()));
        }
    }
    let previous = deployed(repo, &tid)?;
    let percent: u64 = match slice {
        "canary" => target.deployer.get("canary_percent").and_then(|v| match v {
            Cbor::Integer(i) => Some(i128::from(*i) as u64),
            _ => None,
        }).unwrap_or(10),
        _ => 100,
    };
    // Run the adapter.
    let scratch = crate::verifiers::materialize_scratch(repo, &snap.root, &format!("deploy-{}", &snap_id.to_hex()[..12]))?;
    let kind = text(&target.deployer, "kind").unwrap_or_default();
    let mut env: Vec<(String, String)> = vec![
        ("TESSRA_TARGET".into(), target_name.into()),
        ("TESSRA_SLICE".into(), slice.into()),
        ("TESSRA_PERCENT".into(), percent.to_string()),
        ("TESSRA_REVISION".into(), rev_id.to_hex()),
        ("TESSRA_SNAPSHOT_DIR".into(), scratch.display().to_string()),
        ("TESSRA_STEP".into(), step_kind.into()),
    ];
    env.extend(secrets_env(repo, &target.deployer));
    let adapter_out = match kind.as_str() {
        "dir" => {
            let base = PathBuf::from(text(&target.deployer, "path").unwrap_or_default());
            let dest = base.join(slice);
            let _ = std::fs::remove_dir_all(&dest);
            let n = fs::materialize(repo.store(), &snap.root, &dest, false)?;
            json!({ "kind": "dir", "path": dest.display().to_string(), "files": n })
        }
        "run" => {
            let cmd = text(&target.deployer, "cmd").unwrap_or_default();
            let (okk, out) = run_adapter(&cmd, &env, Some(&scratch), Duration::from_secs(600))?;
            if !okk {
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(Error::verb("DEPLOY", format!("deployer failed: {}", out.chars().take(300).collect::<String>())));
            }
            json!({ "kind": "run", "output": out.chars().take(300).collect::<String>() })
        }
        other => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(Error::verb("ADAPTER", format!("deployer kind {other} is not available yet")));
        }
    };
    let _ = std::fs::remove_dir_all(&scratch);
    let release = releases(repo)?.into_iter().find(|(_, r)| r.revision == rev_id).map(|(id, _)| id);
    let dep = Deployment {
        target: tid,
        revision: rev_id,
        release,
        principal: actor.principal(),
        previous: previous.as_ref().map(|(id, _)| *id),
        step: Some(BTreeMap::from([
            ("percent".to_string(), Cbor::Integer(percent.into())),
            ("of".to_string(), Cbor::Text(slice.into())),
            ("kind".to_string(), Cbor::Text(step_kind.into())),
        ])),
        time: now(),
    };
    let dep_id = repo.store().put(&dep)?;
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "deploy",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        vec![Effect::Put { id: dep_id }, Effect::Deploy { target: tid, to: dep_id }],
        now(),
    )?;
    repo.commit_op(&op)?;
    let event_kind = if step_kind == "rollback" { "target.rolled_back" } else { "deployed" };
    let hooks_out = hooks::fire(repo, &Event::new(repo, event_kind, rev_id, &rev, json!({ "target": target_name, "slice": slice, "percent": percent, "deployment": dep_id.to_hex() }))?)?;
    Ok(json!({
        "target": target_name, "slice": slice, "percent": percent, "deployment": dep_id.to_hex(), "revision": rev_id.to_hex(), "title": rev.title,
        "previous": previous.map(|(id, _)| id.to_hex()), "release": release.map(|r| r.to_hex()), "adapter": adapter_out, "step": step_kind, "hooks": hooks_out,
    }))
}

// ------------------------------------------------------------- observe

/// Run the target's observers on its current deployment, record each
/// signal as an attestation on the deployed revision, evaluate the
/// target's standard, and roll back when an observe clause trips and the
/// machine allows automatic reverts.
pub fn observe(repo: &mut Repo, actor: &Actor, target_name: &str) -> Result<Json> {
    let (tid, _, target) = target_by_name(repo, target_name)?;
    let Some((dep_id, dep)) = deployed(repo, &tid)? else {
        return Err(Error::verb("NOT_DEPLOYED", format!("nothing is deployed to {target_name}")));
    };
    let rev: Revision = repo.store().get(&dep.revision)?;
    let slice = dep.step.as_ref().and_then(|s| text(s, "of")).unwrap_or_else(|| "all".into());
    let observer_principal = crate::verifiers::ensure_verifier(repo, &format!("observer-{target_name}"))?;
    let mut signals: BTreeMap<String, f64> = BTreeMap::new();
    let mut attested = Vec::new();
    for o in target.observers.clone().unwrap_or_default() {
        let kind = text(&o, "kind").unwrap_or_default();
        if kind != "run" {
            continue;
        }
        let cmd = text(&o, "cmd").unwrap_or_default();
        let env = vec![
            ("TESSRA_TARGET".to_string(), target_name.to_string()),
            ("TESSRA_SLICE".to_string(), slice.clone()),
            ("TESSRA_DEPLOYMENT".to_string(), dep_id.to_hex()),
            ("TESSRA_REVISION".to_string(), dep.revision.to_hex()),
        ];
        let (_, out) = run_adapter(&cmd, &env, None, Duration::from_secs(120))?;
        let parsed: Json = serde_json::from_str(out.trim()).unwrap_or(Json::Null);
        if let Json::Object(map) = parsed {
            for (k, v) in map {
                if let Some(f) = v.as_f64() {
                    signals.insert(k, f);
                }
            }
        }
    }
    for (name, value) in &signals {
        let permille = (value * 1000.0).round() as i64;
        let mut att = Attestation {
            kind: format!("observe.{name}"),
            subject: Some(ByteBuf::from(dep.revision.0.to_vec())),
            bodies: None,
            subject_type: Some("revision".into()),
            scope: Some(BTreeMap::from([
                ("target".to_string(), Cbor::Text(target_name.into())),
                ("deployment".to_string(), Cbor::Text(dep_id.to_hex())),
                ("slice".to_string(), Cbor::Text(slice.clone())),
            ])),
            result: Cbor::Integer(permille.into()),
            env: None,
            verifier: observer_principal,
            runner: Some(repo.daemon),
            evidence: None,
            time: now(),
            sigkind: "ed25519".into(),
            sig: None,
        };
        att.sign_with(repo.daemon_key())?;
        let id = repo.store().put(&att)?;
        let signer = repo.daemon_signer();
        let op = build::build_op(&repo.log, &signer, None, "attest", build::args_with_idem(EntityId::random().0, BTreeMap::new()), vec![Effect::Put { id }], now())?;
        repo.commit_op(&op)?;
        attested.push(json!({ "signal": name, "value": value, "permille": permille, "attestation": id.to_hex() }));
    }
    // The target's standard over the deployed revision.
    let (snap_id, _) = crate::verbs::root_snapshot_of(repo, &rev)?;
    let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
    let attests = crate::verifiers::collect_for(repo, &dep.revision, &rev, Some(&idx))?;
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let unmet = standard::evaluate(repo.store(), &vs, &view, &target.standard, &dep.revision, &rev, &attests)?;
    let tripped: Vec<&standard::Unmet> = unmet.iter().filter(|u| u.clause.contains("observe(")).collect();
    let mut result = json!({
        "target": target_name, "deployment": dep_id.to_hex(), "revision": dep.revision.to_hex(), "title": rev.title, "slice": slice,
        "signals": attested, "unmet": unmet.iter().map(|u| json!({ "clause": u.clause, "reason": u.reason })).collect::<Vec<_>>(),
        "tripped": !tripped.is_empty(),
    });
    let hooks_out = hooks::fire(repo, &Event::new(repo, if tripped.is_empty() { "observed" } else { "observed.fail" }, dep.revision, &rev, json!({ "target": target_name, "signals": signals, "unmet": result["unmet"].clone() }))?)?;
    result["hooks"] = json!(hooks_out);
    let auto = !matches!(repo.config().get("auto_revert"), Some(Cbor::Bool(false)));
    if !tripped.is_empty() && auto {
        let why = tripped.iter().map(|u| format!("{}: {}", u.clause, u.reason)).collect::<Vec<_>>().join("; ");
        result["rollback"] = json!(rollback(repo, actor, target_name, &why)?);
    }
    Ok(result)
}

/// Roll a target back to its previous deployment's revision, open a task
/// saying what tripped, and tell every channel.
pub fn rollback(repo: &mut Repo, actor: &Actor, target_name: &str, why: &str) -> Result<Json> {
    let (tid, _, _) = target_by_name(repo, target_name)?;
    let Some((dep_id, dep)) = deployed(repo, &tid)? else {
        return Err(Error::verb("NOT_DEPLOYED", format!("nothing is deployed to {target_name}")));
    };
    // The previous deployment of a different revision: a canary of the
    // same revision is not a place to go back to.
    let mut cursor = dep.previous;
    let mut prev: Option<Deployment> = None;
    let mut steps = 0;
    while let Some(id) = cursor {
        steps += 1;
        if steps > 200 {
            break;
        }
        let d: Deployment = repo.store().get(&id)?;
        if d.revision != dep.revision {
            prev = Some(d);
            break;
        }
        cursor = d.previous;
    }
    let Some(prev) = prev else {
        return Err(Error::verb("ROLLBACK", "no earlier deployment of another revision to roll back to"));
    };
    let current_rev: Revision = repo.store().get(&dep.revision)?;
    let deployed_json = deploy(repo, actor, target_name, Some(prev.revision), "all", "rollback")?;
    let task = Memory {
        id: EntityId::random(),
        prev: None,
        kind: "task".into(),
        scope: MemoryScope {
            kind: match current_rev.intent { Some(_) => "intent".into(), None => "repo".into() },
            r#ref: match current_rev.intent { Some(i) => Cbor::Bytes(i.0.to_vec()), None => Cbor::Text(String::new()) },
        },
        body: format!("{target_name} rolled back from {} ({}) to the previous deployment: {why}. Investigate before promoting again.", current_rev.id.to_letters(), current_rev.title),
        anchor: None,
        confidence: 1000,
        author: repo.daemon,
        time: now(),
        expires: None,
        proposed: None,
        status: "active".into(),
        links: Some(vec![dep.revision, prev.revision]),
        visibility: "shared".into(),
    };
    let task_oid = repo.store().put(&task)?;
    let signer = repo.daemon_signer();
    let op = build::build_op(&repo.log, &signer, None, "revert", build::args_with_idem(EntityId::random().0, BTreeMap::new()), vec![Effect::Put { id: task_oid }, Effect::Point { entity: task.id, to: task_oid, from: None }], now())?;
    repo.commit_op(&op)?;
    let message = json!({
        "event": "target.rolled_back", "target": target_name, "from": current_rev.title, "change": current_rev.id.to_letters(),
        "to_revision": prev.revision.to_hex(), "why": why, "task": task.id.to_letters(), "deployment": dep_id.to_hex(),
    });
    let delivered = crate::verbs::deliver_to_channels(repo, &message, None)?;
    Ok(json!({ "rolled_back": true, "to": deployed_json["revision"], "task": task.id.to_letters(), "why": why, "notified": delivered }))
}

/// The delivery stage of a revision: deployed and observed, deployed, released, or none.
pub fn delivery_stage(repo: &Repo, rev_id: &ObjectId) -> Result<Option<&'static str>> {
    let view = repo.log.current_view()?;
    for dep_id in view.deployed.values() {
        if let Ok(d) = repo.store().get::<Deployment>(dep_id) {
            if d.revision == *rev_id {
                let observed = crate::verifiers::attestations_on(repo.store(), rev_id.as_bytes())?
                    .iter()
                    .any(|(_, a)| a.kind.starts_with("observe."));
                return Ok(Some(if observed { "observed" } else { "deployed" }));
            }
        }
    }
    if releases(repo)?.iter().any(|(_, r)| r.revision == *rev_id) {
        return Ok(Some("released"));
    }
    Ok(None)
}

pub fn describe_release(repo: &Repo, id: &ObjectId, r: &Release) -> Json {
    let title = repo.store().get::<Revision>(&r.revision).map(|x| x.title).unwrap_or_default();
    let changelog = repo.store().get_bytes(&r.changelog).ok().flatten().map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    json!({ "release": id.to_hex(), "name": r.name, "revision": r.revision.to_hex(), "title": title, "signer": repo.principal_name(&r.signer).unwrap_or_default(), "time": r.time, "attestations": r.attestations.as_ref().map(|a| a.len()).unwrap_or(0), "changelog": changelog })
}

#[allow(dead_code)]
fn _snapshot_unused(_: &Snapshot) {}
