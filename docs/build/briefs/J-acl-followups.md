# Brief J · ACL check follow-ups from the macOS review

- **Stream:** J · Remote and HPC. **Branch:** `s/J/acl-followups`. **Paths:** `crates/remote/**`
  (the helper scripts, their SLURM snapshots, tests and README).
  - `docs/security/threat-model.md` is stream Q's, so this branch can't change it (CI's path guard
    refuses it). Put the R35 text in the PR body, and the integrator applies it at merge.
- **First read:** [README.md](README.md), the root `CLAUDE.md`, the `crates/remote` README (the way to
  the root, ACLs, `with_tool_path`), and the review on PR #11 (`#issuecomment-5962544997`).

## Goal

PR #11 made the remote helper judge macOS ACLs on the folders above its root (R35). The review found
small gaps around that check. All are low severity, and Apple's own `/bin/ls` on the default tool path
isn't affected, but the check should fail closed by itself rather than rely on that.

## What to fix

1. **An empty ACL listing passes** (`helper.sh` `pc_acl_ok` ~189-199, and the copy in
   `slurm/job.sh` ~102-112).
   - **Problem:** `printf '%s\n' "$pc_acl"` always prints at least one line, so `NR > 0` never
     catches an `ls -lde` that exits 0 and prints nothing.
   - **Fix:** require the header line, e.g. `NR == 1 { if ($1 !~ /^d/) { bad = 1; line = "no listing";
     exit } next }`.
   - **Test:** a stand-in `ls` whose `-lde` prints nothing must be refused.
2. **The `modules_init` file's own ACL** (`job.sh` `pc_safe_file` ~181-199) is checked by mode bits
   only, but the README says the script "and the way to it" are judged by any ACL they show.
   - **Fix:** check the file's ACL too.
     - **On Darwin:** allow only `read`, `execute`, `readattr`, `readextattr` and `readsecurity` for
       others.
     - **Elsewhere:** refuse `+`/`@`, except Linux `+`, where the group bits show the mask.
   - **Test:** add a stand-in case.
3. **Call `/bin/ls` explicitly on Darwin** for both reads, the marker and `-lde`.
   - Today `ls` is found by name, so a `with_tool_path` that puts GNU `ls` first refuses every deploy:
     every macOS home carries `everyone deny delete`, and GNU `ls` rejects `-e`.
   - A uutils or busybox `ls` shows no `+` and skips the check entirely.
   - Document the behaviour next to `with_tool_path`.
4. **Refusing harmless setups (it fails closed, but is unfriendly):**
   - add `synchronize` to the allowed rights, since Apple's `ls` prints it on SMB-style ACLs;
   - make the refusal say how to look, e.g. "run `ls -le <folder>` to see the entry".
   - Allow entries for the user's own account, root or `group:admin` may stay refused, but say so in
     the README.
5. **The bash 3.2 exposure elsewhere:** `helper.sh` `pc_stale` (~286) and ~338 still read the owner
   with `$(cat owner)`. Switch them to the `read -r` pattern PR #11 used for the owner check.
6. **More stand-in cases** in `tests/deploy.rs`:
   - an inherited allow-write entry;
   - a deny entry with write rights, which must pass;
   - a group name with spaces (`CORP\Domain Users`);
   - the empty listing from item 1.

   If `own_umask()` can read `/proc/self/status` where it exists, do that instead of setting the
   umask and restoring it.
7. **The threat model:** write the updated R35 text in the PR body; the integrator applies it.

## Acceptance

- fmt, clippy, `cargo test -p pitcrew-remote --no-fail-fast`, and the guards pass in the VM.
- The SLURM snapshots are regenerated, not hand-edited, if the job script changes.
- On the pull request every CI job passes, `Rust (macos-latest)` included: the real macOS ACL test
  runs there.

## Out of scope

New platforms, and anything outside the helper's checks.
