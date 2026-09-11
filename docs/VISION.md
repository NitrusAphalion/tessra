# Tessra

**The version control system for the era of AI.**

Git records what changed. Tessra records what changed, why, and what is known to be true about the result. Files are one view of the repository, not the repository itself.

## Why now

Code is no longer scarce. Agents write most of it, in parallel, around the clock. That breaks four assumptions every existing version control system was built on: one author at a time, one working directory, a human reading text output, and a human on hand to resolve conflicts.

The bottleneck has moved. It is no longer writing code. It is knowing what to trust, why it exists, and how to coordinate many authors that never sleep.

A version control system for this era is not a better git. It is the substrate that agents think, coordinate, and verify on, and the place humans go to understand what happened and decide what to trust.

## Principles

These are non-negotiable. A feature that violates one is wrong, however useful it looks.

1. **Machine-readable first.** Every fact has a structured form. Human rendering is a view.
2. **Safe to retry, safe to undo.** Every mutation is idempotent under a client request ID and reversible through the operation log.
3. **Nothing blocks.** Conflicts, failed verification, and missing review are states stored in the repository, not errors that halt work.
4. **Trust is explicit.** What is known about a snapshot is recorded as attestations and queried. It is never assumed.
5. **Provenance is complete.** Every byte traces to a principal, a task, an intent, and the model that produced it.
6. **Parallel by default.** One author is the degenerate case of many.
7. **Budgeted by default.** No read is unbounded. Every query takes a size cap and returns a cursor.
8. **Files are a view.** The repository is a graph of semantic units, history, intent, and knowledge. A directory is a projection of it.
9. **Local-first.** Fully functional offline. Syncs through any blob store.
10. **Adoptable incrementally.** A first-class git bridge, so Tessra can run underneath a project that still lives on GitHub.
11. **No screens.** Agents are the interface. Everything a screen would show is a query, everything it would let you do is a verb, and humans get documents on demand in the tools they already use. Decisions that must be human go through channels the system owns, never through an agent.
12. **Signed by default.** Every op, attestation, approval, memory, and grant is signed by a principal. Every principal holds only the capabilities it was granted. History is tamper-evident.

## The bets

Thirteen things no existing system does. Together they are the product.

### 1. Intent is a first-class object

Every change links to the task, the plan, and a reference to the agent context that produced it. This gives blame by intent instead of by line, stale-code detection when a spec changes, and semantic rebase: when a change conflicts with a new base, the system can re-derive it by replaying the intent against the new base and verifying the result. The textual patch stays canonical. Re-derivation is a recorded strategy, not the source of truth. Intents carry dependencies, priority, and status, so issues and roadmaps are intents and swarm planning schedules from the graph.

### 2. Knowledge is part of the content model

A snapshot carries attestations keyed by content hash plus an environment fingerprint: tests passed, type-checked, reviewed by a human, benchmark within budget. Verification never repeats for identical content. Trust is a queryable property, so an agent can ask for the most trusted snapshot that contains its change. Landing policy is a predicate over attestations stored in the repository. Bisect uses the cache and skips known-good states.

### 3. Semantic units, not lines

Tree-sitter parses supported languages into nodes with stable identities matched across versions. Diff, merge, blame, claims, and queries operate on nodes. Two agents editing different functions in one file never conflict. Renames and moves are free. A rename in one change and a new call to the old name in another is a detected semantic conflict, where git reports a clean merge and a broken build.

Blobs remain the source of truth. Nodes are a derived, cached index. This keeps the system language-agnostic with a chunk-level fallback, and it means the hardest algorithm, cross-version node matching, can improve without a format migration.

### 4. Continuous optimistic integration

The trunk is a verified frontier that advances itself. Each agent works in its own workspace. The system continuously attempts integration, warns an agent early when another agent's work overlaps semantically, and lands automatically when the gates pass. A conflict becomes a task framed in intent, handed to an agent with full context rather than dumped as conflict markers.

### 5. Context packs

"What do I need to know to change X, in N tokens." The system maintains incremental summaries per node and per module, versioned alongside the code and invalidated by the same change graph. A pack includes relevant symbols, their recent history, their tests, active claims, and open reviews. It is a code index that cannot go stale, because it is history.

