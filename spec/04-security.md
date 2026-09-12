# 04. Security

Draft 7. Changes from draft 6, all from building M5: human approvals as built, judges, and the anomaly path.

Draft 6. Changes from draft 5, all from building M4: standing grants and delegation as built; containment as built.

Draft 5. Changes from draft 3, all from building M3: the isolation M3 runs at and the busy daemon; how external principals are granted; the rule for whose attestations count lives with the attestation type in 02.

Draft 3. Changes from draft 2, all from building M1: session keys are held by the daemon on disk as well as in memory; the default grant is stated as implemented; every non-owner op carries its capability even for ungated verbs.

Draft 2. Changes from draft 1: keys never live in the repository directory (B11); revocation and rotation are checked against the receiver's current view as well as the parents' view (B3); no step compares clocks (B6); hooks cannot escalate (B4); landing ops are re-checked against their cited attestations everywhere (B5); write scope is checked at promotion, not at snapshot (B8); hard limits are metered by the daemon and advisory ones are advisory (B13); read scope exists and is enforced (B14); agent identity is two-level (B15); the `attest` verb exists (S2); WebAuthn binding is specified (S9); the daemon is an owner and is the trusted computing base (S20).

## Keys

Every principal holds an Ed25519 keypair. Where the private key lives depends on kind.

| Kind | Private key | Lifetime |
|---|---|---|
| daemon, runner, verifier, hook, deployer, observer | The OS keychain when available, otherwise a file under the user's local application data directory with owner-only permissions. Never inside the repository directory, which may be under cloud sync | Rotated on demand |
| agent | None. A durable agent principal has a key only so it can sign the creation of its sessions; that key is daemon-held like the daemon's own | Rotated with the daemon |
| session | Held by the daemon: in memory while it runs, and under the keys directory so a command that is its own daemon can reuse the session. The agent authenticates to the daemon with a bearer token over loopback and never sees the key | `expires`, default four hours, enforced by the daemon that holds the key |
| human | A passkey or hardware key on the human's device. The daemon never holds it | Rotated by the human |
| external | Held by the external system, issued as a principal by an owner | Per the external system |

A session's ops are signed by the daemon under the session's key after the daemon verifies the session token. An agent is therefore a first-class principal that cannot leak a secret it never had. When a session's `expires` passes, the daemon stops signing for it; replicas do not check expiry, because they cannot do so deterministically and the key holder already has.

The daemon token in `.tessra/daemon` authenticates a client to the daemon and nothing more. Until passkeys and identity bindings are built, the daemon holds the keys of the humans and externals it created and gates their use with a per-principal credential: 256 random bits issued by `grant`, shown once in the grant's result, kept by the daemon only as a BLAKE3 hash in the principal's record, and presented with every request that acts as the principal. A request as a principal without the right credential is refused with `CREDENTIAL_REQUIRED` or `CREDENTIAL_BAD`; granting the name again rotates it. The owner is gated the same way: `init` issues an owner credential, kept as a hash beside the daemon key and in clear as `owner.credential` until the owner moves it, and the policy verbs (`standard`, `hook`, `channel`, `target`, `grant`, `revoke`, `config`, `attest`, `revert`, `undo`) refuse a daemon actor that did not present it. Verbs the standard gates, `promote` among them, and every read take the token alone. A process that can read the checkout is therefore neither the owner nor a human; the residual on a single-user machine is that the same OS user can read the keys directory, which the keychain and passkeys close.

The daemon principal that performs `init` is an owner and remains one. It is the trusted computing base: it verifies every op, holds the runner keys, the vault adapter, and the channel credentials, and authors containment. The object store and op log are verifiable without it, so a corrupted daemon cannot hide what it did.

## Op verification

Every op, produced locally or received in sync, passes this algorithm before it is stored. V is the merged view of the op's parents. C is the receiving replica's current merged view. Failure at any step rejects the op.

1. **Decode.** Deterministic CBOR, valid `op` of a known version.
2. **Parents.** Every parent is stored and verified. Compute V.
3. **Author, at V.** The author resolves in V, `status` is `active`, and its `key` equals the op's `key`.
4. **Author, at C.** The author is not in C's `revoked`. If the author's current version in C is not V's version and its `key` differs from the op's `key`, reject: the key was rotated after what the op saw. This step is the one place verification is not identical across replicas during propagation; it is monotone, so once a revocation or rotation reaches a replica nothing further under the old key is accepted there.
5. **Signature.** Strict Ed25519 over the payload under `key`.
6. **Authorization.** If the author is in V's `owners`, continue. Otherwise `cap` is present, is in both V's and C's `caps`, its `subject` is the author, its chain terminates at an owner with every link in `caps` and no issuer revoked, and it authorizes the op's kind and effects per the next section.
7. **Legality.** Every `point` `from` matches V. No `point` moves a landed change except a `restack` authored by the coordinator or the daemon. Every `head` is authored by the line's `coordinator` in V, increments `seq` by one, and its `attests` are present in the store and satisfy the line's standard when evaluated over V and the target revision; that evaluation is deterministic and every replica performs it. `tomb` only in a `redact` op. `owner`, `unowner`, and `coordinator` only by an owner. A `point` that creates or updates a hook satisfies the hook invariant. A `point` that creates a session principal is signed by the parent agent's key. A `claim` effect names the author as the claim's principal.
8. **Determinism.** Applying the effects to V yields the op's `view`.
9. **Store**, and index the idempotency key.

