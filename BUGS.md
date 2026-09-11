# Bugs (archive)

Bugs are tracked as [GitHub issues](https://github.com/NitrusAphalion/tessra/issues); report one with the [bug report template](https://github.com/NitrusAphalion/tessra/issues/new?template=bug_report.md), and [CONTRIBUTING.md](CONTRIBUTING.md) has the workflow a session follows to fix one. This file is frozen: it holds the entries logged before bugs moved to the issue tracker, and it is not updated.

Known design gaps that are **not** bugs (single machine only, the L0 verifier sandbox, stand-in deployer adapters, and the rest) live in `spec/README.md` under the deviations sections.

*Entries below cite commit hashes such as `42a26fb` and `7440272` from the project's history
before it was open-sourced as Tessra. Those commits are not part of this repository.*

The four entries that were still open when this file was frozen are now issues [#1](https://github.com/NitrusAphalion/tessra/issues/1), [#2](https://github.com/NitrusAphalion/tessra/issues/2), [#3](https://github.com/NitrusAphalion/tessra/issues/3), and [#5](https://github.com/NitrusAphalion/tessra/issues/5).

---

## Resolved

### The secret scanner flagged its own source, so every snapshot of this repository carried a secrets flag
- **Status:** resolved · commit `7a8b447` · 2026-09-10
- **When:** 2026-09-10
- **Ran:** `tessra --agent claude --workspace <id> snapshot --title "..."` in this repository, right after `tessra init`
- **Expected:** no flags; the tree holds no secrets
- **Got:** `flags.secrets` named `crates/tessra-daemon/src/fs.rs` twice, for the private-key-block and aws-access-key patterns: `scan_secrets` spelled out the PEM header and footer it looks for, and its unit test's fixture started with the AWS key prefix followed by sixteen alphanumerics. Every snapshot reported `structural(flags.none)` unmet and a risk score of 30 from the `secrets` factor.
- **Where:** M2 · tessra-daemon · fs.rs. Fix: the needles are assembled at compile time with `concat!` and the fixture at run time with `format!`, so the file never contains them verbatim; what the scanner detects is unchanged. An exemption for paths that legitimately hold secret-shaped text remains open as a feature.
- **Env:** Windows 11, release, `tessra 0.1.0` at ed3c646

### `import --branch` leaves the colocated checkout's workspace on the pre-import revision
- **Status:** resolved · commit `42a26fb` · 2026-09-06
- **When:** 2026-09-06
- **Ran:** in vectaris, after `git commit` + `git push` of three root config files: `tessra import --branch main` (landed 1 commit, trunk seq 99 -> 100), then `tessra status` and `tessra workspace --action list`
- **Expected:** the owner's colocated workspace (the checkout itself, whose files are at the new commit) to sit on the landed revision: `state.revision == trunk.head`, and `status` to name the imported commit's title
- **Got:** the root workspace's `base` and `current` still `dc1ef461…` (seq 99) while `trunk.head` is `0f28aa88…` (seq 100); `status` names the old revision and its title. The next owner `snapshot` would diff against seq 99 and re-record the already-landed edits as a new revision on a stale parent, and `context`/`query` for the owner describe the old snapshot.
- **Repro:** `tessra init` in a git checkout; commit something with git; `tessra import --branch main`; compare `status` state.revision with state.trunk.head
- **Where:** M7 · tessra-daemon · bridge.rs (`import` lands through a temporary owner workspace and never advances the root workspace; `restack_children` only handles unlanded changes). Fix: after landing, move the root workspace's base/current to the landed revision when its files match the imported commit (they do by construction), and likewise after `export --format git` resets the checkout.
- **Env:** Windows 11, release, `tessra 0.1.0` at 7532475
- **Fix:** after an import, and after an export that reset the checkout, `bridge::follow_checkout` points the colocated workspace at the landed revision HEAD's commit maps to; a checkout workspace whose current revision is not landed (an owner draft) is left alone. The result reports it under `workspace`. Tests: `import_moves_the_checkout_workspace_to_the_landed_revision`, `import_leaves_a_checkout_workspace_with_unlanded_work_alone`, `export_moves_the_checkout_workspace_to_the_head_it_reset_to`.

### `export --format git` never reset a clean checkout of the exported branch
- **Status:** resolved · commit `42a26fb` · 2026-09-06
- **When:** 2026-09-06, found by the new export test while fixing the entry above
- **Ran:** `export_git` with the checkout on `main`, clean, and one landed revision to export
- **Expected:** `checkout: "updated"` and the working tree reset to the new commit
- **Got:** `checkout: "dirty, not touched"` every time. Export moved `refs/heads/main` with `update-ref` *before* asking `git status` whether the checkout was clean; once the ref sits ahead of the index, git reports the new commit's files as staged deletions, so the checkout always looked dirty and the working tree stayed at the old commit with the branch moved under it.
- **Fix:** judge cleanliness before the ref moves, then reset. Test: `export_moves_the_checkout_workspace_to_the_head_it_reset_to`.
- **Where:** M7 · tessra-daemon · bridge.rs (`export_git`)
- **Env:** Windows 11, git with autocrlf (the test normalizes CRLF)

### `context --path` overshoots its token budget several times over
- **Status:** resolved · commit `7440272` · 2026-09-06
- **When:** 2026-09-06
- **Ran:** `tessra context --path platform/src/lib/stores/auth.svelte.ts --budget 200` (in the vectaris repo, a 2,249-file TypeScript monorepo)
- **Expected:** `budget.used` at or under 200, with a cursor for the rest
- **Got:** `budget.used: 1124`, `budget.limit: 200`; the file entry is cut to 800 characters with `truncated: true`, then the full unit list (12 units, including import statements as unit names) is appended regardless. With `--budget 1500` the same call reports `used: 1813`.
- **Repro:** any `context --path <file>` with a budget smaller than 800 chars of the file plus its unit list
- **Where:** M5 · tessra-daemon · verbs.rs (context pack budget accounting; the per-file floor and the units section are not charged against the limit)
- **Env:** Windows 11, release, `tessra 0.1.0`
- **Fix:** every part of the pack is charged by its serialized size and each list is cut where the budget runs out; the content leaves up to a quarter of the budget for the units; `truncated` and `omitted` say what was cut; text is cut on char boundaries by its encoded size. Tests: `context_path_stays_inside_its_budget_and_keeps_the_map`, `text_is_cut_by_its_encoded_size_on_char_boundaries`.

### A failed verifier's evidence is unreadable through `query`: the head of a 64 KiB tail
- **Status:** resolved · commit `7440272` · 2026-09-06
- **When:** 2026-09-06
- **Ran:** `tessra verify --full` in vectaris with `verifiers.tests.pass` set to a pnpm chain; the run exited 1 with `passed: 0, failed: 0`; then `tessra query --kind object --id <attestation.evidence> --budget 200000`
- **Expected:** enough of the verifier's output to see why it failed (vitest's `FAIL ... Error: Failed to resolve entry for package "backtest-engine"` lines and the summary, which come last)
- **Got:** 2,441 characters of mid-run vitest progress lines and nothing else; `unmet` says only "tests.pass is false on this snapshot; failed: []; fix it and run verify". `attest_as_runner` keeps the last 64 KiB of output as the evidence blob, and `object_json` renders a blob as its first 4,000 bytes with no cursor or offset, so the end of the output (where every test runner puts its summary and errors) is unreachable. With `Filter::None` runners nothing is parsed either, so `failed` is empty.
- **Repro:** any verifier whose output exceeds 4,000 bytes and fails late
- **Where:** M3 · tessra-daemon · verifiers.rs (`attest_as_runner`), verbs.rs (`object_json`, the `unmet` reason for `attest` clauses)
- **Env:** Windows 11, release, `tessra 0.1.0`
- **Fix:** `query --kind object` pages a blob to the budget with `--offset` (and `next_offset` in the response) or `--tail` for its end; a failed run's `ran` entry carries `exit`, `evidence`, and `output_tail`; the attestation scope records `exit`; the unmet reason names the exit code and the query that reads the output. Tests: `object_query_pages_a_blob_to_its_end`, `tail_text_keeps_the_end_of_the_output_whole`, `a_negative_attestation_says_what_to_read`.

### Every CLI command overflowed the stack, `--version` included
- **Status:** resolved · commit `622d6e9` · 2026-09-05
- **Ran:** any `tessra` command, e.g. `tessra --version`
- **Got:** `thread 'main' has overflowed its stack`; the process aborts before doing anything
- **Cause:** in a debug build, clap's derived command builder and the verb dispatch have stack frames past the 8 MiB main-thread limit; adding the M8 subcommands tipped it over.
- **Fix:** `main` now runs the work on a 64 MiB thread and exits with its code. Watch for this if you add more subcommands.
