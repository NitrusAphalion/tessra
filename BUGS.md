# Bugs

Tessra is public, so bugs are filed as [GitHub issues](https://github.com/NitrusAphalion/tessra/issues). This file keeps the template, the workflow a session follows to fix a bug, and the entries logged while the project was dogfooded privately.

## If you hit a bug

File an issue. Keep it cheap to write: a title, the command you ran, and what happened is enough. The more of the template you fill in, the faster it gets fixed.

- **In a browser:** [open a bug report](https://github.com/NitrusAphalion/tessra/issues/new?template=bug_report.md). The form is pre-filled with the template below.
- **From an LLM or agent session** with the `gh` CLI signed in, file it directly. Write the body to a file first so it survives any shell's quoting:

  ```sh
  gh issue create --repo NitrusAphalion/tessra --label bug --title "<short title>" --body-file bug.md
  ```

  If `gh` is not signed in or there is no network, append the entry under **Open** below instead and say so in your handoff, so the next session files it.
- **Inside a repository under Tessra:** `tessra remember --kind gotcha --body "…" --scope-ref <path>` records it as a memory too. Handy in the moment, but the issue is the canonical record.

Issue body template (copy it; `.github/ISSUE_TEMPLATE/bug_report.md` is the same):

```
- **Ran:** `tessra …`   (the exact command, or what you were doing)
- **Expected:** …
- **Got:** …   (paste the panic / error / wrong output, trimmed to the useful lines)
- **Repro:** the smallest steps that trigger it, if known
- **Where:** milestone / crate / file, if you can guess (e.g. M4 · tessra-daemon · swarm.rs)
- **Env:** OS, debug or release, and `tessra --version`
```

Only **title**, **Ran**, and **Got** are required. Leave the rest blank if you don't know.

## How these get fixed

A Claude Code session working on Tessra:

1. lists the open issues with `gh issue list --repo NitrusAphalion/tessra`, plus any **Open** entries below, and triages them most reproducible first;
2. reproduces each in a scratch repo, and writes a failing test wherever one fits;
3. fixes it, runs `cargo test --workspace`, and closes the issue by referencing it from the commit message (`Fixes #12`), or moves a file entry to **Resolved** with the commit hash;
4. leaves anything it cannot reproduce open, with a comment on what it tried.

Known design gaps that are **not** bugs — single machine only, the L0 verifier sandbox, stand-in deployer adapters, and the rest — live in `spec/README.md` under the deviations sections, not here. This file is for defects: crashes, wrong results, and things that surprised you.

*Entries below cite commit hashes such as `42a26fb` and `7440272` from the project's history
before it was open-sourced as Tessra. Those commits are not part of this repository.*

---

## Open

### A refused landing charges the author anomaly points on every frontier pass, and three passes revoke the agent
- **Status:** open
- **When:** 2026-09-10
- **Ran:** `tessra promote --to landed --all` three times as the owner while a proposed change from agent `claude` was refused for `structural(test.weakened)`
- **Expected:** the same refusal three times; the author did nothing in between
- **Got:** the third pass revoked agent `claude`. The refusal branch of `land` in `verbs.rs` records 2 anomaly points against `rev.author` for "landing refused: weakened tests" each time the frontier evaluates the change, `anomaly_revoke` defaults to 6, and revocation is permanent for the name. The agent's proposed change was unwound and its workspaces dropped, by the owner's retries rather than by anything the agent did.
- **Repro:** a standard with `forbid structural(test.weakened)`; an agent proposes a change that edits a test; run `promote --to landed --all` three times
- **Where:** M5 · tessra-daemon · verbs.rs, the refusal branch of `land`, and anomaly.rs
- **Env:** Windows 11, release, `tessra 0.1.0` at 8fb2203

Charging once per revision, or only on the author's own `promote` rather than on the owner's frontier pass, would keep the signal without making a retry lethal.

### `forbid ... unless approved(human)` opens no approval request
- **Status:** open
- **When:** 2026-09-10
- **Ran:** with `forbid structural(test.weakened) unless approved(human)` in the trunk standard and a human principal on an inbox channel, `tessra promote --to landed --all` on a change that edits a test
- **Expected:** a refusal naming an approval request, a file in the inbox, and the request in `query --kind exceptions`, as `require approved(human)` produces
- **Got:** `STANDARD_UNMET` naming `forbid structural(test.weakened)` and no request; the exceptions queue and the inbox stayed empty. `ensure_approval_request` looks only at clauses whose predicate is `approved(...)`, not at an `unless`. The approval had to be attested directly with `tessra --as malon attest --kind approval.human --subject <revision> --result true`, after which the landing passed.
- **Repro:** the standard above, any change that modifies a test, one landing attempt
- **Where:** M5 · tessra-daemon · verbs.rs, `ensure_approval_request`
- **Env:** Windows 11, release, `tessra 0.1.0` at 8fb2203

### `structural(flags.none)` holds at landing even when the snapshot was flagged
- **Status:** open
- **When:** 2026-09-10
- **Ran:** `tessra promote --to landed --all` on the change above, whose snapshot carried the secrets flag and whose `verify` reported `flags.none` unmet
- **Expected:** the landing refused with `STANDARD_UNMET` naming `structural(flags.none)`
- **Got:** landed, `met: 3, unmet: 0`. The clause reads `ctx.revision.flags`, and the revision a landing creates by merging onto trunk carries no flags, so the clause holds there regardless of the snapshot.
- **Repro:** any snapshot with a flag: propose it, land it
- **Where:** M3 · tessra-oplog · standard.rs, the `flags.none` arm, and the landing in tessra-daemon that builds the merged revision
- **Env:** Windows 11, release, `tessra 0.1.0` at ed3c646

Either the merged revision should inherit the flags of the snapshot it lands, or the clause should be evaluated against the proposed revision.

### `server::tests::serve_and_call_over_loopback` failed once under the full parallel run
- **Status:** open
- **When:** 2026-09-06
- **Ran:** `cargo test --workspace` (debug), the run right after adding three bridge tests that drive git in temp checkouts
- **Expected:** pass
- **Got:** panic at `server.rs:308:49`, the `client::call(&endpoint, &req).unwrap()` for the first request: the connection to the just-started daemon failed. Passed when run alone and on three further full runs in a row, so it is timing-dependent: the test polls `Endpoint::read` for the daemon file and calls as soon as it appears, and under load the listener may not be accepting yet, or another test's daemon file race is involved.
- **Repro:** not reproduced; run the full suite repeatedly under load
- **Where:** M1 · tessra-daemon · server.rs (test), or `Endpoint` being written before the listener binds
- **Env:** Windows 11, debug, `tessra 0.1.0` at 42a26fb

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
