//! Building ops: init, ordinary ops from effects, and undo.

use std::collections::BTreeMap;

use ciborium::value::Value;
use tessra_core::object::{Effect, Line, Op, Principal, Revision, Snapshot, TrackingRules, Tree};
use tessra_core::sig::{SecretKey, SignedObject};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};

use crate::dag::OpLog;
use crate::standard::default_trunk_standard;
use crate::view::{Pointer, ViewState};
use crate::{Error, Result};

/// A principal that can sign ops.
pub struct Signer {
    pub principal: EntityId,
    pub key: SecretKey,
}

/// Build the args map with an idempotency key.
pub fn args_with_idem(idem: [u8; 16], extra: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut m = extra;
    m.insert("idem".into(), Value::Bytes(idem.to_vec()));
    m
}

/// Build a signed op on the current heads from effects. Objects the effects
/// point at must already be in the store. Does not accept it into the log.
pub fn build_op<S: ObjectStore>(
    log: &OpLog<S>,
    signer: &Signer,
    cap: Option<ObjectId>,
    kind: &str,
    args: BTreeMap<String, Value>,
    effects: Vec<Effect>,
    time: i64,
) -> Result<Op> {
    let vs = ViewState::new(log.store());
    let parents = log.heads();
    let v = log.merged_view(&parents)?;
    let next = vs.apply(&v, &effects, parents.first().copied())?;
    let view = vs.store_view(&next)?;
    let mut op = Op {
        parents,
        author: signer.principal,
        key: signer.key.public(),
        cap,
        kind: kind.into(),
        args,
        effects,
        view,
        time,
        desc: None,
        sig: None,
    };
    op.sign_with(&signer.key)?;
    Ok(op)
}

/// What init created.
pub struct Initialized {
    pub op: ObjectId,
    pub daemon: EntityId,
    pub standard: EntityId,
    pub trunk: EntityId,
    pub root_change: EntityId,
    pub root_revision: ObjectId,
}

/// Initialize an empty log: a daemon principal that owns the repository, a
/// default standard, an empty root snapshot, an initial revision, and trunk.
/// One imported revision for init. A list of these, oldest first, becomes
/// the trunk history; each is its own change.
pub struct InitContent {
    pub snapshot: ObjectId,
    pub title: String,
    pub body: Option<String>,
    pub intent: Option<tessra_core::object::Intent>,
    /// Change ID to use, for deterministic legacy IDs. Random when absent.
    pub change: Option<EntityId>,
    /// Revision time. The init time when absent.
    pub time: Option<i64>,
}

pub fn init<S: ObjectStore>(
    log: &mut OpLog<S>,
    daemon_key: &SecretKey,
    daemon_name: &str,
    time: i64,
) -> Result<Initialized> {
    init_with(log, daemon_key, daemon_name, time, Vec::new())
}

