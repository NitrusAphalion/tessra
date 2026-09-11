# Gaps

What the design corpus does not yet cover, found by sweeping git's full feature surface and the differentiators of Mercurial, Jujutsu, Sapling, Pijul, Fossil, Perforce, Gerrit, and Unison. Each item has a recommendation. Items in the first group change the M0 object model and must be decided before it is written.

## Must decide before M0

**All twelve decided on 2026-09-04 and folded into the corpus.** Kept here as the record of what was considered and why. Item 9's final names differ from the draft below; VERBS.md is canonical.

### 1. The operation log is a DAG, not a chain

Multiple clones and multiple machines produce concurrent ops. SECURITY.md describes each op referencing the previous op's hash, which is a chain. Jujutsu's op log is a DAG that merges.

**Recommendation.** Per-principal chains that merge into a DAG. Concurrent metadata edits, such as two clones changing the same standard, are conflicts stored as data. Anchoring applies to DAG heads.

### 2. Sync model

What replicates, what is private, what is ephemeral, and who runs the frontier.

**Recommendation.** One replicated graph. Visibility is per object: drafts and per-principal memory are private by default, landed changes and attestations are shared. Claims and workspaces are ephemeral and do not replicate. The hub is the frontier coordinator when present; the local daemon is when not. Offline agents reconcile on reconnect: expired claims, moved frontier, restack.

### 3. Immutability and change versions

Mercurial's phases: a changeset becomes immutable once public. Gerrit's patchsets: every version of a change is retained.

**Recommendation.** Landed changes are immutable. Correction is a new change. Every rewrite of an unlanded change keeps the prior version, linked by supersedes, queryable as the change's history.

### 4. Stacks

Dependent changes built on each other's unlanded work. Jujutsu and Sapling auto-rebase descendants when a change is rewritten, and offer split and absorb.

**Recommendation.** A change may depend on another. Rewriting a change restacks its descendants automatically at node granularity. Split-by-intent and absorb-into-stack are operations. Landing a stack lands in order, each under the standard.

### 5. Lines and backports

Long-lived lines besides trunk: release, LTS, hotfix. Cherry-picking a fix to a release branch.

**Recommendation.** A line is a named, long-lived sequence of landings with its own standard. Applying a change to another line is a semantic cherry-pick: node-level patch first, re-derivation from intent as fallback, verified under the target line's standard. Releases are objects: named, immutable, pointing at a snapshot, carrying a changelog generated from the intents landed since the previous release. Trunk landings get a monotonic number.

### 6. Redaction

Tamper-evident history collides with purging secrets and the right to erasure.

**Recommendation.** Fossil's shunning: a tombstone replaces the object's content while preserving its hash, so references stay valid and integrity checks pass with the tombstone recorded. Redaction is a signed op that requires a capability, and it is itself visible in the op log.

### 7. Tree entry metadata and tracking rules

File modes, executable bit, symlinks, line endings, encoding, case sensitivity, empty directories, generated files.

**Recommendation.** Tree entries carry mode and kind. Line endings are normalized on snapshot. Tracking rules are objects queryable by agents: is this path tracked, ignored, vendored, generated. Generated files carry their generator, so agents regenerate instead of editing, and a standard can forbid direct edits to them.

### 8. Large artifacts

**Decided: in.** Pointer objects and an external store adapter. Binary diff and merge stay out.

VISION.md excludes large binary handling. In the AI era model weights, datasets, and embeddings are part of the work, and environment snapshots in M8 depend on hashing large things.

**Recommendation.** Pointer objects with content hash, size, and an external store adapter, using content-defined chunking. A standard clause can require an exclusive claim before changing a non-mergeable file. Binary diff and merge remain out of scope.

### 9. The verb set

**Decided.** See VERBS.md. Final set: status, context, query, workspace, edit, snapshot, claim, remember, verify, try, promote, revert, undo. Chosen for what an agent would type unprompted; situate became status, pack became context, apply became edit, speculate became try, and undo was added as distinct from revert.

TENETS.md says roughly a dozen verbs. None are named.

**Proposed.**

| Verb | Does |
|---|---|
| situate | Where am I: task, claims, open questions, what changed since my cursor. Resume in one call |
| query | The query language over the graph, budgeted, with cursors |
| pack | A context pack for a scope or an intent, within a token budget |
| workspace | Create, list, or drop a workspace from any snapshot |
| apply | Edit: a text patch, a node replacement, or a semantic operation such as rename or move |
| snapshot | Record the workspace's state. Never blocks |
| claim | Claim or release nodes or paths, with expiry |
| remember | Record a memory of any kind, including a question |
| verify | Run verifiers and return the standard's status, clause by clause |
| speculate | Propose K alternatives, materialize, verify, rank |
| promote | Move a change to a stage or a target, subject to its standard |
| revert | Withdraw, unland, or roll back, with a task attached |

Administrative verbs outside the dozen: grant, revoke, standard, hook, channel, target.

### 10. Identifiers

