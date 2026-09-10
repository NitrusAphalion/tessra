# Lifecycle

Tessra has no user interface and does not need one. Agents are the interface. Everything a screen would show is a query, everything a screen would let you do is a verb, and every human-facing artifact is a document generated on demand at the altitude the reader asked for, delivered wherever they already are. The same substrate carries work from intent to production.

## No screens

A fixed screen is a guess about what someone needs to see. A model can render the exact document a question calls for, for that reader, at that moment. So Tessra ships no UI. It ships a query language, packs, and channels. Anyone can build a visual layer on the query language. Nothing in Tessra depends on one.

What people are used to, and what it becomes:

| Screen | In Tessra |
|---|---|
| Repository browser | A context pack or a query |
| Pull request page | A change with its intent, attestations, blast radius, and review objects |
| Pull request list | The exception queue and the activity pack |
| CI dashboard | Attestations on the frontier |
| Blame view | A query |
| Branch protection, CODEOWNERS | Standards |
| Deployments page | Target objects and their history |
| Issues, tickets, roadmap | Intents, tasks, and questions, with dependencies |
| Settings | Principals, capabilities, standards, channels |

## Agents are the interface

A human asks their agent. The agent queries Tessra and renders the answer. "What landed overnight" is an activity pack. "Why is this in my queue" is an exception with its reasoning attached. "Roll back production" is a verb gated by capability. The human never leaves the tool they already use: a terminal agent, a chat channel, email.

## Humans decide through channels the system owns

Some decisions belong to humans: approvals a standard requires, answers to the question queue, a rollback. If an agent could relay those, an agent could forge them, and every standard that requires a human would be theater. So the system asks, not the agent.

A channel is a configured path from Tessra to a human and back: a chat workspace, email, a terminal prompt, a push notification. The daemon delivers the exception or question through the channel, the human replies there, and the daemon records the reply as an attestation signed with the human's principal. The agent sees the outcome. It never sits between the human and the record. Tenet 10 holds without a UI.

## From intent to production

The stages already gate promotion. Production adds two objects and one stage's worth of verifiers.

**Targets.** A target is somewhere a snapshot can run: staging, a canary slice, production, a preview per change. It records which snapshot it runs, since when, promoted by whom, under which standard.

**Deployments.** A deployment is the act of pointing a target at a snapshot. Deployers are adapters, like verifiers: Kubernetes, a platform host, a container registry, plain SSH. Tessra is the system of record and the control plane. It never needs to be the thing that runs the code.

**Release standards.** Promotion to a target is gated by that target's standard, the same way landing is gated by trunk's. A production standard can require the trunk standard plus a benchmark, plus a soak on the canary slice, plus a human for high risk.

**Observe.** Observers are verifiers that read production signals: error rates, latency, saturation, business metrics, synthetic checks. Their attestations feed the observe stage. A standard can promote automatically when the signals hold and revert automatically when they do not, with a task attached explaining what tripped.

**Progressive delivery.** A standard with steps: a canary at one percent for ten minutes under an observe clause, then ten percent, then everyone. Each step is a promotion. Each promotion is gated.

**Rollback.** Promotion to the previous snapshot. Semantic, cached, and undoable like every operation.

## Lines and releases

Trunk is not the only long-lived line. Release lines, long-term support lines, and hotfix lines are lines: named sequences of landings, each with its own standard. Applying a change to another line is a semantic cherry-pick: the node-level patch first, re-derivation from the intent as fallback, verified under the target line's standard.

A release is an object: named, immutable, pointing at a snapshot, carrying attestations and a changelog generated from the intents landed since the previous release. Trunk landings carry a monotonic number, so a human can say "landing 4231" and mean one thing.

## The loop, end to end

Intent, then swarm planning, then agents in workspaces, then verify, then judge, then land, then release to canary, then observe, then promote, then observe again, and memory records what was learned. Every arrow is a verb or a standard. No arrow needs a human unless a standard says so, and when one does, the system asks through a channel and waits.

## What Tessra does not do

It does not run containers, host services, or collect metrics. Those are adapters. Tessra decides what runs where, proves it met the standard, records what happened, and reverses it when the signals say so. That separation keeps the system small and keeps every adapter replaceable.

## The honest version of "without issues"

Issues happen. The claim is narrower and stronger: every issue a verifier can see is caught before promotion or reverted after it, every action is recorded and reversible, and humans hear about exceptions in the place they already are. The old pipeline failed when a human could not keep up. This one fails when a standard was too weak, and standards are reviewed, versioned, and improved like everything else.