/// Initialize with an optional imported root snapshot and intent.
pub fn init_with<S: ObjectStore>(
    log: &mut OpLog<S>,
    daemon_key: &SecretKey,
    daemon_name: &str,
    time: i64,
    content: Vec<InitContent>,
) -> Result<Initialized> {
    if !log.heads().is_empty() {
        return Err(Error::Other("log already initialized".into()));
    }
    let store = log.store();
    let daemon = EntityId::random();
    let mut principal = Principal {
        id: daemon,
        prev: None,
        key: Some(daemon_key.public()),
        kind: "daemon".into(),
        name: daemon_name.into(),
        parent: None,
        model: None,
        runtime: None,
        bindings: None,
        status: "active".into(),
        expires: None,
        time,
        sig: None,
    };
    principal.sign_with(daemon_key)?;
    let principal_id = store.put(&principal)?;

    let standard = default_trunk_standard(EntityId::random(), "trunk");
    let standard_id = store.put(&standard)?;

    let contents: Vec<InitContent> = if content.is_empty() {
        let rules_id = store.put(&TrackingRules::default())?;
        let tree_id = store.put(&Tree::empty())?;
        let snap_id = store.put(&Snapshot {
            root: tree_id,
            rules: rules_id,
            env: None,
            index: None,
        })?;
        vec![InitContent {
            snapshot: snap_id,
            title: "init".into(),
            body: None,
            intent: None,
            change: None,
            time: None,
        }]
    } else {
        content
    };
    let seq = (contents.len() - 1) as u64;
    let mut intent_effects = Vec::new();
    let mut rev_effects = Vec::new();
    let mut prev_rev: Option<ObjectId> = None;
    let mut root_change = EntityId::random();
    let mut rev_id = ObjectId([0; 32]);
    for c in contents {
        let intent_id = match &c.intent {
            Some(i) => {
                let oid = store.put(i)?;
                intent_effects.push(Effect::Put { id: oid });
                intent_effects.push(Effect::Point {
                    entity: i.id,
                    to: oid,
                    from: None,
                });
                Some(i.id)
            }
            None => None,
        };
        root_change = c.change.unwrap_or_else(EntityId::random);
        let rev = Revision {
            id: root_change,
            prev: None,
            snapshots: BTreeMap::from([("".to_string(), c.snapshot)]),
            parents: prev_rev.into_iter().collect(),
            intent: intent_id,
            title: c.title,
            body: c.body,
            author: daemon,
            time: c.time.unwrap_or(time),
            ops: None,
            flags: None,
        };
        rev_id = store.put(&rev)?;
        rev_effects.push(Effect::Put { id: rev_id });
        rev_effects.push(Effect::Point {
            entity: root_change,
            to: rev_id,
            from: None,
        });
        prev_rev = Some(rev_id);
    }

    let trunk = Line {
        id: EntityId::random(),
        prev: None,
        name: "trunk".into(),
        standard: standard.id,
        roots: vec!["".into()],
        shared: false,
        created: time,
    };
    let trunk_obj_id = store.put(&trunk)?;

    let mut effects = vec![
        Effect::Root { name: "".into() },
        Effect::Owner { principal: daemon },
        Effect::Put { id: principal_id },
        Effect::Point {
            entity: daemon,
            to: principal_id,
            from: None,
        },
        Effect::Put { id: standard_id },
        Effect::Point {
            entity: standard.id,
            to: standard_id,
            from: None,
        },
        Effect::Put { id: trunk_obj_id },
        Effect::Point {
            entity: trunk.id,
            to: trunk_obj_id,
            from: None,
        },
    ];
    effects.extend(rev_effects);
    effects.extend(vec![
        Effect::Head {
            line: "trunk".into(),
            to: rev_id,
            from: None,
            seq,
            attests: vec![],
            id: Some(trunk.id),
        },
        Effect::Coordinator {
            line: "trunk".into(),
            to: daemon,
        },
    ]);
    effects.extend(intent_effects);
    let signer = Signer {
        principal: daemon,
        key: SecretKey::from_bytes(&daemon_key.to_bytes()),
    };
    let op = build_op(log, &signer, None, "init", BTreeMap::new(), effects, time)?;
    let op_id = log.accept(&op)?;
    Ok(Initialized {
        op: op_id,
        daemon,
        standard: standard.id,
        trunk: trunk.id,
        root_change,
        root_revision: rev_id,
    })
}

/// Build an undo of one of the signer's own ops: inverse effects in reverse
/// order, refused if anything has moved on since.
pub fn undo<S: ObjectStore>(
    log: &OpLog<S>,
    signer: &Signer,
    target: &ObjectId,
    idem: [u8; 16],
    time: i64,
) -> Result<Op> {
    undo_as(log, signer, None, &[signer.principal], target, idem, time)
}

