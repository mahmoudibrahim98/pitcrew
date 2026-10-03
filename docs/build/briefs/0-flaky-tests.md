# Brief 0 · Fix the four flaky tests

- **Stream:** 0 · Contracts (test fixes in four streams). **Branch:** `integrator/flaky-tests`.
  **Paths:**
  - `apps/desktop/src-tauri/tests/supervisor.rs`;
  - `benches/src/client.rs`;
  - `apps/ui/e2e/**`, plus the UI's CSS only if item 3 needs it;
  - `crates/ptyd/tests/**` and the PTY client's connect path in `crates/runtime/src/pty/**`, for
    item 4.
- **First read:** [README.md](README.md), the root `AGENTS.md` (or `CLAUDE.md`), and the READMEs of
  `apps/desktop/src-tauri` (the daemon supervisor), `benches` and `apps/ui` (end-to-end tests).
- **Suggested agent:** Codex, or any coding agent.

## Goal

Four tests fail now and then on CI, three for reasons in the tests and one (item 4) because of a
platform difference. They turn unrelated
pull requests red. Make each one deterministic **without weakening what it checks**: no retries, no
wider tolerances without a reason, no skipped assertions.

## What to fix

1. **`it_restarts_with_a_growing_backoff_then_gives_up`**
   (`apps/desktop/src-tauri/tests/supervisor.rs` ~167-200; failed on macOS).
   - **Problem:** it measures gaps between successive `Ready` states and asserts
     `gaps[2] >= gaps[0] + 200ms`. Each gap is the run's own length (~0.2 s) plus the backoff wait
     (0.1, 0.2, 0.4 s), so a slow first start on a loaded runner eats the margin.
   - **Fix:** measure the backoff itself rather than the time between ready states. For example,
     use the fake `pitcrewd`'s own start and exit times (it can log them), or exit-to-next-start,
     and assert each wait grows.
2. **`reads_a_content_length_and_a_chunked_answer`** (`benches/src/client.rs` ~213-235; failed on
   Linux).
   - **Problem:** the stand-in server does one `read` and then replies and closes. If the request's
     body hasn't arrived yet, closing with unread data sends a TCP reset, and the client's
     `request(...)` fails.
   - **Fix:** read the whole request before replying: headers up to `\r\n\r\n`, then
     `Content-Length` bytes. Do the same in any sibling test that has the same pattern.
3. **The onboarding wizard's dark-theme axe check**
   (`apps/ui/e2e/onboarding-fake.spec.ts` ~132, `expectNoAxeViolations` ~20; failed once, on PR #9).
   - **Problem:** axe reported `color-contrast` on an `.inline-flex` element at the welcome step,
     2.2 s into the dark run. The light run takes 13.8 s. The likely cause is axe running while the
     theme is still changing, so it measures a colour mid-transition.
   - **Fix:** confirm the cause from the trace if you can. Then make the test wait for the state it
     means to check: the theme applied and no running animations
     (`document.getAnimations().length === 0`), and/or emulate `reducedMotion: 'reduce'` if the app
     disables transitions under it (it should; check).
   - **If the contrast is really too low in the final dark state,** fix the colour token instead,
     and say so.
4. **`a_ptyd_that_closes_during_hello_counts_as_none`** (`crates/ptyd/tests/protocol.rs` ~312;
   failed on macOS, on PR #13).
   - **What happened:** the stand-in ptyd hangs up during the hello, and the client answered
     `Unavailable("cannot check pitcrew-ptyd's user: Socket is not connected (os error 57)")`
     instead of "no ptyd".
   - **The cause:** on macOS, the peer-credential check (`getpeereid`) fails with `ENOTCONN` once the
     peer has already closed, before the client sees the closed stream.
   - **This one is a product question, not just the test:** a peer that hung up before it could be
     identified should count as "no ptyd", like a hang-up during the hello. Map exactly that case
     (`ENOTCONN` or the platform's equivalent, after the peer has closed) to the same answer, in the
     client.
   - **Keep refusing** a peer whose identity can be read and is wrong, and any other error: those
     stay refusals with a reason (the threat model's ptyd rows).
   - **Test:** the closed-peer case on both Linux and macOS, plus a case showing a wrong-uid peer
     is still refused.

## Acceptance

- Each fixed test passes 20 runs in a row in the environment. Report the loop you used, e.g.
  `for i in $(seq 20); do cargo test ... || break; done`, or `--repeat-each=20` for Playwright.
- The test still fails when the behaviour it guards is broken. Show it once for each test, e.g.
  by setting the backoff to a constant, or making the server reply before reading.
- fmt, clippy with `-D warnings`, the guards, `npm test` and the end-to-end tests pass. Every CI job
  passes on the pull request.

## Out of scope

Other tests, and product changes beyond a colour token for item 3.
