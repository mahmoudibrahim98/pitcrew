# pitcrew-remote

Remote machines: SSH connection manager, helper deployment, launchers (direct, tmux, systemd, SLURM), tunnels, reconnect.

**Owned by stream J** — see [docs/build/streams/J.md](../../docs/build/streams/J.md).

## What is here

Everything goes through the user's own **system OpenSSH** and `~/.ssh/config`. `~` is `HOME`
on Unix; on Windows it is `USERPROFILE` (where Windows' own OpenSSH looks), else `HOME`, for
`~/.ssh/config` and `~/.pitcrew/sites` alike.

- `list_hosts()` lists concrete `Host` names from `~/.ssh/config` and its `Include`s (cycles
  skipped, at most 256 files). `Ssh::resolve(host)` asks `ssh -G` what a host means; the config
  is never reinterpreted here. `ssh -G` runs the config's `Match exec` commands, so it is
  bounded (`RESOLVE_LIMITS`: 10 s, 1 MiB).
- `Ssh::run(host, argv)` runs `ssh [options] -- <host> <command>`.
  - **Any login shell.** The command is sent as
    `/bin/sh -c 'unset -f printf 2>/dev/null; eval "$(printf "\ooo…")"'` (an exported `printf`
    function, which bash would import, is dropped first), every byte of the POSIX-quoted command line an
    octal escape. What the login shell sees has no `\\`, `\'`, `!`, newline or stray `$`, so
    sh, bash, dash, zsh, ksh, fish, csh and tcsh all hand the same script to `/bin/sh`.
  - **xonsh is unsupported and unsafe** as a login shell: it may decode `\ooo` itself, and the
    `printf` layer would then read a `\047` from the command as a real quote, so argv could
    break out. `Ssh::probe` reads `$SHELL` and refuses such hosts (`SshError::UnsupportedShell`).
  - **Length:** a wrapped command may be 128 KiB on Unix (Linux's limit for one argument) and
    30,000 characters on Windows (`CreateProcess` takes 32,767 for the whole command line);
    longer ones are refused before ssh starts.
  - **Host names** are limited to `A-Z a-z 0-9 . _ : % [ ] @ -`, and may not start with `-`
    before or after `@`.
  - **Options:** agent and X11 forwarding, local commands, config forwardings and
    `RemoteCommand` are off; there is no escape character (`EscapeChar=none`: a `~.` in the
    data ends nothing); host keys are confirmed (`StrictHostKeyChecking=ask`).
    `ClearAllForwardings=yes` also clears `-L`/`-R`/`-D`, so the tunnel adds its forward to its
    own master afterwards (`ssh -O forward`). `-o` options do not reach `ProxyJump` hops, which
    read only the user's config.
  - **Unix:** connections are reused (`ControlMaster=auto`, `ControlPersist=10m`) through
    sockets in a private 0700 directory: `$XDG_RUNTIME_DIR/pitcrew-ssh`, else
    `/tmp/pitcrew-ssh-<uid>`, else `~/.pitcrew/s`. A candidate that is squatted, too long for a
    socket path, or has characters `ControlPath` would expand is skipped.
  - **Windows:** its OpenSSH has no ControlMaster, so every call connects and authenticates
    anew (the tunnel too; see below).
  - **Environment:** `Ssh::with_env_passthrough(names)` gives ssh only `MINIMAL_ENV` (home,
    user, path, locale, the agent's socket, Kerberos' cache, …) and those names, instead of the
    app's whole environment. The tunnel always does so.
  - **Stopping:** a cancel, a timeout or a dropped call stops ssh and everything it started
    (askpass, `ProxyJump` hops, `Match exec`): its process group on Unix, its **Job Object** on
    Windows (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the OS also ends it if PitCrew dies). The
    Job Object needs four Win32 calls; `src/job.rs` and `src/pipe_security.rs` (below) are the
    crate's only unsafe code.
  - **Errors:** ssh's own messages go to a log (`-E`, at `LogLevel=ERROR`) apart from the remote
    stderr. Exit 255 is an error only when that log shows ssh failing, and its kind comes only
    from ssh's own message formats, matched as whole lines. ssh logs server text without
    escaping newlines, so reading stops at ssh's terminal message (e.g. "Permission denied
    (…).", whose method list is the server's) and at lines carrying server text (a disconnect
    reason, an algorithm offer, a refused channel). Informational lines never turn a remote
    command's own 255 into an error.
  - `Ssh::run_limited` adds an output cap and a timeout that pauses while a prompt is open.
- **Askpass bridge.** With `Ssh::with_prompts(askpass, handler)`, ssh runs `pitcrew-askpass`
  for passwords, passphrases, one-time codes and host keys; it asks the desktop's
  `PromptHandler` over a private socket (on Windows a named pipe that the current user owns
  and alone may open, with remote clients rejected, which the client opens at identification
  level only). Both ends prove a per-call key first. Answers are never stored
  or logged. Needs OpenSSH 8.4+ on the local machine. The askpass path must be absolute and
  exist (a missing one would fail every prompt). Without a handler, calls run in `BatchMode`
  and fail instead of prompting.
  - The handler is async and gets a `PromptCancel` that fires when the prompt goes stale (ssh
    closed it, or the call ended); the dialog should close then.
  - **ssh never sees askpass fail**, since it would then send an empty password and ask again:
    - **Cancel** stops ssh and everything it started before askpass hears back. Later prompts
      in that call are not shown. (For a yes/no question, Cancel simply answers "no".)
    - If the bridge closes without an answer (PitCrew quit or crashed, or the handshake
      failed), `pitcrew-askpass` kills the ssh that asked, but only while it is still its
      parent and shares its process group, so never a reaper that adopted it (pinned with a
      pidfd on Linux). On Windows it waits instead, and the Job Object ends both.
    - While PitCrew runs, a client that says hello and then fails the handshake stops the
      call with `SshError::Bridge` (on Windows by ending the job); so does ssh being killed
      by its askpass.
  - `PromptKind` is a hint: server-written prompts (`(user@host) …`) are never classed as a
    passphrase or host key, Accept is refused for secrets and Text for yes/no questions. The
    UI must show the raw prompt, escaped. `UpdateHostKeys=ask`'s "Accept updated hostkeys?" is
    a yes/no question (Confirm), so the user's choice of key rotation is kept.
- `Ssh::probe(host)` runs one POSIX-sh script and returns `MachineInfo` plus `$HOME`, the tmux
  version, the login shell, the filesystem type of `$HOME`, and the SLURM tools
  (`SlurmTools`): `sbatch`, `squeue`, `scancel`, `sacct` and `srun` with the first line of
  their `--version`, whether `srun --help` lists `--overlap`, and the partition `sinfo` marks
  as the default (`sinfo` asks the scheduler, so it runs under `timeout 10` where there is
  one). The report needs this call's random markers and exit 0; output is capped at 1 MiB and
  the call at 30 s.
  Only filesystems on an allowlist of local types (ext2/3/4, xfs, btrfs, zfs, tmpfs, f2fs,
  bcachefs, overlayfs, apfs, hfs and similar) count as local; anything else, including
  `UNKNOWN (0x…)`, every FUSE filesystem (`fuseblk`), 9p, virtiofs and vboxsf, counts as
  possibly networked.
- `Ssh::run_with_input` streams bytes to the remote command's stdin while reading its output,
  with progress, under the same limits.

## The helper on a machine (`helper`)

Deploys the static `pitcrewd` over the user's ssh and starts it. Nothing on the machine needs
internet, root, a compiler or a package manager: only a POSIX `sh`, common tools (`dd`, `ls`,
`awk`, `sed`, `find -mmin`, `readlink`, `date +%s`, …) and one of `sha256sum`, `shasum` or
`openssl`.

```rust
let probe = ssh.probe(host).await?;
let target = Target::new(ssh, host, &probe)?;           // refuses unknown platforms
// The desktop picks `target.platform().artefact()` (e.g. pitcrewd-x86_64-unknown-linux-musl)
// and the sha256 compiled in for it.
let helper = Helper::new(target.platform(), VERSION, SHA256, bytes)?;
deploy(&target, &helper, &DeployOptions::default()).await?;
let started = DirectLauncher::default().start(&target).await?;   // or TmuxLauncher
```

- **Platforms:** Linux x86_64 and aarch64 (static musl) and macOS (universal), from the probe's
  `uname -s`/`uname -m`. Anything else (FreeBSD, 32-bit ARM, POWER, …) is refused by name, and a
  helper built for another platform is refused before any call. `Helper::new` checks the
  version, the size (at most 256 MiB) and that the bytes hash to the expected sha256.
- **Layout:** `~/.pitcrew/bin/<version>/pitcrewd`, `bin/current -> <version>` (relative),
  `bin/previous`, `run/endpoint.json`, `run/pitcrewd.sock`, `run/pitcrewd.log`. Every directory
  must be a real directory owned by the user with mode 0700 (`drwx--S---` under a set-group-ID
  parent) and no ACL (on macOS, where `@` hides the `+`, `ls -le` is asked); one that is not is
  refused (`UnsafeDirectory`), never repaired. `Layout::at` puts the root elsewhere.
- **The way there** is checked first, as sshd checks the way to `authorized_keys`: every
  directory from `/` down to the root's parent must belong to root or the user, and be writable
  by no one else unless sticky (as `/tmp`). The walk resolves the path one component at a time,
  following symbolic links (absolute, relative, through `..`; each must belong to root or the
  user, since in a sticky directory its owner could swap it) and checking where they lead, so a
  group-writable project directory on the way is refused: a member could otherwise swap the tree
  after the checks and have their own `pitcrewd` run. The script then `cd -P`s into the root and
  uses relative paths only; a launched helper checks that the directory it starts in is that
  root, by its physical path, and private (tmux enters it by name; every `#` in it is doubled,
  since tmux reads formats there) before it runs. Directories above the root, and a SLURM
  recipe's `modules_init` script and the way to it, are judged by their owner and mode bits,
  and by any ACL they show (`+`, or macOS's `@`, which can hide one; `pc_acl_ok`): on Linux a
  POSIX ACL's grants are bounded by its mask, which `ls` shows as the group bits, so those
  bits suffice; on macOS the mode bits leave the ACL out, so `ls -le` must list only entries
  that deny, or allow reading and searching; on any other system, or when the list cannot be
  read or an entry is not understood, the directory is refused. NFSv4 and GPFS ACLs on Linux
  do not show in `ls` at all, so they are not judged.
- **The remote side** is one script, `src/helper/helper.sh`, sent on **stdin** (the Windows
  command-line limit leaves the shell-neutral wrapper about 7,500 bytes). The command line is a
  fixed bootstrap run by `/bin/sh` (by path, whatever `sh` the user's `PATH` finds). It drops
  `dd`/`echo` functions imported from the environment and `ENV`, `BASH_ENV` and `CDPATH`, puts
  the tool path (`DEFAULT_TOOL_PATH`, `/usr/bin:/bin:/usr/sbin:/sbin`, or
  `Target::with_tool_path`) in front of `PATH`, and reads exactly the script's length with `dd`,
  so the helper bytes behind it stay on stdin. It runs the script only if it has its first line,
  its last line and its length: a `.bashrc` that eats stdin cannot make a tail of it run. The
  script then drops every function standing in for a tool it uses (bash imports exported ones).
  It reports between random markers, like the probe.
- **Deploy** is at most two calls, each under the `bin/.lock` lock:
  1. `check` verifies a copy already installed under the version (sha256 computed on the
     machine, then `--version`) and switches to it. The same deploy again stops here: it only
     verifies. A damaged copy is removed. A missing hash tool, an unsafe directory or a busy
     lock is found here, before anything is uploaded.
  2. `install` streams the helper into `bin/<version>/pitcrewd.tmp.<random>` under `umask 077`
     (so it is 0600 from the first byte, in 0700 directories), checks the byte count, the
     sha256 (`sha256sum`, else `shasum -a 256`, else `openssl dgst -sha256`; a tool that fails
     or prints no hash falls through) and `--version` (whose first line must have the version as
     a word), deleting the file if any fails; then `chmod 700`, a rename into place, the switch,
     and GC.
  - **Switch:** a new link made with `ln -sfn` on a temporary name, renamed over `current` with
    `mv -T` (GNU), else `mv -h` (BSD, macOS). Where `mv` has neither (busybox), `ln -sfn`
    replaces it in place and `Deployed::atomic` is false. `previous` names the version before.
  - **GC** removes every version directory but `current`'s and `previous`'s.
  - **Interrupted uploads** never land in place: the file is a temporary one until verified. The
    script removes it on any exit (and on SIGHUP, SIGPIPE, SIGTERM); if the script itself is
    killed, the next deploy sweeps it.
  - **Bounds:** `DeployOptions::timeout` per call (prompts excluded), `lock_wait`, and
    `stale_lock`, which must exceed both; `progress` reports bytes handed to ssh.
- **Locks** are `mkdir` directories with an `owner` line: host, pid, call tag and time. A lock
  taken on this host (by its name; see `host` below) is stale when its process is gone (so a
  killed deploy does not block the next one for long) or when it is older than the limit by
  this host's own clock. Another host's clock cannot be compared with this one, so a lock from
  another host (a login node sharing the home), without an owner line yet, or whose pid or
  time cannot be read, is stale only when its directory is older than the limit plus 10
  minutes: hosts sharing a home, and the file server, must agree on the time within 10
  minutes. A stale lock is moved aside atomically and removed; if what was moved is not the
  lock judged stale, it is put back while the name is free. Every step that changes
  something (sweeping, `chmod`, removing a damaged copy, the rename, the switch, GC; in the
  launchers removing old records, launching, writing `endpoint.json`, signalling, removing
  records) first checks the run still owns its lock; one that lost it stops (`LockLost`), and
  leaves the lock to its new holder.
- **Launchers** implement `Launcher` (object-safe; for SLURM see the next section):
  - `DirectLauncher`: `setsid nohup` (`nohup` alone where there is no `setsid`, as on macOS),
    double-forked so the helper is nobody's child;
  - `TmuxLauncher`: its own tmux server and session, both named `tmux_name(layout)`
    (`pitcrew-helper-` and 8 hex digits of the root's sha256, so two roots on one host never
    meet), started with `-f /dev/null`; only with tmux 3.2 or newer (`TmuxLauncher::new` refuses
    older, missing or unreadable versions).

  Both run `bin/<version>/pitcrewd serve --listen unix:<root>/run/pitcrewd.sock` (or
  `LaunchOptions::args`) for the version `current` points to, from inside the root, with the
  umask of the user's session (the script's own files are made under 077; the helper's are
  the user's to share). They append its output to `run/pitcrewd.log` (kept 0600), wait for the
  socket (`ready_timeout`; a helper that exits or never binds is reported with the log's last
  lines, and stopped), and write `run/endpoint.json` atomically:
  `{"pid":…,"host":…,"version":…,"started":…,"launcher":…,"socket":…}`. `status` reports
  whether it runs and which version is installed; `stop` sends SIGTERM, then SIGKILL after
  `stop_timeout`, each only while the process still has the start time it had. All are
  idempotent. A pid counts only while alive, not a zombie, and named `pitcrewd`, so a recycled
  pid is never signalled. `host`, as in lock owners, is `<name>+<id>`: `uname -n` (other
  characters made `_`, at most 40) and, for people to tell hosts apart, the first id there is
  that survives a reboot (`hostid` unless all zeros, else the machine id, else the hardware
  UUID; none, and no `+`, without any), e.g. `login01+007f0101`. Whether a record is this
  host's goes by the name alone: a stateless node makes a new machine id at every boot, and its
  records from before must stay its own (judged by their pid, and locks by their age). On
  clusters whose login nodes share `$HOME`, a record from a host of another name is reported
  (`OtherHost`, with the launcher that made it) and never acted on, unless
  `LaunchOptions::take_over` says so. **Recovery:** when that host is gone for good (renamed or
  retired), a direct launcher's `stop` with `take_over` forgets its record, and the next start
  or submit goes ahead.
- **SLURM jobs of the same root:** they share `run/` and its socket, so while `run/slurm.json`
  records a job that squeue says is still queued or running, or squeue cannot be asked, the
  direct and tmux launchers neither start a helper nor remove records (`InUse`), with
  `take_over` or without (under the launch lock, so a submit and a start never interleave).
  Stop the job with the SLURM launcher first.
- **Secrets:** none are involved; nothing here logs. Reports and errors carry paths, the
  first line of `--version` and, for a SLURM job that ended, the last lines of its output, with
  control characters replaced.

## The helper as a SLURM job (`helper::slurm`)

On a cluster, the helper runs inside an allocation on a compute node, and the laptop reaches it
through the login node. Nothing on the cluster needs root, internet or a compiler.

```rust
let site = slurm::generic();                        // or one of slurm::load_sites(dir)
let options = JobOptions { partition: Some("gpu".into()), account: Some("proj0001".into()),
                           time: Some(Duration::from_secs(8 * 3600)), ..Default::default() };
let script = JobSpec::new(&site, &options)?.render(&target)?;
// Show script.text() to the user. Only after they confirm:
let launcher = SlurmLauncher::default().with_script(script);
launcher.submit(&target).await?;                    // returns at once
launcher.job_status(&target).await?;                // pending (Priority), running on node017, …
launcher.cancel(&target).await?;
```

- **Nothing is submitted unseen.** Submitting takes a `JobScript`, which only `JobSpec::render`
  makes, and PitCrew sends exactly its `text()`; the machine checks its length, its first and
  last lines and its sha256 before using it. The script is the fixed text of
  `src/helper/slurm/job.sh` behind `#SBATCH` lines and shell assignments made only of checked
  values (snapshots of two scripts are in `src/helper/slurm/snapshots/`). `#SBATCH` values are
  limited to characters that need no quoting; shell values are quoted besides.
- **Options:** partition, account, QOS, wall time, CPUs (`--cpus-per-task`), memory (`--mem`),
  GPUs (`--gres`), job name, and extra `#SBATCH` options. Extra options must be long options
  from `ALLOWED_SBATCH`, written `--name=value` (only true flags such as `--exclusive` may stand
  alone: sbatch reads all directives as one command line, so an option missing its value would
  take the next directive as one), with a value that does not start with `-` (sbatch could read
  `--comment=--uid=0` as an option). Refused: options that would change which job status and
  stop look at, where its files go, or which cluster (`--job-name`, `--chdir`, `--output`,
  `--error`, `--array`, `--clusters`, `--wrap`, `--wait`, `--uid`, and abbreviations), and ones
  that change the helper's environment or where mail goes (`--export`, `--get-user-env`,
  `--propagate`, `--mail-user`). No `#SBATCH` line may hold `hetjob` or `packjob` in any case,
  the root's included: SLURM up to 20.11 splits a job there. Options are checked for them with
  the rest (so a site recipe holding one does not load); the root, when the script is made.
- **Submitting** (`helper.sh slurm-submit`, under the launch lock, after the usual checks of
  the way to the root): the script goes to a private temporary file; leftovers of a killed
  submit are swept. It is refused while `endpoint.json` records a helper of the direct or tmux
  launcher that runs here (`InUse`: stop it with that launcher) or was recorded on a host of
  another name (`OtherHost`: stop it there, or see the recovery above), since they share
  `run/` and its socket; a record of one that is gone is removed. The `SBATCH_*`, `SQUEUE_*`,
  `SCANCEL_*` and `SACCT_*` variables, and `SLURM_CLUSTERS`, are unset: they would override the
  script's directives, hide a job from `squeue -j` (`SQUEUE_STATES`), make scancel ask or skip
  (`SCANCEL_INTERACTIVE`, `SCANCEL_STATE`), or send the commands to another cluster.
  `sbatch --parsable` runs under `umask 077` with the job name, the root (`--chdir`) and
  `run/slurm-<id>.out` (`--output`) on its command line too, and the user's umask as the
  script's argument. The id is read from sbatch's standard output only. The job is recorded at
  once in `run/slurm.json` (id, name, submit time, host, and the cluster when sbatch answers
  `<id>;<cluster>` with a plain name that does not start with `-` or `.`: squeue, scancel and
  sacct are then run with `-M <cluster>`). A recorded job still queued or running is not
  submitted again.
- **The job, on the node**, checks the way to the root and the root as the launchers do (the
  same shell text as `helper.sh`; a test compares them), makes its output file private, waits
  for its record (so a job whose submission was cut off before it was recorded ends on its own,
  and one whose record names another job does not start), and ends without touching anything
  while `endpoint.json` records another launcher's helper (the direct and tmux launchers do not
  start one while the job is queued or running; this check is for when squeue was wrong). It
  checks the recipe's `modules_init` script as it checks the root (the file, any link to it,
  and every directory on the way belong to root or the user and are writable by no one else),
  sources it, resets the shell's settings and traps and drops the functions and aliases it may
  have made for the tools the job uses, loads the modules, checks it is still in the root, and
  starts `bin/<current>/pitcrewd serve --listen unix:<socket>` with the user's umask. Once the
  socket is there it writes `run/endpoint.json` with `host` the node (`SLURMD_NODENAME`, else
  `hostname -f`, else `uname -n`; at most 64 plain characters) and `job` its id. On SIGTERM
  (scancel, the time limit) it passes the signal on, and removes the endpoint while it names the
  job, and the socket it started: in its own node-local directory, or in `run/` only while the
  endpoint still names the job (never another helper's socket there). SIGUSR1 and SIGUSR2
  (`--signal`) do not end it.
- **Status** (`job_status`, no lock) reads `squeue -h -j <id> -o '%i|%U|%T|%r|%L|%l|%N|%j'`:
  pending with SLURM's reason, running on a node, the time left (`%L`) and the limit, or
  another state. Once squeue no longer lists the job, `sacct` says how it ended (state and exit
  code) where accounting is on, with the last lines of its output; sacct is asked only for
  records with the job's name and the user's uid, and prints no names, so a name holding a
  line break cannot forge a record. If squeue fails (the scheduler unreachable), that is an
  error and nothing is concluded. `Launcher::status` maps these to `HelperState::Running`
  (endpoint written), `Pending`, or `NotRunning`.
- **Stop** (`cancel`) runs `scancel --user=<uid> --name=<name> <id>`, waits up to
  `stop_timeout` (checking once a second) for the job to leave the queue, then forgets it: the
  record, its endpoint and its socket in the root. A job still queued after the wait is
  `StopFailed` (or `Slurm`, with scancel's message, if scancel failed), and the record is kept,
  so stopping again finishes it. Idempotent.
- **Whose job:** a job id is acted on only while squeue lists it with the recorded name and
  this user's uid (`%j`, `%U`; the name is the last field, since it may hold anything; a job
  listed twice, as a name with a line break would make it, is an error). An id that now names
  another job (a cluster that lost its state numbers jobs again) is reported as
  `JobState::NotOurs` and never cancelled; PitCrew only forgets its own record. The name alone
  is easy to guess: the uid is what tells jobs apart.
- **Start** (`Launcher::start`) submits if needed and waits up to `ready_timeout` (squeue every
  2 seconds) for the job to run and its helper to listen. A job still pending is
  `HelperError::Queued`: it stays queued, and starting again waits for the same job.
- **Socket:** `<root>/run/pitcrewd.sock`, or on the node's own disk
  (`$TMPDIR/pitcrew-<job>.<pid>/pitcrewd.sock`, `/tmp` when `$TMPDIR` is unset or not a plain
  path), in a private directory whose way is checked like the root's. The tunnel (below)
  reaches it.
- **Roots** for this launcher must be plain characters (`A-Z a-z 0-9 _ . / + -`), since the
  root goes into `#SBATCH` lines, which do not quote, and into `--output`, where `%` is a
  pattern.

### Site recipes

A recipe says what one cluster needs: job defaults and extra `#SBATCH` options, modules to load
(and, where `/bin/sh` on the nodes has no `module` command, the script that sets it up), the
last hop to a compute node (`ssh <node>` from the login node, or `srun --jobid <id> --overlap`
on sites that forbid ssh to nodes; recorded for the tunnel), and the socket's place.

- `SiteRecipe` is a small trait; `generic()` is the one built in: SLURM's defaults, no modules,
  the socket under the root, `ssh` to the node. Every value a recipe gives is checked when a
  `JobSpec` is made from it, whoever wrote the recipe.
- **A recipe is trusted like a shell script the user runs.** Its `modules_init` script is
  sourced in the job and its modules loaded there, so it runs code as the user on the cluster.
  The checks keep its values from breaking the job script or changing which job PitCrew acts
  on; they do not make someone else's recipe safe to use unread.
- **Adding one:** copy `src/helper/slurm/example-site.toml` to `~/.pitcrew/sites/<name>.toml` on
  the laptop (`sites_dir()`; `<name>` is `a-z 0-9 _ -`) and edit it. `load_sites(dir)` reads every
  `*.toml` there, each failure reported on its own. Files are read strictly: an unknown key, a
  table, or a value of the wrong type is an error naming the key; files over 64 KiB are refused.
  The keys are `description`, `partition`, `account`, `qos`, `time`, `cpus`, `memory`, `gres`,
  `sbatch`, `modules_init`, `modules`, `last_hop` (`"ssh"` or `"srun"`) and `socket`
  (`"root"` or `"node-local"`).
- `check_tools(&probe.slurm, last_hop)` says whether a machine can run the launcher: `sbatch`,
  `squeue` and `scancel`, and `srun --overlap` for the `srun` last hop.

## The tunnel (`tunnel`)

Byte streams from the laptop to the helper's daemon, over the user's OpenSSH: to a login node,
or to a compute node inside the helper's SLURM job.

```rust
let daemon = Daemon::new(target, Arc::new(launcher)).with_last_hop(site.last_hop());
let connector = Connector::start(daemon, ConnectorOptions::default())?;  // in a tokio runtime
let mut state = connector.watch();     // Connecting, Connected { transport }, Unverifiable, …
let stream = connector.connect().await?;   // AsyncRead + AsyncWrite: one HTTP or WS connection
connector.close().await;
```

- **The link.** A connector keeps its own `ssh -N` to the machine, with keepalives every 2 s
  (`ServerAliveInterval=2`). Where nothing else watches (the stdio transport) it gives up after
  8 s of silence (`ServerAliveCountMax=3`); where a forwarded socket is probed (below) it is
  patient (`ServerAliveCountMax=14`, 30 s), so a short outage costs no new login. A link starts
  patient only where a forward worked before and will be tried (not for srun, nor for a
  node-local socket); one that ends up carrying the bridge anyway (the forward failed) is
  started again impatient, so the ten-second bound holds for every transport. On Unix it is
  a ControlMaster whose socket is in the connector's own 0700 directory (`<runtime
  dir>/t<16 hex>`, made new with 8 random bytes, removed on close): the machine is logged in to
  once per (re)connection, and every connection, check and endpoint query is a channel of it.
  Those channels read no config (`-F none`, Unix only), never prompt (`BatchMode`), and with no
  master there fail at once instead of logging in (`ProxyCommand=false`). The link's own options
  come after `ssh -G` says what the user's config sets: `ForkAfterAuthentication` is turned off
  where it is on; a node's `ProxyCommand` runs with `SHELL=/bin/sh` (which the user's `Match
  exec` commands for that link then run with too); `EscapeChar=none` on every call. Its log is
  at `INFO`, for its reasons ("Timeout, server … not responding."). On Windows (no
  ControlMaster) it is a heartbeat, ready once ssh logs that it authenticated.
- **A crash** leaves a ControlMaster running (`ControlPersist=no` only ends it with its last
  client). Each connector holds a lock (`flock`) on `<dir>/lock` while it lives (taken on
  `lock.new`, then renamed, so a sweep never finds it free while the connector is starting); the
  next one to start stops the master of every directory whose lock is free (`ssh -O exit`) and
  removes the directory. On Unix without connection reuse the directory is removed too, but a
  dead app's heartbeat runs on until its connection ends. Windows ends a dead app's ssh with its
  Job Object; its directory (logs only) stays.
- **Where the daemon is** comes from the launcher's `status`, asked through the link before
  every (re)connection, and checked before anything of it reaches ssh's command line: the
  socket path (absolute, `<dir>/pitcrewd.sock`, at most 100 bytes, no control character, no
  empty, `.` or `..` component; a forward is tried only for a path of `A-Z a-z 0-9 . _ + - / @ ,
  =`, since `-L` splits at `:` and ssh expands `%`, `$` and `~` there, else the bridge gets it
  quoted), the version (for the bridge's path
  `<root>/bin/<version>/pitcrewd`), and for a job: still ours and running (squeue's name and
  uid), its endpoint naming it, and the node squeue names being the one the endpoint records,
  1 to 64 characters of `A-Z a-z 0-9 . _ -` starting with a letter or digit. The socket must be
  the one its launcher puts there (`<root>/run/pitcrewd.sock`, or the job's own node-local
  `<dir>/pitcrew-<job id>.<pid>/pitcrewd.sock`): a forward makes no checks of its own on the
  machine. A record that fails is never used
  (`Unreachable { Refused }`, asked again every `retry_every`).
- **A job's node** is reached as the site recipe's `last_hop` says:
  - `ssh`: a link to the node whose `ProxyCommand` is `ssh -W '[%h]:%p'` as a client of the
    login link's master (on Windows, `-J <login>`): like `ssh -J`, but the login node is not
    logged in to a second time (one one-time code per reconnection, not two), and the hop gets
    PitCrew's options. A node named like one of the concrete `Host`s of the user's ssh config
    (`ConnectorOptions::ssh_config`, default `~/.ssh/config` and its `Include`s) is refused:
    ssh would apply that `Host`'s settings; and the name is not canonicalized
    (`CanonicalizeHostname=no`). Left over: the system's `/etc/ssh/ssh_config` is not read for
    names, and a `Host` pattern (`node*`) or a `Match` still applies to the node's name, as it
    would for `ssh node017` typed by hand;
  - `srun`: `ssh <login> exec env -u SLURM_LABELIO -u SLURM_STDINMODE -u SLURM_STDOUTMODE -u
    SLURM_STDERRMODE srun --jobid=<id> --overlap --nodes=1 --ntasks=1 --nodelist=<node> --quiet
    <pitcrewd> connect --socket <path> --nonce <hex> --framed`, which also reaches a socket on
    the node's own disk (the variables would make srun label its lines or send its stdio
    elsewhere). A job on another cluster of a federation is refused there. **Each connection is
    a job step**: the scheduler's work, and counted against the job's `MaxStepCount` (a site's
    limit; often 40,000). Nothing else starts steps (no probe goes through srun), so a desktop
    opening a few connections a minute stays well inside it, but one that reconnects its event
    stream every few seconds would not. Nothing watches the job either: its end shows when a
    connection fails (srun: "Invalid job id"), which makes the connector ask where the helper
    is.
- **Transports** (`Transport`):
  - `Forwarded` (Unix): `ssh -O forward -L <dir>/f<n>:<socket>` adds a forward to the master
    (`StreamLocalBindMask=0177`, `StreamLocalBindUnlink=yes`); connections share it, each a
    forwarded channel, which sshd's `MaxSessions` does not count. It is checked with one
    `GET /v1/host/info` (the API's route without a token). A site with
    `AllowStreamLocalForwarding no` makes ssh close the channel and log "open failed:
    administratively prohibited": that is read (as ssh's own line format), remembered, and the
    stdio bridge used from then on. Any other failure of the forward is not remembered: it is
    tried again at the next (re)connection. Only the root's socket is forwarded: a node-local
    one is in a directory its job removes when it ends, which someone else on a shared node
    could make again with a socket of theirs; a forward checks nothing on the far side, so it
    would carry the person's connections (and tokens) there. The bridge checks the directory,
    the socket and who listens.
  - `Stdio`: each connection runs `pitcrewd connect` (below) on the host, as a session of the
    link (Unix) or a login of its own (Windows). The connection starts after the bridge's ready
    mark, which carries the call's random nonce, so a start-up file's chatter (or a mark it
    prints) is skipped. Always used on Windows: its OpenSSH forwards no unix sockets, neither std
    nor tokio has them there, and a TCP port instead would be open to every local user. Always
    used through `srun`, whose output forwarding may hold back a line until it ends: the bridge
    frames its output there (`--framed`), and the connector takes the frames apart.

  **sshd's `MaxSessions`** (10 by default; 1 or 2 on some sites) limits the stdio transport:
  each open connection is a session of the link. One more is refused with
  `SshError::SessionRefused` (ssh's "Session open refused by peer"): that connection's error
  alone; the link and the state stay (at the first connection, the attempt is tried again soon).
  Watching opens no session, except to ask where the daemon is after a connection failed (see
  below). A later version could carry many connections over one bridge (a multiplexing
  protocol in `pitcrewd connect`, one session for all); for now, few concurrent connections (an
  API client that reuses one) suit such sites.

  The choice is in `LinkState::Connected` and `Connector::transport()`; pass it back as
  `ConnectorOptions::transport` to remember it across runs.
- **Watching.** Connected, the connector watches the link's exit (its keepalives), and runs
  `ssh -O check` every `check_every` (5 s; Unix). Through a forwarded socket it sends a request
  every `probe_every` (4 s, answered within `probe_timeout`, 4 s): one unanswered makes the state
  `Unverifiable` within ten seconds; then one goes a second after the last gave up (about every
  5 s) until one is answered (`Connected` again, same link) or 30 s pass (the way is lost). A
  request the far end closes (or resets) unanswered makes it ask where the daemon is (a job that
  ended or moved); a local socket that is gone is forwarded again while the master lives.
  Neither logs in again. Nothing watches through a session (it would count against
  `MaxSessions`, or be a job step): a stdio link's keepalives are its watch, and notice a lost
  network within ten seconds too. A failed `connect()` makes the connector check at once: the
  master, and where the daemon is after a failure that suggests it (no daemon, a refused socket;
  through srun, any failure). That check is a session of its own, and runs beside the rest of
  the watching. They come at most every 2 s (10 s for where the daemon is) however many
  connections fail; failures within that gap get one more check once it is over. So do `wake()`
  and a jump of the wall clock against the monotonic one (the laptop slept). On Windows the
  monotonic clock runs during sleep: the desktop should call `wake()` on resume.
- **The ladder.** A lost way makes the state `Unverifiable`, stops what is lost, and tries
  again: at once if the connection had held for `give_up_after`, else after a back-off with
  jitter (`backoff_min` 1 s to `backoff_max` 30 s), until failing for `give_up_after` (2 minutes)
  makes it `Unreachable { Network }`. The login link stays while it runs and answers its check
  (a failing squeue, or a slow one, costs no new sign-in). A connection that drops soon after it
  connects counts as failing too. Once unreachable, it tries again every `retry_every` (1
  minute), unless the way kept dropping and reconnecting asked the person each time: that waits
  for `wake()` or `retry()`, so it does not ask for a one-time code every minute. A helper not
  running or a job that ended is `Unreachable { NotRunning }` at once, asked about again every
  `retry_every` through the login link (a new job, on another node, is picked up, with a new
  forward). A failed sign-in or a cancelled prompt (also one for a connection, on Windows) is
  `Unreachable { SignIn }` and waits for `retry()`. Prompts while reconnecting go through the
  askpass bridge; nothing is stored. Resuming the API stream (`since=`) is the caller's.
- **Attempts show.** Each attempt from `Unreachable` (its time came, `retry()`, `wake()`)
  makes the state `Connecting` until it ends: `Connected`, `Unverifiable`, or `Unreachable`
  again, even for the same reason. Connections wait for it (up to `connect_wait`). To follow
  one attempt, mark the watch seen, ask, then wait for a change and for the end:

  ```rust
  let mut state = connector.watch();
  state.borrow_and_update();
  connector.retry();
  state.changed().await?;                    // the attempt started (and may have ended)
  let end = state.wait_for(|s| *s != LinkState::Connecting).await?.clone();
  ```

  The same connector keeps its login link where it can (a helper not running, a refused
  record), so a retry through it costs no new sign-in, where a new connector would.
- **Security:** agent and X11 forwarding, local commands and configured forwardings are off on
  every call; every `-o` is PitCrew's; ssh gets only `MINIMAL_ENV` (and passed-through names);
  local sockets live in the 0700 directory; only the root's socket is forwarded; reasons in
  states and errors carry no paths (Unix's, nor Windows' `C:\…` and `\\server\…`) and no
  secrets.
- **Windows** works with fewer comforts: each connection logs in, and a password or one-time
  code is asked each time (use keys); no periodic probe (it would be a login); each endpoint
  check is a login too, so once one has asked the person, they come no more often than
  `retry_every` (a queued job is asked about every minute, or at `retry()`, not at every
  back-off step); `close()` ends the open connections too. Its cases ran on Linux without
  connection reuse, not on Windows.

## The stdio bridge (`bridge`)

`pitcrewd connect --socket <path> [--framed] [--nonce <hex>]` is the remote end of the stdio
transport. The daemon's CLI hands its arguments to `bridge::main(args)` (or calls
`connect_stdio(&Options)`, which blocks, and may be called inside a tokio runtime). Before a
byte passes it checks, as a client of the daemon does, that the socket's directory is a real
directory of this user's with no access for others, that the socket is a socket (not a link)
of this user's, and that the process listening runs as this user (`SO_PEERCRED`/`getpeereid`);
then it prints its ready mark (`\0pitcrew-bridge 1 ready <nonce>\n`, or `READY` without a
nonce) and copies stdin to the socket and the socket to stdout. Half-closes pass both ways (end
of file on stdin shuts down the socket's write side; the daemon's end of file closes stdout),
and it ends once both sides are done, or as soon as the daemon has closed its side altogether
(seen by `poll`'s `POLLHUP`; macOS's `poll` reports nothing for a descriptor asked about no
events, and takes a half-close for a hang-up when asked about input, so there it asks whether the
socket can be written, which reports the hang-up alone).
Framed (`--framed`), its output is chunks of at most 64 KiB, each `<length as 8 hex digits>:`,
the bytes, and `\n`, ending with `00000000:\n`: every chunk ends a line, so a line-buffered
`srun` passes it on at once. Exit codes: 2 usage, 3 not this user's (`EXIT_UNSAFE`), 4 no
daemon (`EXIT_NO_DAEMON`), 1 other. Messages name what is wrong, never the path.

## Tests

`cargo test -p pitcrew-remote` runs:
- unit tests, including property tests of the quoting (a POSIX lexer model, and models of
  how POSIX, fish and csh shells read the wrapper);
- `tests/fake_ssh.rs`, where the test binary acts as a scripted fake `ssh` that re-asks and
  "sends" credentials like real ssh (so cancel and app-quit tests can prove no empty password
  goes out), and as a fake app that quits mid-prompt;
- `tests/login_shells.rs`, which runs the wrapped command through every shell it finds, or
  those listed in `PITCREW_TEST_SHELLS` (`:`-separated paths);
- `tests/deploy.rs` (Unix), where the test binary is a fake `ssh` that runs the real remote
  script with the local `/bin/sh` in a temporary `HOME`, with a `PATH` holding only the tools
  the script may use. It can cut, pause or corrupt the upload, never read it, swallow the start
  of stdin (as a start-up file would), set the remote umask, or run another shell as
  `/bin/sh`. The binary also plays `pitcrewd` and the hash tools (in the formats of
  `sha256sum`, `shasum`, OpenSSL 1.1 and 3). It covers deploy and the idempotent re-run, hash
  mismatch, interrupted and killed uploads, concurrent, stale (by pid, by age on either clock)
  and lost locks, GC, every hash tool, BSD and busybox `mv`, unknown platforms, unsafe
  directories and unsafe ways to the root (group-writable, someone else's, through symbolic
  links; sticky ones allowed), a partly eaten script, look-alike tools in `PATH` and exported
  bash functions, ACLs behind macOS's `@`, set-group-ID parents, odd host names, the file modes
  during the upload, a stalled upload, the helper's umask, and the direct and tmux launchers
  (start, status, stop, `endpoint.json`, failures, other hosts, two roots on one host). It runs
  the whole flow again with each POSIX shell of `PITCREW_TEST_SHELLS` as the machine's
  `/bin/sh`;
- the SLURM cases in `tests/deploy/slurm.rs` (a `#[path]` module of `deploy.rs`), where the
  binary also plays `sbatch`, `squeue`, `scancel`, `sacct`, `srun` and `sinfo` with their state
  in files (honouring `SQUEUE_STATES`, `SCANCEL_STATE`, `SCANCEL_INTERACTIVE`, scancel's and
  sacct's name and user filters, and `-M` or `SLURM_CLUSTERS` for a named cluster): a job runs
  its script with the machine's `sh` in its own process group, and scancel sends SIGTERM, then
  SIGKILL after a wait. They cover submit, pending with a reason, running on a node with the
  time left, and stop; jobs refused by sbatch, failing at once, losing their node, or slow to
  leave the queue; squeue unreachable, scancel failing, and scancel missing the job; jobs that
  find another job (or none) recorded, or an open root; reused ids (another user's job with
  the same name, or a name forging a line), never cancelled; the direct launcher's helper on
  the same root, never overlapped (a submit and a direct start or stop while the job is
  queued, starting or running, with squeue failing or missing, and a direct helper that got in
  anyway); a named cluster, and one that would read as an option; a user recipe's extra lines
  and modules (and one with an unknown key, and a set-up script whose functions and aliases
  would stand in for tools), with unsafe set-up scripts refused; a node-local socket, and an
  open `$TMPDIR` refused; job scripts
  eaten, cut or changed on the way; the probe; and a job under each POSIX shell, which also
  shrugs off SIGUSR1 and SIGUSR2. The scripts' snapshots are unit tests
  (`PITCREW_UPDATE_SNAPSHOTS=1` rewrites them);
- the tunnel cases in `tests/deploy/tunnel.rs`, where the fake `ssh` also plays the tunnel's
  calls as OpenSSH does them: `ssh -G` (saying the user's config forks after authentication), a
  link (`-N`, a ControlMaster listening on its `ControlPath`, its keepalives timing out after
  `(CountMax + 1) × Interval` seconds of a down network, a `ProxyCommand` run first and ending
  it when it ends, a password asked through askpass), `-O` requests, `-W`, sessions running
  commands (refused beyond the machine's `MaxSessions`, as ssh's mux client reports it), and
  plain logins without connection reuse; it refuses a call without the options PitCrew must
  pass. The machine's network can be up, down or frozen (a laptop asleep); it can forbid
  forwarding unix sockets, fail a forward once, leave forwarded connections unanswered, cap
  sessions, and drop links after a while. `srun` runs job steps (passing their output on a line
  at a time, and labelling the lines where `SLURM_LABELIO` is set, if asked), and the fake
  daemon echoes, answers `GET` and half-closes. They cover a forwarded socket shared by many
  connections, forwarding refused then the stdio bridge (remembered, and not tried again while
  the bridge fails too), a forward that failed once (not remembered), a forward that never
  answers (the bridge, on a link started again impatient), the bridge through `srun --overlap`
  to a node-local socket (framed through a line-buffered, labelling srun; one job step per
  connection, none for watching; no patient link for a remembered forward), an srun job that
  ends (noticed at the next connection, then a new job), a node-local socket on a node that
  takes ssh (never forwarded), a node reached through the login link, nodes that fail the
  re-check or are named like one of the user's `Host`s (nothing started towards them), a
  dropped network noticed within ten seconds then recovered, a short silence that keeps a
  patient link, a link that keeps dropping (given up: waiting for a retry if reconnecting asked
  for a password, else trying again now and then), a wall-clock jump, a job that ended then
  moved to another node (the login kept, the forward made anew), a forwarded socket removed
  (forwarded again, no new login), a failing squeue (the login kept through the retries),
  askpass during a reconnect (and a cancel stopping the attempts), a session over
  `MaxSessions` (that connection's error only), a burst of failed connections (one check, and
  one more after the gap), the links of a crashed app (stopped by the next connector), no
  connection reuse (as on Windows: `close()` ends open connections, a cancelled prompt waits
  for a retry, a queued job polled no more often than `retry_every` once polling asked for a
  password), and both transports with each POSIX shell of
  `PITCREW_TEST_SHELLS` as the machine's `sh`. The bridge alone: byte-exact both ways, a large
  transfer, half-closes both ways, and sockets that are not the user's refused (not one served
  by another user, which needs root to set up);
- `deploy.rs` checks that nothing a case starts outlives it: everything started on a fake
  machine (the fake `ssh` too) carries the run's mark (`PITCREW_TEST_RUN`) in its environment;
  after each case, passed or not, what still carries it is killed and the case fails, and the
  end of the run checks that nothing is left (Linux, through `/proc`);
- `tests/real_sshd.rs`, only when `PITCREW_TEST_SSH_HOST` names a host reachable without
  prompts; it also deploys a stand-in helper into a throwaway directory there and removes it,
  and, on a host sharing this machine's files (`localhost`), tunnels to a stand-in daemon the
  test serves.

The Windows code (Job Object, named pipes) is checked with clippy for
`x86_64-pc-windows-gnu`; the test suites have not run on Windows yet (the deploy tests need a
Unix `sh` and skip there).
