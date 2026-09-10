# Review 1 of the M0 specification

Antagonistic pass over draft 1, 2026-09-04. The goal was to break it: find contradictions, security holes, things that cannot scale, and places where the spec quietly violates a principle from the design documents. Findings are ranked. Blockers must be fixed before M1 writes code against the format. Each finding names the section, the defect, why it matters, and a proposed fix.

## Blockers

### B1. The view rewrites O(N) state on every op

02 `view`, 03. The view holds a pointer for every entity: every change, memory, intent, principal. A repository with a million memories and revisions has a view of about fifty megabytes, and every op writes a new one. Hundreds of landings per hour, the M4 target, is impossible on this format. Jujutsu survives the same design only because its view holds branches, not everything.

**Fix.** Make `entities` a persistent hash array mapped trie of view nodes, each content-addressed, so an op writes O(log N) nodes and the view object holds the trie root. Same for `proposed` and `caps`. The merge algorithm operates per key exactly as written; only the storage shape changes. This must be in the format from M1.

### B2. Ephemeral state inside the hashed view breaks verification on replicas

02 `view.ephemeral`, 03 sync step 5, 04 step 8. The op's `view` field is the hash of the full view including workspaces and claims. Sync strips ephemeral state. A replica therefore cannot recompute the view and cannot perform verification step 8. Every op fails determinism on every peer.

**Fix.** Remove `ephemeral` from the view entirely. Workspaces and claims are daemon-local state with their own file, never hashed into an op. Claims that need to be visible to other replicas, meaning all of them, are published as short-lived shared objects through a `claim` op whose effect is a set membership with expiry, and the set lives in the view under the persistent-trie fix from B1.

### B3. Revocation and key rotation can be bypassed by choosing stale parents

04 step 3, 03 DAG. Verification checks the author's status and key against the merged view of the op's parents. A revoked principal, or a thief with a rotated-out key, writes an op whose parents predate the revocation. Every replica verifies it against the old view and accepts it. Revocation is therefore advisory.

**Fix.** Step 3 checks the parents' view and additionally the receiving replica's current merged view: an author present in current `revoked`, or whose key in the current view differs from the op's `key` with no rotation op on the path between the op's parents and the current heads, is rejected. This is not deterministic across replicas during the propagation window, which is unavoidable, and it is monotone: once the revocation reaches a replica, nothing further from that principal is accepted there. Containment on the revoke event unwinds what slipped through.

### B4. Hooks are a privilege escalation

02 `hook.as`, 04. Any principal holding the `hook` verb can create a hook whose `as` is an owner. The hook's actions then run with owner authority. An agent with a task-scoped capability that includes `hook` becomes an owner on the next event.

**Fix.** Invariant on hook creation and update: the op author MUST be the `as` principal, or an owner, or hold a delegable capability that contains everything the `as` principal holds. Default grants never include `hook`.

### B5. The hub cannot be both untrusted storage and the coordinator that evaluates standards

03 coordination, 03 sync. The hub emits `head` effects for shared lines, which means it evaluates standards and decides what lands. It is also described as untrusted storage that cannot forge anything. A hub that evaluates standards can land a change that violates them, and no replica checks.

**Fix.** A `head` effect MUST cite the attestation object IDs that satisfy the line's standard, and verification step 7 evaluates the line's standard deterministically against those attestations. The hub then holds only the serialization role: it can withhold and reorder, which anchoring detects, but it cannot land what the standard forbids, because every replica re-evaluates. This requires attestations to be fetched eagerly for landing ops, and it requires the standard predicate language to be deterministic, which is a constraint to carry into M3.

### B6. Time-based expiry contradicts deterministic verification

01 time, 04 step 3, 02 `principal.expires`, `capability.limits.expires`. Timestamps are declared informational, then verification compares the op's self-reported `time` to `expires`. An author can lie about `time`, and honest clock skew rejects an op along with every descendant forever, per sync step 3.

**Fix.** For principals whose keys the daemon holds, which is every agent, expiry is enforced by the signing daemon: it refuses to sign after expiry, and replicas do not check it. For humans and externals, there is no time expiry; there is revocation. Capability `limits.expires` becomes advisory to the issuing daemon in the same way. Remove the time comparison from the replica-side algorithm.

### B7. The node index cannot be both derived and stable

02 `nodeindex`. Node IDs are random on first sight, and the index is declared recomputable from the snapshot and the grammar set. Recomputing from scratch produces different IDs. Worse, two agents adding the same function concurrently assign different IDs to what becomes one node after merge, and claims, memories, and coverage attached to either ID dangle.

