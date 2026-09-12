# 05. Layout

Draft 9. Changes from draft 8, all from building M7 and M8: git export as built and its metadata; state bundles and their configuration; the CLI's large-stack thread.

Draft 8. Changes from draft 7, all from building M6: deploy scratch directories, the releases index, and the M6 configuration keys.

Draft 7. Changes from draft 6, all from building M5: human keys, anomaly state, agent runner logs, the M5 configuration keys, and the `activity` and `exceptions` query kinds.

Draft 6. Changes from draft 5, all from building M4: grants and assignments under the keys directory, parallel materialization, and the administrative verbs M4 added.

Draft 5. Changes from draft 4, all from building M3: verifier and external principal keys, verifier scratch directories, the attestation index, the M3 configuration keys, and the `trusted` and `bisect` query kinds.

Draft 4. Changes from draft 3, all from building M2: a workspace's pending semantic operations are a sidecar file; the query subset gains `blame`, `tests`, and `diff`; `context --path` lists units with what they reference and what covers them.

Draft 3. Changes from draft 2, all from building M1: the daemon's transport is specified; git history import is bounded rather than lazy; undoing a snapshot restores the workspace.

Draft 2. Changes from draft 1: keys and the store live outside the repository directory by default, with cloud-sync detection (B11); the conflict sidecar is the source of truth (S13); the colocated git checkout is a workspace with an owner (S14); workspace mechanisms for M1 are realistic (S15); the M1 query subset is defined (S16); git import is lazy (S21).

## The repository directory

Tessra places one directory at the top of the repository, beside `.git/` when colocated, and nothing else in the tree.

```
.tessra/
  TESSRA         format version, one line: "tessra 1"
  config        machine-local configuration, CBOR
  store         one line: absolute path of the store directory for this repository
  workspaces/   one CBOR file per workspace this machine hosts, plus <id>.ops.cbor for its pending semantic operations
  daemon        socket path or port, and the daemon's pid
  lock
```

The store and the keys are not here.

## The store directory

The store holds everything content-addressed and the op log. Its default location is outside the repository and outside any cloud-synced path:

| Platform | Default |
|---|---|
| Windows | `%LOCALAPPDATA%\tessra\stores\<repo-id>\` |
| macOS | `~/Library/Application Support/tessra/stores/<repo-id>/` |
| Linux | `$XDG_DATA_HOME/tessra/stores/<repo-id>/` |

`<repo-id>` is the entity ID assigned at init. A user may place the store inside `.tessra/` by configuration, and the daemon refuses to do so silently when the repository path is under a directory it recognizes as cloud-synced: OneDrive, Dropbox, iCloud Drive, Google Drive, and any path the user lists. The refusal names the reason and the override.

```
<store>/
  objects/     the object store
  ops/         op heads and the idempotency index
  artifacts/   local cache of artifact chunks
  cache/       nodeindexes, verification cache index, context pack summaries
```

`OPEN:` object store engine. Default for M1: an embedded key-value store (redb) keyed by object ID, single-writer through the daemon. The pack format below is the durable contract for sync, export, and bundles; the engine can change.

## Keys

Private keys live in the OS keychain when one is available, otherwise under `%LOCALAPPDATA%\tessra\keys\`, `~/Library/Application Support/tessra/keys/`, or `$XDG_DATA_HOME/tessra/keys/`, with owner-only permissions. They are never under the repository and never under the store, so copying, zipping, or syncing either moves no secret.

## Pack format

A pack is an append-only file of records for the wire and for bundles:

```
magic     "GVPK" 4 bytes
version   u8 = 1
records   repeated:
  id      32 bytes
  type    text, length-prefixed varint
  len     varint, compressed length
  bytes   zstd-compressed object bytes
