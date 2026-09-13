# Changelog

Notable changes to Tessra, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the release pipeline uses the **Unreleased** section as the notes of the next release.

## Unreleased

Nothing yet.

## 0.2.0 - 2026-09-12

### Added

- `import --branch <b> --history` lands the commits a branch gained as history, the way `init --history` lands the commits it imports: each commit's tree becomes the next trunk revision with a legacy intent, outside the standard, with no verifiers and no hooks. It takes the owner credential. The op verifier admits such a landing from an owner's `import` op only, on the head only; an owner could reach the same state by emptying the standard and restoring it, so it grants nothing new and keeps the record honest. History that was checked elsewhere no longer has to pass this repository's standard commit by commit, or wait on a human to approve every test it edited; a request the standard had opened for one of the commits is closed once it is history.
- A design document, docs/SYNC.md, for one repository on many machines: what replicates, who lands, how a machine joins, a blob-store remote before any socket, and the five steps that build it, each with its demo. The spec already carried the mechanism; this sequences it.
- A CI workflow runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test --workspace` on every pull request and push to `main`, on Ubuntu, macOS, and Windows, and checks the workspace builds with the declared minimum Rust.
- `TESSRA_DATA_DIR` overrides where the store, keys, and workspaces live, and `InitOptions::data_dir` does the same for one repository. The test suite uses it so a run leaves nothing under the local application data directory.
- An end-to-end test of the MCP server through the built binary: `initialize`, `tools/list`, and a `tessra_status` call.

### Changed

- The declared minimum Rust is 1.90, which the locked dependencies (tree-sitter 0.27) need; the README said 1.80, which could not build the workspace.
- A verifier runs with the repository released: the daemon decides the runs with the repository, carries them out with it unlocked, and records the attestations with it again, so a `verify` (or `snapshot --then verify`) no longer holds every other agent for the length of a test suite. The `BUSY` answer to every request during a run is gone; instead the `tessra` command refuses to act when `TESSRA_SANDBOX` is in its environment, which every verifier run sets, and `status` reports `verifying` while a run is on. A `tests.fail_on_parent` entry says when the parent could not run the new tests at all.
- The landed set and the standard's status are remembered between ops, so a `status` between landings is a lookup instead of a walk over every landed revision and every attestation.
- `status` lists what happened since the cursor newest first, cut to half its budget, each entry saying what the op did (the change's title, the memory's kind and body, the landing's number) and who did it by name; `since_omitted` counts the rest and `cursor` is what to pass next time. It used to return every op since init, each as hashes.
- `budget.used` is what the verb produced against its budget, and a new `budget.total` is the whole answer including the state echo, so the cost of a call is never hidden.
- Every query kind is cut to its budget and says what it left out: `revision`, `since`, `exceptions`, `trusted`, `workspace --action list`, and the uncovered list of `tests`; `blame` and `tests` charge entries by their real size.
- Over MCP, the `next` suggestions are tool calls (`tessra_verify {"stage":"landed"}`) instead of CLI text, every tool's schema carries descriptions, enumerations, and required fields, `tessra_status` takes a budget, and the server's instructions are the agent manual.
- The CLI's `status` takes `--budget`, and `query --help` names every kind.
- Reaching the daemon no longer makes a process the owner or a human. `init` issues an owner credential, printed once and kept in the keys directory as `owner.credential` until you move it (the daemon stores only its hash), and the owner's policy verbs (`standard`, `hook`, `channel`, `target`, `grant`, `revoke`, `config --set`, `attest`, `revert`, `undo`) ask for it through `--credential`, `TESSRA_CREDENTIAL`, or a prompt at a terminal. `grant --human` and `grant --external` print a credential for the principal, and `--as <name>` needs it. A repository from an earlier release gets an owner credential on its next open; a human or external granted earlier is reissued one by being granted again. An agent session that can run `tessra` in the checkout can therefore no longer loosen the standard, attest as the owner, or approve its own work.

### Fixed

- An `import` retried after the standard refused a commit was rejected by the op verifier, because the refused attempt had left the change pointed at its revision and the retry pointed it again from nothing. The retry is now the next version of that change.
- A memory about a unit or a path is anchored to that content when it is recorded, and recalled as `stale`, listed last, once the content has changed or gone. `remember --supersedes <id>` records a replacement and retires the earlier memory in the same op, and `remember --retire <id>` takes a memory back; retired and superseded memories are not recalled. Two gotchas in this repository that were fixed days ago were still recalled beside their corrections.
- JavaScript and TypeScript `test(...)`, `it(...)`, and `describe(...)` blocks are units of kind `test` named by their title (`.skip` and `x`-prefixed forms are `test.skipped`), so the covering-tests relation, `changed.covered`, and the test-change guards see them; they were nameless statements before, and a JavaScript file had no tests as far as the standard knew.
- The node merge keys units by node ID, resolved through the aliases of the base and both sides, instead of recomputing `(kind, name, ordinal)` per side, so the history-aware identity the index keeps now decides what lines up with what. Same-named siblings such as overloads match by body first, so one inserted above another no longer drifts into a spurious conflict.
- Rust attributes and doc comments, and Python block structure, are part of a unit's body hash, so two sides that add different derives, or a statement moved out of an `if`, merge as the edits they are instead of one side silently winning. A lone trailing comma in a tuple stays semantic, so `(1,)` and `(1)` differ.
- A rename is applied to identifier tokens of the parse and nowhere else: not inside strings or comments, not to a field or property access, a struct key, or a keyword argument, and not to a local or parameter that shadows the name. A rename onto a name the base already gives a unit of the same kind, or that the other side renamed something else to, is a conflict rather than two definitions; `edit --rename` refuses up front when the target name is taken.
- A unit renamed and edited in one change without a recorded rename keeps its identity when its body overlaps the vanished unit's closely enough, so a concurrent call to the old name is carried to the new one instead of landing broken.
- A member inserted above the first member of a container keeps the separator that lives outside the unit's span.
- `context --path` no longer needs a workspace: without one it describes trunk, as `--unit` already did, reading the file or directory from the revision's tree.
- A refused promotion carries `unmet` as a list of clauses, each with its reason and a `fix`, the `stage` refused, and `fix: "verify"`, as VERBS.md describes; the clause list is no longer text inside `message` alone.
- A `snapshot` or `remember` retried with the same idempotency key reports the revision or memory the first call recorded (`retried: true` on a snapshot) and points the workspace at it, instead of naming a revision that was never recorded.
- A session's workspace is kept beside its key, so a daemon that restarted, or a command that is its own daemon, finds the session's own workspace instead of the principal's first one.
- `snapshot --then promote landed` (or `--then land`) lands as the coordinator, as VERBS.md promised; `--then promote` still proposes.
- The workspace path in the state echo and in `workspace --action list` no longer carries the Windows verbatim prefix.
- An approval, a judge's verdict, or an external attestation on a revision no longer counts for a later revision of the same change that the author re-snapshotted with different content. It carries only to the revisions the system derives from the attested one: its landing, its restack, and a conflict merge, whose second parent names it.
- A restacked revision keeps the change's author instead of naming the daemon, so blame and provenance after a restack still point at who wrote the change.

## 0.1.3 - 2026-09-11

### Fixed

- On Windows, a verifier, hook, or deployer command whose first word is a bare name is resolved through `PATH` and `PATHEXT` before it is spawned, so the detected `npm test` runs where only `npm.cmd` exists, and so do `pnpm`, `yarn`, and `npx`. The environment fingerprint names the tool behind a `cmd /c` wrapper instead of `cmd`, and records `node --version` for Node tools (#9).
- `export --format git` fast-forwards a checkout of the branch instead of moving the ref under it, so a dirty checkout with unrelated edits receives the exported files and its index follows HEAD. When a local edit overlaps the export, or the branch holds commits trunk does not know, the export refuses before anything moves and says how to proceed (#13).
- `workspace --action create --path` refuses a path inside the repository or an existing directory with contents, instead of treating the checkout as an empty target and deleting every ignored file in it. `workspace --action rewind` removes only files the tracking rules would snapshot, so dependencies, build output, and local environment files survive a rewind (#7).
- The secret scanner flags a private key only when a `BEGIN … PRIVATE KEY` armour line is followed by at least 64 characters of base64 body and the matching `END` line, so documentation that names the armour, a parser's regular expression, or an empty test fixture no longer carries `flags.secrets`. A key pasted into a JSON string, with its newlines escaped, still does. A snapshot's secrets flag covers only the files the change touched against its base revision, so a match trunk already held does not block unrelated work (#8).
- `remember` from an agent session refuses a repository-wide memory up front, naming the scopes the session's capability allows and that repository-wide memory is the owner's, instead of a step 6 rejection whose only advice is `status`. The `unit` scope the README documents is accepted and stored as the `node` scope that `context --unit` recalls. The README and the MCP tool text say which scopes a session may write (#12).
- A history import indexes each commit's snapshot against the commit before it, and `import --branch` indexes each commit against the trunk head it lands on, so units keep their identity across imported revisions and `query --kind diff` lists what a commit changed instead of every unit in the tree. A revision whose snapshot was imported without an index gets one computed against its parent's on first use, so a store initialized before this release reads the same way. Chunks of a file without a grammar are matched by body before position, so an inserted paragraph no longer changes the identity of every paragraph after it (#10).
- A reference that does not resolve within its file is resolved through the file's imports, and only there: relative and `$lib/`, `@/`, `~/` specifiers in JavaScript and TypeScript, relative and absolute modules in Python, and `crate::`, `self::`, `super::`, and workspace crate paths in Rust. A global such as `Error`, or a name from a package outside the tree such as `vitest`'s `describe`, no longer binds to whichever unit in the repository carries the same name, so `context`, `--unit` dependents, `changed.covered`, and test selection stop crossing package boundaries that the code does not (#11).

## 0.1.2 - 2026-09-10

### Fixed

- A landing refused for a weakened test charges the change's author anomaly points once per revision, not on every frontier pass, so an owner's retries can no longer revoke an agent (#1).
- A `forbid ... unless approved(human)` clause names its escape in the refusal and opens an approval request, as `require approved(human)` does (#2).
- A landing carries the flags of the snapshot it lands, so `structural(flags.none)` refuses a flagged change at landing instead of only reporting it at `verify` (#3).
- `risk_sensitive` accepts the comma-separated text `config --set` stores (#4).
- The daemon writes its endpoint file atomically, and a client retries a timed-out connect with backoff, so a caller that polls for the endpoint never races it (#5).
- Merging two changes that each inserted an item into the same module keeps the module's first item indented (#6).

## 0.1.1 - 2026-09-10

### Added

- README: a get-started guide, mechanism diagrams, a logo, and sections on standards and hooks, working alongside git, reading the repository, swarms, memory, undoing and removing, configuration, and troubleshooting.
- Bugs are filed as GitHub issues, with an issue template that mirrors the entry format in BUGS.md.
- CONTRIBUTING.md, this changelog, and a vulnerability reporting policy in SECURITY.md.
- The repository is developed under Tessra: `.mcp.json` makes a Claude Code session agent `claude`, and DEVELOPING.md documents the loop.

### Fixed

- `snapshot --then verify` runs the verifiers and records their attestations, as `verify` does, instead of only evaluating the standard.
- The secret scanner no longer flags its own source: its needles are assembled at compile time and its test fixture at run time, so a checkout of Tessra snapshots without a secrets flag.

### Changed

- On Windows, commands the daemon runs for itself (verifiers, git, hooks, deployers, tool probes) no longer open a console window. The daemon has no console, and each child used to get a new one that popped in front of whatever the person was doing.
- In this repository, a Claude Code session acts as agent `claude-code`; the name `claude` was revoked by the risk monitor and revocation is permanent.
- Bugs are GitHub issues only. BUGS.md is a frozen archive, and the fix workflow moved to CONTRIBUTING.md.
- The design documents (VISION, TENETS, ADOPTION, ROADMAP, MEMORY, PARALLELISM, PIPELINE, TESTING, LIFECYCLE, HOOKS, RISK, SECURITY, VERBS, GAPS) moved from the top level into `docs/`, beside the diagrams. Links between them are unchanged; the README and spec point at the new paths.
- The repository's own standard now requires a clean `cargo clippy --workspace --all-targets -- -D warnings` as well as passing tests; the seven warnings the workspace had are fixed.
- The workspace is formatted with rustfmt, and the standard requires `cargo fmt --all --check` to pass.

## 0.1.0 - 2026-09-10

### Added

- First public release: the `tessra` binary for macOS, Linux, and Windows on x86_64 and arm64, with shell and PowerShell installers, built and published by a cargo-dist release pipeline.
- Milestones M0 through M8, each with a passing end-to-end demo: substrate, semantic merge, standards and verification, swarms, the agent surface, production targets, the git bridge, and state bundles.
