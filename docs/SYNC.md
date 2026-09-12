# Sync

Tessra is built for one repository on many machines: a team whose agents work on several laptops and servers at once, in the same files, without a merge queue and without a server they have to run. This document says what a team looks like, what moves between machines, who lands, how a machine joins, and in what order the pieces get built. The spec is canonical for the mechanism: sync, coordination, and anchoring in [spec/03-operations.md](../spec/03-operations.md), op verification in [spec/04-security.md](../spec/04-security.md), and the `manifest` and `anchor` objects in [spec/02-objects.md](../spec/02-objects.md). This document explains and sequences it.

## What a team looks like

One repository is one init op, one repository id, one trunk. Every machine that takes part holds a replica: the whole graph, the object store and the operation log, under its own local application data directory. Each replica has a daemon of its own with a key of its own. Its agents open sessions with that daemon, work in workspaces on that machine, and sign ops under session keys that daemon holds.

Three things never leave a machine. Workspaces are local, so nobody's half-typed edit is visible anywhere until it is snapshotted. Private keys never move, so joining a team never copies a secret. Machine-local configuration, the verifier commands, the sandbox level, and the remote's location stay in `.tessra/config`.

Everything else is one replicated graph, as VISION.md decides. Sync moves ops and the objects they reach. There is no push and pull of branches, because there are no branches to push: a replica sends the ops the other side lacks, in parent order, and the other side verifies each one before storing it.

## The same file on two machines

Machine A and machine B each run a swarm on the same file. This is the case the whole design exists for, so here is what happens end to end.

1. **Both sides work as they do today.** An agent on B gets a workspace from B's daemon, edits `login` in `auth.rs`, snapshots, verifies, and proposes. B's runner signs the `tests.pass` attestation. An agent on A does the same to `refresh` in the same file. Neither daemon knows about the other yet, and neither waits.
2. **Sync exchanges the ops.** B's daemon pushes its new ops and the revisions, snapshots, trees, and attestations they reach. A's daemon pulls them and runs the op verification algorithm on each: parents known, author active and not revoked, signature, capability chain to an owner, legality, determinism. The op DAG on A now has two heads and the view merge folds them. B's proposal is in A's `proposed` set.
3. **The coordinator lands.** Trunk has one coordinator, recorded in the view. Without a hub that is the daemon that ran `init`, A's. Its frontier lands the proposals in arrival order, each merged onto trunk at unit granularity. `login` and `refresh` are different units, so both land, and a rename recorded on one side is carried into the other, exactly as on one machine. Two edits to `login` produce a conflict revision; the line does not advance for that change, and the change is pointed at the conflicted revision.
4. **Sync carries the result back.** B's daemon pulls the landing op, the restack ops for anything stacked on what landed, and the conflict revision. Every one is verified again on B, including the standard evaluation the landing cites, so B's replica accepts a landing only if the standard holds.
5. **B reconciles its workspaces.** For each local workspace whose change was restacked, B materializes the restacked revision when the workspace has no unsnapshotted edits, and leaves it alone otherwise, saying so in `status`. For a change that conflicted, B writes the markers and the sidecar into the author's workspace. The agent resolves by editing and removing the sidecar, snapshots, and proposes again. Claims B's agents hold that have expired are retired with `unclaim`.

The only differences from one machine are in steps 2 and 4: proposals travel to the coordinator, and landings, restacks, and conflicts travel back and reach the author's workspace through the reconcile step. Merge, verification, standards, claims, memory, and undo do not change. No step compares clocks, so skew between machines does not matter, and nothing locks.

Claims cross machines for free, because a claim is an object in the view. Once sync runs, an agent on B asking for a context pack on `login` is told that an agent on A holds it, with A's intent, and may proceed anyway.

## What replicates