/// Undo an op authored by any of `allowed`: the signer itself, or sessions
/// of the same durable agent, which roll up to one identity.
pub fn undo_as<S: ObjectStore>(
    log: &OpLog<S>,
    signer: &Signer,
    cap: Option<ObjectId>,
    allowed: &[EntityId],
    target: &ObjectId,
    idem: [u8; 16],
    time: i64,
) -> Result<Op> {
    let target_op = log.get_op(target)?;
    if !allowed.contains(&target_op.author) {
        return Err(Error::Other("undo only takes back your own ops".into()));
    }
    let vs = ViewState::new(log.store());
    let current = log.current_view()?;
    let mut inverse = Vec::new();
    for e in target_op.effects.iter().rev() {
        match e {
            Effect::Put { .. } => {}
            Effect::Point { entity, to, from } => {
                let now = vs.entity(&current, entity)?;
                if now != Some(Pointer::Id(*to)) {
                    return Err(Error::Other(format!(
                        "entity {entity} has moved on since the op; undo refused"
                    )));
                }
                match from.as_ref().and_then(Pointer::from_value) {
                    Some(Pointer::Id(prev)) => inverse.push(Effect::Point {
                        entity: *entity,
                        to: prev,
                        from: Some(Pointer::Id(*to).to_value()),
                    }),
                    Some(Pointer::Conflict(_)) => {
                        return Err(Error::Other("cannot undo a conflict resolution".into()))
                    }
                    None => inverse.push(Effect::Unpoint { entity: *entity }),
                }
            }
            Effect::Propose { rev } => inverse.push(Effect::Unpropose { rev: *rev }),
            Effect::Unpropose { rev } => inverse.push(Effect::Propose { rev: *rev }),
            Effect::Claim { claim, .. } => inverse.push(Effect::Unclaim { claim: *claim }),
            Effect::Cap { cap } => inverse.push(Effect::Uncap { cap: *cap }),
            Effect::Uncap { cap } => inverse.push(Effect::Cap { cap: *cap }),
            Effect::Pause { scope } => inverse.push(Effect::Resume {
                scope: scope.clone(),
            }),
            Effect::Resume { scope } => inverse.push(Effect::Pause {
                scope: scope.clone(),
            }),
            Effect::Head { .. } => {
                return Err(Error::Other("a landing is not undone; revert it".into()))
            }
            Effect::Revoke { .. } => {
                return Err(Error::Other("a revocation is never undone".into()))
            }
            other => return Err(Error::Other(format!("effect {other:?} is not undoable"))),
        }
    }
    let mut args = args_with_idem(idem, BTreeMap::new());
    args.insert("target".into(), Value::Bytes(target.0.to_vec()));
    build_op(log, signer, cap, "undo", args, inverse, time)
}

