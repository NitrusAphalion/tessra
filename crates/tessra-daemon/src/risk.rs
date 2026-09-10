//! The per-change risk score, per RISK.md: every signal contributes points,
//! the total is explainable, and each factor says what would lower it. The
//! score is recorded as a `risk.change` attestation on the revision, signed
//! by the daemon, so standards can branch on it with `when` clauses and
//! every replica sees the same number.

use std::collections::{BTreeMap, HashSet};

use ciborium::value::Value as Cbor;
use tessra_core::object::{Attestation, Effect, Node, Revision};
use tessra_core::sig::SignedObject;
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build;
use tessra_oplog::glob;
use tessra_oplog::verify::landed_revisions;
use serde_bytes::ByteBuf;
use serde_json::{json, Value as Json};

use crate::verifiers;
use crate::{hooks, now, Repo, Result};

#[derive(Clone, Debug)]
pub struct Factor {
    pub name: String,
    pub points: u32,
    pub detail: String,
    pub lower: String,
}

#[derive(Clone, Debug)]
pub struct Risk {
    pub score: u32,
    pub level: &'static str,
    pub factors: Vec<Factor>,
}

pub fn level_of(score: u32) -> &'static str {
    match score {
        0..=19 => "low",
        20..=44 => "medium",
        45..=74 => "high",
        _ => "critical",
    }
}

/// Rank of a level name, for `when risk_at_least`.
pub fn level_rank(level: &str) -> u8 {
    match level {
        "low" => 0,
        "medium" => 1,
        "high" => 2,
        "critical" => 3,
        _ => 0,
    }
}

const DEFAULT_SENSITIVE: &[&str] = &[
    "**/auth/**",
    "**/payments/**",
    "**/security/**",
    "**/secrets/**",
    "**/*.pem",
    "**/*.key",
    "**/migrations/**",
];

const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "Gemfile",
    "pom.xml",
    "build.gradle",
];

fn names(units: &[&Node]) -> String {
    let mut v: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
    v.sort();
    v.dedup();
    let shown: Vec<&str> = v.iter().take(6).copied().collect();
    let more = if v.len() > 6 { format!(" and {} more", v.len() - 6) } else { String::new() };
    format!("{}{more}", shown.join(", "))
}

