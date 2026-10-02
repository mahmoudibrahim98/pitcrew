# Brief J · Remote follow-ups: R34, a steady tunnel test, attempts, Windows homes

- **Stream:** J · Remote and HPC. **Branch:** `s/J/remote-followups`. **Paths:** `crates/remote/**`,
  `fuzz/**` (the R34 input and its relaxation), `docs/security/threat-model.md` (R34, O42), and
  `Cargo.lock`.
- **First read:** [README.md](README.md), the root `CLAUDE.md` (cloud sessions), the
  `crates/remote` README (SLURM checks, the tunnel's `Connector` and `LinkState`, askpass), and
  `docs/security/threat-model.md` (R26, R34, O42).

## Goal

Loose ends found by fuzzing, by the desktop's remote work and by the first native Windows run,
closed in the remote crate.

## What to build

1. **R34:**
   - `check_sbatch_option` still accepts a value holding `hetjob` or `packjob`
     (`--comment=a-hetjob-b`), which `JobSpec::new` refuses. That is the part of R26's fix that never
     landed. Make the option check refuse what `JobSpec::new` refuses, from one shared rule.
   - Rename `fuzz/regressions/remote_slurm/open-r34-…` without `open-`, and remove its
     `PITCREW_FUZZ_SKIP_KNOWN` relaxation in the target.
   - Mark R34 fixed and close O42 in the threat model.
2. **A steady test:** `tests/deploy/tunnel.rs` `tunnel_a_burst_of_failures_makes_one_check` failed
   once under load (around line 194). Make it independent of machine load: gate on events, not
   wall-clock margins. Run it 20 times and report.
3. **Attempts the desktop can see:** a tunnel that is `Unreachable` doesn't change state when an
   attempt starts, and stays silent when the attempt fails again for the same reason. So the
   desktop can't tell where an attempt ends, and builds a fresh tunnel on every retry, which can
   cost an extra password prompt. Do one of these, and document it:
   - set `LinkState::Connecting` at the start of each attempt from `Unreachable`;
   - expose an attempt counter.

   Don't change the desktop crate (it's a separate workspace); say in your report what it should do
   with this.
4. **Windows homes:** `remote::config::home_dir()` reads `HOME` before `USERPROFILE`. On Windows,
   read `USERPROFILE` first: Windows' own OpenSSH looks in the profile folder for `.ssh\config`, and
   `~/.pitcrew/sites` should follow. Unix is unchanged. Unit-test both.
5. **The askpass pipe on Windows** (`src/askpass/server.rs`): it uses the default descriptor, which
   lets everyone read it. Give it a current-user-only descriptor with an explicit owner (as
   `crates/api/src/listener/pipe_security.rs` does), and test the descriptor.

## Acceptance

- Tests for each item. Show that item 1's rule fails a test when weakened, and item 2's 20 runs.
- fmt, clippy, `cargo test --workspace` (in the cloud: run it in the background; mind the disk),
  `npm test`, and the guards pass. CI checks Windows and macOS on your pull request; Windows-only
  code (item 5) must at least compile there.
- In a cloud session, push your branch and open a pull request with your report as its body.

## Out of scope

The desktop crate, and new remote features.
