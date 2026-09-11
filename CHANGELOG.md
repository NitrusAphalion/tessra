# Changelog

Notable changes to Tessra, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the release pipeline uses the **Unreleased** section as the notes of the next release.

## Unreleased

Nothing yet.

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