Objects that steps 6 and 7 need are fetched before verification, per 03-operations.md sync step 4.

## Capability check

A capability authorizes an op when all of the following hold.

- `subject` is the author.
- The op's kind is in `verbs`, or is one of `status`, `context`, `query`, `workspace`, `snapshot`, `claim` on the author's own claims, and `undo` of the author's own ops, which every principal may do without a grant.
- Write scope covers the effects that write scope governs: a `promote` or `head` touches only paths and nodes in `write.paths` and `write.nodes` across roots in `write.roots`, computed as the paths changed between the revision's snapshots and its parents'; `promote` targets a stage in `write.stages`; `head` names a line in `write.lines`; `deploy` names a target in `write.targets`; a memory `point` uses a scope kind in `write.memory_scopes`; standard, hook, channel, target, and line changes require the corresponding administrative verb. A `snapshot`'s `point` is not checked against write scope; the daemon records `out_of_scope` in the revision's flags instead, and `promote` refuses while that flag is set.
- The chain is contained at every link: verbs, write, read, and limits.

Read scope is not part of op verification, because reads are not ops. The daemon enforces `read` on every `status`, `context`, and `query` a session makes, filtering paths and memories to the scope, and the sync visibility filter applies the same scopes per the receiver's manifest.

Hard limits are metered by the daemon that signs for the principal, using what it observes: ops issued, workspaces open, sessions spawned, verifier time consumed, and landings requested per hour as counted by the coordinator. Advisory limits depend on self-reported data such as tokens and are enforced by the issuing daemon only when the reporting runtime is a principal it trusts; a runtime that stops reporting is caught by the risk monitor, not by the budget.

**Default grant** for a task-scoped session: verbs `attest`, `edit`, `promote`, `remember`, `try`, `verify`; `write.stages` = `[proposed]`; `write.paths` = the assignment, or `**` until a planner assigns one; `write.memory_scopes` = `[node, path, intent, root]`; `read.paths` = `**` and `read.memory` = `shared` until read scopes are assigned; hard limits on ops and workspaces. It cannot land, write repository-scoped memory, edit standards or hooks, or touch targets. Ungated verbs such as `snapshot`, `claim`, and `undo` need no grant, but a non-owner op still carries the session's capability so that step 6 can check the chain.

## Revocation and containment

`revoke` is an op by an owner or a holder of the `revoke` verb. It adds the principal to `revoked` and emits a principal version with `status` `revoked`. Revoking an agent revokes all of its sessions. Any op from a revoked principal is rejected at step 4 wherever the revocation has arrived.

Containment is the same op as the revocation, authored by the owner: every unlanded change whose current revision its sessions authored is unproposed and unpointed, every claim of theirs released, and every active memory they wrote pointed to a `retired` version, since nothing confirms a memory yet. Their workspaces are dropped locally afterwards and any planner assignment to the agent is cleared. A revoked agent cannot open a session, and a session the daemon cached is refused as soon as the revocation is in the view.

An owner gives an agent a standing grant with `grant --to <agent>`: extra verbs, a write scope, `delegable`, and an op budget, applied to every session the agent opens from then on. A session whose capability is delegable and holds `plan` delegates when it plans: each assignee's session receives a capability contained in the planner's, signed by the planner, with the group's paths and budget and `parent` set, and verification walks the chain to the owner on every op the assignee signs. The assignee's default capability remains active beside it until an `uncap` exists.

## Human approvals

An approval is an attestation of kind `approval.human` whose `verifier` is the human's principal. It is produced only through a channel.

1. A standard requires an approval, or a hook asks a question. The daemon builds the attestation payload: `subject`, `kind`, `result`, `verifier`, `time`, and a `nonce` in `scope`.
2. The daemon delivers a challenge through a channel the human is reachable on. The challenge is `BLAKE3("tessra:sig:attestation\n" || payload)`.
3. Channel `signing` `key`: the human's client signs with the credential in the principal's `bindings`. For a passkey, the WebAuthn challenge field is the challenge above; the attestation carries `sigkind` `webauthn`, `sig` = the CBOR of `{ authenticatorData, clientDataJSON, signature }`, and no `runner`. The daemon verifies the assertion against the registered credential and checks that `clientDataJSON.challenge` equals the challenge. For a hardware Ed25519 key, `sigkind` `ed25519` and no `runner`.
4. Channel `signing` `channel`: the channel adapter's principal signs an attestation that human H replied R to the challenge. `verifier` = the human, `runner` = the channel principal, `sigkind` `ed25519`.
5. Key-signed is distinguished from channel-attested by the absence of `runner`. A standard's `approved` with `keysigned` true requires the former.

