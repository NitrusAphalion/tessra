# Roadmap

Each milestone ends in a demo that could not be shown before it. If a milestone has no demo, it is not a milestone.

## M0. Foundation (complete 2026-09-05; spec at draft 9, folded findings from every milestone)

- Object model specification: every object type, its fields, its hash rules, its invariants.
- Security model: principals with keys, signed ops in a merging DAG, capability grants, verifier isolation. Designed in before any code, because it cannot be retrofitted.
- The first group of GAPS.md, all decided: the op DAG and sync model, change versions and immutability, stacks, lines and releases, redaction, tree metadata and tracking rules, artifact pointers, multi-root graphs, letter-based change IDs, the cost ledger, the intent graph.
- The agent manual, drafted alongside the spec from VERBS.md.
- Repository layout on disk and the sync format.
- Rust workspace skeleton, CI, conventions.

**Demo:** none. The spec is the deliverable, and it is reviewed before any code depends on it.

## M1. Substrate (complete 2026-09-05)

Layers 1 and 2. Typed object store, history graph, operation log, CLI with JSON output. Colocation inside an existing git repository, importing blobs and trees, so adoption never requires a migration. Memory objects with path scoping, the `remember` verb, budgeted recall, and export to CLAUDE.md and AGENTS.md. Principals with keys, every op signed into the op DAG, capability grants verified by the daemon. Artifact pointers with an external store adapter.

**Demo:** Run `tessra init` inside a real git repository and nothing about the git side changes. Take snapshots, name a change, rewrite it, and refer to it by the same change ID afterward. Merge two divergent changes that conflict, commit the conflicted state, resolve it in a later change. Undo any operation, including the resolution. An agent records a gotcha, a fresh agent in a different tool recalls it, and the exported AGENTS.md contains it. Every op is signed. Altering an object or an op is detected. An agent holding a task-scoped capability is refused when it tries to land.

## M2. Semantic (complete 2026-09-05)

Layer 3. Tree-sitter node index with cross-version matching. Node-level diff, merge, and blame. Set-merge for set-like regions. Format-insensitive node identity. Refactorings recorded as operations. Tests indexed as nodes and the covering-tests relation. Chunk fallback.

**Demo:** Twenty agents edit one file concurrently, each in a different function, several adding imports and enum variants, one reformatting the whole file. Every change merges with no conflict markers. Blame on any line names the node, the change, the agent, and the intent. A rename in one change plus a new call to the old name in another is detected as a semantic conflict and resolved by applying the rename.

## M3. Knowledge (complete 2026-09-05; coverage is the static covering-tests relation and the sandbox is L0, see spec/README.md)

Layer 4. Attestations, environment fingerprints, trust predicates, verification cache, cached bisect. Coverage-mapped attestations, so landing a change runs only the tests that cover the nodes it touched. Standards as data, stages gated by standards, deterministic and structural verifiers, human approval and review objects as attestations. Node-level coverage map, fail-on-parent proof for new tests, test-change guards. Hooks as declarative subscriptions on stage events, outbound webhooks, inbound attestations from external CI under their own principal. Per-change risk score with explained factors. Sandboxed verifier runs with daemon-signed attestations. Secrets scanning before content enters the store.

**Demo:** Run the test gate on a snapshot. Rebuild the identical content through a different path and the gate is free. Change one function and the gate runs only its covering tests. Define a standard that requires coverage on changed nodes. An agent's landing is refused with the unmet clause named, the agent adds tests, and the landing succeeds. A new test that also passes on the parent snapshot is rejected as not proving the change. A change that deletes an existing test is refused without approval. A hook on proposed triggers an existing CI system, whose result arrives as an attestation the standard consumes. A high-risk change is refused with its top risk factors and what would lower each. A test that tries to post its own attestation cannot. Ask for the most trusted snapshot containing a change and get it. Bisect a failing test in fewer steps than the history length because known-good states are skipped.

