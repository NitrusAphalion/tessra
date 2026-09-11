# Risk

Risk is measured continuously, explained, and acted on. The risk monitor is a built-in verifier that runs on every operation and produces risk attestations for changes, scopes, targets, agents, and the frontier. Standards consume risk. Hooks react to it. Humans see it as a document.

## What is measured

**Per change**

- Blast radius: nodes affected, dependents, covering tests.
- Path sensitivity, as declared in standards.
- Size and shape: nodes touched, lines, new dependencies, new patterns.
- Verification state: attestations missing, judge confidence, judge disagreement.
- Test signal: coverage on changed nodes, new tests failing on the parent, tests modified.
- Collision: other agents' concurrent edits on the same nodes.
- Author reputation in this scope.

**Per scope** (path, module, or node set)

- Churn and velocity.
- Open conflicts and tasks.
- Flaky test rate and coverage trend.
- Stale memory count.
- Historical defect rate, from blame on reverts.
- Time since the last full-suite attestation.
- Security findings, dependency vulnerabilities, license changes.

**Per target**

- Observed signals against baseline: error rate, latency, saturation, business metrics.
- Drift since the last promotion.
- Risk budget remaining.

**Per agent**

- Anomaly signals: writes outside claims, unusual volume, repeated failed gates, attempts to weaken tests, attempts to write global memories without capability, tool-call patterns that differ from the agent's history.
- Reputation trend.

**Frontier**

- The aggregate of the above, plus landing rate, revert rate, and exception rate.

## Scoring

Each signal produces a score with a contribution. The total is explainable: every risk attestation lists its top factors and what would lower each. An agent asking why its change is high risk gets a list it can act on: split the change, add tests to these nodes, wait for the conflict on that node to resolve. This is tenet 5 applied to risk.

Weights are configuration, versioned like standards, and can be scoped.

## Consuming risk

- **Standards branch on it.** When risk is high, require a human. When it is medium, require more judges.
- **Adaptive tightening.** The effective standard for a scope tightens when the scope's risk rises, without anyone editing it. A scope with a rising defect rate gets more judges automatically.
- **Swarm planning uses it.** High-risk scopes get fewer concurrent agents and stricter claims.
- **Hooks subscribe to threshold crossings.**

## Responses

- **Circuit breakers.** Pause landing in a scope or on the frontier when risk crosses critical. Resume when it falls, or when a human says so through a channel.
- **Risk budgets.** A target has a budget that landings consume in proportion to their risk. When the budget is spent, promotion requires a human until it refills. An error budget made concrete.
- **Containment.** An agent with anomaly signals is throttled, then revoked. Its unlanded work and unconfirmed memories are unwound.
- **Recall by model.** A model's reputation falls, risk rises on everything it wrote that is still unconfirmed, and hooks open re-verification tasks.

## The risk pack

No dashboard. A risk pack is a document at any altitude: the frontier right now, one scope over the last week, one change, one agent. Delivered through a channel on a cadence or on demand. "What is risky right now and why" is one question.

## Why this belongs in the version control system

Every signal above is already in the graph: blast radius from the semantic index, verification state from attestations, collisions from claims, defect history from blame, reputation from outcomes, production signals from observers. Nothing outside the VCS has this view, which is why risk tools today are bolted on and blind to most of it.