/// Score a revision against its first parent.
pub fn assess(repo: &Repo, rev_id: &ObjectId, rev: &Revision) -> Result<Risk> {
    let mut factors: Vec<Factor> = Vec::new();
    let (snap_id, _) = match rev.snapshots.get("") {
        Some(s) => (*s, ()),
        None => {
            return Ok(Risk {
                score: 0,
                level: "low",
                factors,
            })
        }
    };
    let (_, idx) = crate::semantic::index_for_snapshot(repo.store(), &snap_id)?;
    let parent_idx = match rev.parents.first() {
        Some(p) => {
            let pr: Revision = repo.store().get(p)?;
            match pr.snapshots.get("") {
                Some(s) => Some(crate::semantic::index_for_snapshot(repo.store(), s)?.1),
                None => None,
            }
        }
        None => None,
    };
    let empty = tessra_core::object::NodeIndex {
        root: ObjectId([0; 32]),
        grammars: String::new(),
        parents: vec![],
        nodes: vec![],
        aliases: None,
    };
    let parent_idx = parent_idx.as_ref().unwrap_or(&empty);
    let changed: Vec<&Node> = verifiers::changed_units(parent_idx, &idx)
        .into_iter()
        .filter(|n| !matches!(n.kind.as_str(), "import" | "chunk"))
        .collect();
    let changed_ids: HashSet<EntityId> = changed.iter().map(|n| n.nid).collect();
    let code: Vec<&Node> = changed
        .iter()
        .copied()
        .filter(|n| !matches!(n.kind.as_str(), "test" | "test.skipped" | "impl" | "module" | "variant" | "field"))
        .collect();

    // Size and shape.
    if code.len() >= 4 {
        factors.push(Factor {
            name: "size".into(),
            points: (code.len() as u32 * 3).min(30),
            detail: format!("{} units changed: {}", code.len(), names(&code)),
            lower: "split the change so each lands on its own".into(),
        });
    }

    // Blast radius: units elsewhere that reference what changed.
    let dependents: Vec<&Node> = idx
        .nodes
        .iter()
        .filter(|n| !changed_ids.contains(&n.nid) && n.kind != "test")
        .filter(|n| n.deps.iter().flatten().any(|d| changed_ids.contains(d)))
        .collect();
    if !dependents.is_empty() {
        factors.push(Factor {
            name: "blast_radius".into(),
            points: (dependents.len() as u32 * 2).min(25),
            detail: format!("{} dependents of the changed units: {}", dependents.len(), names(&dependents)),
            lower: "keep the public shape of what changed, or land the callers' updates in the same change".into(),
        });
    }

    // Test signal.
    let covering = crate::semantic::covering_tests(&idx);
    let untested: Vec<&Node> = code
        .iter()
        .copied()
        .filter(|n| tessra_semantic::rename::is_ident(&n.name))
        .filter(|n| covering.get(&n.nid).map(|t| t.is_empty()).unwrap_or(true))
        .collect();
    if !untested.is_empty() {
        factors.push(Factor {
            name: "untested".into(),
            points: (untested.len() as u32 * 8).min(30),
            detail: format!("changed units no test calls: {}", names(&untested)),
            lower: format!("add tests that call {}", names(&untested)),
        });
    }
    let parent_ids: HashSet<EntityId> = parent_idx.nodes.iter().map(|n| n.nid).collect();
    let now_ids: HashSet<EntityId> = idx.nodes.iter().map(|n| n.nid).collect();
    let weakened: Vec<&Node> = changed
        .iter()
        .copied()
        .filter(|n| (n.kind == "test" || n.kind == "test.skipped") && parent_ids.contains(&n.nid))
        .chain(
            parent_idx
                .nodes
                .iter()
                .filter(|n| (n.kind == "test" || n.kind == "test.skipped") && !now_ids.contains(&n.nid)),
        )
        .collect();
    if !weakened.is_empty() {
        factors.push(Factor {
            name: "tests_weakened".into(),
            points: (weakened.len() as u32 * 15).min(30),
            detail: format!("existing tests modified or deleted: {}", names(&weakened)),
            lower: "restore the tests, or land the test change separately with approval".into(),
        });
    }

    // Paths: manifests and sensitive scopes.
    let paths = hooks::changed_paths(repo, rev)?;
    let manifests: Vec<&String> = paths
        .iter()
        .filter(|p| MANIFESTS.iter().any(|m| p.ends_with(m)))
        .collect();
    if !manifests.is_empty() {
        factors.push(Factor {
            name: "dependencies".into(),
            points: 15,
            detail: format!("dependency manifest changed: {}", manifests.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
            lower: "land dependency changes on their own, with approval".into(),
        });
    }
    let sensitive_patterns: Vec<String> = match repo.config().get("risk_sensitive") {
        Some(Cbor::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_text().map(str::to_string))
            .collect(),
        _ => DEFAULT_SENSITIVE.iter().map(|s| s.to_string()).collect(),
    };
    let sensitive: Vec<&String> = paths
        .iter()
        .filter(|p| glob::any_match(&sensitive_patterns, p))
        .collect();
    if !sensitive.is_empty() {
        factors.push(Factor {
            name: "sensitive_scope".into(),
            points: 25,
            detail: format!("touches a sensitive scope: {}", sensitive.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
            lower: "get a human approval for this scope, or keep the change out of it".into(),
        });
    }

    // Flags the snapshot carried.
    if let Some(f) = &rev.flags {
        if f.secrets.as_ref().is_some_and(|s| !s.is_empty()) {
            factors.push(Factor {
                name: "secrets".into(),
                points: 30,
                detail: "content that looks like a secret".into(),
                lower: "remove the secret and snapshot again".into(),
            });
        }
        if f.out_of_scope.as_ref().is_some_and(|s| !s.is_empty()) {
            factors.push(Factor {
                name: "out_of_scope".into(),
                points: 20,
                detail: "paths outside the author's write scope".into(),
                lower: "stay inside the capability's write scope".into(),
            });
        }
    }

    // Collision: unlanded work by others on the same units.
    let view = repo.log.current_view()?;
    let landed = landed_revisions(repo.store(), &view)?;
    let mut colliding: Vec<String> = Vec::new();
    for w in &repo.workspaces {
        let Some(cur) = w.current else { continue };
        if cur == *rev_id || landed.contains(&cur) {
            continue;
        }
        let Ok(other) = repo.store().get::<Revision>(&cur) else { continue };
        if other.id == rev.id {
            continue;
        }
        let Some(os) = other.snapshots.get("") else { continue };
        let Ok((_, oidx)) = crate::semantic::index_for_snapshot(repo.store(), os) else { continue };
        let Some(op) = other.parents.first() else { continue };
        let Ok(opr) = repo.store().get::<Revision>(op) else { continue };
        let Some(ops) = opr.snapshots.get("") else { continue };
        let Ok((_, opidx)) = crate::semantic::index_for_snapshot(repo.store(), ops) else { continue };
        let theirs: HashSet<EntityId> = verifiers::changed_units(&opidx, &oidx).iter().map(|n| n.nid).collect();
        if theirs.iter().any(|n| changed_ids.contains(n)) {
            colliding.push(other.author.to_letters());
        }
    }
    if !colliding.is_empty() {
        colliding.sort();
        colliding.dedup();
        factors.push(Factor {
            name: "collision".into(),
            points: 10,
            detail: format!("other unlanded work changes the same units, by {}", colliding.join(", ")),
            lower: "land or coordinate with the other change first".into(),
        });
    }

    factors.sort_by(|a, b| b.points.cmp(&a.points).then(a.name.cmp(&b.name)));
    let score = factors.iter().map(|f| f.points).sum::<u32>().min(100);
    Ok(Risk {
        score,
        level: level_of(score),
        factors,
    })
}

pub fn to_json(r: &Risk) -> Json {
    json!({
        "score": r.score,
        "level": r.level,
        "factors": r.factors.iter().map(|f| json!({ "name": f.name, "points": f.points, "detail": f.detail, "lower": f.lower })).collect::<Vec<_>>(),
    })
}

fn to_cbor(r: &Risk) -> Cbor {
    Cbor::Map(vec![
        (Cbor::Text("score".into()), Cbor::Integer(r.score.into())),
        (Cbor::Text("level".into()), Cbor::Text(r.level.into())),
        (
            Cbor::Text("factors".into()),
            Cbor::Array(
                r.factors
                    .iter()
                    .map(|f| {
                        Cbor::Map(vec![
                            (Cbor::Text("name".into()), Cbor::Text(f.name.clone())),
                            (Cbor::Text("points".into()), Cbor::Integer(f.points.into())),
                            (Cbor::Text("detail".into()), Cbor::Text(f.detail.clone())),
                            (Cbor::Text("lower".into()), Cbor::Text(f.lower.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// The risk attestation on a revision: the one already recorded, or a new
/// one signed by the daemon. Returns the attestation ID and the risk.
pub fn ensure_attested(repo: &mut Repo, rev_id: &ObjectId, rev: &Revision) -> Result<(ObjectId, Risk)> {
    if let Some((id, a)) = verifiers::on_subject(repo.store(), "risk.change", rev_id.as_bytes())?.into_iter().next() {
        if let Some(r) = from_cbor(&a.result) {
            return Ok((id, r));
        }
    }
    let risk = assess(repo, rev_id, rev)?;
    let mut att = Attestation {
        kind: "risk.change".into(),
        subject: Some(ByteBuf::from(rev_id.0.to_vec())),
        bodies: None,
        subject_type: Some("revision".into()),
        scope: Some(BTreeMap::from([(
            "level".to_string(),
            Cbor::Text(risk.level.into()),
        )])),
        result: to_cbor(&risk),
        env: None,
        verifier: repo.daemon,
        runner: None,
        evidence: None,
        time: now(),
        sigkind: "ed25519".into(),
        sig: None,
    };
    att.sign_with(repo.daemon_key())?;
    let id = repo.store().put(&att)?;
    let signer = repo.daemon_signer();
    let op = build::build_op(
        &repo.log,
        &signer,
        None,
        "attest",
        build::args_with_idem(EntityId::random().0, BTreeMap::new()),
        vec![Effect::Put { id }],
        now(),
    )?;
    repo.commit_op(&op)?;
    Ok((id, risk))
}

/// Read a risk back from an attestation's result.
pub fn from_cbor(v: &Cbor) -> Option<Risk> {
    let Cbor::Map(m) = v else { return None };
    let get = |k: &str| m.iter().find(|(key, _)| matches!(key, Cbor::Text(t) if t == k)).map(|(_, v)| v);
    let score = match get("score") {
        Some(Cbor::Integer(i)) => i128::from(*i) as u32,
        _ => return None,
    };
    let mut factors = Vec::new();
    if let Some(Cbor::Array(a)) = get("factors") {
        for f in a {
            if let Cbor::Map(fm) = f {
                let text = |k: &str| {
                    fm.iter()
                        .find(|(key, _)| matches!(key, Cbor::Text(t) if t == k))
                        .and_then(|(_, v)| v.as_text().map(str::to_string))
                        .unwrap_or_default()
                };
                let points = fm
                    .iter()
                    .find(|(key, _)| matches!(key, Cbor::Text(t) if t == "points"))
                    .and_then(|(_, v)| match v {
                        Cbor::Integer(i) => Some(i128::from(*i) as u32),
                        _ => None,
                    })
                    .unwrap_or(0);
                factors.push(Factor {
                    name: text("name"),
                    points,
                    detail: text("detail"),
                    lower: text("lower"),
                });
            }
        }
    }
    Some(Risk {
        score,
        level: level_of(score),
        factors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_round_trip() {
        assert_eq!(level_of(0), "low");
        assert_eq!(level_of(20), "medium");
        assert_eq!(level_of(70), "high");
        assert_eq!(level_of(90), "critical");
        assert!(level_rank("high") > level_rank("medium"));
        let r = Risk {
            score: 55,
            level: level_of(55),
            factors: vec![Factor {
                name: "untested".into(),
                points: 30,
                detail: "x".into(),
                lower: "add tests".into(),
            }],
        };
        let back = from_cbor(&to_cbor(&r)).unwrap();
        assert_eq!(back.score, 55);
        assert_eq!(back.level, "high");
        assert_eq!(back.factors[0].lower, "add tests");
    }
}