**Fix.** The index is derived from the snapshot, the grammar set, and the indexes of the revision's parents, in that order; matching is history-dependent by definition. State that a lost index is rebuilt by walking from the earliest indexed ancestor. At landing, the merger reconciles IDs: when two sides introduce nodes with equal `(path, kind, name)` or equal body hash, the lower ID wins and the other is recorded as an alias in the landing revision's index, and readers resolve aliases. Add `aliases` to the nodeindex.

### B8. Snapshot blocks in three places, against principle 3

04 capability check, 04 secrets, 02 `trackingrules.case`. Design principle: nothing blocks, snapshot never blocks. The spec refuses a snapshot when the capability scope does not cover a touched path, when the secrets scanner matches, and when names collide case-insensitively.

**Fix.** Snapshot always succeeds within the author's own workspace and records flags: `out_of_scope` paths, `secrets` matches, `case_collision` names. Enforcement moves to `promote`, which refuses with the flags named, and to sync, which never sends a revision carrying `secrets` flags. Capability scope is checked on `promote` and `head` effects, not on the `point` that a snapshot performs. This also resolves the tension that any tool may write files into a workspace.

### B9. Artifact identity includes non-content hints

02 `artifact`. The `store` and `media` fields are part of the hashed pointer. The same content stored through a different adapter yields a different artifact ID and therefore a different tree and snapshot. Two agents with the same file disagree on the tree hash.

**Fix.** The artifact object is `hash`, `size`, `chunks` only. Store hints live in daemon config; media type lives in tracking rules or is sniffed.

### B10. Unknown-key rejection contradicts optional-field additions

01 encoding, 01 schema versions. Validators for a known version MUST reject unknown keys. Adding an optional field does not bump the version. A newer writer's object is rejected by every older validator.

**Fix.** Pick one. Recommended: validators ignore unknown keys and preserve them byte-for-byte; `v` bumps only on incompatible change. The hash covers the bytes regardless, so integrity is unaffected.

### B11. Private keys in the repository directory, which is under cloud sync

05 layout. `.tessra/keys/` sits inside the repository. This project's repositories live under OneDrive. The daemon's and runner's private keys would be uploaded to a cloud account on first run. The embedded store under a syncing folder is also a corruption risk under concurrent access.

**Fix.** Keys live in the OS keychain or under the user's local application data directory, never inside the repository. `.tessra/` holds a pointer to the store location, and the default store location is outside any cloud-synced path, with an explicit warning when the repository itself is inside one. This is an M1 requirement on this machine.

### B12. The verification cache mostly misses at landing

02 `attestation.subject`, 03 landing step 5. Attestations are keyed by snapshot. Landing merges the change onto the head, producing a new snapshot, so every attestation on the change misses and every verifier and judge re-runs at every landing. The design's claim that verification is paid once is false exactly where it matters.

**Fix.** Node-scoped attestation kinds, meaning tests, coverage, lint, and judges over changed nodes, key on the body hashes of the nodes they cover, carried in `scope.bodies`, and transfer to any snapshot whose nodes have those body hashes. Snapshot-scoped kinds, meaning build and integration tests, still key on the snapshot. The landing algorithm reuses transferred attestations and runs only what the merge invalidated.

### B13. Budgets cannot be enforced on self-reported cost

04 step 6. Token budgets are enforced by summing cost attestations, which the agent runtime reports about itself. A runtime under budget pressure stops reporting.

**Fix.** The daemon meters what it observes: ops, workspaces, landings, agents spawned, verifier runtime. Those limits are hard. Token limits are enforced only when the reporting runtime is a principal with its own reputation, and are otherwise advisory. Say so in the capability's `limits` documentation.

### B14. There is no read access control

04 capability check. Every principal may `context` and `query` without a grant. On a hub with several humans, every fresh agent reads everything, including other principals' private memories, since the sync filter is defined by daemon, not by principal.

**Fix.** Add `read` to capability scope: roots, paths, and memory visibility the principal may read. Default grants include read over the assignment's scope plus shared memory. Sync becomes principal-aware: a replica declares the principals it serves, and the sending side filters by their read scopes.

### B15. Agent principals are per session, so reputation and blame point at nothing durable

02 `principal.expires`, 04 keys. Agent principals are minted per session with a fresh entity ID. Blame names a session. Reputation accrues to a session. Recall by model works only through the free-text `model` field.

**Fix.** Two levels. A durable `agent` principal per distinct runtime and configuration, long-lived, with `model` and `runtime` as fields, and a short-lived `session` principal whose `parent` is the durable agent and whose key the daemon holds. Ops are signed by the session; blame, reputation, and recall roll up to the parent.

