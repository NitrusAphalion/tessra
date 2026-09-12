# Developing Tessra

Found a bug while running Tessra? File a [GitHub issue](https://github.com/NitrusAphalion/tessra/issues/new?template=bug_report.md); [CONTRIBUTING.md](CONTRIBUTING.md) has the `gh` one-liner for agent sessions and the workflow a session follows to fix one.

## Toolchain

Rust stable, pinned by `rust-toolchain.toml`. On Windows use the MSVC target: install Visual Studio Build Tools with the C++ workload, then

```
rustup set default-host x86_64-pc-windows-msvc
rustup default stable-x86_64-pc-windows-msvc
```

`rust-toolchain.toml` names the channel only, and rustup resolves it against the default host, so setting the default host is what makes the repository build on MSVC. The C++ tools are also what the semantic index needs from M2 on, since tree-sitter's runtime and grammars are C.

### Windows without Visual Studio (fallback)

The GNU target works without Visual Studio Build Tools, with one arrangement: crates that link Windows DLLs through `raw-dylib` need a `dlltool`, and the self-contained one in rustup's GNU toolchain cannot run without a full binutils. LLVM's archiver acts as a dlltool when named that way, and rustup's `llvm-tools` component ships it.

```
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup default stable-x86_64-pc-windows-gnu
rustup component add llvm-tools
copy %USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\llvm-ar.exe %USERPROFILE%\.cargo\tools\llvm-dlltool.exe
setx CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS "-C dlltool=%USERPROFILE%\.cargo\tools\llvm-dlltool.exe"
```

Two consequences of this arrangement, neither of which applies on MSVC:

- Binaries that link `gix` crash at startup. The git import shells out to the installed `git` instead, which also matches the spec's view of the git object database as an adapter, so this stays.
- clap's color support crashes at startup, because its console calls go through the LLVM dlltool import library. Build `tessra-cli` with clap's `color` feature off on GNU.

## Layout

| Crate | Spec section | Holds |
|---|---|---|
| `tessra-core` | 01, 02 | Canonical CBOR, hashing, identifiers, signatures, the object catalog, the trie, the store contract |
| `tessra-oplog` | 03, 04 | Views, effects, merge, the op DAG, verification, standards, init, undo |
| `tessra-store` | 05 | The redb object store with heads and the idempotency index, the pack format |
| `tessra-daemon` | 05, VERBS | Repository layout, workspaces with snapshot and materialize, file-level tree merge, agents and sessions with capabilities, the verbs, git import, the loopback daemon and its client |
| `tessra-cli` | VERBS, 05 | The `tessra` command and the MCP stdio server |
| `tessra-semantic` | 02 nodeindex, 03 landing | Tree-sitter extraction for Rust, Python, JavaScript, and TypeScript with a chunk fallback, format-insensitive body hashes, unit references, history-aware unit identity, renames as data, and the node-level three-way merge that carries one side's renames into the other |

## Commands

```
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

The spec wins over the code. A test that disagrees with `spec/` is a bug in one of them, and the spec is fixed first.

## Trying it

```
cargo build
target/debug/tessra init            # in any directory; in a git checkout, also imports HEAD
target/debug/tessra status --pretty
target/debug/tessra remember --kind gotcha --body "..." --scope-kind path --scope-ref src
target/debug/tessra --agent my-agent workspace --action create
target/debug/tessra --agent my-agent --workspace <id> edit --path src/x.rs --content "..."
target/debug/tessra --agent my-agent --workspace <id> snapshot --title "..." --then verify
target/debug/tessra --agent my-agent --workspace <id> promote --to proposed
target/debug/tessra --workspace <id> promote --to landed   # as the owner
target/debug/tessra mcp                                    # MCP over stdio, thirteen tools
target/debug/tessra export --format agents-md              # render shared memory for other tools
target/release/tessra bench                                # the M1 budgets, recorded as an attestation
```

Landing a change that conflicts with trunk does not fail: the merged snapshot with conflict entries becomes the next revision of the change, the workspace gets the file with markers plus a `.tessra-conflict` sidecar, and a task memory is opened. Edit the file with `--resolve` to drop the sidecar, snapshot, and promote again.

`tessra init --history N` imports the last N first-parent commits as trunk, oldest first, each as its own change with a legacy intent. The default is 100.

Every snapshot carries a semantic index. `tessra context --path <file>` is the file's content, its units with what each references and which tests cover it, memories, and overlapping claims, all charged against the budget so the response never exceeds it; the content leaves room for the units, and `truncated` with `omitted` counts says what was cut. `tessra query --kind blame --path <file>` names the revision, author, title, and intent that last changed each unit; `--kind tests --path <file>` is the covering-tests relation, with the units no test names; `--kind diff` lists the units a revision added, removed, changed, or renamed against its parent. Landings merge files that both sides changed at unit granularity: different units never conflict, imports and enum variants union, a reformat loses only to an edit of the same unit, and a conflict names the unit.

The trunk standard is data. `tessra standard` shows it; the owner edits it with `--require 'attest(tests.pass)'`, `--require 'attest(tests.fail_on_parent, for=new_tests)'`, `--require 'structural(changed.covered)'`, `--forbid 'structural(test.weakened)'`, and `--remove <index or text>`. Predicates are `kind(name, key=value, ...)`; `all`, `any`, and `not` nest. Structural names: `flags.none`, `intent.linked`, `changed.covered`, `test.modified`, `test.deleted`, `test.weakened`, `test.skipped`.

`tessra verify` runs what the standard still needs and reports it clause by clause. The daemon detects the verifier from the tree (`Cargo.toml` runs `cargo test`, a Python project runs `python -m pytest`, `package.json` runs `npm test`); a `verifiers` map in `.tessra/config` (attestation kind to command line) overrides it, and `verify_timeout_s` bounds a run. Runs happen in a scratch directory under `%LOCALAPPDATA%\tessra\workspaces\<repo>\verify-*` with a stripped environment and no keys, and cargo builds share `verify-target` there. A whole-suite result attests the snapshot, so identical content is never verified twice in the same environment; when every changed unit has a covering test, only those tests run and the result attests the units by body. New tests are laid into the parent's text and run there; one that passes on the parent does not prove the change. `--full` runs everything. A failed run reports `exit`, the `evidence` blob, and `output_tail`, the end of the runner's output; `tessra query --kind object --id <evidence> --tail` reads the end of the whole evidence within the budget and `--offset N` pages through it, and the unmet clause names that command. `tessra attest --kind k [--subject id] [--result v]` records an attestation under your own principal; a session's attestation is stored but never satisfies a clause. Landing cites the attestations that apply and runs what the merge invalidated.

Proposing publishes; landing is what the standard gates. Hooks react to stage events: `tessra hook --name notify-ci --on proposed --do 'webhook(http://ci.local/run)'` posts the event as JSON, signed by the daemon in `X-Tessra-Signature`; other actions are `run(cmd)` with the event on stdin, `verify()`, `attest(kind)`, and `task(text)`; `--where 'scope(src/**)'` filters; `--disable`/`--enable` toggle; `tessra hook` lists. Events: `proposed`, `landed`, `conflict.opened`. `tessra grant --external ci` creates a principal that may only attest and whose attestations count; the CI system calls back with `tessra --as ci attest --kind ci.pass --subject <snapshot> --result true`. While a verifier runs the daemon answers every request with `BUSY`, so code under test cannot act through it.

Every verify and promote records a `risk.change` attestation: a score, a level, and factors each with what would lower it (size, blast radius, untested units, weakened tests, dependency manifests, sensitive paths from `risk_sensitive` in `.tessra/config`, flags, collisions with unlanded work). `tessra standard --when high --require 'approved(human)'` adds clauses that apply from a level up; the refusal names the top factors. `tessra query --kind trusted --change <id>` ranks the trunk snapshots containing a change by their attestations. `tessra query --kind bisect --test <name>` finds the first trunk revision where a test in your revision fails, laying the test into each probed snapshot; results are attested by the bodies of the units the test depends on, so revisions that did not touch them are never run twice.

`tessra plan --intent "..." --paths 'src/**' --agents 50` partitions the units under the paths into groups that keep dependency-connected units together, balanced across the agents, and records a parent intent, one sub-intent per group assigned to an agent, and a task per sub-intent. Each agent's next session is scoped to its group's paths with the op budget from `--ops`; `tessra --agent <name> status` shows the assignment. `claim` answers with the other claims that overlap yours. `tessra promote --to landed --all` is the integration frontier: the owner lands every proposed change that meets the standard, oldest first, and reports each outcome and the landings per hour. After a landing, unlanded changes stacked on the landed revision are restacked onto it when their workspace is clean; a dirty one merges at landing anyway. `tessra revoke --name <agent>` revokes the agent and its sessions and, in the same op, unpoints and unproposes their unlanded changes, releases their claims, retires their memories, and drops their workspaces.

`tessra grant --to planner --verbs plan --paths 'src/**' --delegable --ops 5000` gives an agent's sessions a standing grant. When such an agent runs `plan`, it opens a session for each assignee and issues a child capability contained in its own, signed by the planner, with the group's paths and budget and a `parent` link; the verifier walks the chain to the owner on every op the assignee signs. `tessra hook --name auto-land --on proposed --do 'land()'` makes the frontier continuous: proposing lands when the standard holds. `tessra revert --change <id> [--reason ...]` builds a change that undoes a landed one at unit granularity, opens a task, and lands it at once when the owner asks or proposes it otherwise; it refuses when later changes built on the units it would undo.

`tessra context --unit <name>` (or `path:name`) is the context pack for one unit inside `--budget` tokens: its text, the units it depends on, the units that depend on it, the tests that cover it, memories about it and its file, overlapping claims, and who last changed it, cut to the budget in that order; without a workspace it describes trunk. `tessra try --candidates-file cands.json` takes a JSON array of `{path, content}` or `{path, old, new}` candidates, each with an optional title, materializes each on your revision as a revision of its own, runs the verifiers, scores the risk, and ranks them by unmet clauses, failed tests, risk, and size; `--keep <n>` writes a candidate into your workspace. `tessra query --kind activity --window 1h --altitude summary|changes|ops` is the activity pack.

Approvals: `tessra grant --human maria` creates a human principal whose key the daemon holds and prints her credential once (granting again reissues it), `tessra channel --name oncall --kind inbox --path <dir> --principals maria` a channel (or `--kind webhook --url ...`), and `tessra standard --when high --require 'approved(human, keysigned=true)'` the clause. When a landing or a verify finds the clause unmet, the daemon opens one approval request per revision as a question memory and delivers it to every channel; `tessra --as maria approve --request <id> [--no] [--note ...]` answers with a key-signed `approval.human` attestation on the revision and closes the question. `--as maria` needs her credential (`--credential`, `TESSRA_CREDENTIAL`, or the prompt), so a process that merely reaches the daemon cannot answer as her. Sessions cannot attest approvals. An approval binds to the revision it was given on; it carries to the landing, restack, or conflict merge the system derives from that revision, never to a re-snapshot the author makes afterwards.

Owner actions that set policy take the owner credential the same way: `standard`, `hook`, `channel`, `target`, `grant`, `revoke`, `config --set`, `attest`, `revert`, and `undo` ask for it, and `init` prints it (it stays in the keys directory as `owner.credential`, and the daemon keeps only a hash). Reads, `promote`, `verify`, `export`, `import`, `release`, and `observe` do not, because the standard gates them.

Judges are sessions of agents other than the author's, each attesting `judge.<rubric>` on the proposed revision with `--result '{"ok": true, "confidence": 900, "reasoning": "..."}'` and a `--model`. A clause `judge(matches-intent, judges=2, distinct_models=true, min_confidence=700) unless approved(human)` needs that many agreeing judges with distinct models; one judge saying no is an exception: the daemon opens a request carrying the judges' reasoning, delivers it through the channels, and `tessra query --kind exceptions` lists the queue. A human approves or declines with `approve`. Hooks can spawn agents: `tessra config --set agent_runner=<command>` names the runner, and a hook action `agent(name, intent=..., budget=...)` records an intent and an assignment scoped to the event's paths, then starts the runner detached with `TESSRA_AGENT`, `TESSRA_INTENT`, `TESSRA_EVENT`, `TESSRA_REPO`, and `TESSRA_EXE` in its environment; it speaks to the daemon like any agent. The risk monitor scores anomalies per agent: edits outside its claims, edits and snapshots outside its write scope, landings refused for weakened tests, and attempts made while throttled; past `anomaly_throttle` points the agent's mutations are refused for `anomaly_cooldown_s`, past `anomaly_revoke` it is revoked and unwound; hooks see `anomaly.detected` and `anomaly.revoked`. `tessra config --set key=value` sets machine-local keys, `verifiers.<kind>=<command>` among them.

Production: `tessra target --name prod --deployer 'run(<cmd>)' --observers 'run(<cmd>)' --canary 10 --secrets vault:local:DEPLOY_TOKEN` defines a target with its own release standard extending trunk's (`tessra standard --target prod --require 'observe(error_rate, max=50)'`); a `dir(<path>)` deployer copies the snapshot into `<path>/<slice>`, a `run` deployer gets `TESSRA_TARGET`, `TESSRA_SLICE`, `TESSRA_PERCENT`, `TESSRA_REVISION`, `TESSRA_SNAPSHOT_DIR`, `TESSRA_STEP`, and the secrets from `vault.<NAME>` configuration keys, in its environment only. `tessra release --name v1` cuts a signed release of the trunk head with a changelog and the attestations that apply. `tessra promote --to prod --slice canary` deploys the head to the canary slice under trunk's standard; `--slice all` requires the target's own clauses too. `tessra observe --target prod` runs the observers, records each signal as an `observe.<name>` attestation on the deployed revision, evaluates the target's standard, and when an observe clause trips rolls back to the previous deployment of another revision, opens a task, and tells every channel; `auto_revert=false` in configuration turns the rollback off. `tessra revert --target prod` rolls back on request. `tessra target` lists targets with what is deployed and the latest releases. Hooks see `released`, `deployed`, `observed`, `observed.fail`, and `target.rolled_back`, and a `notify(channel=..., text=...)` action delivers to a channel.

The git bridge: `tessra export --format git --branch main [--push origin]` writes every landed trunk revision that is not yet a commit as one commit on top of the newest that is, authored by the change's author with `Tessra-Change` and `Tessra-Revision` trailers, updates the branch, resets a clean checkout of that branch to it, and pushes when asked; nothing already a commit is rewritten, and the mapping lives in store metadata and in imported revisions' bodies. `tessra import --branch main` lands every commit the branch gained since the last export or import, first-parent order, through the normal landing. Both move the checkout's own workspace to the revision HEAD now is, so the owner's `status` and next snapshot start from it; a checkout workspace holding an unlanded owner snapshot is left alone, and the result says so under `workspace`. The verifier and deployer commands see paths without Windows' verbatim prefix.

State bundles: `tessra snapshot --with-state` (or config `snapshot_state=true`) records an environment on the snapshot with the toolchain versions, the hashes of any lockfile, and one tree per state path (config `state_paths`, default `target,data,.venv,node_modules`), so build state and experiment output travel with the change. `tessra workspace --action create --from <revision> --with-state` restores those state paths beside the files, so a checkout runs with no install step. `tessra workspace --action rewind --to <revision>` puts your current workspace back at a revision, files and state together, so an experiment rewinds including everything it wrote outside the source tree. The landing carries the change's environment onto trunk. The `tessra` CLI runs its work on a 64 MiB thread because clap's derived command builder and the verb dispatch have large frames in a debug build.

`tessra edit --rename old --to new` is the first semantic operation: it renames the identifier as a whole word in every tracked file with a grammar (or one `--path`) and records the op on the change. At landing, each side's renames, recorded or inferred from a unit keeping its identity under a new name, are applied to what the other side contributes, so a concurrent change that added a call to the old name lands calling the new one, in the same file or another. The landing result lists each such resolution under `semantic`. A rename and a body edit of the same unit compose; a unit renamed differently on both sides, or a unit added under a name the other side renamed something to, is a conflict.

Git is optional at run time. `init` runs the `git` command only when a `.git` directory is present, to import the history, and the bridge verbs `export --format git` and `import --branch` run it; no other verb does, so a repository without git works end to end. Every command prints the response shape from VERBS.md. The store and keys live under the local application data directory, never in the repository.

## The daemon

Every command except `init` talks to the repository's daemon, starting one detached if none is running, and falls back to running in-process only if that fails or `--no-daemon` is given. The daemon opens the store once, keeps sessions in memory, serves newline-delimited JSON over loopback TCP, and exits after thirty idle minutes. `.tessra/daemon` holds its port, a bearer token, and its pid; the file is removed on exit. The token authenticates a client to the daemon and nothing more: acting as the owner on a policy verb, or as a human or external with `--as`, takes a credential the daemon checks against a hash it keeps in the keys directory.

```
tessra daemon --idle-minutes 30     # run it in the foreground yourself
```

On Windows a detached child inherits every inheritable handle of its parent, including a pipe a caller is reading. The CLI marks its standard handles non-inheritable before spawning the daemon, or `tessra status | something` would block until the daemon exited.

## Developing under Tessra

The repository is developed under Tessra. The daemon that manages it should be a released binary, not the working tree you are changing, so install one and keep `target/debug/tessra` for trying your changes:

```sh
cargo install --path crates/tessra-cli --locked   # or the installer from the README; either puts `tessra` on PATH
tessra init                                       # once per clone; imports the git history as trunk
tessra status --pretty
```

The store and the standard are per machine, so a fresh clone sets them up once. This is the standard the repository is developed under; `verify` runs the three verifiers in a scratch copy and lands nothing until all of them attest. These are owner commands, so run them from a terminal with the owner credential `init` printed in `TESSRA_CREDENTIAL`, or answer the prompt; an agent session has no credential, which is the point:

```sh
tessra config --set 'verifiers.tests.pass=cargo test --workspace'
tessra config --set 'verifiers.lint.clean=cargo clippy --workspace --all-targets -- -D warnings'
tessra config --set 'verifiers.fmt.clean=cargo fmt --all --check'
tessra standard --require 'attest(tests.pass)' --require 'attest(lint.clean)' --require 'attest(fmt.clean)'
tessra standard --forbid 'structural(test.weakened) unless approved(human)'
```

The trunk standard therefore requires `attest(tests.pass)`, `attest(lint.clean)`, and `attest(fmt.clean)`, so a clippy warning or an unformatted file blocks a landing the way a failing test does, and forbids `structural(test.weakened)` unless a human approves. Run `cargo fmt --all` before you snapshot. `test.weakened` means any existing test that was modified or deleted, so a change that touches a test needs the owner's approval attested on its revision before it lands:

```sh
tessra --as malon attest --kind approval.human --subject <revision> --result true   # asks for malon's credential
```

`tessra grant --human malon` printed that credential once; a human granted before credentials existed gets one by being granted again. Approve first and land once: every refused landing for a weakened test charges the change's author anomaly points, and six points revoke the agent for good. That is how agent `claude` was lost on the first day, which is why a Claude Code session in this checkout acts as agent `claude-code` through `.mcp.json`. Any other agent or person follows the same loop:

```sh
tessra --agent <you> workspace --action create            # a directory of your own; the response names it
# edit there with any tool
tessra --agent <you> --workspace <id> snapshot --title "..." --then verify
tessra --agent <you> --workspace <id> promote --to proposed
tessra promote --to landed --all                           # the owner lands what meets the standard
tessra export --format git --branch main && git push       # landings become commits on main
```

Record what you learn with `tessra remember`; `tessra context --path <file>` shows it to the next session. Testing a change to Tessra itself means running `target/debug/tessra --no-daemon` against a scratch repository, never against this repository's daemon.

## Releasing

Releases are built by [dist](https://axodotdev.github.io/cargo-dist) from `dist-workspace.toml`. `.github/workflows/release.yml` is generated from that file and is not edited by hand. Pushing a tag `vX.Y.Z` that matches `version` under `[workspace.package]` in `Cargo.toml` builds `tessra` for macOS, Linux, and Windows on x86_64 and arm64, and publishes a GitHub release carrying the archives, the shell and PowerShell installers, checksums, and a source tarball. A version with a pre-release suffix, such as `v0.2.0-beta.1`, is published as a pre-release, which `releases/latest` and the install one-liners in the README skip.

```sh
# bump version under [workspace.package] in Cargo.toml and commit, then
git tag v0.1.0
git push origin v0.1.0
```

Pull requests run `dist plan` only. To preview a release locally, or after changing `dist-workspace.toml`, install dist and regenerate the workflow; CI refuses to run while the workflow is out of date:

```sh
cargo install cargo-dist --locked   # or the installer on the dist releases page
dist plan                           # what a release of the current version contains
dist generate                       # rewrite .github/workflows/release.yml
dist build                          # this machine's archive, into target/distrib
```

The binaries are not code-signed. A build downloaded through a browser is blocked by Gatekeeper on macOS until the project signs and notarizes with an Apple developer account, and SmartScreen warns on Windows. The installers fetch from the command line, which on macOS does not set the quarantine attribute.
