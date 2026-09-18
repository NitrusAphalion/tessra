# Contributing

Thanks for looking. Tessra is pre-release and moving quickly, so a short conversation before a large change saves everyone time: open an issue describing what you want to do.

## Bugs

File a [bug report](https://github.com/NitrusAphalion/tessra/issues/new?template=bug_report.md). Only the title, the command you ran, and what happened are required. An agent session with the `gh` CLI signed in files it directly; write the body to a file first so it survives any shell's quoting:

```sh
gh issue create --repo NitrusAphalion/tessra --label bug --title "<short title>" --body-file bug.md
```

Inside a repository under Tessra, `tessra remember --kind gotcha --body "…" --scope-ref <path>` records it as a memory too, but the issue is the record. `BUGS.md` is a frozen archive from before the project had an issue tracker; nothing goes there.

A session working a bug:

1. lists what is open with `gh issue list --repo NitrusAphalion/tessra --label bug` and triages, most reproducible first;
2. reproduces it in a scratch repository, and writes a failing test wherever one fits;
3. fixes it through the Tessra loop, so `verify` runs the suite, and puts `Fixes #N` in the change's title, which becomes the commit that closes the issue when trunk is exported;
4. comments on anything it cannot reproduce with what it tried, and leaves it open.

## Changes

In a checkout under Tessra, which is how the maintainers and their agent sessions work, a change never touches git:

1. `tessra --agent <you> workspace --action adopt`, edit with any tool, `snapshot --then verify`, then `promote --to proposed`. The owner lands it and exports trunk to `main`, and the commit carries your name. A `git commit` in this checkout goes around the standard, and `.claude/settings.json` refuses it to agent sessions.
2. Format with `cargo fmt --all` before you snapshot. The standard refuses a change with a failing test, a clippy warning, or an unformatted file, and `verify` says which; `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` are what it runs. [DEVELOPING.md](DEVELOPING.md) covers the toolchain, the crate layout, every verb, and the daemon.
3. Keep the spec honest. When a design document and [spec/](spec/README.md) disagree, the spec wins and the document gets fixed, so a change in behavior comes with the matching change in the spec or its deviations list.
4. Note user-visible changes under **Unreleased** in [CHANGELOG.md](CHANGELOG.md). The release pipeline uses that section for the release notes.

From a fork, without the maintainers' daemon, the path is git's: branch, sign your commits (`main` requires verified signatures, so set up [commit signing](https://docs.github.com/en/authentication/managing-commit-signature-verification/signing-commits) first), and open a pull request that says what changed, why, and how you verified it. What merges is imported into the store as history.

## Agent sessions

Sessions of Claude Code and other agents contribute here too, under the same rules, and through the loop above rather than git. The repository is developed under Tessra itself; `tessra status` in a checkout shows the standard a change has to meet.

## Releases

Maintainers cut a release by pushing a version tag. The Releasing section of [DEVELOPING.md](DEVELOPING.md) has the steps.
