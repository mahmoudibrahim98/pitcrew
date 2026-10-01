# Brief J · The SLURM launcher and site recipes

- **Stream:** J · Remote and HPC. **Branch:** `s/J/slurm`. **Paths:** `crates/remote/**`.
- **First read:** [README.md](README.md), [J-deploy.md](J-deploy.md) (merged, with its two review
  rounds in mind), `docs/build/streams/J.md` (work packages 3–4), ADR-0009, and the merged
  `crates/remote` (the `Launcher` trait, `helper.sh`, `Layout`, locks and `endpoint.json`).

## Goal

On a SLURM cluster, PitCrew can start the helper **inside an allocation** on a compute node, check
it and stop it. The laptop app reaches it through the login node, and nothing on the cluster needs
root, internet or a compiler. This is the core of milestone M2.

## What to build

1. **`SlurmLauncher`**, implementing `Launcher`:
   - `start` submits a batch job with `sbatch --parsable`, using a generated job script sent over
     stdin. The script runs `pitcrewd serve --listen unix:<run>/pitcrewd.sock` on the compute
     node.
   - Options: partition, account, QOS, time limit, CPUs, memory, GPUs (`--gres`), job name, and
     extra `#SBATCH` lines from the site recipe. Every value is validated and quoted.
   - The job writes `endpoint.json` with `host` set to the compute node (`hostname -f` or
     `SLURMD_NODENAME`), plus the job id, so `status` can tell where it runs.
   - `status` combines `squeue -h -j <id> -o …` (PENDING, RUNNING, COMPLETING or gone) with the
     endpoint. It reports pending with a reason (e.g. Resources or Priority), running on a node,
     or ended with an exit state from `sacct` where available.
   - `stop` runs `scancel <id>`, waits for the job to leave the queue (bounded), then cleans up the
     endpoint. It is idempotent.
   - **Wall-time awareness:** `status` reports the remaining time (`squeue -o %L`). A
     `renew`/hand-over is out of scope; just expose it.
   - The job's working directory is the private root (`--chdir`). Output goes to
     `run/slurm-<id>.out` with private permissions.
2. **Site recipes:** a small `SiteRecipe` trait plus built-in data (TOML or Rust consts, with no
   real site names in the repo).
   - It covers the default `#SBATCH` lines, modules to load (`module load …`, optional), the
     last-hop rule (e.g. compute nodes reachable from the login node by `ssh <node>`, or only
     through `srun --jobid <id> --overlap`), and the socket location (the home may be NFS, so it
     may need node-local `$TMPDIR` plus a relay).
   - Provide a `generic` recipe and a way for users to supply their own file (`~/.pitcrew/sites/*.toml`
     on the laptop). Parse strictly and refuse unknown keys.
   - Validate every value that ends up in a script.
3. **Probe additions:** detect `sbatch`, `squeue`, `scancel` and `sacct`, their versions, the
   default partition, and whether `srun --overlap` exists, all in one probe call. Report them in
   `Probe`.
4. **Security, the same bar as the deploy review:**
   - the job script is fixed text plus validated arguments, with no interpolation of untrusted
     text;
   - all files are created under the private root with `umask 077`;
   - the way to the root is checked as in `pc_safe_way`;
   - the job runs as the user;
   - a job id is only acted on if its name and owner match ours (`squeue -o %j,%u`), so we never
     `scancel` someone else's job, even with the same id after a cluster restart.

## Acceptance

- Fake-SLURM tests (shim `sbatch`, `squeue`, `scancel`, `sacct`, `srun` under the fake ssh):
  - submit, pending with a reason, running on a node, then stop;
  - a job that fails to start or dies, giving a clear status;
  - wall-time remaining;
  - a refusal to cancel a job that isn't ours;
  - a recipe's extra lines and module loads appear in the script, safely quoted;
  - a user recipe with an unknown key is refused;
  - a socket on node-local `$TMPDIR`.
- The script survives a stdin-eating `.bashrc` (begin and end markers, as for the deploy).
- Everything runs under the 9 POSIX `sh` variants in the existing harness.
- No real cluster is contacted, and no real site, account or partition names appear anywhere.

## Out of scope

The tunnel to the compute node and the stdio bridge (next brief), the reconnect ladder,
`systemd-user`, and WSL machines.