trailer   BLAKE3 of everything before it, 32 bytes
```

A pack index beside it maps sorted IDs to offsets. Verification: trailer matches, and every record hashes to its `id` under its `type` domain.

## Roots and git colocation

A repository has one or more roots. `tessra init` inside a git repository creates root `""` mapped to that directory and imports its git history:

| Git | Tessra |
|---|---|
| blob | blob, or artifact above the threshold |
| tree | tree; modes 100644 to file, 100755 to file with executable bit, 120000 to symlink, 040000 to dir, 160000 to an artifact pointer holding the commit ID with a tracking rule marking the path vendored |
| commit | revision with `id` = first 16 bytes of BLAKE3 over `"tessra:legacy-cid" || commit hash`, `parents` from commit parents, `title` and `body` from the message, `author` a legacy human principal per distinct author identity, and an intent synthesized from the message with `legacy` true |
| default branch | line `trunk`, shared false until a hub is configured, head at the tip, `seq` = first-parent depth |
| other branches | not imported by default; imported as lines on request |
| tags | releases named after them, signed by the daemon, empty changelog |

Import is bounded: `tessra init --history N` imports the last N first-parent commits, oldest first, each as its own change with a change ID derived from the commit hash and a legacy intent, through the installed `git` command with one `cat-file --batch` per commit for blobs not yet seen. The default is 100. Lazy import of the rest of history, with the git object database as an object source, remains the design and is scheduled after M1.

The colocated checkout directory is a workspace owned by the daemon's human principal, the repository owner. The daemon watches `.git/refs` and imports new commits as they appear, and snapshots the checkout on each git commit and on demand, so a team still using git on the same directory never diverges.

`export --format git --branch <b>` writes every landed trunk revision that is not yet a commit as one commit on top of the newest that is, in trunk order, authored by the change's author with `Tessra-Change` and `Tessra-Revision` trailers, updates the branch ref, resets a clean checkout of that branch to it, and pushes on request. `import --branch <b>` lands every commit the branch gained since, first-parent order, through the normal landing. `import --branch <b> --history` lands them the way `init --history` does instead: each commit's tree becomes the next trunk revision with a legacy intent, outside the standard, with no verifiers and no hooks, and it takes the owner credential. That is for history that already exists and was checked elsewhere; a commit that is really a proposal goes through the standard. After either moves the checkout, the colocated workspace follows: it is pointed at the landed revision HEAD's commit maps to, unless its current revision is not landed, which is unlanded work of the owner's that landing restacks; the result reports `workspace` as followed, at HEAD, or left alone with why. The revision-to-commit mapping is store metadata under `git:rev:<hex>` and `git:commit:<hash>`, plus the commit hash imported revisions name in their body, so a round trip rewrites nothing. Export is linear; landing merges are not exported as git merges.

`.gitignore` seeds tracking rules with class ignored. `.gitattributes` `eol` seeds `eol`.

## Workspaces

A workspace materializes a revision's snapshots at a path per root. Creation uses the cheapest mechanism the platform offers:

1. Reflink copy of an existing materialization: ReFS block clone on a Windows Dev Drive, APFS clonefile, reflink on btrfs and XFS. Constant time per file. Not yet used.
2. Plain copy from the object store, plain files written by up to eight threads at once. Linear in tree size. What M4 does; single-digit milliseconds on a small crate.
3. Virtual filesystem projection, ProjFS on Windows and FUSE or NFS elsewhere, for repositories where a copy is too large. Not built.

The M1 target on this machine is plain copy, with reflink used when the repository lives on a Dev Drive. Hard links are not used for materialization because an in-place write would corrupt the shared file.

The daemon watches the workspace and snapshots on demand or on a cadence. A snapshot reads the tree, applies tracking rules, normalizes line endings, hashes, writes what changed, and records flags: out-of-scope paths, secrets matches, case collisions, unportable names, generated drift. It never refuses.

Verifier runs and bisect probes materialize into `<workspaces>/<repo-id>/verify-<snapshot prefix>` and `bisect-<snapshot prefix>`, removed after the run; cargo builds across runs share `verify-target` there. Verifier principals keep `<keys>/verifiers/<name>.key` and `.id`; external principals `<keys>/externals/<name>.key` and `.meta`. The owner's standing grants to agents are `<keys>/grants/<agent>.json` and a planner's assignments `<keys>/assignments/<agent id>.json`; both are machine-local and shape the sessions this daemon opens. Human principals keep `<keys>/humans/<name>.key` and `.meta`; anomaly state is `<keys>/anomalies/<agent id>.json`; agent runners a hook starts log to `<workspaces>/<repo-id>/agents/`. A deployer sees the snapshot at `<workspaces>/<repo-id>/deploy-<snapshot prefix>`, removed after the run. Releases are indexed in store metadata under `releases`.

`edit --rename old --to new [--path p]` performs the rename in the workspace, whole word across every tracked file with a grammar or in one path, and appends a `rename` op to `workspaces/<id>.ops.cbor`. The next snapshot moves the pending ops onto the revision's `ops`, after any ops the change's previous revision carried, and removes the file. Dropping the workspace drops them.

Conflict entries materialize as the file with markers plus a sidecar `<name>.tessra-conflict` holding the terms as CBOR:

```
<<<<<<< side A (change kxmp: "make login async")
...
||||||| base
...
=======
...
>>>>>>> side B (change nqrs: "rate limit login")
```

The sidecar is the source of truth. A snapshot records the entry as resolved when the sidecar has been removed, whatever the file contains; it records the entry as still conflicted while the sidecar exists, and if the file's markers have been edited, the sidecar's terms are updated to match. Marker text in an unrelated file is never interpreted.

## Environment fingerprint

At snapshot time the daemon records an environment when it can: operating system and architecture, versions of toolchains the tracking rules name, the container image digest, and the sandbox level. Verifiers record their own environment on attestations. The verification cache keys on `(kind, subject or bodies, environment)`.

## Wire mapping

**gRPC** for tools and the hub. Messages are the CBOR objects as bytes. Service `Tessra` exposes one RPC per verb plus `Sync`, `Objects`, `Events`, and `Manifest`.

**MCP** for agents. Thirteen tools named `tessra_status`, `tessra_context`, `tessra_query`, `tessra_workspace`, `tessra_edit`, `tessra_snapshot`, `tessra_claim`, `tessra_remember`, `tessra_verify`, `tessra_try`, `tessra_promote`, `tessra_revert`, `tessra_undo`. Inputs and outputs are JSON with the response shape in VERBS.md.

| CBOR | JSON |
|---|---|
| bytes32 object ID | lowercase hex |
| bytes16 entity ID | letter string |
| other bytes | base64url |
| int time | RFC 3339 with nanoseconds |
| map | object |
| per-mille uint | number divided by 1000 |
| absent optional | omitted |

Every response includes `state` from the merged view at the time of the call and a `cursor` that is the newest head op the caller has seen.

**CLI** for humans: `tessra <verb>` maps onto the same RPCs, prints JSON by default, renders with `--pretty`.

## The query subset

`query` in M1 supports exactly:

- `--kind memory --scope <node|path|intent|root|repo>[=<ref>] [--kinds ...] --budget N`: memories by scope, ranked per MEMORY.md, cut to budget, with a cursor.
- `--kind revision --change <id>`: the version history of a change.
- `--since <cursor> --budget N`: ops and the entities they touched since the cursor.
- `--kind object --id <prefix> [--offset N | --tail] --budget N`: one object by unique prefix. A blob is paged to the budget: its text from `offset` (default 0) with `next_offset` when more follows, or its end with `tail`. A verifier's evidence, the last 64 KiB of its output, is read that way; the summary and the errors are at the end.
- `--kind blame --path <p>`: for each unit of the path, the revision, author, title, and intent that last changed it. A unit is followed into whichever parent carries the same body under its ID, through `aliases`, or under a unique `(kind, name)`; a merged-in unit is attributed to the side it came from.
- `--kind tests --path <p>`: the covering-tests relation for the path: for each unit, the `test` units anywhere in the root whose `deps` name it, and the list of units no test names. Static; measured coverage is an attestation.
- `--kind diff [--change <id>] [--path <p>]`: the units a revision added, removed, changed, or renamed against its first parent, by identity.
- `--kind trusted --change <id>`: the trunk revisions containing a landed change, from its landing to the head, ranked by the trusted attestations on each: a whole-suite `tests.pass` 3, a selected one 1, `ci.*` 2, low risk 1, others 1.
- `--kind bisect --test <name>`: the first trunk revision, back through first parents, where a test unit of the caller's revision fails. The test is laid into each probed snapshot; results are attested as `test.result` by the bodies of the units the test depends on, so a state already probed is never run again.
- `--kind activity --window <duration> --altitude summary|changes|ops`: what happened in the window, from a headline with counts to the ops.
- `--kind exceptions`: the open requests for a human, each with the change, the clauses, and the judges' reasoning.

`context --unit <name>` is the pack for one unit: its text, the units it depends on, the units that depend on it, the tests that cover it, memories about it and its file, overlapping claims, and who last changed it, cut to the budget in that order; four characters count as a token.

`context --path <p>` is the pack for a path: its content, then its units with their `deps` as names and the tests that cover them, then memories about it and the claims that overlap it. Every part is charged by its serialized size and each list is cut where the budget runs out, so `budget.used` never exceeds the limit; the content leaves up to a quarter of the budget for the units, since they are the map an agent needs to ask for one unit next. `truncated` and the `omitted` counts say what was cut, and `next` suggests a larger budget or a unit pack.

The full query language is M5. The M1 subset is enough for the M1 demo and for `status` and `context` to be built on `query` rather than beside it.

## The daemon

One daemon per repository holds the store open and serves every verb. Every other command, and the MCP server, is a client: it finds the daemon, starts one detached if none is running, and runs in-process only if that fails or when asked not to use a daemon.

- Transport: loopback TCP on an ephemeral port, newline-delimited JSON. A request is `{ token, agent?, model?, write?, workspace?, principal?, verb, args }`, `principal` naming an external principal to act as; a response is the verb's response.
- `.tessra/daemon` holds the port, a bearer token, and the pid. The daemon removes it on exit. A client that finds a stale file removes it and starts a new daemon.
- Sessions live in the daemon's memory keyed by agent name and write scope, and are opened through the same session rules as anywhere else.
- Requests are served one at a time under a lock on the repository. While a verifier runs, every request is answered with `BUSY` instead of waiting. The daemon exits after an idle period, default thirty minutes, or on a `shutdown` request, and lets the reply drain before it goes.
- The attestation index is store metadata: `att:s:<subject hex>` and `att:b:<body hex>` each hold the IDs of the attestations naming that subject or body, and `att:indexed` marks a store that has been indexed once.
- On Windows a detached child inherits every inheritable handle of its parent, including a pipe a caller is reading from, so the client marks its standard handles non-inheritable before spawning.

This is the M1 form of the agent surface. gRPC and MCP over the network arrive with the hub.

## Configuration

`.tessra/config` holds what is machine-local: the store path override, the hub URL, adapter bindings and vault references, the sandbox level this machine offers, retention overrides, the workspace mechanism preference, and the cloud-sync path list. M3 keys: `verifiers` (attestation kind to command line, overriding detection from the tree), `verify_timeout_s`, and `risk_sensitive` (glob patterns whose paths raise risk). M5 keys: `agent_runner` (the command an `agent` hook action starts), `anomaly_throttle`, `anomaly_revoke`, and `anomaly_cooldown_s`. M6 keys: `vault.<NAME>` (a secret a deployer may be handed) and `auto_revert` (false turns automatic rollback off). M8 keys: `snapshot_state` (capture state on every snapshot) and `state_paths` (the directories captured, default `target,data,.venv,node_modules`). `tessra config --set key=value` writes them.

`snapshot --with-state` records an environment with the toolchain versions, the lockfile hashes, and a tree per state path. `workspace --action create --from <rev> --with-state` and `workspace --action rewind --to <rev>` restore those trees beside the files, so a checkout runs with no install step and an experiment rewinds with everything it wrote outside the tree. State restore replays exact bytes; it runs when the machine's toolchain matches what the environment records. The `tessra` CLI runs its work on a 64 MiB thread, because clap's derived command builder and the verb dispatch have frames past the 8 MiB main-thread stack in a debug build. Policy lives in the graph: standards, hooks, channels, targets, lines, principals.

## Performance budgets

Measured in the repository's own CI as attestations, from M1.

| Operation | Budget |
|---|---|
| Workspace creation by reflink | under 100 ms regardless of size |
| Workspace creation by copy, 10,000 files | under 2 s |
| Snapshot with 1,000 changed files | under 1 s |
| `status` | under 50 ms |
| `context` for one node, 4,000-token budget | under 500 ms warm, 2 s cold |
| Verification cache hit | under 10 ms |
| Op verification excluding standard evaluation | under 5 ms |
| Standard evaluation for a landing citing 50 attestations | under 50 ms |
| Landing merge of a change touching 50 nodes | under 200 ms excluding verifiers |
| View trie update for one op | under 1 ms, O(log N) nodes written |
