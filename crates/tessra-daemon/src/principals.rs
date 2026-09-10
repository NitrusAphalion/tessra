//! Agents, sessions, and the default task capability.
//!
//! A durable agent principal is created once per name; its key lives in the
//! keys directory. A session principal is created per run, signed by the
//! agent's key, with a capability issued by the daemon as owner. The session
//! key stays in this process and is never written anywhere.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ciborium::value::Value;
use tessra_core::object::{Capability, Effect, Principal, ReadScope, WriteScope};
use tessra_core::sig::{SecretKey, SignedObject};
use tessra_core::store::ObjectStore;
use tessra_core::{EntityId, ObjectId};
use tessra_oplog::build::{self, Signer};

use crate::{now, paths, Error, Repo, Result};

/// An acting principal: who signs, which capability, which workspace.
pub struct Actor {
    pub signer: Signer,
    pub cap: Option<ObjectId>,
    pub write_paths: Option<Vec<String>>,
    pub workspace: Option<EntityId>,
    pub kind: String,
}

impl Actor {
    pub fn principal(&self) -> EntityId {
        self.signer.principal
    }
}

impl Repo {
    /// The daemon as actor: the owner, acting in the root workspace.
    pub fn daemon_actor(&self) -> Actor {
        Actor {
            signer: self.daemon_signer(),
            cap: None,
            write_paths: None,
            workspace: self.root_workspace().map(|w| w.id),
            kind: "daemon".into(),
        }
    }

    /// Find or create the durable agent principal for a runtime name.
    pub fn ensure_agent(
        &mut self,
        name: &str,
        runtime: Option<&str>,
    ) -> Result<(EntityId, SecretKey)> {
        let agents_dir = self.keys_dir.join("agents");
        std::fs::create_dir_all(&agents_dir)?;
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let key_path = agents_dir.join(format!("{safe}.key"));
        let id_path = agents_dir.join(format!("{safe}.id"));
        if key_path.exists() && id_path.exists() {
            let key = paths::read_key(&key_path)?;
            let id = EntityId::from_letters(std::fs::read_to_string(&id_path)?.trim())?;
            return Ok((id, key));
        }
        let key = SecretKey::generate();
        let id = EntityId::random();
        let mut p = Principal {
            id,
            prev: None,
            key: Some(key.public()),
            kind: "agent".into(),
            name: name.into(),
            parent: Some(self.daemon),
            model: None,
            runtime: runtime
                .map(|r| BTreeMap::from([("framework".to_string(), Value::Text(r.into()))])),
            bindings: None,
            status: "active".into(),
            expires: None,
            time: now(),
            sig: None,
        };
        // Non-session principals are created by an owner and signed with the owner's key.
        p.sign_with(self.daemon_key())?;
        let oid = self.store().put(&p)?;
        let signer = self.daemon_signer();
        let op = build::build_op(
            &self.log,
            &signer,
            None,
            "principal",
            build::args_with_idem(EntityId::random().0, BTreeMap::new()),
            vec![
                Effect::Put { id: oid },
                Effect::Point {
                    entity: id,
                    to: oid,
                    from: None,
                },
            ],
            now(),
        )?;
        self.commit_op(&op)?;
        paths::write_key(&key_path, &key)?;
        std::fs::write(&id_path, id.to_letters())?;
        Ok((id, key))
    }

    /// The durable agent principal for a name, created if needed.
    pub fn agent_id(&mut self, name: &str) -> Result<EntityId> {
        Ok(self.ensure_agent(name, None)?.0)
    }

    fn grant_path(&self, agent_name: &str) -> PathBuf {
        self.keys_dir.join("grants").join(format!("{}.json", safe_name(agent_name)))
    }

