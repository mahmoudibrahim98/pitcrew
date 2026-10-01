# Brief J · Helper deployment and launchers

- **Stream:** J · Remote and HPC. **Branch:** `s/J/deploy`. **Paths:** `crates/remote/**`.
- **First read:** [README.md](README.md), then `docs/build/streams/J.md` (work packages 2–3),
  ADR-0009, [J-ssh-connection.md](J-ssh-connection.md) (merged, including its review
  hardening), and the current `crates/remote`.

## Goal

From the laptop, put the static helper (`pitcrewd`) on a remote machine safely, and start it with
a launcher. **Nothing on the remote may need internet, root, a compiler or a package manager**
(P6).

## What to build

1. **Upload:** stream the binary over the existing `Ssh` (stdin to a remote command built with
   the shell-neutral wrapper) into `~/.pitcrew/bin/<version>/pitcrewd.tmp.<random>`, with
   `umask 077` and a private directory (0700, owner checked).
   - Resume isn't needed; retry the whole file.
   - Cap the size, and time-bound the upload with progress.
2. **Verify:**
   - compute the sha256 remotely, trying `sha256sum`, then `shasum -a 256`, then
     `openssl dgst -sha256`;
   - compare with the expected hash passed in by the caller (the desktop has it compiled in);
   - on mismatch, delete the file and fail;
   - `chmod 700`, then run `<file> --version` and check the version string.
3. **Switch atomically:** rename into place, then update a `current` symlink with `ln -sfn` on
   a temp name plus `mv -T` (and a fallback for BSD `mv`).
   - GC old versions, keeping the current one and one previous.
   - Two concurrent deploys can't corrupt `current`: take a lock (`mkdir` lock with a stale
     timeout).
4. **Detect OS and architecture** from the probe (`uname -sm`) and choose the artefact (Linux
   x86_64/aarch64 musl, macOS). Refuse unknown platforms clearly.
5. **Launchers:**
   - `direct` (`setsid nohup … &`, with its pid recorded);
   - `tmux` (a dedicated `pitcrew-helper` session, only with a tmux version ≥ 3.2, as stream B
     requires; otherwise refuse).

   Each writes `~/.pitcrew/run/endpoint.json` (socket path, pid, version, started) atomically.
   Provide `status` (running? version?) and `stop`. The SLURM launcher is a later brief; keep
   the `Launcher` trait open for it.
6. **Everything is idempotent:** re-running deploy with the same version and hash is a no-op
   that just verifies.

## Acceptance

- Fake-ssh tests cover:
  - the full deploy, then the idempotent re-run;
  - a hash mismatch (file removed, error);
  - an interrupted upload (no partial file left in place);
  - the concurrent-deploy lock;
  - GC keeping exactly two versions;
  - each sha256 tool fallback;
  - an unknown platform;
  - launcher start, status and stop, and `endpoint.json` contents.
- A real local test: deploy to `ssh localhost`-style targets is skipped unless
  `PITCREW_TEST_SSH_HOST` is set; also run the remote-side script logic against a local
  `/bin/sh` in a temp `HOME`.
- No secrets in logs; the uploaded file is never world-readable at any point.

## Out of scope

The SLURM launcher and site recipes, tunnels and the stdio bridge, the reconnect ladder (later
briefs).