/// Point an entity at a new version, computing `from` from the current view.
pub fn point_effect<S: ObjectStore>(
    log: &OpLog<S>,
    entity: EntityId,
    to: ObjectId,
) -> Result<Effect> {
    let vs = ViewState::new(log.store());
    let current = log.current_view()?;
    let from = vs.entity(&current, &entity)?.map(|p| p.to_value());
    Ok(Effect::Point { entity, to, from })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessra_core::object::{Memory, MemoryScope};
    use tessra_core::store::MemoryStore;

    fn memory(author: EntityId, body: &str) -> Memory {
        Memory {
            id: EntityId::random(),
            prev: None,
            kind: "gotcha".into(),
            scope: MemoryScope {
                kind: "repo".into(),
                r#ref: Value::Text("".into()),
            },
            body: body.into(),
            anchor: None,
            confidence: 800,
            author,
            time: 1,
            expires: None,
            proposed: None,
            status: "active".into(),
            links: None,
            visibility: "shared".into(),
        }
    }

    #[test]
    fn init_then_remember_then_undo() {
        let mut log = OpLog::new(MemoryStore::new());
        let key = SecretKey::generate();
        let init = init(&mut log, &key, "test-daemon", 1).unwrap();
        assert_eq!(log.heads(), vec![init.op]);
        let v = log.current_view().unwrap();
        assert_eq!(v.owners, vec![init.daemon]);
        assert_eq!(v.lines["trunk"].seq, 0);
        assert_eq!(v.lines["trunk"].coordinator, init.daemon);

        let signer = Signer {
            principal: init.daemon,
            key: SecretKey::from_bytes(&key.to_bytes()),
        };
        let m = memory(init.daemon, "tests need DATABASE_URL");
        let mid = log.store().put(&m).unwrap();
        let eff = vec![
            Effect::Put { id: mid },
            point_effect(&log, m.id, mid).unwrap(),
        ];
        let op = build_op(
            &log,
            &signer,
            None,
            "remember",
            args_with_idem([1; 16], BTreeMap::new()),
            eff,
            2,
        )
        .unwrap();
        let op_id = log.accept(&op).unwrap();
        assert_eq!(log.heads(), vec![op_id]);
        let v = log.current_view().unwrap();
        assert_eq!(
            ViewState::new(log.store()).entity(&v, &m.id).unwrap(),
            Some(Pointer::Id(mid))
        );

        // Same idempotency key returns the same op without a new head.
        let again = log.accept(&op).unwrap();
        assert_eq!(again, op_id);
        assert_eq!(log.heads().len(), 1);

        // Undo removes the memory.
        let u = undo(&log, &signer, &op_id, [2; 16], 3).unwrap();
        let uid = log.accept(&u).unwrap();
        assert_eq!(log.heads(), vec![uid]);
        let v = log.current_view().unwrap();
        assert_eq!(ViewState::new(log.store()).entity(&v, &m.id).unwrap(), None);
    }

    #[test]
    fn stale_from_is_rejected() {
        let mut log = OpLog::new(MemoryStore::new());
        let key = SecretKey::generate();
        let init = init(&mut log, &key, "d", 1).unwrap();
        let signer = Signer {
            principal: init.daemon,
            key: SecretKey::from_bytes(&key.to_bytes()),
        };
        let m = memory(init.daemon, "a");
        let mid = log.store().put(&m).unwrap();
        // Two ops both created from the same heads, both pointing the same entity.
        let op1 = build_op(
            &log,
            &signer,
            None,
            "remember",
            args_with_idem([1; 16], BTreeMap::new()),
            vec![Effect::Point {
                entity: m.id,
                to: mid,
                from: None,
            }],
            2,
        )
        .unwrap();
        let mut m2 = m.clone();
        m2.body = "b".into();
        let mid2 = log.store().put(&m2).unwrap();
        let op2 = build_op(
            &log,
            &signer,
            None,
            "remember",
            args_with_idem([2; 16], BTreeMap::new()),
            vec![Effect::Point {
                entity: m.id,
                to: mid2,
                from: None,
            }],
            2,
        )
        .unwrap();
        log.accept(&op1).unwrap();
        // op2 is concurrent (same parent) and its from is None while V for it is still None: accepted as a concurrent head.
        let id2 = log.accept(&op2).unwrap();
        assert_eq!(log.heads().len(), 2);
        // The merged view has a conflict on the entity.
        let v = log.current_view().unwrap();
        assert_eq!(
            ViewState::new(log.store()).entity(&v, &m.id).unwrap(),
            Some(Pointer::Conflict({
                let mut x = vec![mid, mid2];
                x.sort();
                x
            }))
        );
        // A new op on the current heads must name the conflict as from; a stale from is rejected.
        let mut m3 = m.clone();
        m3.body = "c".into();
        m3.prev = Some(mid);
        let mid3 = log.store().put(&m3).unwrap();
        let bad = build_op(
            &log,
            &signer,
            None,
            "remember",
            args_with_idem([3; 16], BTreeMap::new()),
            vec![Effect::Point {
                entity: m.id,
                to: mid3,
                from: Some(Pointer::Id(mid).to_value()),
            }],
            3,
        )
        .unwrap();
        assert!(matches!(
            log.accept(&bad),
            Err(Error::Rejected { step: 7, .. })
        ));
        let good = build_op(
            &log,
            &signer,
            None,
            "remember",
            args_with_idem([4; 16], BTreeMap::new()),
            vec![point_effect(&log, m.id, mid3).unwrap()],
            3,
        )
        .unwrap();
        let gid = log.accept(&good).unwrap();
        assert_eq!(log.heads(), vec![gid]);
        let _ = id2;
    }

    #[test]
    fn unsigned_or_foreign_key_is_rejected() {
        let mut log = OpLog::new(MemoryStore::new());
        let key = SecretKey::generate();
        let init = init(&mut log, &key, "d", 1).unwrap();
        let wrong = SecretKey::generate();
        let signer = Signer {
            principal: init.daemon,
            key: wrong,
        };
        let m = memory(init.daemon, "a");
        let mid = log.store().put(&m).unwrap();
        let op = build_op(
            &log,
            &signer,
            None,
            "remember",
            args_with_idem([1; 16], BTreeMap::new()),
            vec![Effect::Point {
                entity: m.id,
                to: mid,
                from: None,
            }],
            2,
        )
        .unwrap();
        assert!(matches!(
            log.accept(&op),
            Err(Error::Rejected { step: 3, .. })
        ));
    }
}
