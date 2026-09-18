//! The object catalog from `spec/02-objects.md`, draft 2.
//!
//! Field names are the CBOR keys. Optional fields are omitted when absent.
//! Types marked entity carry `id` and `prev`. Signed types carry `sig`.

use std::collections::BTreeMap;

use ciborium::value::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::cbor::TessraObject;
use crate::sig::{PublicKey, Sig, SignedObject};
use crate::{EntityId, ObjectId};

macro_rules! object {
    ($ty:ty, $tag:literal) => {
        impl TessraObject for $ty {
            const TAG: &'static str = $tag;
            const VERSION: u64 = 1;
        }
    };
}

macro_rules! signed {
    ($ty:ty) => {
        impl SignedObject for $ty {
            fn sig(&self) -> Option<&Sig> {
                self.sig.as_ref()
            }
            fn set_sig(&mut self, sig: Option<Sig>) {
                self.sig = sig;
            }
        }
    };
}

/// Any object, for dispatch by tag.
pub const TAGS: &[&str] = &[
    "blob",
    "artifact",
    "tree",
    "trackingrules",
    "environment",
    "snapshot",
    "nodeindex",
    "revision",
    "intent",
    "comment",
    "attestation",
    "memory",
    "standard",
    "claim",
    "workspace",
    "principal",
    "capability",
    "hook",
    "channel",
    "manifest",
    "line",
    "release",
    "target",
    "deployment",
    "trie",
    "op",
    "view",
    "tombstone",
    "anchor",
];

