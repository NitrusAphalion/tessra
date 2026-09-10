# Hooks

Standards gate. Hooks react. A hook is a versioned, scoped subscription to an event in the operation log, with an action. It is how Tessra ties into anything outside itself, and how automation inside it is expressed.

## Why not git hooks

Git hooks are shell scripts in an unversioned directory at a handful of fixed points. They block, they cannot be inspected, they carry no provenance, and they run as whoever invoked git. Agents cannot read them, and nobody can ask why one fired.

## Events

The operation log is the event stream. Every op is an event, so any stage transition and any mutation can be subscribed to.

- Stage transitions: snapshot, proposed, verified, landed, released, observed, and their failures.
- Attestation produced. Standard unmet. Standard changed.
- Conflict opened or resolved. Task opened or closed. Question asked or answered.
- Claim made, expired, or overlapped.
- Memory recorded, flagged stale, confirmed.
- Risk threshold crossed. Anomaly detected.
- Principal revoked. Capability changed.
- Target promoted or rolled back.
- Hook run succeeded or failed.

Events carry the full object: the change with its intent, attestations, blast radius, and risk.

## Definition

A hook is data, versioned in the graph, with a scope and a principal. The syntax below is illustrative; the shape is what matters.

```
hook notify-ci {
  on    proposed
  where scope("services/**")
  do    webhook("https://ci.example.com/run", payload: change)
  as    principal("ci-bridge")
}

hook auto-revert-prod {
  on    observed.fail
  where target("prod")
  do    revert(target: "prod")
        task("investigate revert", attach: event)
        notify(channel: "oncall")
}

hook resolve-conflicts {
  on    conflict.opened
  do    agent(intent: "resolve this conflict", context: event, budget: 20000)
}

hook root-cause {
  on    target.rolled_back
  do    agent(intent: "find the cause and record a gotcha", context: event)
}

hook fill-coverage {
  on    risk.crossed
  where signal("coverage.floor") and scope("core/**")
  do    agent(intent: "add tests for the uncovered nodes", context: event)
}
```

## Actions

- **webhook.** Call an external system with a signed payload.
- **verify.** Run a verifier now.
- **attest.** Record an attestation under the hook's principal.
- **promote, revert.** Move a change or a target between stages, subject to the standard.
- **task, question, notify.** Open a task, ask a human through a channel, send a message.
- **remember.** Record a memory.
- **claim, release.** Adjust claims.
- **agent.** Spawn an agent with an intent, a context pack built from the event, and a budget.
- **pause, resume.** Circuit breakers on landing in a scope or on a target.
- **revoke.** Containment.

Actions run with the hook's principal and its capabilities, never more. Creating or changing a hook that runs as a principal requires being that principal, being an owner, or holding a delegable capability that contains everything that principal holds, so a hook can never be a way to borrow authority.

## Standards gate, hooks react

A hook never blocks a promotion. If a check must block, it is a verifier referenced by a standard. This keeps the pipeline's truth in one place: what is required lives in standards, what happens next lives in hooks. A hook can cause a promotion to fail only indirectly, by producing an attestation a standard consumes.

## Delivery

- Every hook run is an op, with the triggering event, the outcome, and the duration. "Why did this fire" and "why didn't it" are queries.
- At-least-once delivery keyed by op ID. Actions are idempotent under that ID.
- Failures open a task and are retried with backoff. Nothing is dropped silently.
- Hooks run asynchronously by default. Ordering within one event is declared, never assumed.

## Tying into existing CI and CD

Nobody has to abandon their pipeline on day one.

- **Outbound.** A hook on proposed or landed calls the CI system with the snapshot to test.
- **Inbound.** The CI system posts its result as an attestation through the attestation API, using a principal issued for it. The standard consumes it like any other attestation.
- **Deployers.** A hook on released calls the existing deploy tool, which reports back as a deployment record.
- **Observers.** Monitoring systems post observations as attestations.

Over time the same checks can move inside Tessra as verifiers, node-scoped and cached. Until then, the external system is a verifier that lives elsewhere.

## Reverting at any stage

- **Proposed.** Withdraw. The change is hidden again.
- **Landed.** Unland. A new landing inverts the change on the frontier at node granularity, attributed to both the original and the reverting principal. Dependents that landed on top are checked, and any that break become tasks.
- **Released, target.** Rollback. Promote the previous snapshot to the target.
- **Memory.** Retire. Memories recorded by the reverted change are flagged.

Every revert opens a task with the reason attached. The original intent stays open so the work can be redone.

## Agentic hooks

The important actions are agents. A hook that spawns an agent with an intent and a context pack turns every event into a place where work can be delegated: conflicts resolved, coverage floors filled, root causes recorded, stale memories re-verified, dependency updates proposed. This is how the system runs itself. Agentic hooks work under the same standards as every other agent, with budgets, and their work lands only if it meets the standard.