### 6. Speculation

Propose K alternatives, materialize K workspaces, run the gates, rank, keep one. Cheap because workspaces are copy-on-write and verification is cached. The repository becomes a search space over solutions, which is how agents actually work.

### 7. The repository is a database

A real query language over the graph: changes by agent, touching symbol, under task, failing test, since snapshot. Every read takes a budget and returns a cursor. Materializing a directory is a projection for tools that need one.

### 8. Memory lives in the version control system

Agents have no memory across sessions, and every framework patches that with a hand-maintained file. Tessra stores memory as typed objects in the graph: facts, decisions, conventions, gotchas, preferences, task state, questions. Each has a scope, provenance, an anchor to the content it describes so it is flagged when that content changes, and trust. Agents record with one verb and recall through context packs. Humans set up nothing. Any framework that does not speak Tessra gets a generated file instead of a hand-written one. See MEMORY.md.

### 9. Massive parallelism, even within one file

Files are projections of nodes, so two agents editing different functions in one file never conflict. Set-like regions such as imports, enum variants, and match arms merge as sets. Ordering is never a conflict. Formatting is a projection, not content. Refactorings such as rename, move, and add-parameter are recorded as operations so they compose with other agents' edits instead of fighting them. Landing a change runs only the tests that cover the nodes it touched. Nothing locks: claims route work, and same-node collisions become tasks resolved asynchronously while everyone else keeps going. See PARALLELISM.md.

### 10. Standards, not review

Agents write code faster than humans can read it. Tessra changes what humans review. People define standards once: declarative, scoped predicates over attestations that say what good enough means. A change cannot be proposed, landed, or released until the system has proven it meets the standard, and the agent's own claims carry no weight. Humans review standards, exceptions, and samples instead of every diff, so human attention scales with the exception rate, not the code rate. See PIPELINE.md.

### 11. Tests are proof, coverage is a map

Tests are nodes in the graph, linked to the intents they verify. Coverage is recorded per node as attestations, so the system knows exactly which tests prove which code, runs only those for a change, and can name every untested node. A standard can require that a new test fails on the parent snapshot and passes on the change, so agent-written tests demonstrate something rather than pass trivially, and it can forbid weakening existing tests without approval. Flaky tests are measured and quarantined automatically. Coverage floors open tasks that the swarm fills. See TESTING.md.

### 12. The whole lifecycle, headless

Intent to production with no UI and no human unless a standard asks for one. Targets record what runs where. Deployers and observers are adapters, like verifiers. Release standards gate promotion to each target, progressive delivery is a standard with steps, and rollback is promotion to the previous snapshot. When a standard requires a human, the daemon asks through a channel the system owns and records the signed reply. The agent never sits between the human and the record. See LIFECYCLE.md.

### 13. Standards gate, hooks react, risk adapts

The operation log is an event stream, and a hook is a versioned, scoped subscription to it with an action: call an external system, run a verifier, open a task, ask a human, spawn an agent, promote, revert, pause. Existing CI and deploy tools plug in as verifiers and deployers that live elsewhere, so nobody abandons their pipeline on day one. A built-in risk monitor scores every change, scope, target, and agent continuously, explains every score, and feeds standards, swarm planning, circuit breakers, risk budgets, and containment. See HOOKS.md and RISK.md.

## What the era of AI adds

Consequences of the mission that go beyond agents as authors.

- **Recall by model.** When a model version is found to have a systematic flaw, find everything it wrote and re-verify or regenerate it.
- **Regenerate from intent.** Intent, specs, and evals are the durable assets. Code is regenerable from them.
- **Activity packs for humans.** The inverse of context packs: "what happened while I was away," at any altitude, from one line to a full audit.
- **Containment.** Revoke an agent and unwind all of its unlanded work across every workspace in one operation. The version control system is the last line of defense against a compromised or misbehaving agent.
- **Environments, not just files.** A workspace can snapshot dependencies and build state, so any point in history is runnable and any experiment is rewindable.
- **Cost is recorded.** Tokens and compute per operation, attributed to intent, agent, and model. What a change cost is a query, and capability budgets are measured against it.

