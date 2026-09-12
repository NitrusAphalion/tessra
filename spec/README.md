# Tessra M0 Specification

Status: draft 9, 2026-09-05. Every milestone M0 through M8 is built with a passing demo; the spec matches what runs. Draft 2 resolved every finding in REVIEW-1.md. Drafts 3 to 8 folded in what building M1 to M6 taught: each file opens with the list of what changed from the draft before.

This is the M0 deliverable from ROADMAP.md: every object type with its fields, hash rules, and invariants; the operation log and sync model; the security model; the on-disk layout; and the agent manual. Nothing in M1 may depend on anything not written here. When the design documents and this spec disagree, this spec wins and the design document gets fixed.

| File | Covers |
|---|---|
| [01-conventions.md](01-conventions.md) | Encoding, hashing, identifiers, time, signatures, schema versions, sizes |
| [02-objects.md](02-objects.md) | The object catalog: twenty-nine types in six groups |
| [03-operations.md](03-operations.md) | The operation DAG, effects, the view merge, stages, landing, coordination, undo, idempotency, sync, anchoring, garbage collection |
| [04-security.md](04-security.md) | Keys, the op verification algorithm, capabilities, revocation and containment, human approvals, verifier isolation, secrets, redaction |
| [05-layout.md](05-layout.md) | Repository and store directories, keys, pack format, git colocation and lazy import, workspaces, conflict materialization, wire mapping, the M1 query subset, performance budgets |
| [06-manual.md](06-manual.md) | The agent manual. About 1,100 tokens |
| [REVIEW-1.md](REVIEW-1.md) | Antagonistic review of draft 1. Every item is resolved in draft 2 |

## Reading conventions

- MUST, SHOULD, and MAY carry their RFC 2119 meanings.
- `OPEN:` marks a decision not yet made. Each has a stated default, and M1 proceeds on the default unless the decision is made first.
- Field tables list the CBOR key, the type, whether it is required, and the meaning. Types: `bytes32`, `bytes16`, `bytes64`, `text`, `uint`, `int`, `bool`, `map`, `array`, `any`.

## The load-bearing decisions in draft 2

- The view's large maps are persistent tries, so an op writes O(log N) nodes. Nothing local to a machine is in the view.
- A landing cites its attestations and every replica re-evaluates the standard. The coordinator serializes; it does not decide.
- Revocation and rotation are checked against the receiver's current view as well as the op's parents' view, so stale parents cannot resurrect a dead key.
- No verification step reads a clock. Expiry is enforced by whoever holds the key or hosts the state.
- Snapshot never blocks. Flags record what will block promotion, and promotion is where enforcement happens.
- Node-scoped attestations key on body hashes and survive the landing merge.
- Agent identity is a durable agent principal plus short-lived sessions whose keys the daemon holds.
- Capabilities carry read scope as well as write scope, and hard limits are metered by the daemon while advisory ones are advisory.
- Keys and the store live outside the repository directory and outside cloud-synced paths.

## What M1 builds from this

The object store with the trie, the operation log with the full verification algorithm, the entity pattern, the view and its merge, lazy git import into a root with the colocated checkout as a workspace, the `remember` verb over memory objects, the M1 query subset, signed ops with capability checks including read scope, and the thirteen-verb surface over MCP with the response shape in VERBS.md. The semantic index, standards, hooks, and everything above layer 2 are specified so M1 does not paint them into a corner, and are built in M2 and later.

## What M2 built from this

The nodeindex for Rust, Python, JavaScript, and TypeScript with the chunk fallback, history-aware node identity, `deps`, the node-level landing merge with `rename` as the first composed operation, aliases, blame, the covering-tests relation, and the node-level diff. Draft 4 folded in what M2 taught; nothing from M2 remains a deviation.

## What M3 built from this

Standards as data with the text predicate grammar, verifiers run by the daemon as runner with signed attestations and environments, the verification cache by snapshot and by body, test selection by the covering-tests relation, the fail-on-parent proof, test-change guards, hooks with webhooks, external principals, the risk score with `when` clauses, the busy daemon during verifier runs, and the `trusted` and `bisect` queries. Draft 5 folded in what M3 taught.

## What M4 built from this

Planning by blast radius with sub-intents, assignments, and tasks; standing grants and delegated capabilities with a verified chain; overlapping-claim warnings; the frontier, one pass or continuous through a hook; restack of clean stacked workspaces; revoke with containment in one op; revert at unit granularity; parallel workspace materialization. Draft 6 folded in what M4 taught.

## What M5 built from this