    /// The standing grant for an agent's sessions, set by the owner.
    pub fn grant_template(&self, agent_name: &str) -> Option<GrantTemplate> {
        let bytes = std::fs::read(self.grant_path(agent_name)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn set_grant_template(&self, agent_name: &str, t: &GrantTemplate) -> Result<()> {
        let p = self.grant_path(agent_name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, serde_json::to_vec_pretty(t).unwrap_or_default())?;
        Ok(())
    }

    /// The session files for an agent and write scope: `(meta, key)`.
    pub fn session_files(&self, agent_name: &str, write_paths: &[String]) -> (PathBuf, PathBuf) {
        let sessions_dir = self.keys_dir.join("sessions");
        let scope_tag = {
            let mut h = blake3::Hasher::new();
            for p in write_paths {
                h.update(p.as_bytes());
                h.update(b"\n");
            }
            hex::encode(&h.finalize().as_bytes()[..6])
        };
        let base = sessions_dir.join(format!("{}-{scope_tag}", safe_name(agent_name)));
        (base.with_extension("meta"), base.with_extension("key"))
    }

    /// The write scope a session for this agent gets: the planner's
    /// assignment, else the owner's grant, else what the caller asked for.
    pub fn effective_write_paths(&self, agent_id: &EntityId, agent_name: &str, asked: Vec<String>) -> Vec<String> {
        if let Some(a) = crate::swarm::load_assignment(self, agent_id) {
            if !a.paths.is_empty() {
                return a.paths;
            }
        }
        if let Some(t) = self.grant_template(agent_name) {
            if !t.paths.is_empty() {
                return t.paths;
            }
        }
        asked
    }

    /// Point a persisted session at a new capability, such as one a planner
    /// delegated to it.
    pub fn set_session_cap(&self, agent_name: &str, write_paths: &[String], cap: ObjectId) -> Result<()> {
        let (meta_path, _) = self.session_files(agent_name, write_paths);
        let meta = std::fs::read_to_string(&meta_path)?;
        let mut lines: Vec<String> = meta.lines().map(str::to_string).collect();
        if lines.len() >= 2 {
            lines[1] = cap.to_hex();
            std::fs::write(&meta_path, format!("{}\n", lines.join("\n")))?;
        }
        Ok(())
    }

    /// A principal's name, when it resolves.
    pub fn principal_name(&self, id: &EntityId) -> Option<String> {
        let view = self.log.current_view().ok()?;
        let vs = tessra_oplog::ViewState::new(self.store());
        let (_, p) = tessra_oplog::verify::resolve_principal(self.store(), &vs, &view, id).ok()??;
        match p.kind.as_str() {
            "session" => {
                let parent = p.parent?;
                let (_, a) = tessra_oplog::verify::resolve_principal(self.store(), &vs, &view, &parent).ok()??;
                Some(a.name)
            }
            _ => Some(p.name),
        }
    }

    /// The agent a session belongs to.
    pub fn session_agent(&self, session: &EntityId) -> Option<EntityId> {
        let view = self.log.current_view().ok()?;
        let vs = tessra_oplog::ViewState::new(self.store());
        let (_, p) = tessra_oplog::verify::resolve_principal(self.store(), &vs, &view, session).ok()??;
        if p.kind == "session" {
            p.parent
        } else {
            None
        }
    }

    pub fn is_revoked(&self, id: &EntityId) -> bool {
        let Ok(view) = self.log.current_view() else { return false };
        let vs = tessra_oplog::ViewState::new(self.store());
        vs.principal_revoked(&view, id).unwrap_or(false)
    }

    /// Open a session for an agent: a session principal signed by the agent
    /// and a default capability issued by the daemon. An assignment from the
    /// planner narrows the write scope and sets the op budget.
    pub fn open_session(
        &mut self,
        agent_name: &str,
        model: Option<&str>,
        write_paths: Vec<String>,
    ) -> Result<Actor> {
        let (agent_id, agent_key) = self.ensure_agent(agent_name, None)?;
        if self.is_revoked(&agent_id) {
            return Err(Error::verb("REVOKED", format!("agent {agent_name} is revoked")));
        }
        let assignment = crate::swarm::load_assignment(self, &agent_id);
        let template = self.grant_template(agent_name);
        let write_paths = self.effective_write_paths(&agent_id, agent_name, write_paths);
        // Reuse an unexpired session held by this daemon for the same agent and scope.
        std::fs::create_dir_all(self.keys_dir.join("sessions"))?;
        let (meta_path, key_path) = self.session_files(agent_name, &write_paths);
        if let (Ok(meta), Ok(key)) = (
            std::fs::read_to_string(&meta_path),
            paths::read_key(&key_path),
        ) {
            let mut lines = meta.lines();
            if let (Some(id), Some(cap), Some(exp)) = (lines.next(), lines.next(), lines.next()) {
                if let (Ok(id), Ok(cap), Ok(exp)) = (
                    EntityId::from_letters(id.trim()),
                    ObjectId::from_hex(cap.trim()),
                    exp.trim().parse::<i64>(),
                ) {
                    if exp > now() && !self.is_revoked(&id) {
                        return Ok(Actor {
                            signer: Signer { principal: id, key },
                            cap: Some(cap),
                            write_paths: Some(write_paths),
                            workspace: None,
                            kind: "session".into(),
                        });
                    }
                }
            }
        }
        let session_key = SecretKey::generate();
        let session_id = EntityId::random();
        let t = now();
        let mut session = Principal {
            id: session_id,
            prev: None,
            key: Some(session_key.public()),
            kind: "session".into(),
            name: format!("{agent_name} session"),
            parent: Some(agent_id),
            model: model.map(|m| m.to_string()),
            runtime: None,
            bindings: None,
            status: "active".into(),
            expires: Some(t + 4 * 3600 * 1_000_000_000),
            time: t,
            sig: None,
        };
        session.sign_with(&agent_key)?;
        let session_oid = self.store().put(&session)?;

        let mut cap = default_capability(self.daemon, session_id, write_paths.clone(), t);
        if let Some(tpl) = &template {
            for v in &tpl.verbs {
                if !cap.verbs.contains(v) {
                    cap.verbs.push(v.clone());
                }
            }
            cap.verbs.sort();
            cap.delegable = tpl.delegable;
            if let Some(ops) = tpl.ops {
                cap.hard.get_or_insert_with(BTreeMap::new).insert("ops".to_string(), ops.max(1));
            }
        }
        if let Some(a) = &assignment {
            let hard = cap.hard.get_or_insert_with(BTreeMap::new);
            hard.insert("ops".to_string(), a.ops.max(1));
        }
        cap.sign_with(self.daemon_key())?;
        let cap_id = self.store().put(&cap)?;

        let signer = self.daemon_signer();
        let op = build::build_op(
            &self.log,
            &signer,
            None,
            "session",
            build::args_with_idem(EntityId::random().0, BTreeMap::new()),
            vec![
                Effect::Put { id: session_oid },
                Effect::Point {
                    entity: session_id,
                    to: session_oid,
                    from: None,
                },
                Effect::Put { id: cap_id },
                Effect::Cap { cap: cap_id },
            ],
            t,
        )?;
        self.commit_op(&op)?;
        paths::write_key(&key_path, &session_key)?;
        std::fs::write(
            &meta_path,
            format!(
                "{}\n{}\n{}\n",
                session_id.to_letters(),
                cap_id.to_hex(),
                session.expires.unwrap_or(0)
            ),
        )?;
        Ok(Actor {
            signer: Signer {
                principal: session_id,
                key: session_key,
            },
            cap: Some(cap_id),
            write_paths: Some(write_paths),
            workspace: None,
            kind: "session".into(),
        })
    }

    fn named_paths(&self, dir: &str, name: &str) -> (PathBuf, PathBuf) {
        let dir = self.keys_dir.join(dir);
        let safe = safe_name(name);
        (dir.join(format!("{safe}.key")), dir.join(format!("{safe}.meta")))
    }

    /// Create an external principal, such as a CI system, with a capability
    /// that grants `attest` and nothing else, and hold its key. The daemon
    /// is the issuer, so only the owner reaches this.
    pub fn grant_external(&mut self, name: &str) -> Result<(EntityId, ObjectId)> {
        self.grant_named(name, "external")
    }

    /// Create a human principal the daemon holds a key for, so a person on
    /// this machine can approve with `--as <name>`. A passkey binding is the
    /// alternative. The capability grants `attest` only.
    pub fn grant_human(&mut self, name: &str) -> Result<(EntityId, ObjectId)> {
        self.grant_named(name, "human")
    }

    fn grant_named(&mut self, name: &str, kind: &str) -> Result<(EntityId, ObjectId)> {
        let dir = if kind == "human" { "humans" } else { "externals" };
        let (key_path, meta_path) = self.named_paths(dir, name);
        if let Ok(meta) = std::fs::read_to_string(&meta_path) {
            let mut lines = meta.lines();
            if let (Some(id), Some(cap)) = (lines.next(), lines.next()) {
                if let (Ok(id), Ok(cap)) = (
                    EntityId::from_letters(id.trim()),
                    ObjectId::from_hex(cap.trim()),
                ) {
                    return Ok((id, cap));
                }
            }
        }
        if let Some(d) = key_path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let key = SecretKey::generate();
        let id = EntityId::random();
        let t = now();
        let mut p = Principal {
            id,
            prev: None,
            key: Some(key.public()),
            kind: kind.into(),
            name: name.into(),
            parent: Some(self.daemon),
            model: None,
            runtime: None,
            bindings: None,
            status: "active".into(),
            expires: None,
            time: t,
            sig: None,
        };
        p.sign_with(self.daemon_key())?;
        let oid = self.store().put(&p)?;
        let mut cap = Capability {
            issuer: self.daemon,
            subject: id,
            verbs: vec!["attest".into()],
            write: WriteScope {
                roots: Some(vec!["".into()]),
                paths: Some(vec![]),
                nodes: None,
                lines: None,
                targets: None,
                stages: None,
                memory_scopes: None,
            },
            read: ReadScope {
                roots: Some(vec!["".into()]),
                paths: Some(vec!["**".into()]),
                memory: Some("shared".into()),
            },
            hard: Some(BTreeMap::from([("ops".to_string(), 100_000u64)])),
            advisory: None,
            delegable: false,
            parent: None,
            nonce: EntityId::random(),
            time: t,
            sig: None,
        };
        cap.sign_with(self.daemon_key())?;
        let cap_id = self.store().put(&cap)?;
        let signer = self.daemon_signer();
        let op = build::build_op(
            &self.log,
            &signer,
            None,
            "grant",
            build::args_with_idem(EntityId::random().0, BTreeMap::new()),
            vec![
                Effect::Put { id: oid },
                Effect::Point {
                    entity: id,
                    to: oid,
                    from: None,
                },
                Effect::Put { id: cap_id },
                Effect::Cap { cap: cap_id },
            ],
            t,
        )?;
        self.commit_op(&op)?;
        paths::write_key(&key_path, &key)?;
        std::fs::write(
            &meta_path,
            format!("{}\n{}\n", id.to_letters(), cap_id.to_hex()),
        )?;
        Ok((id, cap_id))
    }

    /// Act as an external or human principal this daemon holds the key for.
    pub fn open_external(&self, name: &str) -> Result<Actor> {
        let (ext_key, ext_meta) = self.named_paths("externals", name);
        let (hum_key, hum_meta) = self.named_paths("humans", name);
        let (key_path, meta_path, kind) = if hum_meta.exists() {
            (hum_key, hum_meta, "human")
        } else {
            (ext_key, ext_meta, "external")
        };
        let meta = std::fs::read_to_string(&meta_path).map_err(|_| {
            Error::verb(
                "PRINCIPAL",
                format!("no principal {name}; the owner grants one with `tessra grant --external {name}` or `--human {name}`"),
            )
        })?;
        let mut lines = meta.lines();
        let (id, cap) = match (lines.next(), lines.next()) {
            (Some(i), Some(c)) => (
                EntityId::from_letters(i.trim()).map_err(|e| Error::verb("PRINCIPAL", e.to_string()))?,
                ObjectId::from_hex(c.trim()).map_err(|e| Error::verb("PRINCIPAL", e.to_string()))?,
            ),
            _ => return Err(Error::verb("PRINCIPAL", "external principal record is damaged")),
        };
        let key = paths::read_key(&key_path)?;
        Ok(Actor {
            signer: Signer { principal: id, key },
            cap: Some(cap),
            write_paths: Some(vec![]),
            workspace: None,
            kind: kind.into(),
        })
    }
}

/// A standing grant for an agent's sessions beyond the default: extra
/// verbs, a write scope, delegability, and an op budget.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct GrantTemplate {
    #[serde(default)]
    pub verbs: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub delegable: bool,
    #[serde(default)]
    pub ops: Option<u64>,
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// The default task-scoped grant from `spec/04-security.md`.
pub fn default_capability(
    issuer: EntityId,
    subject: EntityId,
    write_paths: Vec<String>,
    time: i64,
) -> Capability {
    Capability {
        issuer,
        subject,
        verbs: vec![
            "attest".into(),
            "edit".into(),
            "promote".into(),
            "remember".into(),
            "try".into(),
            "verify".into(),
        ],
        write: WriteScope {
            roots: Some(vec!["".into()]),
            paths: Some(write_paths.clone()),
            nodes: None,
            lines: None,
            targets: None,
            stages: Some(vec!["proposed".into()]),
            memory_scopes: Some(vec![
                "node".into(),
                "path".into(),
                "intent".into(),
                "root".into(),
            ]),
        },
        read: ReadScope {
            roots: Some(vec!["".into()]),
            paths: Some(vec!["**".into()]),
            memory: Some("shared".into()),
        },
        hard: Some(BTreeMap::from([
            ("ops".to_string(), 10_000u64),
            ("workspaces".to_string(), 8u64),
        ])),
        advisory: None,
        delegable: false,
        parent: None,
        nonce: EntityId::random(),
        time,
        sig: None,
    }
}

impl Error {
    pub fn not_yet(verb: &str, when: &str) -> Self {
        Error::verb(
            "NOT_YET",
            format!("{verb} arrives in {when}; see ROADMAP.md"),
        )
    }
}

#[allow(dead_code)]
fn _assert_store<S: ObjectStore>(_s: &S) {}