## Should fix in draft 2

- **S1. The coordinator is not in the view.** 03 says only the coordinator emits `head` effects, but nothing records who that is. Add `coordinator` (principal) to each line entry in the view.
- **S2. Missing `attest` verb.** Verifiers, judges, and external systems write attestations, and no verb grants it. Add `attest` to the administrative set and to the default grant for verifier kinds.
- **S3. Restack after landing is unspecified.** When the bottom of a stack lands, the next revision's `parents` still name the unlanded revision. Specify that the coordinator emits a restack effect rewriting `parents` to the landing revision, under the daemon principal, and that it is a `point` on the dependent change.
- **S4. Multi-root ancestry.** Landing computes a latest common ancestor per root through `parents`, but a revision's parents may not carry every root. Define per-root ancestry: the ancestor for root R is found by following `parents` until a revision with a snapshot for R.
- **S5. Trees above the size limit.** "Split into subtrees" is undefined and would invent directories. Either raise the limit to 256 MiB or define hashed sharding of a directory's entries into a `tree` of `treeshard` objects with a deterministic fan-out.
- **S6. Merging more than two heads.** Define the order: fold heads pairwise in ascending op ID, each step against the pair's latest common ancestor.
- **S7. Garbage collection roots are ambiguous.** Ops are never collected and every op lists its `put` objects, so nothing is ever unreachable. Define roots as the current merged view, every line's history through `parents`, tombstones, and the retention windows; `put` lists are not roots.
- **S8. Lazy fetch versus verification.** Step 5 needs the revision, snapshots, and trees to check scope, so those are not lazy. State which objects verification requires eagerly.
- **S9. Passkey binding.** Specify that the WebAuthn challenge is the BLAKE3 of the attestation payload and that the verifier parses `clientDataJSON` to compare. Also, a key-signed approval from a hardware Ed25519 key is indistinguishable from a channel-attested one by `sigkind`; make the distinguishing field the absence of `runner`.
- **S10. Windows names.** Add reserved names, trailing dots and spaces, and backslashes to the tracking-rules validation, since materialization on Windows fails on them.
- **S11. Sync privacy needs principal identity.** Folded into B14.
- **S12. Cost appears twice.** On the revision and as an attestation kind. Keep the attestation, drop the revision field.
- **S13. Conflict detection by marker is a heuristic.** Make the sidecar the source of truth: resolved when the sidecar is removed.
- **S14. The colocated git checkout is nobody's workspace.** Define it as a workspace owned by the daemon's human principal, snapshotted on git commit.
- **S15. ProjFS in M1 is optimistic.** It needs an optional Windows feature, and Dev Drive needs a ReFS volume that OneDrive folders cannot be. Default M1 to hard-link farms with copy-on-write when the volume supports it, and ProjFS in M4.
- **S16. M1 needs a minimal query.** The M1 demo recalls a memory from another tool, which needs at least `query` over memories by scope. Specify that subset.
- **S17. Signature strictness.** Require strict Ed25519 verification that rejects non-canonical signatures, or the same payload has multiple valid signatures and therefore multiple op IDs.
- **S18. Conflicted revisions and stages.** State that a conflicted revision may be proposed, may not be verified, and may never land.
- **S19. Undo and revocation.** Containment is described as undoable by an owner, and `revoked` is never removed. Say that undo restores everything except the revocation itself.
- **S20. The daemon is an owner.** Containment ops need owner authority. State plainly that the daemon principal is in `owners` and is the trusted computing base.
- **S21. Import must be lazy.** Importing a large history eagerly creates millions of snapshots. Import the head fully and ancestors on demand.
- **S22. The op's `args` can carry content.** An `edit` with a large patch inlines it in the op. Reference blobs instead.

## Accepted weaknesses, to document rather than fix

- **A1.** `model` and `runtime` on a revision are self-reported. A standard's `distinct_models` trusts them. Verifiable model attestation from a runtime is out of scope until runtimes can sign.
- **A2.** The idempotency index expires after thirty days; a retry after that re-executes.
- **A3.** L1 sandboxing on a shared machine does not stop a verifier from reading other agents' workspaces.
- **A4.** Entity IDs use the same alphabet as Jujutsu change IDs, which is confusable in a repository colocated with both.

## What the review did not find

No problem with content addressing, the entity pattern, conflict entries in trees, effects as compare-and-set, the verb set, the manual, or the pack format. The op DAG with per-op views is the right shape; the blockers are about what goes in the view, what verification checks, and who is trusted to land.