// ---------------------------------------------------------------- Group A

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub hash: ObjectId,
    pub size: u64,
    pub chunks: Vec<ObjectId>,
}
object!(Artifact, "artifact");

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Artifact,
    Conflict,
}
impl From<EntryKind> for u8 {
    fn from(k: EntryKind) -> u8 {
        k as u8
    }
}
impl TryFrom<u8> for EntryKind {
    type Error = String;
    fn try_from(v: u8) -> Result<Self, String> {
        Ok(match v {
            0 => EntryKind::File,
            1 => EntryKind::Dir,
            2 => EntryKind::Symlink,
            3 => EntryKind::Artifact,
            4 => EntryKind::Conflict,
            _ => return Err(format!("bad entry kind {v}")),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConflictTerm {
    pub sign: i8,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u8>,
    pub r#ref: ObjectId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: String,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terms: Option<Vec<ConflictTerm>>,
}

/// A directory: flat or sharded, never both.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<TreeEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shards: Option<Vec<Option<ObjectId>>>,
}
object!(Tree, "tree");

impl Tree {
    pub const SHARD_THRESHOLD: usize = 4096;

    pub fn flat(mut entries: Vec<TreeEntry>) -> Self {
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        Tree {
            entries: Some(entries),
            shards: None,
        }
    }

    pub fn empty() -> Self {
        Tree {
            entries: Some(Vec::new()),
            shards: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub pattern: String,
    pub class: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackingRules {
    pub rules: Vec<Rule>,
    pub eol: String,
    pub case: String,
    pub portable_names: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_threshold: Option<u64>,
}
object!(TrackingRules, "trackingrules");

impl Default for TrackingRules {
    fn default() -> Self {
        TrackingRules {
            rules: Vec::new(),
            eol: "lf".into(),
            case: "check".into(),
            portable_names: true,
            artifact_threshold: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub os: String,
    pub arch: String,
    pub tools: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, Value>>,
}
object!(Environment, "environment");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub root: ObjectId,
    pub rules: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<ObjectId>,
}
object!(Snapshot, "snapshot");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub nid: EntityId,
    pub path: String,
    pub kind: String,
    pub name: String,
    pub span: (u64, u64),
    pub body: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deps: Option<Vec<EntityId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setlike: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeIndex {
    /// The root tree the index describes. Draft 3 called this `snapshot`;
    /// objects written then still decode.
    #[serde(alias = "snapshot")]
    pub root: ObjectId,
    pub grammars: String,
    pub parents: Vec<ObjectId>,
    pub nodes: Vec<Node>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<BTreeMap<ByteBuf, EntityId>>,
}
object!(NodeIndex, "nodeindex");

// ---------------------------------------------------------------- Group B

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemOp {
    pub kind: String,
    pub args: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Flags {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_of_scope: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<SecretMatch>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_collisions: Option<Vec<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unportable_names: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_drift: Option<Vec<String>>,
}

impl Flags {
    pub fn is_empty(&self) -> bool {
        self.out_of_scope.is_none()
            && self.secrets.is_none()
            && self.case_collisions.is_none()
            && self.unportable_names.is_none()
            && self.generated_drift.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecretMatch {
    pub path: String,
    pub pattern: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub snapshots: BTreeMap<String, ObjectId>,
    pub parents: Vec<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<EntityId>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub author: EntityId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops: Option<Vec<SemOp>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<Flags>,
}
object!(Revision, "revision");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evals: Option<Vec<ObjectId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends: Option<Vec<EntityId>>,
    pub priority: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<EntityId>,
    pub created_by: EntityId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy: Option<bool>,
}
object!(Intent, "intent");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub subject: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<BTreeMap<String, Value>>,
    pub body: String,
    pub author: EntityId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggest: Option<ObjectId>,
}
object!(Comment, "comment");

// ---------------------------------------------------------------- Group C

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attestation {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<ByteBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bodies: Option<Vec<ObjectId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<BTreeMap<String, Value>>,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ObjectId>,
    pub verifier: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ObjectId>,
    pub time: i64,
    pub sigkind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Sig>,
}
object!(Attestation, "attestation");
signed!(Attestation);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryScope {
    pub kind: String,
    pub r#ref: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub kind: String,
    pub scope: MemoryScope,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<ObjectId>,
    pub confidence: u16,
    pub author: EntityId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed: Option<bool>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<ObjectId>>,
    pub visibility: String,
}
object!(Memory, "memory");

pub const MEMORY_KINDS: &[&str] = &[
    "fact",
    "decision",
    "convention",
    "gotcha",
    "preference",
    "task",
    "question",
    "summary",
    "resolution",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Predicate {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clause {
    pub op: String,
    pub pred: Predicate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unless: Option<Predicate>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopedClauses {
    pub pattern: String,
    pub clauses: Vec<Clause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RiskClauses {
    pub risk_at_least: String,
    pub clauses: Vec<Clause>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Standard {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extends: Option<EntityId>,
    pub clauses: Vec<Clause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<ScopedClauses>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<Vec<RiskClauses>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_permille: Option<u16>,
}
object!(Standard, "standard");

// ---------------------------------------------------------------- Group D

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimTarget {
    pub kind: String,
    pub r#ref: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub principal: EntityId,
    pub targets: Vec<ClaimTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<EntityId>,
    pub exclusive: bool,
    pub expires: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
object!(Claim, "claim");

/// Local to a daemon; never stored in the object store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: EntityId,
    pub principal: EntityId,
    pub base: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<ObjectId>,
    pub paths: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ObjectId>,
    pub created: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    /// A principal other than the owner working in this directory: an agent
    /// that adopted the colocated checkout. Its edits and snapshots are that
    /// principal's until it releases the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<EntityId>,
}
object!(Workspace, "workspace");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub provider: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<ByteBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Principal {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<PublicKey>,
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<BTreeMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bindings: Option<Vec<Binding>>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Sig>,
}
object!(Principal, "principal");
signed!(Principal);

pub const PRINCIPAL_KINDS: &[&str] = &[
    "human", "agent", "session", "verifier", "hook", "deployer", "observer", "external", "daemon",
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WriteScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<EntityId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<EntityId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<EntityId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stages: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_scopes: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub issuer: EntityId,
    pub subject: EntityId,
    pub verbs: Vec<String>,
    pub write: WriteScope,
    pub read: ReadScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hard: Option<BTreeMap<String, u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory: Option<BTreeMap<String, i64>>,
    pub delegable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ObjectId>,
    pub nonce: EntityId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Sig>,
}
object!(Capability, "capability");
signed!(Capability);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookAction {
    pub kind: String,
    pub args: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hook {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub name: String,
    pub on: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#where: Option<Predicate>,
    pub r#do: Vec<HookAction>,
    pub r#as: EntityId,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
}
object!(Hook, "hook");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub kind: String,
    pub config: BTreeMap<String, Value>,
    pub principals: Vec<EntityId>,
    pub signing: String,
    pub time: i64,
}
object!(Channel, "channel");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub replica: EntityId,
    pub serves: Vec<EntityId>,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Sig>,
}
object!(Manifest, "manifest");
signed!(Manifest);

// ---------------------------------------------------------------- Group E

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub name: String,
    pub standard: EntityId,
    pub roots: Vec<String>,
    pub shared: bool,
    pub created: i64,
}
object!(Line, "line");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub name: String,
    pub line: EntityId,
    pub revision: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<ObjectId>,
    pub changelog: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestations: Option<Vec<ObjectId>>,
    pub signer: EntityId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Sig>,
}
object!(Release, "release");
signed!(Release);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<ObjectId>,
    pub name: String,
    pub deployer: BTreeMap<String, Value>,
    pub standard: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observers: Option<Vec<BTreeMap<String, Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_budget: Option<BTreeMap<String, u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<String>,
}
object!(Target, "target");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Deployment {
    pub target: EntityId,
    pub revision: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<ObjectId>,
    pub principal: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<BTreeMap<String, Value>>,
    pub time: i64,
}
object!(Deployment, "deployment");

// ---------------------------------------------------------------- Group F

/// A conflict value in a view map or a `from` field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConflictValue {
    pub conflict: Vec<ObjectId>,
}

/// One effect of an op. Serialized with `kind` as the discriminator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    Put {
        id: ObjectId,
    },
    Point {
        entity: EntityId,
        to: ObjectId,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<Value>,
    },
    Unpoint {
        entity: EntityId,
    },
    Head {
        line: String,
        to: ObjectId,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<Value>,
        seq: u64,
        attests: Vec<ObjectId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<EntityId>,
    },
    Propose {
        rev: ObjectId,
    },
    Unpropose {
        rev: ObjectId,
    },
    Claim {
        claim: EntityId,
        to: ObjectId,
    },
    Unclaim {
        claim: EntityId,
    },
    Cap {
        cap: ObjectId,
    },
    Uncap {
        cap: ObjectId,
    },
    Revoke {
        principal: EntityId,
    },
    Tomb {
        of: ObjectId,
        tomb: ObjectId,
    },
    Deploy {
        target: EntityId,
        to: ObjectId,
    },
    Pause {
        scope: String,
    },
    Resume {
        scope: String,
    },
    Owner {
        principal: EntityId,
    },
    Unowner {
        principal: EntityId,
    },
    Root {
        name: String,
    },
    Unroot {
        name: String,
    },
    Coordinator {
        line: String,
        to: EntityId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub parents: Vec<ObjectId>,
    pub author: EntityId,
    pub key: PublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap: Option<ObjectId>,
    pub kind: String,
    pub args: BTreeMap<String, Value>,
    pub effects: Vec<Effect>,
    pub view: ObjectId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Sig>,
}
object!(Op, "op");
signed!(Op);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineState {
    pub id: EntityId,
    pub head: Value,
    pub seq: u64,
    pub coordinator: EntityId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PauseState {
    pub scope: String,
    pub by: EntityId,
    pub op: ObjectId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub entities: ObjectId,
    pub proposed: ObjectId,
    pub claims: ObjectId,
    pub caps: ObjectId,
    pub revoked: ObjectId,
    pub tombstoned: ObjectId,
    pub lines: BTreeMap<String, LineState>,
    pub roots: Vec<String>,
    pub owners: Vec<EntityId>,
    pub deployed: BTreeMap<EntityId, ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<Vec<PauseState>>,
}
object!(View, "view");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub of: ObjectId,
    pub r#type: String,
    pub reason: String,
    pub op: ObjectId,
    pub time: i64,
}
object!(Tombstone, "tombstone");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub heads: Vec<ObjectId>,
    pub by: EntityId,
    pub time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Sig>,
}
object!(Anchor, "anchor");
signed!(Anchor);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor::{decode, encode};
    use crate::sig::SecretKey;

    fn oid(n: u8) -> ObjectId {
        ObjectId([n; 32])
    }

    #[test]
    fn tree_sorts_and_round_trips() {
        let t = Tree::flat(vec![
            TreeEntry {
                name: "b".into(),
                kind: EntryKind::File,
                mode: Some(0),
                r#ref: Some(oid(1)),
                terms: None,
            },
            TreeEntry {
                name: "a".into(),
                kind: EntryKind::Dir,
                mode: None,
                r#ref: Some(oid(2)),
                terms: None,
            },
        ]);
        assert_eq!(t.entries.as_ref().unwrap()[0].name, "a");
        let e = encode(&t).unwrap();
        let back: Tree = decode(&e.bytes).unwrap();
        assert_eq!(back, t);
        assert_eq!(
            encode(&Tree::empty()).unwrap().id,
            encode(&Tree::empty()).unwrap().id
        );
    }

    #[test]
    fn revision_round_trip_with_optional_fields() {
        let r = Revision {
            id: EntityId::random(),
            prev: None,
            snapshots: BTreeMap::from([("".to_string(), oid(3))]),
            parents: vec![],
            intent: None,
            title: "first".into(),
            body: None,
            author: EntityId::random(),
            time: 1_700_000_000_000_000_000,
            ops: None,
            flags: Some(Flags {
                out_of_scope: Some(vec!["x/y".into()]),
                ..Default::default()
            }),
        };
        let e = encode(&r).unwrap();
        let back: Revision = decode(&e.bytes).unwrap();
        assert_eq!(back, r);
        assert!(!back.flags.unwrap().is_empty());
    }

    #[test]
    fn op_signs_and_effects_tag_by_kind() {
        let k = SecretKey::generate();
        let mut op = Op {
            parents: vec![oid(9)],
            author: EntityId::random(),
            key: k.public(),
            cap: None,
            kind: "snapshot".into(),
            args: BTreeMap::from([("idem".to_string(), Value::Bytes(vec![1; 16]))]),
            effects: vec![
                Effect::Put { id: oid(1) },
                Effect::Point {
                    entity: EntityId::random(),
                    to: oid(1),
                    from: None,
                },
                Effect::Head {
                    line: "trunk".into(),
                    to: oid(2),
                    from: Some(Value::Bytes(oid(1).0.to_vec())),
                    seq: 4,
                    attests: vec![],
                    id: None,
                },
            ],
            view: oid(5),
            time: 1,
            desc: None,
            sig: None,
        };
        op.sign_with(&k).unwrap();
        op.verify_with(&k.public()).unwrap();
        let e = encode(&op).unwrap();
        let back: Op = decode(&e.bytes).unwrap();
        assert_eq!(back, op);
        back.verify_with(&k.public()).unwrap();
        // The discriminator is the `kind` key.
        let v: Value = ciborium::de::from_reader(&e.bytes[..]).unwrap();
        let s = format!("{v:?}");
        assert!(s.contains("Text(\"point\")"));
    }

    #[test]
    fn principal_and_capability_sign() {
        let k = SecretKey::generate();
        let mut p = Principal {
            id: EntityId::random(),
            prev: None,
            key: Some(k.public()),
            kind: "daemon".into(),
            name: "local".into(),
            parent: None,
            model: None,
            runtime: None,
            bindings: None,
            status: "active".into(),
            expires: None,
            time: 1,
            sig: None,
        };
        p.sign_with(&k).unwrap();
        p.verify_with(&k.public()).unwrap();
        let mut c = Capability {
            issuer: p.id,
            subject: EntityId::random(),
            verbs: vec!["edit".into(), "remember".into()],
            write: WriteScope {
                paths: Some(vec!["src/**".into()]),
                ..Default::default()
            },
            read: ReadScope {
                memory: Some("shared".into()),
                ..Default::default()
            },
            hard: None,
            advisory: None,
            delegable: false,
            parent: None,
            nonce: EntityId::random(),
            time: 1,
            sig: None,
        };
        c.sign_with(&k).unwrap();
        c.verify_with(&k.public()).unwrap();
        let e = encode(&c).unwrap();
        let back: Capability = decode(&e.bytes).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn entry_kind_encodes_as_small_int() {
        let bytes = crate::cbor::to_canonical_bytes(&EntryKind::Symlink).unwrap();
        assert_eq!(bytes, vec![0x02]);
        let k: EntryKind = crate::cbor::from_canonical_bytes(&[0x04]).unwrap();
        assert_eq!(k, EntryKind::Conflict);
        assert!(crate::cbor::from_canonical_bytes::<EntryKind>(&[0x09]).is_err());
    }
}
