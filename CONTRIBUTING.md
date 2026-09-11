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
3. fixes it through the Tessra loop, so `verify` runs the suite, and puts `Fixes #N` in the change's title, which becomes the commit that closes the issue;
4. comments on anything it cannot reproduce with what it tried, and leaves it open.

## Changes

1. Fork and branch. `main` is protected: changes land through a pull request, and every commit must carry a verified signature, so set up [commit signing](https://docs.github.com/en/authentication/managing-commit-signature-verification/signing-commits) before you start.
2. Build and test with `cargo test --workspace` and `cargo clippy --workspace --all-targets`. [DEVELOPING.md](DEVELOPING.md) covers the toolchain, the crate layout, every verb, and the daemon.
3. Keep the spec honest. When a design document and [spec/](spec/README.md) disagree, the spec wins and the document gets fixed, so a change in behavior comes with the matching change in the spec or its deviations list.
4. Note user-visible changes under **Unreleased** in [CHANGELOG.md](CHANGELOG.md). The release pipeline uses that section for the release notes.
5. Open the pull request. Say what changed, why, and how you verified it.

## Agent sessions

Sessions of Claude Code and other agents contribute here too, under the same rules. The repository is developed under Tessra itself; `tessra status` in a checkout shows the standard a change has to meet.

## Releases

Maintainers cut a release by pushing a version tag. The Releasing section of [DEVELOPING.md](DEVELOPING.md) has the steps.