Unit context packs, also over MCP; speculation with `try`; the activity pack; human principals, channels, approval requests, and the exception queue; judged clauses; agentic hooks through a configured runner; the anomaly path from throttle to revoke; machine-local configuration through the CLI. Draft 7 folded in what M5 taught.

## What M6 built from this

Targets with adapters and their own release standards, signed releases, deployments by slice as effects on the view, observation as attestations with the `observe` predicate, automatic rollback with a task and a channel message, and the delivery events for hooks. Draft 8 folded in what M6 taught.

## What M7 and M8 built from this

M7: lossless git export and import, reusing every commit and rewriting nothing. M8: state bundles on snapshots, checkout with build state and no install step, and workspace rewind. Draft 9 folded both in and is the last draft; every milestone has a passing demo.

## Deviations from M7 and M8

- Git export is linear: each landed revision becomes one commit whose parent is the previous trunk revision's commit; landing merges are not exported as git merges. Authors are `<agent>@tessra.local`; the committer is `tessra`; dates are the revision's time. The revision-to-commit mapping is store metadata (`git:rev:<hex>`, `git:commit:<hash>`) plus the commit hash imported revisions name in their body.
- Git import is first-parent and lands each commit as its own change through the coordinator; a commit that conflicts with trunk stops the import there. With `--history`, an owner lands the commits as history outside the standard, as `init --history` lands them. Content round-trips byte for byte inside Tessra; a checkout's line endings follow git's own settings. Executable bits and symlinks round-trip; submodule pointers, signed commits, notes, and tags do not.
- State is captured as content-addressed trees per configured `state_paths`, with lockfile hashes and probed toolchain versions; it is not a container image, restore replays exact bytes, and there is no chunk-level dedup beyond per-file sharing. `snapshot --with-state` or `snapshot_state=true` opts in; the landing carries the change's environment forward.
- The CLI runs its work on a 64 MiB thread to survive clap's derived builder and the verb dispatch frames in a debug build.

## Deviations carried from earlier milestones

- Artifact chunking is fixed 1 MiB pending the FastCDC decision below. Git history import is bounded by `--history N`; lazy import through the git object database and watching `.git/refs` are scheduled later.
- Structural similarity in node matching is an interim: a unit that vanished and one that appeared under the same parent with the same kind are the same unit when their body token bigrams, own name removed, overlap by Jaccard 0.6 or more; GumTree is not implemented. The merge keys units by node ID through the aliases of base and both sides, and falls back to kind and name, body first, only for units with no shared ID. Of the `revision.ops` kinds only `rename` is recorded, and a rename is applied to identifier tokens of the parse, never to strings, comments, field accesses, or shadowing locals.
- Coverage is the static covering-tests relation; measured `coverage.node` attestations are not produced. The verifier sandbox is L0. Secrets are flagged at snapshot, not kept out of the store.
- Hooks run only as the daemon and fire on `proposed`, `landed`, `conflict.opened`, `released`, `deployed`, `observed`, `observed.fail`, `target.rolled_back`, `anomaly.detected`, and `anomaly.revoked`; `webhook` speaks plain HTTP. `scopes` and `audit_permille` are not evaluated; risk and observation are per change and on request.
- Workspaces are copied, not reflinked or projected. Split-by-intent, absorb, and atomic landing across roots are not built; the last waits for multi-root repositories. A delegated session's default capability stays active beside the delegated one.
- Human keys are held by the daemon; passkeys, hardware keys, and channel-attested replies are not built. Channels are inbox and webhook. Judges' reasoning is text; there is no rubric object; the `agent` hook action starts the configured runner, not a model.
- Not built: audit sampling, reputation, flaky quarantine, coverage floors, mutation testing, per-scope and per-target risk, circuit breakers, risk budgets, risk packs, key rotation, gRPC, platform deployers, baselines and canary comparison, the hub and continuous multi-machine sync. Try candidates are never collected.
- The bridge to git beyond bounded import, and the hub, sync, and scale work of M7 and M8, are specified but not built.

## Open decisions

| Where | Decision | Default |
|---|---|---|
| 01 | Artifact chunking parameters | FastCDC 256 KiB / 1 MiB / 4 MiB |
| 02 | Directories with more than about a million entries | Reject |
| 02 | Node matching similarity algorithm | GumTree |
| 03 | Anchoring cadence and public log | Hourly to hub, daily to Rekor when configured |
| 03 | Op checkpointing | None in M1 |
| 04 | Redaction-in-place for secrets at snapshot | Flag only |
| 05 | Object store engine | redb behind the pack contract |
