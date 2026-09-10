# 02. Objects

Draft 9. Changes from draft 8, all from building M8: the environment carries state trees and lockfile hashes; the snapshot's environment is what a checkout restores.

Draft 8. Changes from draft 7, all from building M6: deployer and observer adapters as built, the deployment step, the observe attestation and predicate, releases as indexed, and the `notify` hook action.

Draft 7. Changes from draft 6, all from building M5: the shapes of judge and approval attestations and of an approval request; the `judge` and `approved` predicates as built; channels as built; the `agent` hook action; how human principals are created.

Draft 6. Changes from draft 5, all from building M4: the intent hierarchy a plan writes; standing grants and delegated capabilities as built; the `land` hook action; what a claim answers.

Draft 5. Changes from draft 4, all from building M3: which subjects an attestation may name for a revision and the shapes of the test and risk kinds; who may attest what; the predicate grammar as text with the structural names that exist; `when` blocks read the risk attestation; hooks gain the `run` action, the event patterns, and the `where` grammar that exist; `external` and `verifier` principals say how they are created; the sandbox level M3 runs at.

Draft 4. Changes from draft 3, all from building M2: the nodeindex names its root tree, not its snapshot, because the snapshot names the index; the body hash has two normalizations so a trailing separator is not an edit and a pure rename keeps its hash; matching is by `(parent, kind, name, ordinal)` then by body under the same parent, with structural similarity still open; `deps` says how far a reference resolves; functions a language marks as tests have kind `test`; `revision.ops` defines `rename` and reserves the other kinds.

Draft 3. Changes from draft 2, all from building M1: the `paused` entry records the pausing op's first parent (its own ID would be circular); principal signing rules are spelled out per kind; the `head` effect can create a line. See 03 and 04 for the rest.

Draft 2. Twenty-nine types in six groups. Changes from draft 1: the view's large maps are persistent tries (B1); workspaces left the view and claims became replicated short-lived objects (B2); hooks carry a creation invariant (B4); the node index is history-derived with aliases (B7); revisions carry snapshot flags instead of being refused (B8); artifacts are content only (B9); attestations can key on node body hashes (B12); capabilities have read scope and hard versus advisory limits (B13, B14); agent identity is two-level (B15); lines record their coordinator (S1); trees shard deterministically (S5); Windows-hostile names are flagged (S10); cost left the revision (S12); ops reference blobs instead of inlining content (S22); the manifest type declares which principals a replica serves (B14).

Each entry gives the type tag, whether the type is content-addressed only, an entity with versions, or local to a daemon, the fields, and the invariants a validator MUST enforce.

## The entity pattern

Some things have an identity that outlives any one version: a change is rewritten, an intent changes status, a principal rotates a key. These are entities. An entity has a 16-byte entity ID assigned at creation, and a sequence of immutable, content-addressed version objects. Every version carries:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | The entity ID. Identical across all versions |
| `prev` | bytes32 | no | Object ID of the previous version. Absent on the first version |

The current version of each entity is recorded in the view (03-operations.md). Versions never change; the view's pointer moves. "Rewrite a change" means: create a new revision with the same `id` and `prev` set to the old one, and have an op point the view at it. Two concurrent versions with the same `prev` are both valid; the view merge records a conflict value and a later op resolves it.

Entities: revision, intent, memory, standard, hook, principal, channel, line, target, claim. Everything else is content-addressed with no identity, or local to a daemon.

## Group A. Content

### blob

Tag `blob`. Raw bytes, no envelope. Invariant: size MUST be at or below the artifact threshold; larger content MUST be an artifact.

### artifact

Tag `artifact`, `v` 1. A pointer to large content that lives in the artifact store, not the object store. Content only, so equal content always produces the same artifact ID and therefore the same tree.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `hash` | bytes32 | yes | BLAKE3 over the full content with domain `tessra:artifact-content` |
| `size` | uint | yes | Content length in bytes |
| `chunks` | array of bytes32 | yes | Chunk IDs in order, each the hash of that chunk's bytes with domain `tessra:chunk` |

Invariants: the concatenation of the chunks hashes to `hash` and has length `size`. Which store holds the chunks, and the media type, are daemon configuration and tracking rules respectively, never part of the object.

### tree

Tag `tree`, `v` 1. A directory. Either flat or sharded, never both.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `entries` | array of map | one of | Flat form. Sorted by the UTF-8 bytes of `name`, ascending, unique. At most 4,096 entries |
| `shards` | array of bytes32 or null | one of | Sharded form. Exactly 256 slots. Slot i holds the tree of entries whose name hashes to first byte i, or null when empty. Present only when the directory has more than 4,096 entries |

