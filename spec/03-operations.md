# 03. Operations

Draft 8. Changes from draft 7, all from building M6: delivery has its own section.

Draft 7. Changes from draft 6, all from building M5: speculation, approval requests and the exception queue, and the anomaly path have their own sections.

Draft 6. Changes from draft 5, all from building M4: the frontier and restack as built, landing clears every proposed revision of the change, and `revert` has its own section.

Draft 5. Changes from draft 4, all from building M3: proposing is not gated by the standard; the risk attestation is recorded before promotion; landing steps 6 to 8 say what is collected, what is rerun, and what is cited; verification and hooks have their own sections.

Draft 4. Changes from draft 3, all from building M2: landing step 2 is written out at unit granularity, with renames as the first operation the merge composes and carries into the other side; step 3 rewrites references to the surviving ID; the landing revision keeps the change's author; concurrent additions keep landing order.

Draft 3. Changes from draft 2, all from building M1: the `head` effect creates a line when it carries the line's entity ID; entity-map merge lets a removal win over a concurrent change; a conflicting landing points the change at the conflicted revision so resolution is a plain snapshot; undo rolls up to the durable agent and restores the workspace; the local daemon is trunk's coordinator until a hub exists.

Draft 2. Changes from draft 1: the view holds tries and no local state (B1, B2); landing cites attestations and every replica re-evaluates the standard (B5); claims replicate and expire by read, never by view (B2, B6); node IDs are reconciled at landing (B7); flags block at promotion, not at snapshot (B8); transferred attestations make landing cheap (B12); the coordinator is recorded per line (S1); restack is specified (S3); per-root ancestry (S4); merge order for many heads (S6); garbage collection roots (S7); which objects verification needs eagerly (S8); conflicted revisions and stages (S18); undo and revocation (S19).

The operation log is the single source of mutation. Every change to the repository's state is an op. Ops form a DAG. Each op points at the view it produced, and the current state of the repository is the merge of the views at the DAG's heads.

## The DAG

The first op is `init`, with no parents. Every later op names one or more parent ops: the heads the writer knew about. Two writers that do not know about each other produce two heads. That is normal. Nothing rewrites or removes an op; undo and redaction are new ops.

An op enters a store only after the verification algorithm in 04-security.md. Verification uses the merged view of the op's parents for everything except revocation and rotation, which are also checked against the receiving replica's current view.

## Effects

An op's `effects` list what it did, in order. Applying them to the merged parents' view MUST produce the op's `view`, and every replica recomputes this.

| Effect | Fields | Meaning |
|---|---|---|
| `put` | `id` | An object was written. Listed for fetching, not for the view and not as a garbage collection root |
| `point` | `entity`, `to`, `from` | The entity's current version moved. `from` MUST equal the current pointer in the parents' view, or be a conflict value containing it |
| `unpoint` | `entity` | Removed from `entities`. Redaction and containment only |
| `head` | `line`, `to`, `from`, `seq`, `attests`, `id` | A line advanced. `seq` is the parents' plus one. `attests` lists the attestation IDs that satisfy the line's standard for `to`. When the line does not exist yet, `id` names the line entity, `from` is absent, `seq` is 0, and only an owner may emit it |
| `propose`, `unpropose` | `rev` | Membership in `proposed` |
| `claim`, `unclaim` | `claim`, `to` | A claim entity's pointer in `claims` |
| `cap`, `uncap` | `cap` | Capability activated or retired |
| `revoke` | `principal` | |
| `tomb` | `of`, `tomb` | |
| `deploy` | `target`, `to` | |
| `pause`, `resume` | `scope` | |
| `owner`, `unowner` | `principal` | Only by an existing owner |
| `root`, `unroot` | `name` | |
| `coordinator` | `line`, `to` | Only by an owner |

The `from` fields make every pointer move a compare-and-set against what the author saw, which turns concurrent edits into detectable conflicts instead of silent overwrites.

## The view merge

When the DAG has more than one head, the current view is the merge of the head views. Heads are folded pairwise in ascending op ID: merge the two lowest into an intermediate view, then merge that with the next, and so on. Each pairwise merge is three-way with the base being the view of the two inputs' latest common ancestor op; for an intermediate input, the base is the latest common ancestor of all ops folded so far and the next head.

Per key, in every trie and map:

1. If only one side differs from the base, take that side.
2. If both sides equal each other, take it.
3. If both differ from the base and from each other, the value becomes `{ "conflict": [a, b] }`, sorted. Merging a conflict value with another value unions the lists.