| Object | Replicates | Notes |
|---|---|---|
| Ops | Always | The single source of mutation. Never rewritten, never collected |
| Revisions, snapshots, trees, standards, principals, capabilities, hooks, lines | With the ops that reach them | Fetched before verification when a check needs them |
| Blobs, artifacts, nodeindexes | With the ops at first, lazily later | A replica that materializes a workspace needs the head's blobs; one that only coordinates does not |
| Attestations | Always | A landing cites them, and every replica evaluates the standard against them |
| Claims | Always | Short-lived. Expiry is applied by readers; the issuing daemon retires them |
| Shared memories | Always | A fresh replica knows everything the agents learned, as MEMORY.md promises |
| Private memories | Only to replicas serving the author | Decided by the receiver's manifest |
| Revisions flagged `secrets` | Never | The sync filter, beside the `promote` refusal |
| Workspaces, keys, `.tessra/config` | Never | Local to a machine |

Read scope applies on the wire the way it applies to reads: a replica whose manifest names only task-scoped principals receives only what those principals may read.

## Who lands

Every shared line has one coordinator, and only its ops may advance the line. The role is a serializer, not a trust boundary. Because a landing cites its attestations and every replica re-evaluates the standard, a coordinator cannot land what the standard forbids. It can withhold or reorder, which anchoring makes visible.

- **Without a hub**, the coordinator is the daemon that ran `init`. On any other replica, `promote --to landed` records the proposal and answers that the coordinator lands it, naming which replica that is. The frontier on the coordinator runs after every pull, through the `land` hook action, or on request.
- **Proposal order** is the order proposals reach the coordinator. Today the frontier sorts by the revision's time, which is one machine's clock; across machines it becomes arrival order, which needs no clock.
- **Moving the role** is the `coordinator` effect, owner-only and already in the spec. A team whose always-on machine changes moves it once.
- **The hub** is a replica with two properties: it is always on, and it holds the coordinator role for the shared lines. Nothing else about it is special. It runs the frontier continuously, serves as the remote, and receives anchors. It is built last, after two laptops can do everything above between themselves.

Attestations signed by another replica's runner are the open question here. Among replicas that are all owners, a `tests.pass` from B's runner satisfies A's standard, since B is as trusted as A. A standard clause will be able to restrict which runners count, for a team that wants a designated machine to be the one whose test runs prove a landing.

## Joining

A machine joins a repository; it never initializes one. Running `init` on a second machine creates a second repository with a second init op and nothing in common with the first.

1. On A, an owner writes a bundle: a pack of every op and every object they reach.
2. On B, `tessra join <bundle>` creates `.tessra/`, a fresh daemon key, and the store, imports the pack, and sets the heads. It prints a join request holding B's daemon name and public key.
3. On A, the owner approves the request with `grant --replica <name> --key <public key>`. That emits the op that makes B's daemon a principal the graph knows, and, in the first form, an owner.
4. B pulls once more, finds its own key among the owners, and its daemon opens the replica.

Keys never move. What moves is a bundle, which holds no secret, and a public key. The invite form, where A creates the key and hands it over, is not offered, because a key that has been in two places is a key that has been in one place too many.

The first form makes every replica a co-owner. It is the right form for a team of people who already trust each other's machines and matches how a daemon finds itself in the graph today, by its key among the owners. The second form, a replica that is not an owner and holds a delegable capability from one, is the zero-trust form: it can sign for its sessions and propose, and it cannot land, change a standard, or revoke. It arrives when a team needs a machine it does not fully trust, and the only code it touches is how a daemon opens a replica.

## The remote

The first transport is a blob store, not a socket. A remote is any location every replica can read and write: a directory on a network share, a folder under OneDrive or Dropbox, a bucket through an adapter. VISION.md's ninth principle says Tessra syncs through any blob store, and this is why: it works offline, it needs no server and no open port, it needs no TLS to build, and the packs it writes are also the bundle and backup format.

```
<remote>/
  heads/<replica id>        the replica's op DAG heads, signed by its daemon
  packs/<replica id>/<n>    numbered packs of the ops and objects since the previous pack
```

Each replica writes only under its own id. Packs are immutable and content-addressed, so two replicas never write the same file and a cloud-synced folder never sees a conflicting edit; that is what makes a synced folder safe as a remote when it would never be safe as the store. Push writes a pack of the ops since the last push and then the heads. Pull reads every other replica's heads, fetches the packs it has not seen, and accepts the ops in parent order; an op that fails verification is rejected with everything that descends from it, and the rest are kept.

