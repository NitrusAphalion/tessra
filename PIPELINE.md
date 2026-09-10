# Pipeline

Agents generate code faster than humans can read it. Every team is hitting the same wall: the review queue. Tessra solves it by changing what humans review. People define standards once, the system proves every change against them before it lands, and humans review exceptions and samples instead of diffs. Human attention scales with the exception rate, not with the code rate.

## The three objects

**Verifiers produce attestations.** A verifier is anything that examines a snapshot and signs a fact about it: a test runner, a type checker, a linter, a coverage tool, a security scanner, a benchmark, a judge agent, a human. Each attestation has a kind, a result, the verifier's principal, the content hash it applies to, and the environment fingerprint. Attestations are cached by content hash. The same content is never verified twice.

**Standards consume attestations.** A standard is a declarative predicate over attestations that people define and version in the repository. It says what "good enough" means, scoped by path, node, risk, or stage. It is data, not a shell script, so agents can read it, humans can review it, and the system can tell an agent exactly which clause is unmet.

**Stages are gated by standards.** A change moves through stages: snapshot, proposed, verified, landed, released, observed. Each promotion requires the standard for that stage. Promotion is the block. Nothing else is.

Landed changes are immutable. Correction is a new change. Before landing, every rewrite of a change keeps its prior version, linked by supersedes, so a change's own history is queryable.

## Where the block sits

Snapshots never block. An agent can always record the state of its workspace, so work is never lost and undo always works. The block is on promotion: proposing a change to others, landing on trunk, releasing. To everyone else on the team, landing is what "commit" means, and that is where the standard is enforced. A standard can gate proposal too, so a change is not even visible to others until it meets a minimum bar.

## Proving

An agent does not assert that its code meets the standard. It asks the system to verify, the system runs the verifiers in a sandbox, and the daemon-side runner signs the attestations, so code under test cannot attest to itself. The agent's own claims carry no weight. Judged clauses are run by judge agents that are distinct from the authoring agent, and a standard can require that judges use a different model from the author.

When the agent asks to promote, the response is the standard's status: every clause, satisfied or not, and for each unmet clause what would satisfy it. The agent loops until the standard is met. The block is not a wall. It is the loop's driver.

## Example standard

The surface syntax is undecided. What matters is that a standard is declarative, scoped, versioned, readable by agents and humans, and that every clause maps to an attestation kind.

```
standard trunk {
  require tests.pass
  require typecheck.clean
  require lint.clean
  require coverage(changed_nodes) >= 0.9
  require intent.linked
  require change.touches_only_claimed_nodes
  require every_changed_node.has_covering_test
  require tests.new.fail_on_parent
  require judge("matches-intent")      { judges: 2, distinct_models: true, min_confidence: 0.8 }
  require judge("follows-conventions") { source: memory.conventions }
  forbid  new_dependency unless approved(human, capability: "deps")
  forbid  test.weakened unless approved(human)
  forbid  test.skipped
  forbid  secrets

  scope "payments/**" {
    require approved(human, role: "payments-owner")
    require judge("security-review") { judges: 2 }
  }
  scope "docs/**" {
    only lint.clean
  }
  when risk >= high {
    require approved(human)
  }
}

standard release extends trunk {
  require benchmark.within_budget
  require observed(canary, minutes: 30, error_rate < baseline)
}
```

## Kinds of clauses

- **Deterministic.** Tests, types, lint, build, coverage per changed node, new tests failing on the parent snapshot, tests weakened or skipped, secrets, dependency changes, binary size, benchmarks. Cheap, cached, no judgment involved. See TESTING.md.
- **Structural.** Linked to an intent. Touches only claimed nodes. Blast radius under a limit. Size under a limit. No unrelated changes.
- **Judged.** A model applies a rubric: matches the intent, follows the conventions in memory, adequate error handling, documentation updated, no obvious security issue. Stochastic, so the standard controls how many judges, whether they must be distinct models, and the confidence threshold. Judges are verifiers, and their attestations carry the model in provenance.
- **Process.** Approved by a human with a role. Reviewed by N principals. The question queue for this change is empty. Reproduced independently by a second agent.
- **Reputation.** The authoring agent's track record in this scope is above a threshold.

## Review by exception

Humans review four things. None of them is every diff.

1. **Standards.** The highest-leverage review that exists: define once what good means. Changing a standard is itself a change with its own standard.
2. **Exceptions.** Changes that fail a judged gate, exceed a risk threshold, or hit a scope that requires a human are routed to a queue with the intent, the blast radius, the attestations, and the judges' reasoning. The human sees why it is in front of them.
3. **Samples.** A standard can set an audit rate per scope. A fraction of auto-landed changes are sampled for human review. Findings feed reputation and calibrate the judges.
4. **Activity.** What landed while I was away, at any altitude, with drill-down.

Humans can still review any change directly. Review comments are objects scoped to nodes, suggested edits are changes, and approval is an attestation that standards can require.

## Hooks

Standards gate. Hooks react. A hook is a versioned subscription to an event in the operation log with an action: call a CI system, open a task, ask a human, spawn an agent, promote, revert, pause. A hook never blocks promotion. A check that must block is a verifier a standard references. Existing CI and deploy tools plug in through hooks as verifiers and deployers that live elsewhere. See HOOKS.md.

## Risk routing

Every change gets a risk score from blast radius, path sensitivity, size, novelty, judge confidence, the author's reputation in the scope, and the historical defect rate of the area. The score is computed continuously by the risk monitor and every score is explained. See RISK.md. Standards branch on risk. Low risk lands automatically. Medium risk gets more judges. High risk gets a human. Risk is how a fully automated flow stays automated for most changes and stays safe for the rest.

## Reputation

Outcomes after landing feed back: reverts, audit findings, defects traced by blame, judge disagreements. Reputation is tracked per principal, per model, per scope. Standards can reference it, so an agent that has earned trust in one area is gated less there. Trust compounds and verification gets cheaper as it is earned. Recall by model uses the same records.

## Observe and revert

Landing is not the end. An observe stage watches canaries, error rates, or any signal a verifier can produce, and its standard can auto-revert with a task attached. Because reverts are semantic and every operation is undoable, automated landing is a smaller risk than it is in git.

## The fully automated flow

Intent, then swarm planning, then agents in workspaces, then verify, then judge, then land, then observe, then revert if needed. Humans see activity packs and the exception queue. Human attention scales with exceptions and audit samples, not with the volume of code. That is the throughput fix.

## What this replaces

CI configuration files, branch protection rules, CODEOWNERS, review checklists, and the merge queue. All of them become standards, verifiers, and stages inside the repository, readable by agents and enforced by the system.