Sets merge add-wins. `revoked` and `tombstoned` never lose elements. In entity maps, a removal on one side against a change on the other lets the removal win, because only containment and redaction remove and both are owner-authored. `seq` on a line takes the maximum. A head conflict on a line is resolved only by a landing whose `from` names the conflict value. Trie merge is structural, so it costs the size of the differences, not the size of the view.

Claims are never dropped by the merge. Expiry is applied by readers, and the daemon that issued a claim retires it with an `unclaim` op on a cadence. Nothing in a view ever depends on a clock.

An op of kind `merge_view` carries only the effects needed to resolve conflict values it chooses to resolve; it records that the writer observed and reconciled the heads.

## Resolving conflict values

A conflict value on an entity is resolved by an op with a `point` effect whose `from` is the conflict value. The resolution may be either side, a new version merging both, or a task opened for a human. A conflict value on a line head is resolved only by a landing op.

## Stages

A revision's stage is derived, never stored:

| Stage | Definition |
|---|---|
| snapshot | Exists |
| proposed | In `proposed` |
| verified | Attestations present satisfy the `verified` standard of its intended line |
| landed | Head of a line or an ancestor of one through `parents` |
| released | A release references it |
| observed | A deployment references it and observer attestations exist |

A conflicted revision, meaning one whose snapshot contains a conflict entry, may be a snapshot and may be proposed so others can see and help; it cannot be verified and can never land.

Promotion is op kind `promote` with `args.to`. Proposing publishes: it requires only that the revision is not conflicted. Landing evaluates the line's standard and the author's capability and refuses with the unmet clauses, the out-of-scope paths, and the flags that block, all named. Before either, the daemon records a `risk.change` attestation on the revision if none exists. A landed revision is immutable except for `restack`.

## Verification

`verify` runs, for every `attest` clause the standard still leaves unmet, the verifier that proves it, and reports the standard clause by clause afterwards. The daemon is the runner: it materializes the snapshot into a directory of its own, runs the tool with a stripped environment, records the tool versions as an environment, and signs the attestation. A whole-suite run attests the snapshot, so identical content is never verified twice in the same environment. When every changed unit has a covering test, only the covering tests of the changed units and of their dependents run, and the result attests by bodies. For `tests.fail_on_parent`, the new test units are laid into the parent's text inside their containers and run there; passing there means the test does not prove the change. While a verifier runs the daemon answers every other request with `BUSY`.

## Speculation

`try` takes candidate edits and makes each a revision of its own on the caller's current revision, a change with a fresh ID, then runs the verifiers and the risk score on it and evaluates the standard. Candidates are ranked by unmet clauses, then failed tests, then risk, then size. `keep` writes one into the workspace; the candidate revisions stay in the store.

## Asking a human

When a verify or a landing finds an `approved` clause unmet, or a judged clause unmet because a judge said no, the daemon opens one request per revision: a `question` memory scoped to the change's intent, linking the revision, naming the clauses, the risk, and what each judge said, and delivers it through every channel. The open requests are the exception queue. A human answers with `approve`, which records the `approval.human` attestation and closes the question; a landing closes any request left open on its revision.

## Anomalies

The daemon scores anomalies per agent, locally: edits outside the agent's claims, edits and snapshots outside its write scope, landings refused for weakened tests, and mutations attempted while throttled. Past `anomaly_throttle` points the agent's mutations are refused for `anomaly_cooldown_s`; past `anomaly_revoke` the daemon, as owner, revokes and unwinds it. Hooks see `anomaly.detected` and `anomaly.revoked`.

## Delivery

A release is cut from the line's head by an owner or the coordinator: signed by the cutter, with a changelog of the titles since the previous release and the attestations that apply to the head. Deploying is `promote` to a target with a slice: the revision is evaluated against the standard the target's own extends for a canary and against the target's own for everyone, the adapter runs, and the op carries a `deploy` effect naming the new deployment, whose `previous` is what the target showed before. Observing runs the target's observers on its current deployment, records each signal as an attestation on the deployed revision, and evaluates the target's standard; when an `observe` clause trips and the machine allows automatic reverts, the daemon deploys the previous deployment of another revision as a rollback step, opens a task naming what tripped, delivers it through every channel, and fires `target.rolled_back`. Observation is on request in M6.

## Hooks

