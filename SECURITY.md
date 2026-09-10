# Security

Signing and security are load-bearing. Attestations, human approvals, capabilities, provenance, containment, hooks that run as principals, and inbound results from external CI are all meaningless unless signatures and a security model make them unforgeable. Git signs commits and stops there. Tessra signs everything and scopes everyone.

## Threat model

What the design defends against.

1. **A compromised or confused agent.** Prompt injection through repository content, memory, or tool output; a buggy framework; a bad model. It may try to write outside its scope, forge attestations, weaken tests, poison memory, exfiltrate secrets, land unreviewed code, or approve its own work.
2. **A forged human approval.** An agent claiming a human said yes.
3. **Tampered history.** Rewritten ops, deleted evidence, altered attestations.
4. **A malicious or compromised verifier or external CI** posting false results.
5. **Code under test forging its own attestation.**
6. **Supply chain.** Bad dependencies. Code from a model later found flawed.
7. **Secrets** leaking into snapshots, memories, packs, or channels.
8. **Transport and sync.** Tampering, replay, a hub that withholds.
9. **Stolen or leaked keys.**
10. **Runaway automation.** A swarm or an agentic hook spawning without bound.

## Principals and keys

Every principal has a keypair: humans, agents, sessions, verifiers, hooks, deployers, observers, external systems, and the daemon itself. Names are bindings that can change. Agent identity is two-level: a durable agent principal per runtime and configuration, which is what blame, reputation, and recall by model point at, and a short-lived session principal per run whose key the daemon holds and the agent never sees. Human keys live in a device keychain, a hardware key, or a passkey, and are never held by an agent. No private key ever lives inside the repository directory, which may be under cloud sync.

## Everything is signed

Every op in the operation log is signed by the principal that performed it and carries the capability that authorized it. Attestations are signed by the verifier that produced them. Approvals are signed by the human. Memories, standards, hooks, and capability grants are signed. An unsigned or wrongly signed op is rejected before it is applied.

## Tamper-evident history

Objects are content-addressed, so altering one changes its hash and breaks every reference to it. Ops are signed, so authorship cannot be forged. Each op references the hashes of the ops it follows. Each principal writes a chain, and chains from different clones merge into a DAG, so concurrent work is recorded faithfully and deletion or reordering is detectable. Concurrent edits to the same metadata, such as two clones changing one standard, are conflicts stored as data. DAG heads can be anchored on a cadence to a transparency log or to the hub, so a local rewrite is detectable against the anchor. The operation log is the audit log: complete, signed, and queryable.

## Redaction

Tamper-evident history has to coexist with purging a leaked secret and with the right to erasure. The model is Fossil's shunning. A tombstone replaces the object's content while preserving its hash, so every reference stays valid and integrity checks pass with the tombstone recorded. Redaction is a signed op that requires a capability, and the redaction itself is visible in the operation log. What was removed is gone; that it was removed, by whom, and why is not.

## Capabilities, not roles

A capability is a signed grant from an issuer to a subject: which verbs, on which scope (paths, nodes, targets, stages), under which constraints (expiry, budget, maximum risk, maximum blast radius), and whether it may be delegated. Chains of delegation are verifiable offline. Every op carries its capability proof, and the daemon verifies it before applying the op.

Least privilege is the default. A task-scoped agent gets: write to its own workspace, claims on its assigned nodes, remember scoped to its task and nodes, propose. It does not get: land, global memory, standards, targets, other agents' workspaces. The swarm planner holds a delegable capability and issues narrower ones to the agents it spawns. Agentic hooks are bounded the same way, including how many agents they may spawn.

"Who authorized this agent to land in payments" is a query over the capability chain.

## Human approvals

An approval is an attestation signed with the human's key. The daemon sends a challenge through a channel. The human's client signs it with a passkey or hardware key, which is a system prompt on their device, not a Tessra screen. Channels that cannot carry a signature, such as plain email, produce channel-attested approvals with lower trust, and a standard can require key-signed approval for sensitive scopes. An agent cannot produce either kind. It holds no human key and no channel credential.

