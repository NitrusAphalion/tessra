# Memory

Agents have no memory across sessions. Today every framework patches this with a hand-maintained file: CLAUDE.md, AGENTS.md, .cursorrules, copilot-instructions.md. Those files are flat prose loaded whole into context, curated by humans, duplicated per tool, silently stale the moment the code changes, and forked or lost across branches.

Tessra makes memory a first-class part of the version control system. Agents record what they learn by talking to the VCS. The VCS decides what to surface, when, and to whom. Humans set up nothing.

## What a memory is

A memory is a typed object in the graph, never a file in the tree.

| Field | Meaning |
|---|---|
| kind | fact, decision, convention, gotcha, preference, task, question, summary |
| scope | what it is about: a node, a path, a task, a principal, or the whole repository |
| body | the content. Short. Structured where the kind allows it |
| provenance | principal, model, change, and operation that recorded it, and when |
| anchor | content hash of the thing the memory describes |
| confidence | the recorder's confidence |
| trust | attestations on the memory itself: confirmed by a human, confirmed by another agent, contradicted, pinned |
| supersedes | an older memory this one replaces |
| expiry | optional |

### Kinds

- **fact.** Something true about the codebase that is not derivable from the code. "The payments client must never be called in tests; use the fake in testing/fakes."
- **decision.** A choice and its rationale, linked to the change that made it. Structured decision records without the ceremony.
- **convention.** How code is written here. Scoped to a path or a language.
- **gotcha.** Something that cost an agent time. "Tests need DATABASE_URL set or they hang." The highest-value kind and the one most often lost today.
- **preference.** How a human principal likes to work. Belongs to the principal and travels with them across repositories.
- **task.** Plan, progress, blockers, and open threads for an intent. The thing a fresh agent resumes from.
- **question.** Something an agent needs a human to answer. Questions form a queue.
- **summary.** System-maintained description of a node or module. The backbone of context packs.

## Where memory lives

In the graph, alongside history, outside the file tree. Consequences:

- No dotfiles. The tree stays clean.
- No merge conflicts on memory.
- Nothing lost or forked across branches and workspaces.
- Memory syncs with the repository. A fresh clone knows everything the agents learned.
- Every framework shares it. What one agent learns in one tool, every agent knows in every tool.
- Preferences sync with their principal, not with any one repository.

## Recording

One verb: `remember`. Zero setup. The agent manual says: when you learn something not derivable from the code, record it, with a scope and a confidence.

### Auto-capture

The system proposes memories from the operation log without being asked:

- Verification failed, then passed after an environment change: a gotcha.
- A human edited an agent's change: a preference or a convention.
- A review comment: a convention or a preference.
- A task completed: the task memory is closed and its lessons are kept.
- A question answered by a human: a fact.

Proposed memories are marked as proposed and rank below recorded ones until confirmed.

## Recall

There is no "load the memory file." Recall is a query, and it is budgeted.

- **Context packs include memory automatically.** A pack for "change function X" carries memories scoped to X, to X's module, to X's dependents, repository-wide conventions, the active task, and the requesting human's preferences. Ranked, deduplicated, cut to budget.
- **Explicit recall through the query language.** Memories by kind, scope, principal, trust, age, staleness.
- **Resume.** A fresh agent asks for its situation and gets the task memory, open questions, active claims, and what changed since the task's last cursor.

### Ranking

1. Pinned and human-confirmed.
2. Scope proximity to the request.
3. Confidence, and how often the memory has been recalled without being contradicted.
4. Stale memories last, and labeled.

## Staleness

Every memory anchors to a content hash. When the anchored content changes, the memory is flagged, not deleted. The next agent to touch that scope sees the flag and confirms, supersedes, or retires it. Memory decays into truth or into nothing, never into silent wrongness.

## Trust and safety

Memory is generated output. Tenet 3 applies: it is not trusted until confirmed, and its provenance is always visible. Tenet 10 applies harder: a memory is content the model will read as instructions, so a poisoned memory is a prompt injection with persistence.

- Every memory is rendered with its provenance.
- Writing repository-wide conventions requires a capability. An agent with a task-scoped capability can write task memories and scoped facts, not global rules.
- Humans can pin, veto, and edit. Pinned memories outrank everything.
- Containment covers memory. Revoking an agent retires its unconfirmed memories.

## Humans

Humans never write a memory file. They can:

- Read a generated view of what the agents know about this repository, by scope.
- Answer the question queue.
- Pin, veto, and edit.
- Export. One command renders memory into CLAUDE.md, AGENTS.md, or .cursorrules for tools that do not speak Tessra yet. The file is generated, never hand-maintained, and regenerated on every change.

Export is the migration path. Delete the hand-written file, and the agents lose nothing.

## Why this is the first wedge

Every developer using an agent has this problem today, and none of the rest of Tessra is needed to solve it. Memory works with path scoping before the semantic index exists. It needs the object store, provenance, and one verb. It ships in M1.

The pitch: your agents stop relearning the same things. Whatever one agent learns on Monday, every agent knows on Tuesday, in every tool, and nobody maintains a file.
