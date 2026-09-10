//! Swarm, per PARALLELISM.md: partition an intent into disjoint unit sets
//! by blast radius and assign them; the integration frontier that lands
//! every landable proposed change; restack of stacked work after a
//! landing; and revoking an agent with everything unlanded of theirs
//! unwound in one op.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use ciborium::value::Value as Cbor;
use tessra_core::cbor;
use tessra_core::object::{
    Capability, Claim, Effect, Intent, Memory, MemoryScope, Node, NodeIndex, Principal, Revision,
    Snapshot, TrackingRules, Workspace,
};
use tessra_core::sig::SignedObject;
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build::{self, point_effect};
use tessra_oplog::glob;
use tessra_oplog::verify::landed_revisions;
use tessra_oplog::view::{Pointer, ViewState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::principals::Actor;
use crate::tree;
use crate::{fs, now, Error, Repo, Result};

// ------------------------------------------------------------- assignments

/// What the planner handed one agent: the paths it may write, the units
/// it owns, and an op budget. Daemon-local; consumed when the agent's next
/// session opens, which gets a capability shaped by it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assignment {
    pub agent: EntityId,
    pub intent: EntityId,
    pub title: String,
    pub paths: Vec<String>,
    pub units: Vec<String>,
    pub ops: u64,
}

fn assignment_path(repo: &Repo, agent: &EntityId) -> PathBuf {
    repo.keys_dir
        .join("assignments")
        .join(format!("{}.json", agent.to_letters()))
}

pub fn save_assignment(repo: &Repo, a: &Assignment) -> Result<()> {
    let p = assignment_path(repo, &a.agent);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, serde_json::to_vec_pretty(a).unwrap_or_default())?;
    Ok(())
}

pub fn load_assignment(repo: &Repo, agent: &EntityId) -> Option<Assignment> {
    let bytes = std::fs::read(assignment_path(repo, agent)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn clear_assignment(repo: &Repo, agent: &EntityId) {
    let _ = std::fs::remove_file(assignment_path(repo, agent));
}

// ------------------------------------------------------------- partition

/// One partition: units that must move together, with their files and
/// how many units elsewhere depend on them.
#[derive(Clone, Debug)]
pub struct Group {
    pub units: Vec<(String, String, EntityId)>,
    pub paths: Vec<String>,
    pub dependents: usize,
}

const NOT_UNITS: &[&str] = &["import", "chunk", "test", "test.skipped", "module", "variant", "field", "impl"];

/// Partition the units under `paths` into at most `agents` groups.
/// Units joined by a dependency edge, in either direction, stay together;
/// components are then balanced by size across the agents.
pub fn partition(idx: &NodeIndex, paths: &[String], agents: usize) -> Vec<Group> {
    let candidates: Vec<&Node> = idx
        .nodes
        .iter()
        .filter(|n| glob::any_match(paths, &n.path))
        .filter(|n| !NOT_UNITS.contains(&n.kind.as_str()))
        .filter(|n| tessra_semantic::rename::is_ident(&n.name))
        .collect();
    let index_of: HashMap<EntityId, usize> = candidates.iter().enumerate().map(|(i, n)| (n.nid, i)).collect();
    // Union-find over dependency edges within the candidate set.
    let mut parent: Vec<usize> = (0..candidates.len()).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        let mut c = i;
        while p[c] != r {
            let next = p[c];
            p[c] = r;
            c = next;
        }
        r
    }
    for (i, n) in candidates.iter().enumerate() {
        for d in n.deps.iter().flatten() {
            if let Some(&j) = index_of.get(d) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..candidates.len() {
        let r = find(&mut parent, i);
        components.entry(r).or_default().push(i);
    }
    let mut comps: Vec<Vec<usize>> = components.into_values().collect();
    comps.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| {
        candidates[a[0]].path.cmp(&candidates[b[0]].path).then(candidates[a[0]].name.cmp(&candidates[b[0]].name))
    }));
    // Greedy balance into at most `agents` buckets, largest components first.
    let buckets = agents.max(1).min(comps.len().max(1));
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); buckets];
    for c in comps {
        let (i, _) = groups
            .iter()
            .enumerate()
            .min_by_key(|(i, g)| (g.len(), *i))
            .unwrap();
        groups[i].extend(c);
    }
    let all_nids: HashSet<EntityId> = candidates.iter().map(|n| n.nid).collect();
    let _ = all_nids;
    groups
        .into_iter()
        .filter(|g| !g.is_empty())
        .map(|g| {
            let mut units: Vec<(String, String, EntityId)> = g
                .iter()
                .map(|&i| (candidates[i].path.clone(), candidates[i].name.clone(), candidates[i].nid))
                .collect();
            units.sort();
            let nids: HashSet<EntityId> = units.iter().map(|u| u.2).collect();
            let mut paths: Vec<String> = units.iter().map(|u| u.0.clone()).collect();
            paths.sort();
            paths.dedup();
            let dependents = idx
                .nodes
                .iter()
                .filter(|n| !nids.contains(&n.nid) && n.kind != "test")
                .filter(|n| n.deps.iter().flatten().any(|d| nids.contains(d)))
                .count();
            Group { units, paths, dependents }
        })
        .collect()
}

