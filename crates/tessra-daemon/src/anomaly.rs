//! The anomaly path of RISK.md, per agent: writes outside claims or write
//! scope, attempts to weaken tests, and repeated failed gates add points.
//! Past one threshold the agent is throttled for a cooldown; past the next
//! it is revoked and its unlanded work unwound, by the daemon as owner.
//! State is daemon-local. Hooks see `anomaly.detected` and `anomaly.revoked`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use tessra_core::object::Revision;
use tessra_core::store::ObjectStore;
use tessra_core::EntityId;

use crate::hooks::{self, Event};
use crate::{now, Error, Repo, Result};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnomalyState {
    pub score: u32,
    #[serde(default)]
    pub events: Vec<(i64, String)>,
    #[serde(default)]
    pub throttled_until: i64,
    #[serde(default)]
    pub throttles: u32,
    /// Keys already charged through `record_once`, so one cause counts once.
    #[serde(default)]
    pub charged: Vec<String>,
}

fn path(repo: &Repo, agent: &EntityId) -> PathBuf {
    repo.keys_dir
        .join("anomalies")
        .join(format!("{}.json", agent.to_letters()))
}

pub fn load(repo: &Repo, agent: &EntityId) -> AnomalyState {
    std::fs::read(path(repo, agent))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(repo: &Repo, agent: &EntityId, s: &AnomalyState) -> Result<()> {
    let p = path(repo, agent);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, serde_json::to_vec_pretty(s).unwrap_or_default())?;
    Ok(())
}

fn config_u64(repo: &Repo, key: &str, default: u64) -> u64 {
    match repo.config().get(key) {
        Some(ciborium::value::Value::Integer(i)) => i128::from(*i).max(0) as u64,
        Some(ciborium::value::Value::Text(t)) => t.parse().unwrap_or(default),
        _ => default,
    }
}

/// Refuse a mutation while the session's agent is throttled.
pub fn check(repo: &Repo, principal: &EntityId) -> Result<()> {
    let Some(agent) = repo.session_agent(principal) else {
        return Ok(());
    };
    let s = load(repo, &agent);
    if s.throttled_until > now() {
        let left = (s.throttled_until - now()) / 1_000_000_000;
        let last: Vec<&str> = s
            .events
            .iter()
            .rev()
            .take(3)
            .map(|(_, e)| e.as_str())
            .collect();
        return Err(Error::verb(
            "THROTTLED",
            format!(
                "throttled for {left} s after anomalies ({}); wait, then stay inside your claims and write scope",
                last.join("; ")
            ),
        ));
    }
    Ok(())
}

/// Record an anomaly at most once per `key`, for a signal the same cause
/// raises again and again: a landing refused for a weakened test is the
/// author's doing once, however many times the frontier re-evaluates it.
pub fn record_once(
    repo: &mut Repo,
    principal: &EntityId,
    what: &str,
    points: u32,
    key: &str,
) -> Result<Option<Json>> {
    let Some(agent) = repo.session_agent(principal) else {
        return Ok(None);
    };
    let mut s = load(repo, &agent);
    if s.charged.iter().any(|k| k == key) {
        return Ok(None);
    }
    s.charged.push(key.to_string());
    if s.charged.len() > 200 {
        s.charged.remove(0);
    }
    save(repo, &agent, &s)?;
    record(repo, principal, what, points)
}

/// Record an anomaly for the agent behind a principal. Returns what
/// happened when the principal is a session: the score, and whether it
/// was throttled or revoked.
pub fn record(
    repo: &mut Repo,
    principal: &EntityId,
    what: &str,
    points: u32,
) -> Result<Option<Json>> {
    let Some(agent) = repo.session_agent(principal) else {
        return Ok(None);
    };
    let mut s = load(repo, &agent);
    s.score += points;
    s.events.push((now(), what.to_string()));
    if s.events.len() > 50 {
        s.events.remove(0);
    }
    let throttle_at = config_u64(repo, "anomaly_throttle", 3) as u32;
    let revoke_at = config_u64(repo, "anomaly_revoke", 6) as u32;
    let cooldown = config_u64(repo, "anomaly_cooldown_s", 60) as i64;
    let name = repo
        .principal_name(&agent)
        .unwrap_or_else(|| agent.to_letters());
    let mut out = json!({ "agent": name, "what": what, "points": points, "score": s.score });
    let event_of = |repo: &Repo, kind: &str, extra: Json| -> Result<Event> {
        let (rev_id, rev) = match repo
            .workspaces
            .iter()
            .find(|w| w.principal == *principal)
            .and_then(|w| w.current)
        {
            Some(cur) => (cur, repo.store().get::<Revision>(&cur)?),
            None => {
                let (head, _) = crate::verbs::trunk_head_of(repo)?;
                (head, repo.store().get::<Revision>(&head)?)
            }
        };
        let snapshot = rev.snapshots.get("").copied().unwrap_or(rev_id);
        Ok(Event {
            kind: kind.into(),
            change: rev.id,
            revision: rev_id,
            snapshot,
            title: format!("anomaly: {what}"),
            author: *principal,
            paths: vec![],
            extra,
        })
    };
    if s.score >= revoke_at {
        save(repo, &agent, &s)?;
        let actor = repo.daemon_actor();
        let unwound = crate::swarm::revoke_agent(repo, &actor, agent)?;
        out["revoked"] = json!(true);
        out["unwound"] = unwound["unwound"].clone();
        let ev = event_of(repo, "anomaly.revoked", out.clone())?;
        out["hooks"] = json!(hooks::fire(repo, &ev)?);
        return Ok(Some(out));
    }
    if s.score >= throttle_at {
        s.throttled_until = now() + cooldown * 1_000_000_000;
        s.throttles += 1;
        out["throttled_s"] = json!(cooldown);
        save(repo, &agent, &s)?;
        let ev = event_of(repo, "anomaly.detected", out.clone())?;
        out["hooks"] = json!(hooks::fire(repo, &ev)?);
        return Ok(Some(out));
    }
    save(repo, &agent, &s)?;
    Ok(Some(out))
}

/// The state for a named agent, for status and tests.
pub fn state_for(repo: &Repo, agent: &EntityId) -> AnomalyState {
    load(repo, agent)
}
