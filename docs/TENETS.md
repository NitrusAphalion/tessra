# Tenets

Constraints that follow from how large language models actually work. Unlike features, these do not change with fashion. Each tenet states the model fact, the rule it forces, what to build because of it, and what to skip.

VISION.md says what to build. ADOPTION.md says how it wins. This document says why the feature set is shaped the way it is, and it is the tiebreaker when scope is argued.

## 1. The repository is the agent's memory

**Fact.** Models have no memory across sessions. Every context starts empty.

**Rule.** Everything an agent needs to resume must live in the repository: intent, plan, task status, claims, open questions. A fresh agent reconstructs its situation from the repository alone, in one call.

**Build.** Resume as a primitive. Intent and task objects with status. Claims that outlive the session that made them.

**Skip.** Anything that assumes the same agent returns, or that state lives in a conversation.

## 2. The system chooses what the model sees

**Fact.** Context is finite, and attention degrades as it fills. The model cannot afford to read the repository to discover what matters.

**Rule.** Every read is budgeted. Retrieval and summarization are the system's responsibility.

**Build.** Context packs. Budgeted queries with cursors. Hierarchical summaries per node and module, versioned with the code.

**Skip.** Any unbounded output. Any workflow that requires the model to page through history to find something.

## 3. Nothing is trusted until verified, and nothing is verified twice

**Fact.** Outputs are stochastic. The same intent yields different code on every run. A model's report of what it did is itself a generated output.

**Rule.** Verification is mandatory and recorded as an attestation. It is cached by content hash so the cost is paid once.

**Build.** Attestations. The verification cache keyed by content hash and environment fingerprint. Trust predicates. Cached bisect.

**Skip.** Trusting a model's own claim that it ran the tests. Any landing path without a gate.

## 4. Sample many, keep the best

**Fact.** A stochastic generator is best used by sampling and selecting. Agents parallelize at near-zero marginal cost, unlike humans.

**Rule.** Parallel is the default. Speculation is a primitive, not a pattern.

**Build.** Tournaments: propose K, materialize K, verify K, rank, keep one. Copy-on-write workspaces. The integration frontier that lands verified work continuously.

**Skip.** Optimizing for a single sequential agent.

## 5. State is returned, never remembered

**Fact.** Models hallucinate state they have not observed, and drift from reality across steps.

**Rule.** Every mutation returns the resulting state. Observing state is cheap enough to do on every step.

**Build.** State echo on every response. Dry-run for every mutation. Errors that include the corrected call.

**Skip.** Any operation whose outcome the model has to infer rather than read.

## 6. Few verbs, one query language

**Fact.** Tool-use accuracy falls as the number of tools and the complexity of their schemas rise.

**Rule.** A small set of orthogonal verbs, roughly a dozen, and one query language for everything read-only.

**Build.** The thirteen verbs in VERBS.md with one response shape. The query language over the graph.

**Skip.** Git's hundred and fifty subcommands. Flags that change semantics.

## 7. Fewer round trips beat cheaper calls

**Fact.** Cost and latency scale with round trips and tokens.

**Rule.** The observe, act, verify loop should be as few calls as possible. Work that can run in parallel inside one call does.

**Build.** Compound operations: try-verify-keep as one call. Batch apply. Parallel speculation inside one request.

**Skip.** Fine-grained plumbing as the primary interface.

## 8. Show the blast radius

**Fact.** Models are good at local edits and bad at global consistency. They change a function and forget its callers.

**Rule.** Every change comes back with what it affects.

**Build.** The semantic index. Dependents and covering tests in every context pack and every change response. Semantic conflict detection where git reports a clean merge.

**Skip.** Line-only tooling as the ceiling.

## 9. Speak git where git is right

**Fact.** Models are trained on git. That vocabulary is already in the weights and costs nothing to use.

**Rule.** Use git's words where the semantics match. Introduce new words only for genuinely new concepts. The manual covers only the delta.

**Build.** Aligned vocabulary: commit, branch, merge, diff, blame keep their meanings. New words: change, snapshot, attestation, claim, pack. A manual any model reads in two thousand tokens.

**Skip.** Renaming familiar operations for style. Reusing a git word with different semantics, which causes silent misuse.

## 10. The model is not the security boundary

**Fact.** Models follow instructions found in content they read. Repository content can be adversarial.

**Rule.** Policy, capabilities, and provenance are enforced by the system, outside the model.

**Build.** Principals with keys. Every op signed and carrying its capability. Capability grants with scope, expiry, and budget. Landing policy as data. Sandboxed verifiers so code under test cannot attest to itself. Containment: revoke an agent and unwind its unlanded work. See SECURITY.md.

**Skip.** Relying on prompts or model judgment for safety.

## 11. Time is a cursor

**Fact.** Models have no sense of what happened while they were not looking.

**Rule.** "Since X" is a first-class query for agents and humans.

**Build.** Since-cursor deltas. Activity packs at any altitude.

**Skip.** Expecting a model to diff two log outputs.

## 12. Models change, the format does not

**Fact.** Models improve, differ in strengths, and get deprecated.

**Rule.** Provenance records the model. Nothing in the format is model-specific.

**Build.** Model identity in every change's provenance. Recall by model.

**Skip.** Model-specific formats, prompts baked into objects, or behavior that depends on a particular model.

## 13. Documents, not screens

**Fact.** A model can render any view on demand, tailored to the question and the reader.

**Rule.** No fixed screens. Everything a screen would show is a query; everything it would let you do is a verb. Humans get documents in the tools they already use.

**Build.** The query language. Packs at any altitude. Channels the system owns for the decisions that must be human, so an agent can never forge an approval.

**Skip.** A UI layer. Tessra never depends on one; anyone may build one on the query language.

## What the tenets scope in first

Directly forced by how models work, so they are the core:

- Context packs and budgeted reads (2)
- Attestations and the verification cache (3)
- The verb set, state echo, and the query language (5, 6)
- The semantic index for blast radius and semantic conflicts (8)
- Intent objects and resume (1)
- Speculation and copy-on-write workspaces (4, 7)
- Principals and containment (10)
- Channels for human decisions (10, 13)

Follows from cheap parallelism: the integration frontier (4).

Added by the tenets that the bets missed: state echo on every response, a fixed small verb set, resume from the repository alone, since-cursor deltas.

## What the tenets push later

Justified by adoption or infrastructure rather than model cognition. They stay on the roadmap, and nothing in the core waits for them.

- Environment snapshots
- The hub and network layer
- Lossless git export
- Semantic rebase by re-derivation from intent