**Recommendation.** Change IDs are short, prefix-unique, and drawn from an alphabet visibly distinct from content hashes, as Jujutsu does. Stable across rewrites. Snapshots and ops are content hashes. Every object has one canonical ID and the query language accepts unique prefixes.

### 11. Cost ledger

Tokens and compute per operation, attributed to intent, agent, and model.

**Recommendation.** Cost is an attestation kind recorded on every op that an agent runtime reports. "What did this intent cost" and "what does this agent cost per landed change" are queries. Capabilities already carry budgets; this is what they are measured against.

### 12. Intent graph

Intents need dependencies, priority, status, and ownership for "issues and roadmap become intents" to be true, and for swarm planning to schedule.

**Recommendation.** Intent objects carry parent, depends-on, priority, status, and assignee. Swarm planning reads the graph. Activity packs summarize by intent.

## Workflows to name

### 13. Search

Full-text, regex, symbol, and embedding search over nodes and history, budgeted. Git's pickaxe and forge code search. Belongs in the query language.

### 14. Remembered resolutions

Git's rerere. A conflict resolution is a memory kind that reapplies when the same conflict recurs.

### 15. Stash and interactive add

Stash is unnecessary: snapshot the workspace, switch, come back. Interactive add becomes split-by-intent. Both belong in the glossary as "what you do instead."

### 16. Glossary

The tenets say to align vocabulary with git. The mapping has not been written and the agent manual depends on it.

**Draft.**

| Git | Tessra | Note |
|---|---|---|
| repository | repository | |
| commit (noun) | change, snapshot | A change has a stable ID; a snapshot is content |
| commit (verb) | snapshot | Landing is what others see |
| staging area, index | none | |
| working directory | workspace | One per agent, copy-on-write |
| HEAD | the workspace's snapshot | |
| branch (long-lived) | line | Trunk, release, LTS |
| branch (feature) | change or stack | |
| merge | merge | Node-level |
| rebase | restack | Automatic for descendants |
| cherry-pick | apply to line | Semantic, with re-derivation fallback |
| revert | revert | |
| stash | snapshot | |
| tag | release | An object with a changelog |
| push, pull, fetch | sync | |
| remote | hub, peer | |
| hook | hook | Reactive only; blocking checks are verifiers |
| blame | blame | By node, with intent |
| bisect | bisect | Cached |
| log | query | |
| diff | diff | Node-level, budgeted |
| .gitignore, .gitattributes | tracking rules | Objects, queryable |
| submodule | cross-repository reference | |
| LFS | artifact pointer | |
| reflog | operation log | |
| notes | attestation, memory | |
| pull request | change at the proposed stage, with review | |
| CODEOWNERS, branch protection | standards | |
| CI | verifiers | |
| issue | intent | |

### 17. Defaults

A new repository needs a default standard, default hooks such as auto-revert on observe failure, and a default channel. Adoption dies if day one is "write a standard." Ship templates for common stacks.

### 18. Cross-repository changes

**Decided: in.** One graph, many roots. Atomic landing across roots when one coordinator holds their frontiers.

An agent changing an API and its consumers in two repositories, landed atomically. Decide whether one graph can have many roots. Recommendation: yes, a change may span roots, and landing is atomic across them when the same coordinator holds both frontiers.

### 19. Identity binding

Principals bound to GitHub, OIDC, or Sigstore identities, so a human's key is discoverable and the hub can map accounts to principals.

### 20. Legacy history

Imported git commits have no intents. Synthesize one per commit from the message, mark it legacy, and give it no weight in reputation.

## Operational

### 21. Scale targets and platform strategy

State numbers: repository size, history length, file count, concurrent agents. Lazy and partial fetch by default. A virtual filesystem for large repositories the way Sapling's EdenFS does it: ProjFS on Windows, FUSE or NFS elsewhere. Copy-on-write through ReFS and Dev Drive on Windows, APFS clonefile on macOS, reflink on btrfs and XFS, with a fallback when unavailable. Development is on Windows, so ProjFS and Dev Drive matter from M1.

### 22. Retention and garbage collection

Speculation produces losers, claims expire, attestations accumulate. GC must respect the op log and tombstones. Retention is per object kind and per standard.

### 23. Performance budgets and dogfooding

Numbers for workspace creation, pack generation, verification cache hit, landing latency, tested in CI as attestations. Tessra develops under Tessra, colocated in git, from M1.

### 24. Non-code content

Notebooks, data files, infrastructure-as-code, configuration. Indexers per format, chunk fallback for everything else.

### 25. Diff rendering for agents

Compact node diffs within a budget, moved-code detection, word-level changes inside a node, and a human rendering that is the same data.

## Disposition

Items 1 through 12 and item 18 are decided and folded into VISION.md, SECURITY.md, PIPELINE.md, PARALLELISM.md, LIFECYCLE.md, ROADMAP.md, and VERBS.md. Items 13 through 17, 19, and 20 remain open with their recommendations and become short sections in the existing documents once accepted. Items 21 through 25 become an operations document before M1 begins.