The daemon pushes after every op that proposes, pulls on a cadence and before the frontier runs, and reports in `status` under `sync`: the remote, how many ops it is ahead and behind, when it last pulled, which replica coordinates trunk, and what changed for the caller since its cursor. Sync is not a verb an agent types. The daemon does it, and `status` says where things stand.

A live transport comes after: a `Sync` endpoint on the daemon speaking the manifest handshake from the spec over TLS, so two machines on one network exchange ops in seconds instead of on a cadence, and so a hub can be one. Nothing in the pack layer is replaced by it; the same packs travel over the socket.

## What is built and what is not

| Built | Not built |
|---|---|
| The op DAG with many heads, the view merge, and acceptance in parent order | The walk that collects the objects an op reaches, for sending with it |
| The nine-step op verification, including revocation and rotation checked at the receiver | A receiver that takes a pack of ops and rejects a bad op with its descendants |
| The GVPK pack format: write, read, import | `join`, the join request, and `grant --replica` |
| A coordinator per line, recorded in the view; the frontier refuses anyone else | The remote, push, pull, and the sync cadence |
| Claims as replicated objects with reader-side expiry | The reconcile step after a pull |
| `manifest` and `anchor` as object types | The visibility filter, the secrets filter on the wire, `sync` in `status`, anchoring |
| The git bridge, which moves landed trunk between machines today | A live transport, and the hub |

## Sequence

Each step ends in a demo that could not be shown before it, in the manner of ROADMAP.md.

1. **Bundles.** A walk from a set of heads back to a set of known ops, collecting the objects verification needs; a pack of the result; a receiver that accepts a pack in parent order. `tessra bundle --out <file> [--since <cursor>]` and `tessra unbundle <file>`. **Demo:** two repositories in one test process. The second is created from the first's bundle. An op signed on one is accepted by the other; an op whose author the other has revoked is rejected with its descendants.
2. **Join.** `tessra join`, the join request, `grant --replica`, and a daemon that opens a replica it did not initialize. **Demo:** a second machine, or a second data directory on one, joins by bundle, its daemon opens, and an agent there proposes a change the first machine can see after one more bundle each way.
3. **The remote.** The `remote` configuration key, the layout above, push after propose, pull on a cadence, and the reconcile step. **Demo:** two machines share a folder. Agents on both edit different functions in one file. The coordinator lands both with no one running a command. A same-function collision reaches the author's workspace as markers and a sidecar, is resolved, and lands. `status` on either machine says what the other did.
4. **The coordinator across machines.** The proposed-not-landed answer on a non-coordinator, the frontier after every pull, arrival order, the `coordinator` move, and the runner clause. **Demo:** trunk's coordinator moves from a laptop to a desktop in one op, and the next landing happens there.
5. **The live transport and the hub.** The `Sync` endpoint over TLS, then a headless daemon that is always on, coordinates, serves as the remote, and receives anchors. **Demo:** fifty agents on five machines land through one hub, and a replica that sees heads the hub's anchor does not account for raises a task.

The first three steps are enough for a team. The fourth is a week of polish. The fifth is the product ADOPTION.md describes, and it should not be started before the third step has been used in anger.

## Decisions

`OPEN:` marks a decision not yet made, with the default the sequence proceeds on.

| Decision | Default |
|---|---|
| `OPEN:` replica trust | Co-owner replicas. Keep the check that opens a replica in one place so the non-owner form is a change there and nowhere else |
| `OPEN:` first transport | A blob store: a shared folder, then a bucket adapter. A socket after |
| `OPEN:` when the hub | After step 3 is in daily use. Design it as a replica with the coordinator role and nothing more |
| `OPEN:` attestations from another runner | Trusted among owner replicas. A standard clause may name the runners that count |
| `OPEN:` proposal order at the coordinator | Arrival order. Revision time is one machine's clock and decides nothing |

## Until then

The git bridge is the team path today. `export --format git` on one machine, then `git pull` and `import --branch` on the other, moves landed trunk between machines, one commit per landing, and the round trip rewrites nothing. What it does not move is everything that makes Tessra more than git: proposals, attestations, memories, claims, and the standard's verdicts stay on the machine that produced them, and the second machine lands each imported commit again under its own standard.
