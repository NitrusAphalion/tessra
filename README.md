<h1 align="center">Tessra</h1>

<p align="center"><strong>The version control system for the era of AI.</strong></p>

<p align="center">
Git records what changed. Tessra records what changed, <em>why</em>, and <em>what is known to be true</em> about the result.<br>
Built for agents that write code around the clock, and for the humans who decide what to trust.
</p>

<p align="center">
<a href="#quick-start">Quick start</a> ·
<a href="#how-agents-use-it">How agents use it</a> ·
<a href="#what-you-get">What you get</a> ·
<a href="#architecture">Architecture</a> ·
<a href="#status">Status</a> ·
<a href="#design-documents">Design documents</a>
</p>

<p align="center">
<img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue">
<img alt="Rust 1.80+" src="https://img.shields.io/badge/rust-1.80%2B-orange">
<img alt="Status: pre-release" src="https://img.shields.io/badge/status-pre--release-yellow">
<img alt="Interface: CLI and MCP" src="https://img.shields.io/badge/interface-CLI%20%2B%20MCP-brightgreen">
</p>

---

## Why Tessra

Code is no longer scarce. Agents write most of it, in parallel, and they never sleep. That breaks the assumptions every version control system was built on: one author at a time, one working directory, a human reading text output, and a human on hand to resolve conflicts.

The bottleneck has moved from writing code to knowing what to trust, why it exists, and how to coordinate many authors. Tessra is built for that bottleneck:

- **Nothing blocks.** A conflict, a failed check, or a missing review is a state stored in the repository, never an error that stops an agent.
- **Nothing lands unproven.** Landing is gated by a standard: a rule set the owner writes as data, satisfied only by signed attestations from verifiers. An agent's own claim that the tests pass carries no weight.
- **Files are a view.** The repository is a graph of semantic units, history, intents, and memory. Two agents editing different functions of one file never conflict.
- **Agents are the interface.** There is no UI. Thirteen verbs over a CLI and MCP, every response in one machine-readable shape, every read within a token budget.
- **Git stays where it is.** Tessra runs colocated with an existing git checkout and bridges losslessly in both directions, so a project can adopt it without leaving GitHub.

## What you get

| | |
|---|---|
| **Semantic merge** | Tree-sitter parses Rust, Python, JavaScript, and TypeScript into units with identities that survive edits and renames; other files fall back to chunks. Landings merge at unit granularity. A rename in one change is carried into a concurrent change that still calls the old name. Twenty agents editing one file at once all land. |
| **Proof, not promises** | `verify` runs the verifiers the daemon detects (`cargo test`, `pytest`, `npm test`, or any command you configure) in a scratch copy with a stripped environment, and signs the result as an attestation cached by content and toolchain. When every changed unit has a covering test, only those tests run. |
| **Standards as data** | `tessra standard --require 'attest(tests.pass)' --require 'structural(changed.covered)' --forbid 'structural(test.weakened)'`. Clauses can require new tests to fail on the parent, a human above a risk level, or agreeing judges. A refusal names the unmet clause and what would satisfy it. |
| **Memory in the repository** | `remember` records gotchas, decisions, conventions, and questions as signed objects scoped to a unit, path, or intent. `context` recalls them next to the code. `export` renders them as `AGENTS.md` or `CLAUDE.md` for tools that do not speak Tessra. |
| **Context packs** | "What do I need to know to change X, in N tokens": the unit's text, what it depends on, what depends on it, the tests that cover it, memories about it, overlapping claims, and who last changed it. |
| **Swarms without a merge queue** | `plan` partitions an intent across agents by dependency blast radius and hands each a scoped, budgeted capability. The frontier lands every verified change, on demand or continuously through a hook. Fifty agents in the same files at once; tens of thousands of landings per hour on a small crate. |
| **Speculation** | `try` materializes several candidate edits as revisions of their own, verifies each, scores risk, and ranks them. Keep the best in one call. |
| **Humans by exception** | Approvals, judged exceptions, and questions reach people through channels the daemon owns, and a reply is a key-signed attestation the agent never touched. Ask what happened in the last hour at the altitude you want. |
| **A risk monitor** | Every change carries a score with its factors and what would lower each. Agents that write outside their claims or scope are throttled, then revoked, and their unlanded work is unwound in one op. |
| **Intent to production** | Targets, releases, canary slices, observers, and automatic rollback when an observed signal trips the target's standard. Tessra is the control plane; your deployer runs the containers. |
| **The git bridge** | `export --format git` turns landed trunk revisions into a clean linear history, one commit per landing authored by its agent. `import --branch` lands the commits your teammates made with git. |
| **State that travels** | A snapshot can carry the toolchain, lockfile hashes, and build state as trees, so checking out an earlier revision runs with no install step. |
| **Signed by default** | Every principal has a key; every op is signed, chained, and checked against a capability the verifier walks back to the owner. History is tamper-evident. |