## Architecture

Seven layers. Each is only worth building if the one below it holds.

1. **Typed object store.** Content-addressed Merkle DAG with typed objects: blob, tree, node, artifact, snapshot, change, intent, memory, attestation, standard, hook, claim, review, principal, capability, line, release, target, deployment, channel, op. Every op is signed, and op logs form a DAG that merges across clones, so history is tamper-evident and concurrent work is recorded faithfully. One replicated graph with per-object visibility, and one graph may have many roots. Tree entries carry mode and kind; tracking rules are objects. Large artifacts are pointer objects backed by an external store. Local-first, syncs through any blob store.
2. **History graph.** Stable, letter-based change IDs visibly distinct from content hashes. Every version of a change retained through supersedes links. Landed changes immutable. Stacks with automatic restack of descendants. Lines and releases. Conflicts stored as data. Operation log with universal undo. Derivation links from intent to change to attestation.
3. **Semantic index.** Tree-sitter nodes with cross-version matching. Node-level diff, merge, and blame. Chunk fallback for unsupported files.
4. **Knowledge layer.** Attestations, environment fingerprints, trust predicates, verification cache, cached bisect. Standards. Sandboxed verifier runs with daemon-signed attestations. The risk monitor.
5. **Coordination.** Copy-on-write workspaces, expiring claims, principals with keys and capability grants verified on every op, hooks on the operation log, the integration frontier.
6. **Agent surface.** A daemon speaking MCP and gRPC as the primary interface. The thirteen verbs in VERBS.md with one response shape. JSON everywhere. Budgets and cursors. The query language, context packs, speculation. Channels the system owns for the decisions that must be human. A thin CLI for humans who want one.
7. **Git bridge.** Import and export so Tessra can live next to GitHub. A door, not compatibility.

## Decisions

- Snapshot model with stable change IDs. Semantic merge layered above. Not patch theory.
- Own object store. No git or Jujutsu substrate.
- The semantic index is a launch requirement.
- Environment-level workspaces are a goal, sequenced after the core.
- Daemon-first. MCP and gRPC are the primary interface. The CLI is a thin client.
- Multi-agent coordination is in v1.
- Large artifacts are in: pointer objects with content hashes and an external store adapter, and a standard clause can require an exclusive claim on non-mergeable files. Binary diff and merge stay out.
- One graph, many roots. A change may span repositories, and landing across roots is atomic when one coordinator holds their frontiers.
- The agent surface is thirteen verbs, chosen for what an agent would type unprompted. See VERBS.md.
- Rust.

## Not doing

Git wire-protocol compatibility beyond the bridge. Submodules, which multi-root graphs replace. Binary diff and merge. A UI of any kind. Running containers, hosting services, or collecting metrics, which are adapters.

## Companion documents

- TENETS.md: constraints derived from how models work. The tiebreaker when scope is argued.
- MEMORY.md: memory as typed objects in the graph. Agents record and recall through the VCS; humans set up nothing.
- PARALLELISM.md: how many agents work in one repository and one file without a merge queue.
- PIPELINE.md: standards, verifiers, stages, and review by exception. How fully automated flows stay trustworthy.
- TESTING.md: tests as nodes, coverage as a node-level relation, tests as proof of a change, guards against weakened tests.
- LIFECYCLE.md: no screens, agents as the interface, channels for human decisions, and intent to production.
- HOOKS.md: subscriptions to the operation log with actions, including agents. How existing CI and CD plug in. Revert at every stage.
- RISK.md: the continuous risk monitor, explained scores, adaptive standards, circuit breakers, risk budgets, containment.
- SECURITY.md: threat model, principals and keys, signed ops, capabilities, verifier isolation, secrets, provenance.
- VERBS.md: the thirteen agent verbs, the response shape, and the manual.
- GAPS.md: the sweep against git and competing systems, with what was decided and what remains open.
- ADOPTION.md: how Tessra becomes more popular than git.
- ROADMAP.md: milestones, each defined by a demo.