/// Plan an intent: a parent intent, one sub-intent per group assigned to an
/// agent, a task memory per sub-intent, and an assignment per agent that
/// shapes its next session's capability. One op.
pub fn plan(
    repo: &mut Repo,
    actor: &Actor,
    title: &str,
    paths: &[String],
    agents: &[String],
    ops_budget: u64,
) -> Result<Json> {
    let (head_id, _) = crate::verbs::trunk_head_of(repo)?;
    let head: Revision = repo.store().get(&head_id)?;
    let snap_id = *head
        .snapshots
        .get("")
        .ok_or_else(|| Error::verb("ROOT", "trunk head has no root snapshot"))?;
    let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
    let groups = partition(&idx, paths, agents.len());
    if groups.is_empty() {
        return Err(Error::verb("PLAN", "no units under those paths to partition"));
    }
    let t = now();
    let parent = Intent {
        id: EntityId::random(),
        prev: None,
        title: title.into(),
        body: Some(format!("{} sub-intents over {} paths", groups.len(), paths.join(", "))),
        spec: None,
        evals: None,
        parent: None,
        depends: None,
        priority: 0,
        status: "open".into(),
        assignee: None,
        created_by: actor.principal(),
        time: t,
        legacy: None,
    };
    let parent_oid = repo.store().put(&parent)?;
    let mut effects = vec![
        Effect::Put { id: parent_oid },
        Effect::Point {
            entity: parent.id,
            to: parent_oid,
            from: None,
        },
    ];
    let mut out_groups = Vec::new();
    let mut assignments = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let agent_name = &agents[i];
        let agent_id = repo.agent_id(agent_name)?;
        let unit_names: Vec<String> = g.units.iter().map(|u| format!("{}:{}", u.0, u.1)).collect();
        let sub = Intent {
            id: EntityId::random(),
            prev: None,
            title: format!("{title}: {}", g.units.iter().map(|u| u.1.as_str()).collect::<Vec<_>>().join(", ")),
            body: Some(format!("units {}", unit_names.join(", "))),
            spec: None,
            evals: None,
            parent: Some(parent.id),
            depends: None,
            priority: 0,
            status: "open".into(),
            assignee: Some(agent_id),
            created_by: actor.principal(),
            time: t,
            legacy: None,
        };
        let sub_oid = repo.store().put(&sub)?;
        effects.push(Effect::Put { id: sub_oid });
        effects.push(Effect::Point {
            entity: sub.id,
            to: sub_oid,
            from: None,
        });
        let task = Memory {
            id: EntityId::random(),
            prev: None,
            kind: "task".into(),
            scope: MemoryScope {
                kind: "intent".into(),
                r#ref: Cbor::Bytes(sub.id.0.to_vec()),
            },
            body: format!(
                "{}: change {} in {}; claim the paths first, keep the edit inside these units, and {} units elsewhere depend on them",
                agent_name,
                unit_names.join(", "),
                g.paths.join(", "),
                g.dependents
            ),
            anchor: None,
            confidence: 1000,
            author: actor.principal(),
            time: t,
            expires: None,
            proposed: None,
            status: "active".into(),
            links: Some(vec![sub_oid]),
            visibility: "shared".into(),
        };
        let task_oid = repo.store().put(&task)?;
        effects.push(Effect::Put { id: task_oid });
        effects.push(Effect::Point {
            entity: task.id,
            to: task_oid,
            from: None,
        });
        assignments.push(Assignment {
            agent: agent_id,
            intent: sub.id,
            title: sub.title.clone(),
            paths: g.paths.clone(),
            units: unit_names.clone(),
            ops: ops_budget,
        });
        out_groups.push(json!({
            "agent": agent_name,
            "intent": sub.id.to_letters(),
            "units": unit_names,
            "paths": g.paths,
            "dependents": g.dependents,
            "task": task.id.to_letters(),
        }));
    }
    for a in &assignments {
        save_assignment(repo, a)?;
    }
    // A planner holding a delegable capability delegates: each assignee's
    // session gets a child capability, issued and signed by the planner,
    // contained in the planner's own, with the group's paths and budget.
    let mut delegated: Vec<Json> = Vec::new();
    let parent_cap: Option<(ObjectId, Capability)> = match actor.cap {
        Some(id) if actor.kind == "session" => repo.store().get::<Capability>(&id).ok().map(|c| (id, c)),
        _ => None,
    };
    if let Some((parent_id, parent)) = parent_cap.filter(|(_, c)| c.delegable) {
        for (i, a) in assignments.iter().enumerate() {
            let agent_name = &agents[i];
            let session = repo.open_session(agent_name, None, a.paths.clone())?;
            let mut hard = parent.hard.clone().unwrap_or_default();
            let parent_ops = parent.hard.as_ref().and_then(|h| h.get("ops").copied()).unwrap_or(u64::MAX);
            hard.insert("ops".to_string(), a.ops.min(parent_ops).max(1));
            let mut child = Capability {
                issuer: actor.principal(),
                subject: session.principal(),
                verbs: parent
                    .verbs
                    .iter()
                    .filter(|v| !matches!(v.as_str(), "plan" | "grant" | "revoke"))
                    .cloned()
                    .collect(),
                write: tessra_core::object::WriteScope {
                    roots: parent.write.roots.clone(),
                    paths: Some(a.paths.clone()),
                    nodes: parent.write.nodes.clone(),
                    lines: parent.write.lines.clone(),
                    targets: parent.write.targets.clone(),
                    stages: parent.write.stages.clone(),
                    memory_scopes: parent.write.memory_scopes.clone(),
                },
                read: parent.read.clone(),
                hard: Some(hard),
                advisory: None,
                delegable: false,
                parent: Some(parent_id),
                nonce: EntityId::random(),
                time: t,
                sig: None,
            };
            child.sign_with(&actor.signer.key)?;
            let child_id = repo.store().put(&child)?;
            effects.push(Effect::Put { id: child_id });
            effects.push(Effect::Cap { cap: child_id });
            repo.set_session_cap(agent_name, &a.paths, child_id)?;
            delegated.push(json!({ "agent": agent_name, "session": session.principal().to_letters(), "capability": child_id.to_hex(), "ops": a.ops.min(parent_ops) }));
        }
    }
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "plan",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        effects,
        t,
    )?;
    repo.commit_op(&op)?;
    Ok(json!({
        "intent": parent.id.to_letters(),
        "title": title,
        "groups": out_groups,
        "assigned": assignments.len(),
        "idle": agents.len().saturating_sub(assignments.len()),
        "delegated": delegated,
        "how": "each agent's next session is scoped to its paths and op budget; status shows its assignment",
    }))
}