Entry:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | text | yes | NFC, no `/` or `\`, not empty, not `.` or `..` |
| `kind` | uint | yes | 0 file, 1 dir, 2 symlink, 3 artifact, 4 conflict |
| `mode` | uint | for file | Bit 0: executable. Other bits MUST be zero |
| `ref` | bytes32 | for kinds 0 to 3 | Blob for file, tree for dir, blob holding the target path for symlink, artifact for artifact |
| `terms` | array of map | for kind 4 | See below |

Sharding is by `BLAKE3("tessra:shard" || name)[0]`. A shard tree is itself a flat tree and MUST NOT be sharded again; a shard with more than 4,096 entries is a directory with more than a million entries, and that is `OPEN:` with default reject. The threshold is fixed so that every directory has exactly one encoding.

A conflict entry records an unresolved merge at this name, following Jujutsu's model. Each term is `{ "sign": -1 or 1, "kind": uint, "mode": uint, "ref": bytes32 }`. The number of positive terms is one more than the number of negative terms. Resolution replaces the conflict entry with an ordinary entry.

Invariants: entries sorted and unique; exactly one of `entries` or `shards`; shard placement correct. The empty tree has a fixed ID. Directories with no entries are omitted from their parent; empty directories are not tracked.

### trackingrules

Tag `trackingrules`, `v` 1. Which paths are tracked and how content is normalized. Referenced by a snapshot, so versioned with the code.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `rules` | array of map | yes | Evaluated in order; last match wins, as gitignore does |
| `eol` | text | yes | `lf` normalizes line endings to LF on snapshot; `keep` stores bytes as found |
| `case` | text | yes | `sensitive`, or `check` to flag snapshots whose names collide case-insensitively |
| `portable_names` | bool | yes | When true, names Windows cannot materialize are flagged: reserved device names such as `CON` and `NUL` with or without an extension, trailing dots or spaces, and characters `<>:"|?*` |
| `artifact_threshold` | uint | no | Overrides the default |

Rule: `{ "pattern": text, "class": uint, "generator": text, "media": text }`. Class: 0 tracked, 1 ignored, 2 vendored, 3 generated, 4 artifact. `generator` is required for class 3 and names the tool or intent that produces the file. `media` optionally declares a media type for matching paths. Pattern syntax is gitignore's.

Nothing in tracking rules refuses a snapshot. Every check here produces a flag on the revision (see revision `flags`), and promotion is where flags block.

### environment

Tag `environment`, `v` 1. A fingerprint of where something ran or was built.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `os` | text | yes | |
| `arch` | text | yes | |
| `tools` | map text to text | yes | Toolchain name to version |
| `image` | text | no | Container image digest |
| `sandbox` | text | no | Sandbox level and spec, see 04-security.md. M3 records `L0` |
| `extra` | map | no | Adapter-defined. A snapshot's environment uses `state`, an array of `{ "path": text, "tree": bytes32, "files": uint }` capturing directories outside the tracked tree as content-addressed trees (build output, experiment data), and `locks`, a map of lockfile name to content hash. A checkout that asks for state materializes each `state` tree beside the files |

### snapshot

Tag `snapshot`, `v` 1. The complete content of one root at one moment. No parents; history lives in revisions.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `root` | bytes32 | yes | Tree |
| `rules` | bytes32 | yes | Trackingrules |
| `env` | bytes32 | no | Environment the snapshot was taken in, carrying its state trees. A landing carries the change's environment onto the landing revision so a trunk checkout can restore it |
| `index` | bytes32 | no | Nodeindex. A snapshot without one is valid; a reader computes one on demand without a parent and caches it |

Conflict entries under `root` are permitted; the snapshot is then conflicted, and 03-operations.md says what a conflicted revision may do.

### nodeindex

Tag `nodeindex`, `v` 1. The semantic index of one snapshot. It is derived from the snapshot's root tree, the grammar set, and the indexes of the revision's parent snapshots, in that order. Matching is history-dependent by definition, so it is not recomputable from the snapshot alone. A lost index is rebuilt by walking from the earliest indexed ancestor. When no ancestor index exists, every node gets a fresh ID.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `root` | bytes32 | yes | The root tree the index describes. The snapshot names its index, so the index cannot name the snapshot |
| `grammars` | text | yes | Identifier of the grammar set and matcher version |
| `parents` | array of bytes32 | yes | Nodeindexes matching was performed against. Empty when none existed |
| `nodes` | array of map | yes | Sorted by `path` then `span` start |
| `aliases` | map bytes16 to bytes16 | no | Node ID to the node ID it was merged into. Written by landing when two sides introduced the same node; readers resolve through it |

