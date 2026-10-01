# Brief Q · Round 3: fuzz what merged since round 2; the threat model follows

- **Stream:** Q · Security. **Branch:** `s/Q/round-3`. **Paths:** `fuzz/**`, `docs/security/**`.
- **First read:** [README.md](README.md), [Q-fuzz-and-model-refresh.md](Q-fuzz-and-model-refresh.md)
  (merged), `docs/security/threat-model.md`, `fuzz/` (targets, `seed.py`, regressions),
  `.github/workflows/fuzz.yml` (now builds once and takes its matrix from `cargo fuzz list`), and
  the merged briefs since your last round: J-slurm, G-jira-read, K-shell-and-gateway,
  0-daemon-recaps, C-import-and-reopen, and the recap chain (0-recap-contract, E-recap-index,
  H-recap-routes).

## Goal

Fuzz the parsers and trust checks that merged since round 2, mark the findings that are now
fixed, and bring the threat model up to date with main.

## What to build

1. **Housekeeping.**
   - R7 is fixed (import is all or nothing). Remove `store_import`'s now-dead `skip_known`
     branch, and keep R7's input in `fuzz/regressions/` as a regression that must pass.
   - R8, R9 and R10 are fixed in sync-github; check their inputs now pass, and keep them as
     regressions.
   - Update §6, §7 and §8 accordingly.
2. **New targets.** Each asserts a property, not just "no panic":
   - **`crates/remote` SLURM:**
     - `parse_wall_time`/`format_wall_time` round trip;
     - `JobExit::parse`;
     - `check_sbatch_option`: nothing accepted contains a newline, `#`, a leading `-` value or
       hetjob/packjob;
     - `Site::from_toml`: unknown keys refused, the 64 KiB cap, values that pass all also pass
       `JobSpec::render`'s checks.
   - **`crates/sync-jira`:**
     - ADF to text: bounded depth, nodes and output; hidden characters gone;
     - `ProjectRef::new`: anything accepted matches `^[A-Z][A-Z0-9]{1,9}$`;
     - the issue-key check;
     - the wire JSON for issues.
   - **`crates/sync-github`:**
     - `origin::trusted_next_url`: an accepted URL's host and port equal the base's, and its
       path is under the base, by an independent WHATWG check;
     - `html_url` pinning;
     - closing-reference parsing (`Fixes owner/repo#n`): accepted owners and repos match
       `[A-Za-z0-9._-]+` and aren't `..`.
   - **`crates/api` recaps:** the query parameters for both routes give 200 or 400, never 500,
     with a fake source.
   - **`crates/hub-work` recaps:** the index over arbitrary event sequences equals a rebuild.
     The crate has property tests; fuzz it only if cheap. Otherwise say why not.
   - **The desktop gateway's path check** lives in the desktop's own workspace. If depending on
     it from `fuzz/` would pull in Tauri, don't. Propose a seam instead (move the check into a
     small crate the gateway uses), and fuzz a copy of the rules only if the desktop agrees.
   - **K's deep-link parser:** add it only if `s/K/tray-notifications-links` has merged by then.
     Otherwise list it in §8.4.
3. **Run each new target** for 60 seconds, one at a time. Report the executions per second and
   the result. A crash is a finding: reduce it, and file it for the owning stream with a
   reproduction in `fuzz/regressions/`.
4. **The threat model** (`docs/security/threat-model.md`), updated with main:
   - **The desktop app as built:** the CSP with `style-src 'unsafe-inline'` and why; the
     capabilities; the navigation guard; the daemon's identity checked before the token is sent;
     stderr redaction; refusal of planted binaries; the back-pressure caveat (released at fetch
     start); dev-mode exposure.
   - **SLURM as merged:** script integrity by sha256; never cancelling another user's job by uid
     and name; launcher overlap refused; host ids that survive a reboot; recipe trust.
   - **Recaps:** derived untrusted text (hidden characters including tag characters), and
     memory growth (a follow-up for E).
   - **GitHub and Jira:** link trust (the host pinned, the path under the base), `html_url`
     pinning, key validation, and the cursor's time-zone handling.
   - **The store:** all-or-nothing import, and lease checks before every write.
   - Update the coverage table and the open items.

## Acceptance

- Every new target builds and runs for 60 seconds without a crash, or the crash is reported as a
  finding with a reproduction.
- The regressions for R7–R10 now pass.
- The threat model names an owner stream for every new control and open item.
- `npm test`, the path guard and the ownership audit pass.

## Out of scope

Changing other streams' code, and new CI jobs (propose them).
