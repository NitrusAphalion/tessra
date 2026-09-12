# Verbs

The agent surface is thirteen verbs. Each name was chosen by one test: it is the word an agent would type without reading a manual, and it does not reuse a git word with different semantics. Everything else is administrative and lives outside this set.

| Verb | Does | Why this name |
|---|---|---|
| status | Where am I: task, plan, claims, open questions, my change's standard status, what changed since my cursor. Resume in one call. | The first thing every agent types. Git's status is a subset of this. |
| context | A context pack for a scope or an intent within a token budget: nodes, dependents, covering tests, memories, claims, reviews. | "Context" is the word agents use for what they need to know. |
| query | The query language over the graph, budgeted, with cursors. Search included: text, regex, symbol, embedding. | The general read. |
| workspace | Create, list, or drop a workspace from any snapshot. | Jujutsu, Sapling, Cargo, and editors all say workspace. |
| edit | Change content in a workspace: a text patch, a node replacement, or a semantic operation such as rename, move, extract, add-parameter. | Every agent framework's mutation tool is called edit. Plain file edits through any other tool are fine; snapshot picks them up. |
| snapshot | Record the workspace's state as a snapshot, optionally naming or updating a change. Never blocks. | Not commit, because nothing is published. |
| claim | Claim or release nodes or paths, with expiry. Advisory. | |
| remember | Record a memory of any kind, including a question for a human. Recall happens through context and query. | |
| verify | Run the verifiers for a stage and return the standard's status, clause by clause, with what would satisfy each unmet clause. | Matches verifiers and attestations. |
| try | Propose K alternatives, materialize each in a workspace, verify, rank, return. Try-verify-keep in one call. | What an agent says for "attempt these and tell me which works." |
| promote | Move a change to a stage (proposed, landed, released) or to a target, subject to its standard. | Stages make promote the general word. Landing is the common case. |
| revert | Public inverse: unland a change, roll back a target, or withdraw a proposal. Opens a task. | Same meaning as git revert. |
| undo | Take back my own recent operations through the operation log. Private, no task. | Different from revert, and every agent expects it. |

## Administrative verbs

Outside the set because they are rare and gated by capability: init, grant, revoke, attest, standard, hook, channel, target, line, release, redact. Verifiers, judges, and external systems hold `attest`; task-scoped sessions hold it only for cost and self-descriptive kinds, never for approvals. Sync is not a verb. The daemon does it continuously and status reports it.

## Every response has the same shape

```
{
  "ok": true,
  "result": { ... },
  "state": {
    "workspace": "ws-7f2a",
    "snapshot": "b3e1...",
    "change": "kxmp",
    "stage": "snapshot",
    "claims": ["auth.py::login"],
    "standard": { "met": 7, "unmet": 2 },
    "cursor": "op-91c4"
  },
  "next": ["verify --stage landed", "remember --kind gotcha ..."],
  "budget": { "used": 1830, "limit": 4000 }
}
```

`state` is the state echo: what is true now, so the agent never has to remember or infer it. `next` is one or two suggested calls, which is in-context learning at no cost; over MCP they are tool calls. `budget.used` is what the verb produced against the budget it was given, and `budget.total` is the whole response with the state echo, so the cost of a call is never hidden. Errors carry a code, the unmet clauses if any, and a corrected call:

```
{
  "ok": false,
  "code": "STANDARD_UNMET",
  "message": "2 clauses unmet for stage landed",
  "unmet": [
    { "clause": "coverage(changed_nodes) >= 0.9", "actual": 0.71, "fix": "add tests covering auth.py::login, auth.py::refresh" },
    { "clause": "tests.new.fail_on_parent", "failing": ["test_login_rate_limit"], "fix": "the test passes on the parent; make it exercise the change" }
  ],
  "fix": "verify --stage landed --explain"
}
```

## Compound by default

Every verb accepts dry-run. Every mutating verb accepts an idempotency key, so a retry is safe. `try` and `verify` run their work in parallel inside one call. `snapshot` accepts a follow-on, so `snapshot --then verify` and `snapshot --then promote landed` make the common loop a single call.

## The manual

The agent manual is this table, the response shape, and the glossary. It stays under two thousand tokens, and it is the whole onboarding surface for the users who matter most.