Node:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `nid` | bytes16 | yes | Stable node identity. The same unit across versions keeps its `nid` |
| `path` | text | yes | Root-relative path |
| `kind` | text | yes | Grammar-defined: `function`, `class`, `import`, `test`, `chunk`, and others. A function the language marks as a test has kind `test`: a `#[test]`-style attribute in Rust, a `test_` prefix in Python, `test` or `it` in JavaScript and TypeScript |
| `name` | text | yes | Qualified name within the file. Empty for chunks |
| `span` | array of two uint | yes | Byte range in the file's blob, end exclusive |
| `body` | bytes32 | yes | Body hash |
| `parent` | bytes16 | no | Enclosing node |
| `deps` | array of bytes16 | no | Nodes this one references, sorted: every unit of its own file that an identifier in it names, plus top-level units elsewhere in the root whose name is unambiguous. Computed when the file is indexed; a file unchanged from its parent keeps the deps it had |
| `setlike` | bool | no | True for regions that merge as sets |

The body hash is BLAKE3 with domain `tessra:nodebody` over the node's token stream: tokens as produced by the grammar including indentation tokens where the grammar emits them, comments removed, tokens joined by the byte 0x1F, literals verbatim, with two normalizations: a separator token directly before a closing bracket is dropped, so a formatter's trailing comma is not an edit, and the first occurrence of the unit's own name is dropped, so a pure rename keeps its hash and the matcher can follow it. Two nodes with the same body hash are the same content regardless of formatting. A container's tokens include its children's.

Files with no supported grammar are indexed as chunk nodes: one per run of lines between blank-line boundaries, `kind` `chunk`, `name` empty, body hash over the chunk's bytes with trailing whitespace stripped per line.

Node ID assignment across versions, per file against the parent index's nodes for the same path: match by `(parent, kind, name, ordinal among siblings with that kind and name)` first, then by body hash under the same parent, which is how a rename keeps its ID, then by structural similarity above a threshold, `OPEN:` the similarity algorithm with GumTree as the default. Unmatched nodes get fresh IDs. At landing, the merged index is matched against the head's index, then reconciled: where both sides introduced a node with equal `(path, kind, name)` or equal body hash, the lexically lower ID survives in `nodes` and the other is written to `aliases`; `parent` and `deps` references are rewritten to the survivor.

## Group B. History

### revision

Tag `revision`, `v` 1. Entity. A version of a change. The change ID is the entity ID.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | The change ID |
| `prev` | bytes32 | no | Previous revision of this change |
| `snapshots` | map text to bytes32 | yes | Root name to snapshot. Single-root repositories use root name `""`. At least one entry |
| `parents` | array of bytes32 | yes | Revisions this one is built on. Empty only for the first revision in a root |
| `intent` | bytes16 | no | |
| `title` | text | yes | One line |
| `body` | text | no | |
| `author` | bytes16 | yes | Principal of kind `session`, `human`, `hook`, or `daemon` |
| `time` | int | yes | |
| `ops` | array of map | no | Semantic operations recorded by `edit`: `{ "kind": text, "args": map }`. Defined: `rename` with args `from` and `to`, identifier names, and optional `path` scoping it to one file; without `path` it covered every tracked file with a grammar. Reserved: `move`, `extract`, `inline`, `add_param`, `add_import`, `reorder`. Advice to the merger; the snapshot is canonical |
| `flags` | map | no | Set by the daemon at snapshot time. See below |

Flags: `out_of_scope` (array of text paths touched outside the author's write scope), `secrets` (array of `{ "path": text, "pattern": text }`), `case_collisions` (array of arrays of text), `unportable_names` (array of text), `generated_drift` (array of text paths whose content differs from their generator's output). A snapshot with flags always succeeds. Promotion refuses while any flag is present that the stage's standard or the capability forbids, naming it.

Provenance of model and runtime is on the author's session principal, not on the revision. Cost is an attestation with the revision as subject.

Invariants: `prev`, when present, MUST have the same `id`. `parents` MUST be acyclic. A revision whose `id` is landed on any line MUST NOT be followed by a further revision of the same `id` except a restack, which changes only `parents`; correction is a new change.

### intent

Tag `intent`, `v` 1. Entity.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `title` | text | yes | |
| `body` | text | no | The specification in prose |
| `spec` | bytes32 | no | Blob with an attached specification |
| `evals` | array of bytes32 | no | Blobs or artifacts with evaluation definitions |
| `parent` | bytes16 | no | Enclosing intent |
| `depends` | array of bytes16 | no | Sorted |
| `priority` | int | yes | Higher is sooner |
| `status` | text | yes | `open`, `active`, `blocked`, `done`, `dropped` |
| `assignee` | bytes16 | no | Principal. A plan writes one sub-intent per unit group, `parent` the planned intent, `assignee` the agent's durable principal, and a task memory scoped to it |
| `created_by` | bytes16 | yes | Principal |
| `time` | int | yes | |
| `legacy` | bool | no | Synthesized from an imported commit message. No reputation weight |