## Quick start

### Install

Prebuilt binaries for macOS, Linux, and Windows, on x86_64 and arm64, are attached to every [release](https://github.com/NitrusAphalion/tessra/releases). The installers put `tessra` in `~/.local/bin` (`%USERPROFILE%\.local\bin` on Windows) and add it to your `PATH`. Tessra needs `git` on the `PATH`.

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/NitrusAphalion/tessra/releases/latest/download/tessra-cli-installer.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/NitrusAphalion/tessra/releases/latest/download/tessra-cli-installer.ps1 | iex"
```

Every release also carries `.tar.xz` and `.zip` archives per platform, a `sha256.sum` file, and a source tarball.

To build from source instead you need Rust 1.80+ (stable) and git. On Windows, Visual Studio Build Tools with the C++ workload, which the tree-sitter grammars need. See [DEVELOPING.md](DEVELOPING.md) for toolchain details.

```sh
git clone https://github.com/NitrusAphalion/tessra
cd tessra
cargo install --path crates/tessra-cli
```

### First steps

Then, inside any git checkout:

```sh
tessra init                                 # imports HEAD and the last 100 commits as trunk
tessra status --pretty                      # where am I: change, stage, standard, what changed
tessra context --path src/lib.rs --budget 2000
tessra remember --kind gotcha --body "the build needs GOFLAGS=-mod=mod" --scope-kind path --scope-ref backend
```

An agent's loop, as an agent named `bot`:

```sh
tessra --agent bot workspace --action create
tessra --agent bot --workspace <id> edit --path src/x.rs --content "..."
tessra --agent bot --workspace <id> snapshot --title "handle the empty case" --then verify
tessra --agent bot --workspace <id> promote --to proposed
tessra promote --to landed --all            # the owner lands everything that meets the standard
tessra export --format git --branch main    # landed revisions become commits
```

Add `.tessra/` to your `.gitignore`. The object store and keys live under your local application data directory, never in the tree.

### Claude Code and other MCP clients

`tessra mcp` serves the thirteen verbs over stdio as `tessra_status`, `tessra_context`, `tessra_edit`, and so on. For Claude Code, add to `.mcp.json`:

```json
{
  "mcpServers": {
    "tessra": { "type": "stdio", "command": "tessra", "args": ["--agent", "claude", "mcp"] }
  }
}
```

The session acts as agent `claude`, proposes changes, and the owner lands them.

## How agents use it

Every session follows one loop, and the repository remembers everything between sessions so the agent does not have to.

1. **`status`** first: your assignment, claims, open questions, your change's standard status, and what changed since you last looked.
2. **`context`** for what you are about to touch, within a token budget.
3. **`workspace`** if you do not have one. It is yours alone.
4. **`edit`**, then **`snapshot`**. Snapshot never blocks and never publishes; problems come back as flags.
5. **`verify`** before claiming anything. It returns the standard clause by clause, with what would satisfy each unmet one.
6. **`promote`** to land. If the standard is unmet, the response says exactly which clauses and how to fix them.
7. **`remember`** what you learned that is not in the code.

Every response has the same shape, so an agent reads state instead of inferring it:

```json
{
  "ok": true,
  "result": { "...": "..." },
  "state": {
    "workspace": "onqvqttk", "change": "qywskrnr", "stage": "proposed",
    "claims": ["auth.py:login"], "standard": { "met": 2, "unmet": 1 },
    "trunk": { "head": "dc1ef461", "seq": 99 }, "cursor": "755b5ef3"
  },
  "next": ["verify", "remember --kind gotcha ..."],
  "budget": { "used": 1830, "limit": 4000 }
}
```

Errors carry a `code`, the `unmet` clauses, and a `fix` you can run. Every mutation takes an idempotency key, so a retry never happens twice, and `undo` takes back your own recent operations.

The thirteen verbs are `status`, `context`, `query`, `workspace`, `edit`, `snapshot`, `claim`, `remember`, `verify`, `try`, `promote`, `revert`, and `undo`. Administrative verbs such as `standard`, `hook`, `grant`, `target`, and `release` are owner-gated. [VERBS.md](VERBS.md) explains each name; [spec/06-manual.md](spec/06-manual.md) is the whole manual for an agent, in about a thousand tokens.

## Architecture

```
  CLI / MCP client ──► daemon (loopback, per repository) ──► object store + op log + view
                          │
                          ├── workspaces: one directory per agent, materialized in milliseconds
                          ├── verifiers: run as the daemon, in a scratch copy, output signed
                          └── hooks, channels, deployers: how the repository acts on the world
```

| Crate | Holds |
|---|---|
| `tessra-core` | Canonical CBOR, BLAKE3 identities, Ed25519 signatures, the object catalog, the persistent trie |
| `tessra-oplog` | Views, effects, deterministic merge, the op DAG and its verification, standards, undo |
| `tessra-store` | The redb object store, heads, the idempotency index, the pack format |
| `tessra-semantic` | Tree-sitter extraction, format-insensitive body hashes, history-aware unit identity, renames as data, node-level three-way merge |
| `tessra-daemon` | Repository layout, workspaces, principals and sessions, the verbs, verifiers, hooks, risk, swarm, delivery, the git bridge, the loopback server |
| `tessra-cli` | The `tessra` command and the MCP server |

A few decisions that shape everything else, argued in [VISION.md](VISION.md) and settled in [spec/](spec/README.md):

- **Snapshots with stable change IDs**, not patch theory. Semantic merge is layered above.
- **An operation log** of signed, chained effects; every replica re-evaluates a landing's standard against the attestations it cites, so the hub can be untrusted storage.
- **Blobs are the source of truth**; the semantic index is derived and cached, so matching can improve without a format migration.
- **Budgets everywhere.** No read is unbounded; every response reports what it used.
- **Daemon-first.** The CLI is a thin client; the daemon opens the store once, keeps sessions in memory, and exits when idle.

## Status

Tessra is pre-release and under active development. Every milestone on the [roadmap](ROADMAP.md), M0 through M8, has a passing end-to-end demo: substrate, semantic merge, standards and verification, swarms, the agent surface, production, the git bridge, and state bundles. The repository is developed under Tessra, and it is being dogfooded on a real product monorepo.

What that does and does not mean today:

- **Single machine.** The hub and multi-machine sync are specified but not built. One daemon serves one repository over loopback.
- **Verifiers run as the daemon** with a stripped environment, not in a container. Secrets are flagged at snapshot, not kept out of the store.
- **Git export is linear** and landing merges are not exported as git merges. Import is first-parent. Submodules, signed commits, notes, and tags do not round-trip.
- **Grammars:** Rust, Python, JavaScript, TypeScript. Everything else is chunk-matched, which still merges but with coarser identity.
- **Platform:** developed and tested on Windows 11. macOS and Linux paths exist and are untested.
- **Performance budgets** from the spec, such as `status` under 50 ms, a context pack under 500 ms warm, and a verification cache hit under 10 ms, are measured by `tessra bench` and recorded as signed attestations.

The full list of deviations from the spec is in [spec/README.md](spec/README.md). Bugs found while dogfooding are logged in [BUGS.md](BUGS.md).

## Design documents

Start with [VISION.md](VISION.md). The rest are companions it links to.

| Document | What it covers |
|---|---|
| [VISION.md](VISION.md) | Thesis, principles, the thirteen bets, architecture, decisions |
| [TENETS.md](TENETS.md) | Constraints derived from how models work. The tiebreaker when scope is argued |
| [ADOPTION.md](ADOPTION.md) | Why git won and how Tessra wins |
| [ROADMAP.md](ROADMAP.md) | Milestones M0 through M8, each defined by a demo |
| [MEMORY.md](MEMORY.md) | Memory as typed objects in the graph. Agents record and recall through the VCS |
| [PARALLELISM.md](PARALLELISM.md) | Many agents in one repository and one file, without a merge queue |
| [PIPELINE.md](PIPELINE.md) | Standards, verifiers, stages, and review by exception |
| [TESTING.md](TESTING.md) | Tests as nodes, coverage as a node-level relation, tests as proof |
| [LIFECYCLE.md](LIFECYCLE.md) | No screens, agents as the interface, channels, intent to production |
| [HOOKS.md](HOOKS.md) | Subscriptions to the operation log with actions, including agents |
| [RISK.md](RISK.md) | The continuous risk monitor and what it drives |
| [SECURITY.md](SECURITY.md) | Threat model, keys, signed ops, capabilities, verifier isolation |
| [VERBS.md](VERBS.md) | The thirteen agent verbs, the response shape, and the manual |
| [GAPS.md](GAPS.md) | The sweep against git and competing systems: what was decided and what remains open, with a draft glossary |

The specification lives in [spec/](spec/README.md): conventions, the object catalog, operations and sync, security, on-disk layout, and the agent manual. When a design document and the spec disagree, the spec wins and the design document gets fixed.

## Developing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

[DEVELOPING.md](DEVELOPING.md) covers the toolchain, the crate layout, every verb with examples, and the daemon. Found a bug? Log it in [BUGS.md](BUGS.md); a session working on this repository triages the open entries first.

## License

MIT.