After an op that proposes or lands a change or records a conflict, and on anomalies, releases, deployments, and observations, the daemon runs every enabled hook whose `on` matches the event and whose `where` holds, as the hook's principal, synchronously under the repository lock; an `agent` action starts its runner detached and returns. Each action's outcome is returned to the caller under `hooks`. A failing action never changes the op's result: standards gate, hooks react.

## Snapshot never blocks

A `snapshot` op in the author's own workspace always succeeds. It is a `point` on the author's own change from the workspace's current revision, and verification does not check write scope or flags on it. Scope and flags are checked at `promote`, at `head`, at `deploy`, and by sync, which never sends a revision carrying `secrets` flags. Any tool may write into a workspace; the daemon records what it finds and says what will block later.

## Landing

Landing change C on line L with head H:

1. For each root the line spans, find the base revision B for that root: walk `parents` from both H and C until reaching the latest common ancestor that carries a snapshot for that root. Roots may have different bases.
2. Merge the trees of H, C, and B for each root. Paths only one side changed are taken from it. A path both sides changed is merged at unit granularity using the three nodeindexes:
   - Units line up by `(parent, kind, name, ordinal)`. A unit whose ID the base knows under another name lines up by the base's name.
   - Different units: take both. Additions both sides made after the same unit keep landing order, H's run first. Setlike regions: union, deduplicated.
   - A unit changed on one side only, by body or by name: take it. Unchanged on both but formatted differently: take the side whose text differs from the base.
   - A unit changed on both: if one side's text equals the base's text with that side's renames applied, take the other side's text with those renames applied. Otherwise, when both are containers, keep the header and footer from the side that changed them and merge the children by this same procedure. Otherwise a conflict entry naming the unit.
   - Renames. A side's renames are the `rename` ops recorded on its revisions since B, plus every unit that kept its ID under a different identifier name. They are applied as whole-word identifier replacement to everything the other side contributes: to units taken from it in merged paths, and to paths only it changed. A top-level unit's rename reaches every path with a grammar; a nested unit's rename stays in its file. Each application is a resolved semantic conflict and is reported in the landing response under `semantic`.
   - A unit renamed differently on both sides, or added on one side under a name the other side renamed a unit to, is a conflict entry.