## M4. Swarm (complete 2026-09-05; workspaces are copied not reflinked, split-by-intent, absorb, and multi-root landing are deferred, see spec/README.md)

Layer 5. Copy-on-write workspaces, delegated capabilities with budgets from the swarm planner to the agents it spawns, expiring claims, the integration frontier, swarm planning. Stacks with automatic restack. Atomic landing across roots.

**Demo:** Give one intent to the system and it partitions the work into disjoint node sets by blast radius and assigns claims. Fifty agents work concurrently on one repository, many in the same files. Each gets a workspace in constant time. Overlapping claims produce early warnings. Verified changes land on trunk continuously without a human merge queue, hundreds per hour. A same-node collision becomes a task and is resolved by a resolver agent without stopping anyone else. Revoke one agent and every piece of its unlanded work and its unconfirmed memories are unwound in one operation.

## M5. Agent surface (demo complete 2026-09-05; judges are sessions attesting a rubric, the resolver is a configured runner, see spec/README.md for what is not built)

Layer 6. Daemon, MCP and gRPC, query language, context packs, speculation, budgets and cursors on every read. Judge agents, risk routing, the exception queue, audit sampling, reputation, and the observe stage with auto-revert. Flaky test quarantine, coverage floors that open tasks, mutation testing on a cadence. Channels the system owns for human approvals and questions. Agentic hooks. The continuous risk monitor: scope, target, and agent risk, anomaly detection, circuit breakers, risk budgets, risk packs. Key-signed approvals through channels via passkeys. Key rotation and revocation with containment hooks.

**Demo:** An agent connects over MCP, asks for a context pack to change one function, and gets it inside a token budget. It proposes three alternatives, the system materializes and verifies all three, and returns a ranked result. A fifty-change swarm lands under a standard with judged clauses; two changes are routed to a human as exceptions, each carrying the judges' reasoning. A human asks what happened in the last hour and gets an activity pack at the altitude they choose. A high-risk change asks a human for approval through a chat channel; the reply is recorded as a signed attestation the agent never touched. A conflict opens and a hook spawns a resolver agent that lands the resolution under the standard. An agent that starts writing outside its claims trips an anomaly, is throttled, then revoked, and its unlanded work is unwound.

## M6. Production (demo complete 2026-09-05; adapters are commands and a directory copy, see spec/README.md)

Targets, deployments, deployer adapters, release standards, observers as verifiers, progressive delivery, rollback. Deployer secrets through a vault adapter, injected by capability.

**Demo:** One intent goes from plan to production with no human action: swarm, verify, land, release to a canary slice, observe, promote to everyone. Then a synthetic error spike trips the observe clause, production reverts to the previous snapshot automatically, a task is opened with what tripped, and the human learns about it from a message in their chat channel.

## M7. Bridge (demo complete 2026-09-05; the demo's remote is a bare repository on disk, see spec/README.md)

Layer 7. Full git export and round-tripping. Import landed in M1; this milestone makes the reverse direction lossless.

**Demo:** Run a swarm on a real GitHub project under Tessra. Export the landed trunk back as a clean git history that the rest of the team never notices came from somewhere else.

## M8. Environments (demo complete 2026-09-05; state is captured as trees, not a container image, see spec/README.md)

Workspaces snapshot dependencies and build state alongside files.

**Demo:** Check out any point in history and it runs, with no install step. Rewind an experiment including everything it wrote outside the source tree.

## Sequencing notes

- M1 through M3 are strictly ordered. Each depends on the one before.
- M4 and M5 can overlap once M3 is stable.
- M6 needs standards from M3 and channels from M5. It can start as soon as both exist.
- Git import is in M1 because colocation is the adoption path (see ADOPTION.md). The object model is locked in M0 so git cannot distort it. Export waits until M7.
- M8 is last because it is platform-specific work that touches nothing in the object model.
