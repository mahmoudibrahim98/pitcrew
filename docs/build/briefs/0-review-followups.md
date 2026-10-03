# Brief 0 · Follow-ups from the reviews of #20, #24 and #25

- **Stream:** 0 · Contracts (small fixes in several crates). **Branch:** `integrator/review-followups`.
  **Paths:**
  - `crates/daemon/src/{serve,state,runtime}.rs`;
  - `crates/hub-work/src/{recap,recap_db}.rs`;
  - `crates/cli/src/**` (item 5 only);
  - `crates/trust/**` and its users' manifests (item 6 only);
  - the READMEs of `crates/runtime`, `crates/ptyd`, `crates/remote` and `apps/desktop/src-tauri`;
  - `crates/remote/src/lib.rs` (comments only);
  - `tests/conformance/**` and `apps/mock-hub/src/{validate,recaps}.ts` (items 7 and 8).
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the integrator's merge notes on PRs #20 (trust sweep) and #25 (memory budget);
  - the READMEs of the crates you touch.
- **Suggested agent:** Codex, or any coding agent.

## What to do

1. **Prove the daemon uses the recap file** (`crates/daemon/src/serve.rs` ~250). Today, removing
   `.with_recap_file(...)` would fail no test.
   - Add a unit test with the existing `open_with` harness: `work.sync_recaps()`, then assert
     `<state>/recaps.sqlite3` exists, then drop the hub and assert the file is gone.
2. **The days query reads every block's body, even on a cache hit**
   (`crates/hub-work/src/recap.rs` ~745 `db.window`, `recap_db.rs` ~298-320).
   - `DayCache::get_or_make` only needs `(id, last)` to check for a hit. Store `last` as a column,
     have `window` return `(id, last)`, and decode the bodies only on a miss.
   - Show with a test that answers are unchanged, and report `/v1/recaps/days` timing before and
     after at 10,000 sessions if you can run the bench.
3. **The recap cache on a network filesystem** (`recap_db.rs` ~115-141).
   - When the state directory is on a network filesystem (`fs_kind::detect(..).is_network()`, as the
     store uses), put the cache in a private local directory: the runtime directory, or a private
     temp folder with the same permissions. Otherwise keep it in memory, and log which.
   - Test with the detection stubbed.
4. **Docs and comments left stale:**
   - list `recaps.sqlite3` in the state-file table in `crates/daemon/src/state.rs`;
   - fix `crates/remote/src/lib.rs` ~26-27: only `job.rs` holds `unsafe` now;
   - reword the `lock_dir` doc comment in `crates/daemon/src/runtime.rs` ~586: it prefers
     `LOCALAPPDATA` over the known folder;
   - update the runtime, ptyd, remote and desktop READMEs where the trust sweep (PR #20) moved
     `check_trusted` and the Windows pipe security into `crates/trust`.
5. **Bare `pitcrew` run by an old Claude Code must not block it.**
   - **The problem:** a Claude Code older than 2.1.139 ignores `args` and runs the bare `command`,
     so `pitcrew` with no arguments exits 2 with usage text. Exit 2 is Claude Code's blocking code:
     a UserPromptSubmit or Stop hook would block.
   - **Fix:** when `pitcrew` gets no subcommand, stdin isn't a terminal, and a Claude Code hook
     environment is present (`CLAUDE_PROJECT_DIR` set), exit 0 silently. Keep the usage text and
     exit 2 for people at a terminal.
   - **Test:** both cases.
6. **Optional: keep the desktop off the pipe-security module.** It links all of `crates/trust`'s
   pipe-security code just to call `check_trusted`. Put the pipe code behind a cargo feature that
   the daemon, runtime and remote enable, and the desktop doesn't. Skip it if it complicates the
   build; say so.

7. **Conformance: agent tokens on person-only writes.** The suite checks a 403 for an agent token on the person-only GET routes and four creates, but not on the other person-only writes:
   `PATCH /v1/tasks/{id}`, `POST /v1/tasks/{id}/assign`, `POST /v1/tasks/{id}/dispatch`, `PATCH /v1/workstreams/{id}`, `POST /v1/sessions/{id}/{send,keys,interrupt,end}` and `POST /v1/sessions/{id}/link` (see `crates/hub-work/src/routes.rs` and `crates/daemon/src/sessions.rs`).
   Add them, against both targets.
8. **One `queryId`:** `apps/mock-hub/src/validate.ts` and `recaps.ts` each have one; keep one and import it.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of every crate touched, the guards, and every CI job
  pass.

## Out of scope

New features, and anything the reviews didn't raise.