The daemon refuses an `attest` of kind `approval.human` from any principal that is not of kind `human`. M5 builds the key-signed form with keys the daemon holds for humans it created, usable only with the credential their grant issued; the challenge, passkeys, and channel-attested replies are not built. A human's answer also closes the request that asked. An approval is bound to the revision it names: it applies to that revision and to the revisions the system derives from it by merging it onto a new base, whose second parent names it (a landing, a restack, a conflict merge), and not to a revision the author snapshots afterwards, even though that revision names the approved one as `prev`.

Judges are sessions of agents other than the author's; a verdict from the author's own agent never counts. Their attestations are ordinary session attestations, trusted only by the `judge` predicate, never by `attest`.

The anomaly path of RISK.md is built per agent on the daemon: points for writes outside claims and write scope, for weakened tests at landing, and for attempts made while throttled; a throttle, then a revocation performed by the daemon as owner with the M4 containment.

## Verifier isolation

A verifier that runs code runs under the runner in a sandbox. The runner materializes the snapshot, executes the verifier, observes the result, and signs the attestation with the runner key, setting `runner` to itself and `verifier` to the tool's principal. The code under test has no key and no session token.

| Level | Meaning |
|---|---|
| L0 | No isolation. External systems reporting through the inbound API are L0 |
| L1 | Process isolation: separate user, no network unless allowlisted, no access to keys or the daemon socket. Does not stop reading other workspaces on a shared machine (A3) |
| L2 | Container with a read-only image, the workspace mounted, no network unless allowlisted |
| L3 | MicroVM |

A standard requires a minimum level with `attest` `args.env.sandbox_at_least`. Default L1 on a developer machine, L2 in CI. M3 runs at L0 and records it: a directory of the daemon's own under the workspaces directory, an environment stripped to what the toolchain needs with nothing named `*TOKEN*`, `*SECRET*`, `*KEY*`, or `TESSRA*`, no repository directory and so no daemon token, and `TESSRA_SANDBOX=1`. The `tessra` command refuses to act when `TESSRA_SANDBOX` is set, so code under test cannot act through the daemon by the usual door, and the daemon does not refuse other callers while a verifier runs; on a shared machine code under test can still read files (A3). Secrets reach a sandbox only through the vault adapter, by a capability naming the secret reference, as environment variables or files that exist only for the run.

Judges are sessions with `read` over the change and the rubric and the `attest` verb only. A judge's parent agent MUST differ from the author's parent agent, and a standard's `distinct_models` requires their sessions' `model` fields to differ; those fields are self-reported by runtimes (A1).

External verifiers post attestations under their own principal through an `attest` op. An owner creates such a principal with `grant --external <name>`: kind `external`, a capability whose only verb is `attest`, and a key the daemon holds so `--as <name>` acts as it, with the credential the grant printed; bindings to an identity provider are the alternative. A standard decides the weight of that principal's attestations. A compromised external system is contained by revoking it.

## Secrets

At snapshot time the daemon runs the secrets scanner over changed blobs and records matches in the revision's `secrets` flag. The snapshot succeeds. `promote` refuses while the flag is set, sync never sends a revision with the flag, and a hook may open a task. `OPEN:` offering redaction-in-place at snapshot time; default is flag only.

Secrets for adapters are vault references `vault:<adapter>:<path>`, resolved for the runner only. The resolved value never enters an object, a context pack, a memory, or a channel message. Context packs are scanned before they are returned.

## Redaction

`redact` requires the `redact` verb. Its effects are `tomb` entries. The daemon writes a tombstone per object, replaces the object's bytes with the tombstone, and records the ID in `tombstoned`. Trees referencing the object stay valid. Materializing a tombstoned blob produces an empty file and a report. Peers apply the same replacement when the op verifies. The redaction op is never redacted.

## Threats to controls

| Threat | Control |
|---|---|
| Compromised agent | Task-scoped write and read, no key in the agent's hands, containment on revoke, memory provenance and trust |
| Forged approval | Approvals only through channels, key-signed where the channel allows, refused from sessions and agents |
| Tampered history | Content addressing, signed ops, the op DAG, anchors |
| Stale-parent replay after revocation or rotation | Step 4 |
| Hook escalation | The hook invariant |
| Coordinator landing what policy forbids | Cited attestations, deterministic standard evaluation on every replica |
| Malicious verifier | Verifier weight in standards, revocation, runner-signed attestations |
| Code under test attesting to itself | No key or token in the sandbox, runner signs |
| Supply chain | Provenance on every revision through the session principal, dependency attestations, recall by model through the parent agent |
| Secrets leakage | Scanner and flags, vault references, pack scanning, sync filter |
| Transport | Content addressing, signatures, TLS, idempotency index |
| Stolen key | Short sessions with daemon-enforced expiry, rotation, revocation, human keys on devices, keys outside the repository |
| Runaway automation | Hard limits metered by the daemon, risk monitor anomalies, circuit breakers |
| Unauthorized reading | Read scope on every read and on sync |