Invariant: `depends` acyclic across current versions in the view.

### comment

Tag `comment`, `v` 1. Content-addressed. One review comment.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `subject` | bytes32 | yes | Revision |
| `scope` | map | no | `{ "nid": bytes16 }` or `{ "path": text, "span": [uint, uint] }` |
| `body` | text | yes | |
| `author` | bytes16 | yes | |
| `time` | int | yes | |
| `reply_to` | bytes32 | no | |
| `suggest` | bytes32 | no | A revision containing a suggested edit |

## Group C. Knowledge

### attestation

Tag `attestation`, `v` 1. Content-addressed, signed. A fact about an object or about content, produced by a verifier.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `kind` | text | yes | Namespaced: `tests.pass`, `tests.fail_on_parent`, `coverage.node`, `typecheck.clean`, `lint.clean`, `judge.<rubric>`, `approval.human`, `risk.change`, `risk.scope`, `cost`, `observe.<signal>`, `secrets.clean`, `deps.scan`, `memory.confirmed`, `memory.contradicted`, `deploy.live`, `deploy.failed`, and adapter-defined others |
| `subject` | bytes32 or bytes16 | one of | The object or entity this is about |
| `bodies` | array of bytes32 | one of | Node body hashes this is about, sorted. Node-scoped kinds use this and transfer to any snapshot containing nodes with these body hashes |
| `subject_type` | text | with subject | Type tag of the subject |
| `scope` | map | no | `nodes` (array of bytes16), `paths` (array of text), `tests` (array of text). Informational |
| `result` | any | yes | Bool, integer, or map. Ratios are per-mille uints |
| `env` | bytes32 | no | Environment the verification ran in |
| `verifier` | bytes16 | yes | Principal that produced it |
| `runner` | bytes16 | no | Daemon runner or channel principal that observed and signed when the verifier holds no key |
| `evidence` | bytes32 | no | Blob or artifact with logs |
| `time` | int | yes | |
| `sigkind` | text | yes | `ed25519` or `webauthn` |
| `sig` | bytes | yes | 64 bytes for ed25519; the CBOR-encoded WebAuthn assertion otherwise |

Exactly one of `subject` or `bodies` is present. The cache key of an attestation is `(kind, subject or bodies, env)`. An attestation keyed on `bodies` applies to every snapshot whose nodeindex contains nodes with all of those body hashes; that is how a test result on a change survives the landing merge. Kinds that are inherently whole-snapshot, such as a build or an integration suite, use `subject`. A standard reading attestations for revision R accepts `subject` = R, R's `prev`, or any snapshot of R.

