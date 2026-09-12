# Changelog

Notable changes to Tessra, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the release pipeline uses the **Unreleased** section as the notes of the next release.

## Unreleased

### Fixed

- On Windows, a verifier, hook, or deployer command whose first word is a bare name is resolved through `PATH` and `PATHEXT` before it is spawned, so the detected `npm test` runs where only `npm.cmd` exists, and so do `pnpm`, `yarn`, and `npx`. The environment fingerprint names the tool behind a `cmd /c` wrapper instead of `cmd`, and records `node --version` for Node tools (#9).
- `export --format git` fast-forwards a checkout of the branch instead of moving the ref under it, so a dirty checkout with unrelated edits receives the exported files and its index follows HEAD. When a local edit overlaps the export, or the branch holds commits trunk does not know, the export refuses before anything moves and says how to proceed (#13).
- `workspace --action create --path` refuses a path inside the repository or an existing directory with contents, instead of treating the checkout as an empty target and deleting every ignored file in it. `workspace --action rewind` removes only files the tracking rules would snapshot, so dependencies, build output, and local environment files survive a rewind (#7).
- The secret scanner flags a private key only when a `BEGIN … PRIVATE KEY` armour line is followed by at least 64 characters of base64 body and the matching `END` line, so documentation that names the armour, a parser's regular expression, or an empty test fixture no longer carries `flags.secrets`. A key pasted into a JSON string, with its newlines escaped, still does. A snapshot's secrets flag covers only the files the change touched against its base revision, so a match trunk already held does not block unrelated work (#8).

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
