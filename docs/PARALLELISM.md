# Parallelism

Tessra is built for massive parallel work: many agents on one repository, many agents in one file, continuously, without a human merge queue. This document says what makes that possible, what still conflicts, and what happens when it does.

## Why git cannot do this

Git's unit is the file and its merge is a three-way line merge. Two edits near each other conflict. Two edits on one line always conflict. A reformat conflicts with everything. Two agents each adding an import conflict. Parallelism inside a file is bounded by line locality, which is why teams partition work by file and serialize through pull requests. That ceiling is the file, and agents hit it immediately.

## The unit is the node

Files are projections of nodes: functions, types, imports, statements at the top level, and the equivalent in every supported language. Two agents editing different nodes commute. Merge cost is proportional to nodes touched, not to file size or to the number of concurrent editors. A file with fifty concurrent editors is still one projection, regenerated on materialization.

## What never conflicts

1. **Different nodes.** The common case. Two functions in one file are as independent as two files.
2. **Ordering.** Nodes have identities and position hints, not line numbers. A deterministic rule places concurrent insertions. Two agents each appending a function both land, in a stable order.
3. **Set-like regions.** Imports, enum variants, match arms, registries, dependency lists, test tables. Recognized per language and merged as sets. Duplicates are deduplicated.
4. **Formatting.** Node identity is format-insensitive. Canonical formatting is applied on materialization. A whole-file reformat is a no-op to the merge.
5. **Refactorings recorded as operations.** Rename, move, extract, inline, add-parameter, add-import. When an agent performs one of these through Tessra, it is recorded as a semantic operation, not as resulting text. On merge, the operation is applied to other agents' concurrent edits. A rename in one change and a new call to the old name in another merges to the new name. Text replacement remains the fallback for everything else.

## What conflicts, and what happens

- **Same node, both edited.** A real collision. It becomes a conflict task with both intents attached. Resolution strategies in order: statement-level merge inside the node where the edits touch different statements; a resolver agent given both intents and a context pack; a tournament where both versions are verified and the passing one wins; a human decision. Nobody else waits.
- **Semantic interaction.** Both changes verify alone and the union fails. The frontier detects it at landing. It becomes a task attributed to both changes.
- **Signature change without a recorded operation.** Detected through the dependents index as a semantic conflict. Resolved as above.

In every case the conflict is stored as data, attached to intents, and resolved asynchronously. A conflict never stops the frontier and never stops any other agent.

## Stacks

Agents build on each other's unlanded work. A change may depend on another change, and a chain of them is a stack. When a change in a stack is rewritten, its descendants restack automatically at node granularity, so nobody rebases by hand. Split-by-intent divides one change into several, and absorb folds a fix into the change in the stack it belongs to. A stack lands in order, each change under the standard, and a failure partway leaves the rest stacked on the new frontier.

## Nothing locks

Concurrency control is optimistic. Claims are advisory and expire. A claim routes work: an agent asking for a context pack on a claimed node is told who holds it and what their intent is, and may proceed anyway. The system never blocks a write on a claim. Locks would reintroduce the serialization the whole design exists to remove.

## Swarm planning

Massive parallelism needs partition, not just merge. Given an intent, the system uses the semantic graph and the blast radius index to propose N sub-intents over disjoint node sets, assigns claims, and hands each to an agent. Coupled nodes go to the same agent. This is how fifty agents work productively in one file: the partition is by node, not by file, and the system draws the boundaries.

## Incremental verification

The frontier cannot run the full suite for every landing. Coverage maps tests to nodes and is recorded as attestations. Landing a change runs only the tests that cover the nodes it touched plus the tests that cover their dependents. Full-suite runs happen on a cadence and attest the whole frontier. The verification cache makes repeated verification of unchanged content free. Without this, throughput is bounded by suite time; with it, throughput is bounded by the number of nodes changing.

## The frontier

Trunk is a sequence of landings. Each landing merges a change onto the frontier at node granularity, runs incremental verification, attests, and advances. Only the final advance is serialized. Merging and verifying many candidates proceeds in parallel. A landing whose union fails verification is rejected into a conflict task, not lost, and the agent is told why.

## Scale mechanics

- Workspaces are copy-on-write and created in constant time.
- The semantic index is incremental. Only changed files re-parse.
- The object store is append-only. The operation log is the only serialization point.
- Merge is proportional to nodes touched.
- Materialization regenerates files from nodes on checkout.

## The next level down

Statement-level identity within a node, so two agents editing different branches of one function still merge without a conflict task. This is the same matching problem one level deeper. It is scheduled after node-level matching is solid, and the same fallback chain applies until then.