3. Reconcile node IDs: the merged index is matched against H's; where H and C each introduced a node with equal `(path, kind, name)` or equal body hash, keep the lower ID, rewrite `parent` and `deps` references to it, and write the other into `aliases` of the landing revision's nodeindex.
4. Build landing revision R: same `id` as C, `prev` = C, `parents` = [H, C], the merged snapshots, `author` = C's author. The coordinator is the author of the op, not of the revision.
5. If R is conflicted, the line does not advance. R is stored, the change is pointed at R, and a task memory is opened naming the conflicted paths and linking R. Pointing the change is what lets the author's workspace hold the conflict: the files carry markers and each has a sidecar, and the sidecar is the source of truth. Resolving is editing the file and removing the sidecar, then snapshotting, which produces a plain revision with `prev` R and `parents` [H, C]; when H is still the head, that landing fast-forwards.
6. Collect the attestations that apply to R: by `subject` on R, on C (R's `prev`), and on R's snapshots; by `bodies` on every body hash in R's nodeindex. An attestation on a revision's `prev` applies only when the revision's second parent is that `prev`, which is the shape of a landing, a restack, and a conflict merge: revisions the system derived from the attested one. A revision the author snapshots names its predecessor as `prev` too, and inherits nothing from it, so an approval, a judge's verdict, or an external result is for the content a human or a verifier saw.
7. Evaluate L's standard deterministically over R and the collected attestations. If unmet and the merge produced a snapshot that is not C's, run the verifiers for the unmet `attest` clauses on the merged snapshot, collect again, and evaluate again: only what the merge invalidated is rerun. If still unmet, fail with the clauses named. R is discarded.
8. Emit the op: `put R`, `head L from H to R seq+1 attests [every collected attestation]`, `unpropose` every proposed revision of the change (C and any a conflict or restack left behind), `point C.id → R`.

## The frontier

The coordinator lands proposed changes in proposal order, each merged onto the head the one before produced, in one pass on request (`promote --to landed --all`) or continuously through a hook whose action is `land`. A refusal or a conflict is reported and does not stop the rest. After each landing, every unlanded revision whose first parent is the landed change's revision is restacked onto the landing revision: a new revision with the same `id`, `prev` the old one, `parents` [the landing revision, the old one], snapshots re-merged at unit granularity, keeping the change's author, proposed again if it was, and materialized into its workspace. A workspace with unsnapshotted edits is left alone, and a restack that would conflict is skipped; either way the change lands later through the three-way merge, whose base is unchanged.

## Revert

`revert` on a landed change C with landing revision R builds a new change whose snapshot is the head merged with R's first parent, base R: the head with C's edits undone at unit granularity. It refuses when later changes touched the same units, naming them. It opens a task memory naming the reverted change and the reason, links both revisions, and is attributed to the reverter with the original author named in its body. The coordinator lands it at once; anyone else proposes it. Dependents that break are caught by the landing's verifiers.

Every replica that receives this op re-evaluates step 7 against the cited attestations before accepting it (04-security.md step 7). The coordinator serializes; it does not get to decide.

A stack lands bottom up. After each landing, the coordinator emits `restack` for every revision whose `parents` named the just-landed revision: a new revision with the same `id`, `prev` = the old one, `parents` = [the landing revision, the old one], snapshots re-merged, keeping the change's author; the coordinator authors the op. The second parent is what carries the old revision's attestations, a human's approval among them, to the restacked one. A failure partway leaves the remaining revisions restacked on the new head.

A change spanning several roots lands on a line that spans them, in one op.

## Coordination

Every shared line has one coordinator, recorded in the view, and only the coordinator's ops may carry `head` effects for it. The hub is the coordinator when the repository is connected to one, otherwise the local daemon, whose principal is the owner created at init. A line with `shared` false is coordinated by whichever daemon owns it. The coordinator is a serializer, not a trusted party: because every replica re-evaluates the standard against the cited attestations, a coordinator cannot land what the standard forbids. It can withhold and reorder, which anchoring detects.

## Undo

`undo` takes back the author's own ops, where "own" rolls up to the durable agent: a session may undo an op authored by any session of the same agent. It emits a new op whose effects are the inverse of the target op's effects in reverse order. When the undone op moved the workspace's change, the workspace's current revision falls back to what the op moved from, or to the revision's first parent when it created the change, and that revision is materialized so the files match the record. An inverse `point` moves the pointer from the target's `to` back to its `from`. If the current pointer is no longer the target's `to`, undo refuses and names the dependent op. Undo never touches another principal's effects and never touches a landed head. A revocation is never undone; undoing a containment op restores everything the containment did except the revocation itself.

## Idempotency

Every op carries `args.idem`. The daemon indexes `(author, idem)` to the op it produced. A repeated call returns the same op. Keys expire from the index after thirty days (A2).

## Sync

Sync moves ops and the objects they reach between replicas. Continuous with a hub, on demand otherwise.

1. Manifests: each side presents a signed manifest naming the principals it serves.
2. Heads: each side sends its op DAG heads.
3. Walk: each side requests missing ops by walking parents from the other's heads until reaching ops it has.
4. Eager objects: with each op, the receiver fetches what verification needs before verifying: the revisions and views the op points at, the snapshots and the trees along every changed path when write scope must be checked, the attestations cited by `head` effects, and the standards, principals, capabilities, and hooks those checks reference. Blobs, artifacts, and nodeindexes not on a changed path are lazy.
5. Verify: every received op passes 04-security.md before it is stored. A rejected op is rejected with every op that descends from it.
6. Visibility: the sender omits private memories whose author is not in the receiver's manifest, omits objects outside the read scopes of the principals the manifest names, and never sends workspaces, which are not in the graph anyway.
7. Tombstones: when the sender holds a tombstone for a requested object, it sends the tombstone, and the receiver accepts it only if the redaction op verifies.

Wire format is CBOR framed over HTTP/2 with TLS. Replay is impossible because ops are content-addressed and idempotency keys are indexed.

## Anchoring

On a cadence the daemon signs an anchor naming the current heads and posts it to the hub and, when configured, to a public transparency log. A replica that sees heads inconsistent with an anchor it trusts raises a task. `OPEN:` cadence and log; default hourly to the hub, daily to a Sigstore Rekor instance when configured.

## Garbage collection

Ops are never collected. The roots for object reachability are: every trie and map in the current merged view; every line's history through `parents` and each entity's history through `prev`; every tombstone; and objects reachable from ops younger than the retention window for their kind. An op's `put` list is not a root. An object unreachable from all of these is collected. Defaults: losing `try` candidates seven days, retired claims immediately, superseded attestations one year, everything else forever. `OPEN:` op checkpointing for repositories with tens of millions of ops. Default: none in M1.