## Verifier isolation

Code under test must not be able to attest to itself. Verifiers run in sandboxes: the workspace is materialized in an isolated environment with no network unless declared, no secrets except those explicitly granted for that run, and no access to any signing key. The daemon-side runner observes the result and signs the attestation. The environment fingerprint on the attestation includes the sandbox specification, so an attestation from a weaker sandbox is distinguishable from one produced under full isolation.

External verifiers such as an existing CI system post attestations under their own principal. A standard decides how much weight that principal's attestations carry. A compromised external system is contained by revoking its key.

## Judges

Judge agents hold read-only capabilities, cannot be the author of the change they judge, and a standard can require distinct models and multiple judges. Repository content shown to a judge is data. A judge's attestation carries its model in provenance and has only the weight the standard assigns it.

## Memory

Memories are signed and ranked by trust. Repository-wide conventions require a capability that task-scoped agents do not hold. Agent-written memories render with provenance, and a standard can require human confirmation before a convention influences judges. Revoking an agent retires its unconfirmed memories. A poisoned memory is a persistent prompt injection, and every one of these controls exists because of that.

## Secrets

A secrets verifier scans before content enters the object store. Snapshots, memories, and hook payloads that contain secrets are refused or redacted, and a task is opened. Secrets for verifiers and deployers are held by the daemon through a vault adapter and injected into sandboxes by capability. They never enter a context pack, a memory, or a channel message. Context packs redact by policy.

## Sync and transport

One replicated graph. Visibility is per object and per principal: drafts and private memory replicate only to replicas that serve their author, landed changes and attestations are shared, and every read is filtered by the reader's read scope. Workspaces are local to a machine and never enter the graph. Claims are short-lived shared objects that replicate, so every agent on every replica sees who is working where. The hub coordinates the frontier when present and the local daemon when not; the coordinator serializes landings and cannot land what a standard forbids, because a landing cites its attestations and every replica re-evaluates the standard. A change may span roots, and landing across roots is atomic when one coordinator holds their frontiers. Offline agents reconcile on reconnect: expired claims, a moved frontier, and restack.

Objects are content-addressed, so integrity is inherent to transfer. Ops are signed, so authenticity is inherent. Transport is encrypted. Replay is prevented by the op chain and idempotency IDs. The hub is untrusted storage by default: it cannot forge ops, and withholding is detectable through anchoring and peer comparison.

## Provenance and supply chain

Every change carries the principal, the model, and a reference to the intent. The agent runtime can add a signed statement of framework, model version, and prompt hash. Attestations map to SLSA-style provenance and export to in-toto formats, so the rest of the ecosystem can consume them. Dependency changes are attested with hashes and vulnerability scans, and standards forbid new dependencies without approval. Recall by model is a query over this provenance.

## Key lifecycle

Rotation is an op. Revocation is an op that hooks react to: containment unwinds the revoked principal's unlanded work and unconfirmed memories. Agent keys expire on their own. Human key recovery is a capability grant from another trusted principal.

## Budgets as a control

Capabilities carry budgets: tokens, agents spawned, workspaces, landings per hour. A runaway swarm or a looping hook stops at its budget, and the risk monitor's anomaly signals catch it earlier.

## Trusted computing base

The daemon is the enforcement point. It verifies every signature and every capability before applying an op. It holds the runner keys, the vault adapter, and the channel credentials. Agents never do. The object store and op log formats are verifiable independently of the daemon, so a corrupted daemon cannot hide what it did. The daemon is kept small for that reason.

## What this replaces

Commit signing, branch protection, CODEOWNERS as access control, CI secrets configuration, and the audit log bolted onto a forge. In Tessra these are consequences of principals, capabilities, and signed ops.