Kinds with defined shapes: `tests.pass` has `result` bool and `scope` `tests` (the names that ran), `failed`, `passed`, and `selected`; a whole-suite run names the snapshot as `subject`, and a run selected by the covering-tests relation names by `bodies` the changed units and the tests that ran. `tests.fail_on_parent` names the new tests by `bodies`, is true when every one failed on the parent snapshot, and carries `scope.passed_on_parent` otherwise. `test.result` is one test at one state of the units it depends on transitively, by `bodies`, with `scope` `test` and `fingerprint`. `risk.change` names the revision as `subject` with `result` `{ "score": uint, "level": text, "factors": [{ "name", "points", "detail", "lower" }] }` and `scope.level`. `judge.<rubric>` is a judge session's verdict on a proposed revision: `subject` the revision, `result` `{ "ok": bool, "confidence": permille, "reasoning": text }`. `approval.human` is a human's answer to a request: `subject` the revision, `result` bool, `scope` `request` (the question memory's ID), `nonce`, and `note`; key-signed, so no `runner`.

Who may attest what is the standard's call. An `attest` clause counts an attestation only when its signer, `runner` when present else `verifier`, is a principal of kind `verifier`, `daemon`, `hook`, `external`, `human`, `observer`, or `deployer`. A `session` or `agent` may record attestations; they never satisfy a clause unless the clause says `self`.

Invariants: `sig` verifies under `runner`'s key when `runner` is present, otherwise under `verifier`'s key. A key-signed human approval has no `runner`; a channel-attested one has the channel principal as `runner`. Attestations are never versioned or deleted; a superseding fact is a new attestation.

### memory

Tag `memory`, `v` 1. Entity. Semantics in MEMORY.md.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `kind` | text | yes | `fact`, `decision`, `convention`, `gotcha`, `preference`, `task`, `question`, `summary`, `resolution` |
| `scope` | map | yes | `{ "kind": "node"|"path"|"intent"|"principal"|"root"|"repo", "ref": bytes16 or text }` |
| `body` | text | yes | |
| `anchor` | bytes32 | no | Body hash of the node, or tree ID of the path, the memory describes |
| `confidence` | uint | yes | Per mille |
| `author` | bytes16 | yes | |
| `time` | int | yes | |
| `expires` | int | no | Advisory; readers rank expired memories last |
| `proposed` | bool | no | Auto-captured and not yet confirmed |
| `status` | text | yes | `active`, `stale`, `retired`, `superseded` |
| `links` | array of bytes32 | no | |
| `visibility` | text | yes | `shared` or `private`. Private memories are readable only by their author and sync only to replicas serving the author |

Trust is expressed by attestations whose subject is the memory's entity ID. A `resolution` memory stores a recorded conflict resolution keyed by the body hashes of the conflicting terms.

### standard

Tag `standard`, `v` 1. Entity. A deterministic predicate over attestations and the graph, gating a stage or a target. Determinism is a requirement: given the same view and the same set of attestations, every replica computes the same result, because landing ops are re-checked everywhere (B5).

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `name` | text | yes | |
| `extends` | bytes16 | no | |
| `clauses` | array of clause | yes | |
| `scopes` | array of map | no | `{ "pattern": text, "clauses": array of clause, "only": bool }` |
| `when` | array of map | no | `{ "risk_at_least": text, "clauses": array of clause }`. Risk is read from the latest `risk.change` attestation on the revision |
| `audit_permille` | uint | no | |

Clause: `{ "op": "require"|"forbid", "pred": predicate, "unless": predicate }`.

Predicate: `{ "kind": text, "name": text, "args": map }`, written and shown as `kind(name, key=value, ...)`. Kinds: `attest` (an attestation of kind `name` that counts per the trust rule above: without `for`, one on the revision, its `prev`, or a snapshot, or by `bodies` covering every changed unit; `for=new_tests` requires bodies covering every new test unit; `for=changed_nodes` every changed unit; `self=true` accepts any signer; `env.sandbox_at_least` and `verifier_kind` are reserved), `judge` (counts one `judge.<name>` verdict per judge agent whose parent differs from the author's; needs `judges` agreeing ones at or above `min_confidence`, with distinct session `model` fields when `distinct_models`; a single negative verdict is unmet with its reasoning quoted, and the change becomes an exception), `approved` (`by=human` only, an `approval.human` attestation on the revision signed by a principal of kind `human`; `keysigned` requires no `runner`; a declined answer is unmet as such), `reputation` (`min`; not built), `observe` (the latest trusted `observe.<name>` attestation on the revision, in permille, against `max` and `min`; unmet without one), `structural` (`name` in `flags.none`, `intent.linked`, `changed.covered` (every changed unit is named in the `deps` of a `test` unit), `test.modified`, `test.deleted`, `test.weakened`, `test.skipped`; reserved: `touches_only_claimed`, `blast_radius_max`, `size_max`, `new_dependency`), `all`, `any`, `not`.

`when`: each block's clauses apply when the revision's risk level is at least `risk_at_least`, read from the latest trusted `risk.change` attestation on the revision; levels are `low`, `medium`, `high`, `critical`. An unmet clause under `when` reports the level, the score, and the top factors with what would lower each. Without a risk attestation the block is unmet. `scopes` and `audit_permille` are stored and evaluated from M5.

Invariant: `extends` acyclic. Predicate evaluation reads only the view, the revision, its snapshots and indexes, and attestations cited by the op being verified or present in the store; it never reads a clock.

## Group D. Coordination

### claim

Tag `claim`, `v` 1. Entity. Short-lived and replicated, so every agent on every replica sees who is working where. A `claim` answers with every active claim of another principal whose paths overlap the new one, so the agent learns a merge is coming before it edits.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `principal` | bytes16 | yes | |
| `targets` | array of map | yes | `{ "kind": "node"|"path"|"root", "ref": bytes16 or text }` |
| `intent` | bytes16 | no | |
| `exclusive` | bool | yes | Advisory unless a standard requires it |
| `expires` | int | yes | Readers ignore expired claims. The hosting daemon retires them with an `unclaim` op on a cadence. The view never drops them by time |
| `note` | text | no | |

### workspace

Tag `workspace`, `v` 1. Local to a daemon. Never enters the object store, never hashed into a view, never synced. The shape is specified so tools and the daemon agree.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `principal` | bytes16 | yes | Owner |
| `base` | bytes32 | yes | Revision the workspace was created from |
| `current` | bytes32 | no | Revision holding the latest snapshot of the working copy |
| `paths` | map text to text | yes | Root name to absolute path on this machine |
| `env` | bytes32 | no | |
| `created` | int | yes | |
| `expires` | int | no | |

### principal

Tag `principal`, `v` 1. Entity. Signed. Anyone or anything that can perform an op or sign an attestation. Agent identity is two-level: a durable `agent` principal per distinct runtime and configuration, and a short-lived `session` principal per run, whose `parent` is the agent and whose key the daemon holds. Ops are signed by sessions; blame, reputation, and recall by model roll up to the parent.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `key` | bytes32 | yes unless revoked | Current Ed25519 public key |
| `kind` | text | yes | `human`, `agent`, `session`, `verifier`, `hook`, `deployer`, `observer`, `external`, `daemon` |
| `name` | text | yes | |
| `parent` | bytes16 | for session | The agent this session belongs to. For other kinds, the principal that created this one |
| `model` | text | for session | Model identifier for this run |
| `runtime` | map | for agent and session | `framework`, `version`, `config` (a hash), adapter-defined others |
| `bindings` | array of map | no | `{ "provider": text, "subject": text, "proof": bytes }` for GitHub, OIDC, Sigstore, and WebAuthn credentials |
| `status` | text | yes | `active`, `revoked` |
| `expires` | int | for session | Enforced by the daemon that holds the key. Not checked by replicas |
| `time` | int | yes | |
| `sig` | bytes64 | yes | First version of a `session`: by the parent agent's key. First version of any other kind: by an owner's key, except the daemon at init, which is self-signed. Rotation: by the previous key. Revocation: by an owner or a holder of `revoke` |

Invariant: a `session` MUST have a `parent` of kind `agent`. A revision's `author` MUST NOT be of kind `agent`; it is the session.

An `external` principal, such as a CI system, is created by an owner with `grant --external <name>`, with a capability whose only verb is `attest`; when the daemon created it, the daemon holds its key and acts as it on request. A `human` principal is created the same way with `grant --human <name>` until a passkey binding exists; only a principal of kind `human` may attest `approval.human`. A `verifier` principal is created by the daemon the first time a tool runs, with a key that never signs: the daemon signs the tool's attestations as `runner`.

### capability

Tag `capability`, `v` 1. Content-addressed, signed. A grant of verbs over a write scope and a read scope to a principal, with limits.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `issuer` | bytes16 | yes | |
| `subject` | bytes16 | yes | |
| `verbs` | array of text | yes | Sorted. From the verb set plus administrative verbs including `attest` |
| `write` | map | yes | `roots`, `paths` (patterns), `nodes`, `lines`, `targets`, `stages`, `memory_scopes`. Effects are checked against this at promotion, landing, deployment, and standard or hook changes |
| `read` | map | yes | `roots`, `paths` (patterns), `memory`: `own`, `shared`, or `all`. Enforced by the daemon on every `status`, `context`, and `query`, and by the sync filter |
| `hard` | map | no | Limits the daemon meters itself: `ops`, `workspaces`, `agents`, `landings_per_hour`, `verifier_ms`. Enforced at signing and by the coordinator |
| `advisory` | map | no | Limits that depend on self-reported data: `tokens`, `micros`, `expires`. Enforced at signing by the issuing daemon when it trusts the reporter; never by replicas |
| `delegable` | bool | yes | |
| `parent` | bytes32 | no | The capability this one was delegated from. A planner's session holding a delegable capability issues one per assignee session: `issuer` the planner, `subject` the assignee's session, verbs its own minus `plan`, `grant`, and `revoke`, `write.paths` the group's paths, `hard.ops` the budget, signed by the planner. The daemon shapes the default capability of a session from the owner's standing grant to the agent (`grant --to`: extra verbs, paths, `delegable`, `ops`) and from the planner's assignment (paths, `ops`) |
| `nonce` | bytes16 | yes | |
| `time` | int | yes | |
| `sig` | bytes64 | yes | By `issuer`'s key |

Invariants: with `parent`, `issuer` equals the parent's `subject`, the parent is `delegable`, and `verbs`, `write`, `read`, and every limit are contained in the parent's. A chain terminates at a capability whose `issuer` is an owner.

### hook

Tag `hook`, `v` 1. Entity.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `name` | text | yes | |
| `on` | text | yes | Event pattern: an event name, `family.*`, or `*`. Events in M3: `proposed`, `landed`, `conflict.opened`; the rest of HOOKS.md as built |
| `where` | predicate | no | Over the event: `scope(glob)` against the changed paths, `author(prefix)`, `title(text)`, `event(pattern)`, and `all`, `any`, `not` |
| `do` | array of map | yes | `{ "kind": text, "args": map }`: `webhook` (POST the event as JSON, `X-Tessra-Signature` by the principal over the body), `run` (a local command with the event on stdin and in `TESSRA_EVENT`; machine-local), `verify`, `attest`, `remember` and `task`, `land` (land the event's revision now as the coordinator if the standard holds: the continuous frontier), `agent` (`name`, `intent`, `budget`: record an intent assigned to the agent and an assignment scoped to the event's paths, then start the configured `agent_runner` detached with the event in its environment), `notify` (`channel`, `text`: deliver the event to one channel or all), and reserved `promote`, `revert`, `question`, `claim`, `release`, `pause`, `resume`, `revoke` |
| `as` | bytes16 | yes | Principal the actions run as. The daemon in M3 |
| `enabled` | bool | yes | |
| `order` | int | no | |

Invariant (B4): the op that creates or updates a hook MUST be authored by `as`, or by an owner, or by a principal holding a delegable capability that contains everything `as` holds. Default grants never include `hook`.

### channel

Tag `channel`, `v` 1. Entity.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `kind` | text | yes | `chat`, `email`, `terminal`, `push`, `webhook`, and `inbox` (a directory requests are written to as JSON). M5 builds `inbox` and `webhook` |
| `config` | map | yes | `name`, and per kind `path` or `url`. Secrets are vault references |
| `principals` | array of bytes16 | yes | Humans reachable through it |
| `signing` | text | yes | `key` or `channel` |
| `time` | int | yes | |

### manifest

Tag `manifest`, `v` 1. Content-addressed, signed. A replica's declaration of which principals it serves, presented at sync so the sending side can apply read scopes and private-memory visibility.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `replica` | bytes16 | yes | The daemon principal |
| `serves` | array of bytes16 | yes | Principals with sessions on this replica, sorted |
| `time` | int | yes | |
| `sig` | bytes64 | yes | By the daemon's key |

A sender trusts a manifest only from a daemon principal it knows, and sends private memories and read-scoped objects only for principals the manifest names.

## Group E. Delivery

### line

Tag `line`, `v` 1. Entity. The head, the landing counter, and the coordinator live in the view.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `name` | text | yes | `trunk` is created by init |
| `standard` | bytes16 | yes | |
| `roots` | array of text | yes | |
| `shared` | bool | yes | Shared lines are advanced only by their coordinator |
| `created` | int | yes | |

### release

Tag `release`, `v` 1. Content-addressed, signed.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | text | yes | |
| `line` | bytes16 | yes | |
| `revision` | bytes32 | yes | |
| `since` | bytes32 | no | |
| `changelog` | bytes32 | yes | Blob |
| `attestations` | array of bytes32 | no | |
| `signer` | bytes16 | yes | |
| `time` | int | yes | |
| `sig` | bytes64 | yes | |

### target

Tag `target`, `v` 1. Entity.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | bytes16 | yes | |
| `prev` | bytes32 | no | |
| `name` | text | yes | |
| `deployer` | map | yes | `kind` `dir` with `path` (the snapshot is copied to `<path>/<slice>`) or `run` with `cmd` (run with `TESSRA_TARGET`, `TESSRA_SLICE`, `TESSRA_PERCENT`, `TESSRA_REVISION`, `TESSRA_SNAPSHOT_DIR`, `TESSRA_STEP`); `canary_percent`; `secrets`, vault references `vault:local:<NAME>` resolved from machine-local configuration into the command's environment only. Platform adapters are not built |
| `standard` | bytes16 | yes | The target's release standard, created with the target, extending the line's. Its own clauses gate the `all` slice; a canary is gated by what it extends |
| `line` | bytes16 | no | |
| `observers` | array of map | no | `kind` `run` with `cmd`: a command printing a JSON object of numeric signals, each recorded as an `observe.<name>` attestation in permille on the deployed revision |
| `risk_budget` | map | no | `capacity`, `refill_per_hour` |
| `sensitivity` | text | no | |

### deployment

Tag `deployment`, `v` 1. Content-addressed.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `target` | bytes16 | yes | |
| `revision` | bytes32 | yes | |
| `release` | bytes32 | no | |
| `principal` | bytes16 | yes | |
| `previous` | bytes32 | no | |
| `step` | map | no | `{ "percent": uint, "of": "canary" | "all", "kind": "deploy" | "rollback" }`. `previous` chains deployments of the target; a rollback goes to the previous deployment of another revision |
| `time` | int | yes | |

Status is an attestation stream on the deployment.

## Group F. Log

### trie

Tag `trie`, `v` 1. Content-addressed. One node of a persistent hash array mapped trie. The view's large maps are tries so that an op writes O(log N) nodes instead of the whole map (B1).

| Key | Type | Required | Meaning |
|---|---|---|---|
| `bitmap` | uint | yes | 16 bits. Bit i set means slot i is present |
| `slots` | array | yes | One entry per set bit, in ascending bit order. Each is `{ "leaf": [key, value] }` or `{ "node": bytes32 }` |

Keys are bytes16 or bytes32; the trie consumes one nibble of the key per level starting from the first nibble. Canonical form: a subtree holding exactly one key is stored as a leaf in its parent, never as a node; a node exists only where two or more keys share the prefix to that depth. The empty trie is a node with `bitmap` 0 and no slots, and has a fixed ID. Values are bytes32, a conflict value, or `true` for sets. Two tries with the same contents have the same root ID.

Merge of two tries against a base is structural: at each node, a slot equal to the base on one side takes the other side's slot wholesale, so the cost is proportional to the differences, and per-key three-way rules from 03-operations.md apply at the leaves.

### op

Tag `op`, `v` 1. Content-addressed, signed. One mutation of the repository. Semantics in 03-operations.md.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `parents` | array of bytes32 | yes | Empty only for the init op |
| `author` | bytes16 | yes | |
| `key` | bytes32 | yes | The public key used. MUST match the author's key at the parents |
| `cap` | bytes32 | no | Required unless `author` is an owner |
| `kind` | text | yes | A verb, an administrative verb, or `init`, `sync`, `merge_view`, `land`, `restack`, `redact`, `contain` |
| `args` | map | yes | The call as made, with content referenced by object ID rather than inlined. MUST contain `idem` (bytes16) except for `init` and `sync` |
| `effects` | array of map | yes | See 03-operations.md |
| `view` | bytes32 | yes | The view after this op |
| `time` | int | yes | |
| `desc` | text | no | |
| `sig` | bytes64 | yes | |

### view

Tag `view`, `v` 1. Content-addressed. The mutable state of the repository at one op. Every op points at the view it produced. Nothing local to a machine is in it.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `entities` | bytes32 | yes | Trie root: entity ID to current version object ID, or a conflict value |
| `proposed` | bytes32 | yes | Trie root: set of revision IDs at the proposed stage |
| `claims` | bytes32 | yes | Trie root: claim entity ID to current version |
| `caps` | bytes32 | yes | Trie root: set of active capability IDs |
| `revoked` | bytes32 | yes | Trie root: set of principal IDs. Never shrinks |
| `tombstoned` | bytes32 | yes | Trie root: set of object IDs. Never shrinks |
| `lines` | map text to map | yes | Line name to `{ "id": bytes16, "head": bytes32 or conflict, "seq": uint, "coordinator": bytes16 }` |
| `roots` | array of text | yes | Sorted |
| `owners` | array of bytes16 | yes | Sorted. The daemon principal that performed init is always among them |
| `deployed` | map bytes16 to bytes32 | yes | Target to current deployment |
| `paused` | array of map | no | `{ "scope": text, "by": bytes16, "op": bytes32 }`. `op` is the first parent of the op that paused, since an op cannot name its own ID inside the view it produces |

A conflict value is `{ "conflict": array of bytes32 }`, sorted.

### tombstone

Tag `tombstone`, `v` 1. Content-addressed. Replaces a redacted object in the store.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `of` | bytes32 | yes | |
| `type` | text | yes | |
| `reason` | text | yes | |
| `op` | bytes32 | yes | The redaction op |
| `time` | int | yes | |

### anchor

Tag `anchor`, `v` 1. Content-addressed, signed.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `heads` | array of bytes32 | yes | Sorted |
| `by` | bytes16 | yes | |
| `time` | int | yes | |
| `sig` | bytes64 | yes | |

## Type index

| Tag | Group | Identity | Signed |
|---|---|---|---|
| blob | A | content | |
| artifact | A | content | |
| tree | A | content | |
| trackingrules | A | content | |
| environment | A | content | |
| snapshot | A | content | |
| nodeindex | A | content, history-derived | |
| revision | B | entity | |
| intent | B | entity | |
| comment | B | content | |
| attestation | C | content | yes |
| memory | C | entity | |
| standard | C | entity | |
| claim | D | entity, short-lived | |
| workspace | D | local to a daemon | |
| principal | D | entity | yes |
| capability | D | content | yes |
| hook | D | entity | |
| channel | D | entity | |
| manifest | D | content | yes |
| line | E | entity | |
| release | E | content | yes |
| target | E | entity | |
| deployment | E | content | |
| trie | F | content | |
| op | F | content | yes |
| view | F | content | |
| tombstone | F | content | |
| anchor | F | content | yes |