// ------------------------------------------------------------- claims

/// Active claims held by other principals that overlap these paths.
pub fn overlapping_claims(repo: &Repo, mine: EntityId, paths: &[String]) -> Result<Vec<Json>> {
    let view = repo.log.current_view()?;
    let t = tessra_core::trie::Trie::new(repo.store());
    let mut out = Vec::new();
    for (_, v) in t.entries(&view.claims)? {
        let Some(Pointer::Id(oid)) = Pointer::from_trie(&v) else { continue };
        let c: Claim = repo.store().get(&oid)?;
        if c.principal == mine || c.expires <= now() {
            continue;
        }
        let theirs: Vec<String> = c
            .targets
            .iter()
            .filter_map(|t| match &t.r#ref {
                Cbor::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let hit: Vec<&String> = paths
            .iter()
            .filter(|p| theirs.iter().any(|q| glob::contained(p, q) || glob::contained(q, p) || glob::matches(q, p) || glob::matches(p, q)))
            .collect();
        if !hit.is_empty() {
            let who = repo.principal_name(&c.principal).unwrap_or_else(|| c.principal.to_letters());
            out.push(json!({
                "principal": c.principal.to_letters(),
                "who": who,
                "paths": theirs,
                "overlap": hit,
                "exclusive": c.exclusive,
                "note": c.note,
                "expires": c.expires,
            }));
        }
    }
    Ok(out)
}

// ------------------------------------------------------------- restack

/// After change C landed as R, restack every unlanded revision whose first
/// parent is C's landed revision onto R: a new revision with the same ID,
/// parents [R], snapshots re-merged, authored by the daemon. Only clean
/// workspaces are rewritten; a dirty one keeps its files and lands later
/// through the three-way merge, whose base is still correct.
pub fn restack_children(repo: &mut Repo, rev_id: ObjectId, landing_id: ObjectId) -> Result<Vec<Json>> {
    let view = repo.log.current_view()?;
    let landed = landed_revisions(repo.store(), &view)?;
    let workspaces: Vec<Workspace> = repo.workspaces.clone();
    let mut out = Vec::new();
    for ws in workspaces {
        let Some(x_id) = ws.current else { continue };
        let was_proposed = {
            let view = repo.log.current_view()?;
            let vs = ViewState::new(repo.store());
            vs.in_set(&view.proposed, &x_id)?
        };
        if x_id == landing_id || x_id == rev_id || landed.contains(&x_id) {
            continue;
        }
        let x: Revision = repo.store().get(&x_id)?;
        if x.parents.first() != Some(&rev_id) {
            continue;
        }
        let (x_snap_id, x_snap) = match x.snapshots.get("") {
            Some(s) => (*s, repo.store().get::<Snapshot>(s)?),
            None => continue,
        };
        // Clean means the files match the revision.
        let rules: TrackingRules = repo.store().get(&x_snap.rules)?;
        let dir = PathBuf::from(ws.paths.get("").cloned().unwrap_or_default());
        if !dir.exists() {
            continue;
        }
        repo.store().begin_batch();
        let probe = fs::snapshot_dir(repo.store(), &dir, &rules, None, None);
        repo.store().end_batch()?;
        let probe = probe?;
        if probe.tree != x_snap.root {
            out.push(json!({ "change": x.id.to_letters(), "restacked": false, "why": "workspace has unsnapshotted edits; it will merge at landing" }));
            continue;
        }
        let base: Revision = repo.store().get(&rev_id)?;
        let landing: Revision = repo.store().get(&landing_id)?;
        let (base_snap_id, base_snap) = match base.snapshots.get("") {
            Some(s) => (*s, repo.store().get::<Snapshot>(s)?),
            None => continue,
        };
        let (land_snap_id, land_snap) = match landing.snapshots.get("") {
            Some(s) => (*s, repo.store().get::<Snapshot>(s)?),
            None => continue,
        };
        let base_flat = tree::flatten(repo.store(), &base_snap.root)?;
        let a = tree::flatten(repo.store(), &land_snap.root)?;
        let b = tree::flatten(repo.store(), &x_snap.root)?;
        let (_, base_idx) = crate::semantic::index_for_snapshot(repo.store(), &base_snap_id)?;
        let (a_idx_id, a_idx) = crate::semantic::index_for_snapshot(repo.store(), &land_snap_id)?;
        let (_, b_idx) = crate::semantic::index_for_snapshot(repo.store(), &x_snap_id)?;
        let ops_a = crate::semantic::recorded_renames(landing.ops.as_deref().unwrap_or(&[]));
        let ops_b = crate::semantic::recorded_renames(x.ops.as_deref().unwrap_or(&[]));
        let tm = crate::semantic::merge_trees(repo.store(), &base_flat, &a, &b, Some(&base_idx), &a_idx, &b_idx, &ops_a, &ops_b)?;
        if !tm.conflicts.is_empty() {
            out.push(json!({ "change": x.id.to_letters(), "restacked": false, "why": format!("conflicts: {}", tm.conflicts.join(", ")) }));
            continue;
        }
        let root = tree::build(repo.store(), &tm.flat)?;
        let mut nodes = crate::semantic::build_nodes(repo.store(), &tm.flat, Some((&a, &a_idx)))?;
        let aliases = crate::semantic::reconcile_aliases(&mut nodes, Some(&base_idx), &b_idx);
        let idx = crate::semantic::put_index_with_aliases(repo.store(), root, vec![a_idx_id], nodes, Some(aliases))?;
        let snap = repo.store().put(&Snapshot {
            root,
            rules: x_snap.rules,
            env: None,
            index: Some(idx),
        })?;
        let restacked = Revision {
            id: x.id,
            prev: Some(x_id),
            snapshots: BTreeMap::from([("".to_string(), snap)]),
            parents: vec![landing_id],
            intent: x.intent,
            title: x.title.clone(),
            body: x.body.clone(),
            author: repo.daemon,
            time: now(),
            ops: x.ops.clone(),
            flags: x.flags.clone(),
        };
        let new_id = repo.store().put(&restacked)?;
        let mut effects = vec![Effect::Put { id: new_id }, point_effect(&repo.log, x.id, new_id)?];
        if was_proposed {
            effects.push(Effect::Unpropose { rev: x_id });
            effects.push(Effect::Propose { rev: new_id });
        }
        let signer = repo.daemon_signer();
        let op = build::build_op(
            &repo.log,
            &signer,
            None,
            "restack",
            build::args_with_idem(EntityId::random().0, BTreeMap::new()),
            effects,
            now(),
        )?;
        repo.commit_op(&op)?;
        fs::materialize(repo.store(), &root, &dir, false)?;
        if let Some(w) = repo.workspace_mut(&ws.id) {
            w.current = Some(new_id);
            w.base = landing_id;
            let w2 = w.clone();
            repo.save_workspace(&w2)?;
        }
        out.push(json!({ "change": x.id.to_letters(), "restacked": true, "revision": new_id.to_hex(), "semantic": tm.resolved }));
    }
    Ok(out)
}

// ------------------------------------------------------------- revoke

/// Revoke an agent and every session of it, and unwind in the same op:
/// unlanded changes its sessions authored are unpointed and unproposed,
/// its claims released, its active memories retired. Its workspaces are
/// dropped afterwards.
pub fn revoke_agent(repo: &mut Repo, actor: &Actor, agent: EntityId) -> Result<Json> {
    let view = repo.log.current_view()?;
    let vs = ViewState::new(repo.store());
    let landed = landed_revisions(repo.store(), &view)?;
    // Sessions of the agent, from the entities.
    let mut sessions: Vec<EntityId> = Vec::new();
    let mut memories: Vec<(EntityId, ObjectId, Memory)> = Vec::new();
    for (id, ptr) in vs.entities(&view)? {
        let Ok(oid) = ptr.single() else { continue };
        let Some(bytes) = repo.store().get_bytes(&oid)? else { continue };
        match cbor::peek_tag(&bytes).ok().as_deref() {
            Some("principal") => {
                if let Ok(p) = cbor::decode::<Principal>(&bytes) {
                    if p.kind == "session" && p.parent == Some(agent) && p.status == "active" {
                        sessions.push(id);
                    }
                }
            }
            Some("memory") => {
                if let Ok(m) = cbor::decode::<Memory>(&bytes) {
                    memories.push((id, oid, m));
                }
            }
            _ => {}
        }
    }
    let mine: HashSet<EntityId> = sessions.iter().copied().chain(std::iter::once(agent)).collect();
    let mut effects = vec![Effect::Revoke { principal: agent }];
    for s in &sessions {
        effects.push(Effect::Revoke { principal: *s });
    }
    // Unlanded changes.
    let mut changes: Vec<String> = Vec::new();
    let mut to_drop: Vec<EntityId> = Vec::new();
    let mut seen_changes: HashSet<EntityId> = HashSet::new();
    for ws in repo.workspaces.clone() {
        if !mine.contains(&ws.principal) {
            continue;
        }
        to_drop.push(ws.id);
        let Some(cur) = ws.current else { continue };
        if landed.contains(&cur) {
            continue;
        }
        let rev: Revision = repo.store().get(&cur)?;
        if !mine.contains(&rev.author) || !seen_changes.insert(rev.id) {
            continue;
        }
        if vs.in_set(&view.proposed, &cur)? {
            effects.push(Effect::Unpropose { rev: cur });
        }
        if vs.entity(&view, &rev.id)?.is_some() {
            effects.push(Effect::Unpoint { entity: rev.id });
        }
        changes.push(rev.id.to_letters());
    }
    // Claims.
    let t = tessra_core::trie::Trie::new(repo.store());
    let mut claims: Vec<String> = Vec::new();
    for (k, v) in t.entries(&view.claims)? {
        let (Ok(id), Some(Pointer::Id(oid))) = (EntityId::from_slice(&k), Pointer::from_trie(&v)) else { continue };
        let c: Claim = repo.store().get(&oid)?;
        if mine.contains(&c.principal) {
            effects.push(Effect::Unclaim { claim: id });
            claims.push(id.to_letters());
        }
    }
    // Unconfirmed memories: everything active the sessions wrote.
    let mut retired: Vec<String> = Vec::new();
    for (id, oid, m) in memories {
        if !mine.contains(&m.author) || m.status != "active" {
            continue;
        }
        let mut next = m.clone();
        next.prev = Some(oid);
        next.status = "retired".into();
        let new_oid = repo.store().put(&next)?;
        effects.push(Effect::Put { id: new_oid });
        effects.push(Effect::Point {
            entity: id,
            to: new_oid,
            from: Some(Pointer::Id(oid).to_value()),
        });
        retired.push(id.to_letters());
    }
    let op = build::build_op(
        &repo.log,
        &actor.signer,
        actor.cap,
        "revoke",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        effects,
        now(),
    )?;
    let op_id = repo.commit_op(&op)?;
    let mut dropped = 0;
    for id in to_drop {
        if let Some(ws) = repo.workspace(&id).cloned() {
            let p = PathBuf::from(ws.paths.get("").cloned().unwrap_or_default());
            if p.exists() && p.starts_with(crate::paths::local_data_dir()) {
                let _ = std::fs::remove_dir_all(&p);
            }
            repo.remove_workspace(&id)?;
            dropped += 1;
        }
    }
    clear_assignment(repo, &agent);
    Ok(json!({
        "agent": agent.to_letters(),
        "sessions": sessions.len(),
        "op": op_id.to_hex(),
        "unwound": { "changes": changes, "claims": claims, "memories_retired": retired, "workspaces_dropped": dropped },
    }))
}
