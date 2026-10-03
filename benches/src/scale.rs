//! The measurements that need a person's whole history: `pitcrewd` with 10,000 transcripts.
//!
//! One run does this, in a temp folder it deletes at the end, with the real `pitcrewd` binary
//! and synthetic homes ([`crate::homes`]) that no real home is ever mixed with:
//!
//! 1. **Generate** the homes.
//! 2. **First scan.** Start a daemon on an empty state, set the workspace up (which starts the
//!    runner) and wait until the runner's index holds every transcript, caught up to its end.
//!    Time from the setup request. Sample the daemon's memory throughout; after the scan, let it
//!    go idle and read its memory again. Stop it cleanly and size the database.
//! 3. **Start.** Start it again with the index present, several times: time to the ready line.
//!    Then once more, to stay up: memory after it has caught up and gone idle.
//! 4. **Hook.** Send hooks for one session and time each until the `events` frame that carries
//!    its state change arrives on the stream.
//! 5. **Growth.** Write more turns to the live sessions, count the events they become, and size
//!    the database again.
//!
//! Each number is printed as a line [`crate::external`] reads (`scale <metric>: value … best …`),
//! so `benches/run.sh --scale` folds them into the same report as everything else.
//!
//! Nothing is left running: every daemon is stopped with SIGTERM and waited for (and killed if a
//! run fails half way), tmux servers started under the run's own `TMUX_TMPDIR` are killed, and
//! the folder is removed unless asked to keep it.

mod more;

use crate::client::{self, Ws};
use crate::homes::{self, Generated, Homes, Spec};
use crate::procfs;
use rusqlite::{Connection, OpenFlags};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Which measurements to take. Each stage needs the ones before it to have run, so a stage runs
/// them too and prints its own numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Generate, first scan, memory, database size.
    Scan,
    /// Scan, then cold start and memory with the index present.
    Start,
    /// Scan, then the hook-to-frame time.
    Hook,
    /// Scan, then the database's growth with new events.
    Growth,
    /// All historical measurements.
    All,
    /// Three-minute idle CPU with 50 static and slowly growing transcripts.
    Cpu,
    /// CLI verbs with the full history.
    Verbs,
    /// CLI hook with a loaded daemon, then with it down.
    HookCli,
    /// Hook-to-frame while fifty transcripts grow.
    HookLive,
    /// All remaining measurements (opt-in; does not change historical baselines).
    More,
}

impl Stage {
    /// The name on the command line.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scan" => Self::Scan,
            "start" => Self::Start,
            "hook" => Self::Hook,
            "growth" => Self::Growth,
            "all" => Self::All,
            "cpu" => Self::Cpu,
            "verbs" => Self::Verbs,
            "hook-cli" => Self::HookCli,
            "hook-live" => Self::HookLive,
            "more" => Self::More,
            _ => return None,
        })
    }

    /// Whether this is an opt-in remaining-budget stage.
    #[must_use]
    pub fn more(self) -> bool {
        matches!(
            self,
            Self::Cpu | Self::Verbs | Self::HookCli | Self::HookLive | Self::More
        )
    }

    fn starts(self) -> bool {
        matches!(self, Self::Start | Self::All)
    }

    fn hooks(self) -> bool {
        matches!(self, Self::Hook | Self::All)
    }

    fn grows(self) -> bool {
        matches!(self, Self::Growth | Self::All)
    }
}

/// What to run, and how hard.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The stage.
    pub stage: Stage,
    /// Transcripts to generate.
    pub sessions: usize,
    /// The generator's seed.
    pub seed: u64,
    /// The `pitcrewd` binary.
    pub pitcrewd: PathBuf,
    /// Cold starts to time with tmux detection as shipped.
    pub starts: usize,
    /// Cold starts to time with tmux refused, to tell what its detection costs.
    pub starts_no_tmux: usize,
    /// The least a daemon must be quiet before its memory is read.
    pub idle: Duration,
    /// Hooks to time, after `warmup` more that are not counted.
    pub probes: usize,
    /// Hooks sent first and not counted.
    pub warmup: usize,
    /// Turns written to each live session per round of the growth stage.
    pub growth_turns: usize,
    /// Events the growth stage waits for at least.
    pub growth_events: u64,
    /// Keep the folder (and say where it is) instead of deleting it.
    pub keep: bool,
    /// Drop the page cache before the first scan (when allowed: it needs root), so the scan
    /// reads the transcripts from disk as a first run does, not from the memory the generator
    /// just wrote them into.
    pub drop_cache: bool,
    /// Where to make the folder; the system temp dir if not given.
    pub work_parent: Option<PathBuf>,
    /// The least free space, in GiB, that must remain after the run's estimated need.
    pub min_free_gib: u64,
    /// How long the first scan may take before the run gives up.
    pub scan_timeout: Duration,
    /// Window for each idle CPU measurement (three minutes by default).
    pub cpu_window: Duration,
    /// CLI binary, normally next to pitcrewd.
    pub pitcrew: PathBuf,
}

impl Options {
    /// The usual run of a stage over [`homes::TRANSCRIPTS`] transcripts.
    #[must_use]
    pub fn new(stage: Stage, pitcrewd: PathBuf) -> Self {
        Self {
            pitcrew: pitcrewd
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("pitcrew"),
            stage,
            sessions: homes::TRANSCRIPTS,
            seed: 2026,
            pitcrewd,
            starts: 5,
            starts_no_tmux: 3,
            idle: Duration::from_secs(20),
            probes: 40,
            warmup: 5,
            growth_turns: 40,
            growth_events: 5_000,
            keep: false,
            drop_cache: true,
            work_parent: None,
            min_free_gib: 6,
            scan_timeout: Duration::from_secs(30 * 60),
            cpu_window: Duration::from_secs(180),
        }
    }
}

type Result<T> = std::result::Result<T, String>;

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn bytes_mib(bytes: u64) -> f64 {
    bytes as f64 / (1 << 20) as f64
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Prints a metric line in the form [`crate::external`] reads.
fn emit(name: &str, value: f64, best: f64, unit: &str, note: &str) {
    println!("scale {name}: value {value:.2} best {best:.2} {unit}   ({note})");
}

/// The middle value of a sorted list, and the value at fraction `p` of it.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let at = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    v
}

// ─── The daemon ─────────────────────────────────────────────────────────────────────────────────

/// Where a run keeps things.
struct Env {
    bin: PathBuf,
    work: PathBuf,
    homes: PathBuf,
    state: PathBuf,
    /// `TMUX_TMPDIR` for the daemons: short, because tmux's socket path must fit a unix socket
    /// address (the daemon falls back to a shared `/tmp/pitcrew-<uid>` when it would not).
    tmux: PathBuf,
}

impl Env {
    fn hub_db(&self) -> PathBuf {
        self.state.join("hub.db")
    }

    fn log(&self, n: usize) -> PathBuf {
        self.work.join(format!("daemon-{n}.log"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tmux {
    /// Detection as shipped, on a socket of the run's own.
    AsShipped,
    /// A socket whose directory is refused, so tmux is never started.
    Refused,
}

struct Daemon {
    child: Child,
    pid: u32,
    port: u16,
    ready_in: Duration,
    log: PathBuf,
    stopped: bool,
}

impl Daemon {
    fn start(env: &Env, tmux: Tmux, n: usize) -> Result<Self> {
        Self::start_inner(env, tmux, n, false)
    }

    fn start_inner(env: &Env, tmux: Tmux, n: usize, demo: bool) -> Result<Self> {
        let log = env.log(n);
        let mut command = Command::new(&env.bin);
        command
            .arg("--state-dir")
            .arg(&env.state)
            .args(["serve", "--listen", "tcp:127.0.0.1:0", "--homes"])
            .arg(&env.homes)
            .env("PITCREW_LOG", "info");
        if demo {
            command.arg("--demo");
        }
        match tmux {
            Tmux::AsShipped => {
                command
                    .env("TMUX_TMPDIR", &env.tmux)
                    .env_remove("XDG_RUNTIME_DIR");
            }
            Tmux::Refused => {
                command
                    .arg("--tmux-socket")
                    .arg(env.work.join("no-tmux").join("missing").join("tmux"));
            }
        }
        // Never a real home: the daemon's HOME and every variable that points elsewhere are the
        // run's own, and the check refuses to start it otherwise.
        pitcrew_fixtures::homes::private_home(&mut command, &env.work.join("home"));
        pitcrew_fixtures::homes::check_private_home(&command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?,
            ));
        let started = Instant::now();
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", env.bin.display()))?;
        let pid = child.id();
        let out = child.stdout.take().ok_or("no stdout")?;
        let (lines, ready) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(out)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
        let mut daemon = Self {
            child,
            pid,
            port: 0,
            ready_in: Duration::ZERO,
            log,
            stopped: false,
        };
        match ready.recv_timeout(Duration::from_secs(120)) {
            Ok(line) => {
                daemon.ready_in = started.elapsed();
                daemon.port = line
                    .strip_prefix("pitcrewd listening on http://127.0.0.1:")
                    .and_then(|p| p.trim().parse().ok())
                    .ok_or_else(|| format!("unexpected ready line {line:?}"))?;
                Ok(daemon)
            }
            Err(_) => Err(format!(
                "pitcrewd was not ready ({}):\n{}",
                daemon
                    .child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map_or_else(|| "still running".to_owned(), |s| s.to_string()),
                daemon.log_tail()
            )),
        }
    }

    /// The distinct warnings and errors in the log, without their times, and how many lines
    /// there were. What a fresh daemon says before its workspace is set up, and that it listens
    /// on loopback TCP, is left out: those lines are always there.
    fn log_problems(&self) -> (usize, Vec<String>) {
        const EXPECTED: [&str; 5] = [
            "the workspace is not set up yet",
            "the workspace has no local machine",
            "the workspace has no person yet",
            "the runner is off: the workspace has no local machine",
            "listening on loopback TCP",
        ];
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let mut seen: Vec<String> = Vec::new();
        let mut count = 0;
        for line in text.lines() {
            let Some((_, rest)) = line
                .split_once(" WARN ")
                .or_else(|| line.split_once(" ERROR "))
            else {
                continue;
            };
            if EXPECTED.iter().any(|e| rest.contains(e)) {
                continue;
            }
            count += 1;
            let rest: String = rest.chars().take(160).collect();
            if !seen.contains(&rest) && seen.len() < 5 {
                seen.push(rest);
            }
        }
        (count, seen)
    }

    fn log_tail(&self) -> String {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(20)..].join("\n")
    }

    fn token(&self, env: &Env) -> Result<String> {
        fs::read_to_string(env.state.join("device.token"))
            .map(|t| t.trim().to_owned())
            .map_err(|e| format!("no device token: {e}"))
    }

    /// SIGTERM, then waits for a clean exit. Returns how long that took.
    fn stop(&mut self) -> Result<Duration> {
        let started = Instant::now();
        let sent = Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .status()
            .map_err(|e| format!("cannot run kill: {e}"))?;
        if !sent.success() {
            return Err("kill -TERM failed".to_owned());
        }
        loop {
            if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
                self.stopped = true;
                return if status.success() {
                    Ok(started.elapsed())
                } else {
                    Err(format!(
                        "pitcrewd exited with {status}:\n{}",
                        self.log_tail()
                    ))
                };
            }
            if started.elapsed() > Duration::from_secs(60) {
                return Err(format!(
                    "pitcrewd did not stop in 60 s:\n{}",
                    self.log_tail()
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if !self.stopped && matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Kills the tmux servers the run's daemons may have started (their sockets are under the run's
/// own `TMUX_TMPDIR`).
fn kill_tmux(env: &Env) {
    let mut stack = vec![env.tmux.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "tmux") {
                let _ = Command::new("tmux")
                    .arg("-S")
                    .arg(&path)
                    .arg("kill-server")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

// ─── Reading the daemon's state ─────────────────────────────────────────────────────────────────

/// Read-only views of the hub's log and the runner's index, for watching progress without
/// asking the daemon's API for ten thousand sessions.
struct Watch {
    hub: Option<Connection>,
    index: Option<Connection>,
    state: PathBuf,
}

impl Watch {
    fn new(state: &Path) -> Self {
        Self {
            hub: None,
            index: None,
            state: state.to_path_buf(),
        }
    }

    fn open(path: &Path) -> Option<Connection> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        conn.busy_timeout(Duration::from_secs(2)).ok()?;
        Some(conn)
    }

    fn hub(&mut self) -> Option<&Connection> {
        if self.hub.is_none() {
            self.hub = Self::open(&self.state.join("hub.db"));
        }
        self.hub.as_ref()
    }

    fn index(&mut self) -> Option<&Connection> {
        if self.index.is_none() {
            let dir = fs::read_dir(self.state.join("runner")).ok()?;
            let path = dir
                .flatten()
                .map(|e| e.path().join("runner.sqlite3"))
                .find(|p| p.is_file())?;
            self.index = Self::open(&path);
        }
        self.index.as_ref()
    }

    /// The newest revision of the log (revisions have no gaps, so this is the event count).
    fn rev(&mut self) -> Option<u64> {
        self.hub()?
            .query_row("SELECT coalesce(max(rev), 0) FROM events", [], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
            .and_then(|n| u64::try_from(n).ok())
    }

    /// Sessions the log has announced.
    fn discovered(&mut self) -> Option<u64> {
        self.hub()?
            .query_row(
                "SELECT count(*) FROM events WHERE type = 'session_discovered'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .and_then(|n| u64::try_from(n).ok())
    }

    /// Transcripts the runner has indexed, and how many of them it has read to their end.
    fn indexed(&mut self) -> Option<(u64, u64)> {
        self.index()?
            .query_row(
                "SELECT count(*), coalesce(sum(caught_up AND discovered), 0) FROM transcripts",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok()
            .map(|(all, done)| {
                (
                    u64::try_from(all).unwrap_or(0),
                    u64::try_from(done).unwrap_or(0),
                )
            })
    }

    /// Events by type, most first.
    fn event_types(&mut self) -> Vec<(String, u64)> {
        let Some(conn) = self.hub() else {
            return Vec::new();
        };
        let Ok(mut stmt) =
            conn.prepare("SELECT type, count(*) FROM events GROUP BY type ORDER BY 2 DESC")
        else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        else {
            return Vec::new();
        };
        rows.flatten()
            .map(|(t, n)| (t, u64::try_from(n).unwrap_or(0)))
            .collect()
    }
}

/// The `n` busiest threads as text. The runner starts its sink thread before its watcher, and the
/// kernel cuts both names to the same 15 characters, so the lower thread id is the sink.
fn busiest(threads: &[procfs::Thread], n: usize) -> String {
    let mut runner: Vec<u32> = threads
        .iter()
        .filter(|t| t.name == "pitcrew-runner-")
        .map(|t| t.tid)
        .collect();
    runner.sort_unstable();
    threads
        .iter()
        .take(n)
        .map(|t| {
            let name = match runner.iter().position(|tid| *tid == t.tid) {
                Some(0) => "runner sink",
                Some(1) => "runner watcher",
                _ => t.name.as_str(),
            };
            format!("{name} {:.1} s", t.ticks as f64 / 100.0)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

/// The database and its write-ahead log, which a clean stop removes.
fn hub_bytes(env: &Env) -> u64 {
    let db = env.hub_db();
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    file_len(&db) + file_len(Path::new(&wal))
}

fn index_bytes(env: &Env) -> u64 {
    let Ok(dir) = fs::read_dir(env.state.join("runner")) else {
        return 0;
    };
    dir.flatten()
        .flat_map(|d| fs::read_dir(d.path()).into_iter().flatten().flatten())
        .map(|f| file_len(&f.path()))
        .sum()
}

/// Waits until the daemon has used (almost) no CPU for two seconds, at least `min` and at most
/// ten times that. Returns whether it went quiet.
fn wait_idle(pid: u32, min: Duration) -> bool {
    let started = Instant::now();
    let mut last = procfs::cpu_ticks(pid).unwrap_or(0);
    let mut quiet = 0;
    while started.elapsed() < min * 10 {
        thread::sleep(Duration::from_millis(500));
        let now = procfs::cpu_ticks(pid).unwrap_or(last);
        quiet = if now.saturating_sub(last) <= 1 {
            quiet + 1
        } else {
            0
        };
        last = now;
        if started.elapsed() >= min && quiet >= 4 {
            return true;
        }
    }
    false
}

fn free_kib(path: &Path) -> Option<u64> {
    let out = Command::new("df").args(["-Pk"]).arg(path).output().ok()?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// The available KiB in `df -Pk` output.
fn parse_df(text: &str) -> Option<u64> {
    text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

// ─── The run ────────────────────────────────────────────────────────────────────────────────────

/// What the first scan found out.
struct Scanned {
    db_bytes: u64,
}

/// Runs the stage and prints its numbers.
///
/// # Errors
///
/// Anything that stops a measurement: no binary, too little disk, a daemon that does not start,
/// stop or finish its scan.
pub fn run(options: &Options) -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Err("the scale measurements need Linux (/proc)".to_owned());
    }
    if !options.pitcrewd.is_file() {
        return Err(format!(
            "no pitcrewd at {}; build it (cargo build --profile bench -p pitcrew-daemon) or pass --pitcrewd",
            options.pitcrewd.display()
        ));
    }
    let parent = options
        .work_parent
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    // Roughly 150 KiB a transcript for the homes and 50 KiB for the state, with room to spare.
    let need_kib = options.sessions as u64 * 250 + options.min_free_gib * (1 << 20);
    if let Some(free) = free_kib(&parent)
        && free < need_kib
    {
        return Err(format!(
            "only {:.1} GiB free in {}; a run of {} transcripts needs about {:.1} GiB (including \
             {} GiB to spare)",
            free as f64 / (1 << 20) as f64,
            parent.display(),
            options.sessions,
            need_kib as f64 / (1 << 20) as f64,
            options.min_free_gib
        ));
    }
    let dir = tempfile::Builder::new()
        .prefix("pitcrew-scale-")
        .tempdir_in(&parent)
        .map_err(|e| format!("cannot make a folder in {}: {e}", parent.display()))?;
    let work = dir.path().to_path_buf();
    let keep = options.keep;
    let outcome = measure(options, &work);
    if keep {
        let kept = dir.keep();
        eprintln!("kept {}", kept.display());
    } else if let Err(e) = dir.close() {
        eprintln!("warning: cannot remove {}: {e}", work.display());
    }
    let left = procfs::find_by_cmdline(&work.to_string_lossy());
    if !left.is_empty() {
        eprintln!("warning: processes of this run are still running: {left:?}");
    }
    outcome
}

fn measure(options: &Options, work: &Path) -> Result<()> {
    let short = std::env::temp_dir();
    let short = if short.as_os_str().len() <= 40 {
        short
    } else {
        PathBuf::from("/tmp")
    };
    let tmux = tempfile::Builder::new()
        .prefix("pcs-")
        .tempdir_in(&short)
        .map_err(|e| format!("cannot make a folder in {}: {e}", short.display()))?;
    // The daemon uses TMUX_TMPDIR only if it is private to this user; a temp dir follows the
    // umask.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(tmux.path(), fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot make {} private: {e}", tmux.path().display()))?;
    }
    let env = Env {
        bin: options.pitcrewd.clone(),
        work: work.to_path_buf(),
        homes: work.join("homes"),
        state: work.join("state"),
        tmux: tmux.path().to_path_buf(),
    };
    fs::create_dir_all(env.work.join("home")).map_err(|e| e.to_string())?;
    let result = stages(options, &env);
    kill_tmux(&env);
    result
}

fn stages(options: &Options, env: &Env) -> Result<()> {
    println!("scale: machine {}", crate::report::machine());
    println!("scale: load average at the start {}", load_average());

    // 1. The homes.
    let mut spec = Spec::new(options.seed, options.sessions);
    if options.stage == Stage::Cpu {
        spec.claude = 50;
        spec.subagents = 0;
        spec.codex = 0;
        spec.opencode = 0;
    }
    if options.stage.more() {
        spec.hot = if matches!(options.stage, Stage::More | Stage::HookLive) {
            51
        } else {
            50
        };
    }
    let started = Instant::now();
    let mut made = homes::generate(&spec, &Homes::new(&env.homes), SystemTime::now())
        .map_err(|e| format!("cannot generate the homes: {e}"))?;
    let s = &made.stats;
    println!(
        "scale: homes {} transcripts in {} project folders (claude {} + {} sub-agents, codex {}, \
         opencode {}), {:.2} GiB, {} records, largest {:.1} MiB; written in {:.1} s",
        s.transcripts(),
        s.projects,
        s.claude,
        s.subagents,
        s.codex,
        s.opencode,
        s.bytes as f64 / (1u64 << 30) as f64,
        s.records,
        bytes_mib(s.largest),
        started.elapsed().as_secs_f64()
    );
    let total = s.transcripts() as u64;
    sync();
    let dropped = options.drop_cache && drop_caches();
    println!(
        "scale: page cache {} before the first scan",
        if dropped {
            "dropped"
        } else if options.drop_cache {
            "could not be dropped (not allowed); the generator's files are still in memory"
        } else {
            "kept"
        }
    );

    if options.stage.more() {
        return more::measure(options, env, &mut made, total);
    }

    // 2. The first scan.
    let scanned = first_scan(options, env, total, dropped)?;
    sync();

    // 3. Starts with the index present.
    let mut last_db = scanned.db_bytes;
    if options.stage.starts() {
        cold_starts(options, env, total)?;
    }
    // 4 and 5 start one daemon each and stop it.
    if options.stage.hooks() {
        let probe = made
            .probe
            .clone()
            .ok_or("the homes have no session to send hooks to")?;
        last_db = hooks(options, env, &probe.native_id, total)?;
    }
    if options.stage.grows() {
        growth(options, env, &mut made, last_db)?;
    }
    Ok(())
}

fn sync() {
    let _ = Command::new("sync").status();
}

/// Drops the page cache, if the system allows it. Returns whether it did.
fn drop_caches() -> bool {
    sync();
    fs::write("/proc/sys/vm/drop_caches", "3").is_ok()
}

fn load_average() -> String {
    fs::read_to_string("/proc/loadavg")
        .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Starts a daemon on the empty state to size an empty database, stops it, starts it again,
/// sets the workspace up, and watches the runner index the homes.
fn first_scan(options: &Options, env: &Env, total: u64, dropped: bool) -> Result<Scanned> {
    let mut first = Daemon::start(env, Tmux::AsShipped, 0)?;
    let empty_start = first.ready_in;
    first.stop()?;
    let empty_db = file_len(&env.hub_db());
    println!(
        "scale: empty store: ready in {:.0} ms, database {:.2} MiB",
        ms(empty_start),
        bytes_mib(empty_db)
    );

    let mut daemon = Daemon::start(env, Tmux::AsShipped, 1)?;
    let token = daemon.token(env)?;
    let mut watch = Watch::new(&env.state);
    let ticks0 = procfs::cpu_ticks(daemon.pid).unwrap_or(0);
    let setup = r#"{"workspace_name":"Scale","person":{"name":"Sam","handle":"@sam"},"machine_name":"bench"}"#;
    let began = Instant::now();
    let reply = client::request(daemon.port, "POST", "/v1/setup", Some(&token), Some(setup))
        .map_err(|e| format!("setup: {e}"))?;
    if reply.status != 200 {
        return Err(format!("setup answered {}: {}", reply.status, reply.body));
    }

    // Watch until the runner has read every transcript to its end.
    let mut first_session: Option<Duration> = None;
    let mut marks: Vec<(u64, Duration)> = Vec::new();
    let mut table: Vec<(Duration, u64, u64, u64)> = Vec::new();
    let mut done: Option<Duration> = None;
    let mut last_row = Instant::now();
    while began.elapsed() < options.scan_timeout {
        thread::sleep(Duration::from_millis(100));
        if !procfs::alive(daemon.pid) {
            return Err(format!(
                "pitcrewd died during the scan:\n{}",
                daemon.log_tail()
            ));
        }
        let at = began.elapsed();
        let seen = watch.discovered().unwrap_or(0);
        let (_, caught) = watch.indexed().unwrap_or((0, 0));
        for target in [1u64, 100, 1000] {
            if seen >= target && !marks.iter().any(|(t, _)| *t == target) {
                marks.push((target, at));
            }
        }
        if seen >= 1 && first_session.is_none() {
            first_session = Some(at);
        }
        if last_row.elapsed() >= Duration::from_secs(5) {
            last_row = Instant::now();
            let rss = procfs::memory(daemon.pid).map_or(0, |m| m.rss);
            table.push((at, seen, caught, rss));
        }
        if caught >= total {
            done = Some(at);
            break;
        }
    }
    let Some(scan) = done else {
        return Err(format!(
            "the first scan did not finish in {:?}:\n{}",
            options.scan_timeout,
            daemon.log_tail()
        ));
    };
    let ticks_scan = procfs::cpu_ticks(daemon.pid)
        .unwrap_or(0)
        .saturating_sub(ticks0);
    let threads = procfs::threads(daemon.pid);
    let rev = watch.rev().unwrap_or(0);
    let discovered = watch.discovered().unwrap_or(0);
    for (at, seen, caught, rss) in &table {
        println!(
            "scale: progress {:>5.0} s: {seen} sessions announced, {caught} read to the end, resident {:.1} MiB",
            at.as_secs_f64(),
            mib(*rss)
        );
    }
    let at_end = procfs::memory(daemon.pid).unwrap_or_default();
    println!(
        "scale: first scan {total} transcripts in {:.1} s ({:.0} transcripts/s); {rev} events; \
         cpu {:.1} s of {:.1} s wall; resident {:.1} MiB, peak {:.1} MiB",
        scan.as_secs_f64(),
        total as f64 / scan.as_secs_f64(),
        ticks_scan as f64 / 100.0,
        scan.as_secs_f64(),
        mib(at_end.rss),
        mib(at_end.peak)
    );
    println!(
        "scale: cpu of the busiest threads (whole life): {}",
        busiest(&threads, 5)
    );

    // Idle: the office and the runner settle, then read the memory.
    let quiet = wait_idle(daemon.pid, options.idle);
    let steady = procfs::memory(daemon.pid).unwrap_or_default();
    let rev_after = watch.rev().unwrap_or(rev);
    let types = watch.event_types();
    let live_db = hub_bytes(env);
    println!(
        "scale: after {:.0} s idle ({}): resident {:.1} MiB (heap {:.1}, mapped files {:.1}), peak {:.1} MiB; \
         {rev_after} events ({} since the scan finished)",
        options.idle.as_secs_f64(),
        if quiet { "quiet" } else { "still busy" },
        mib(steady.rss),
        mib(steady.anon),
        mib(steady.file),
        mib(steady.peak),
        rev_after.saturating_sub(rev)
    );
    let mut by_type = String::new();
    for (t, n) in types.iter().take(8) {
        let _ = write!(by_type, "{t} {n}, ");
    }
    println!("scale: events by type: {}", by_type.trim_end_matches(", "));
    println!(
        "scale: database while running {:.1} MiB; sessions announced {discovered}",
        bytes_mib(live_db)
    );
    // Everything the runner indexed is there to list.
    let listed = Instant::now();
    let reply = client::request(daemon.port, "GET", "/v1/sessions", Some(&token), None)
        .map_err(|e| format!("GET /v1/sessions: {e}"))?;
    let list_ms = ms(listed.elapsed());
    let count = serde_json::from_str::<serde_json::Value>(&reply.body)
        .ok()
        .and_then(|v| v.as_array().map(Vec::len))
        .unwrap_or(0);
    println!(
        "scale: GET /v1/sessions answered {} with {count} sessions, {:.1} MiB, in {list_ms:.0} ms",
        reply.status,
        bytes_mib(reply.body.len() as u64)
    );
    if count as u64 != total {
        println!(
            "scale: warning: the hub lists {count} sessions, the homes hold {total} transcripts"
        );
    }
    let (problems, distinct) = daemon.log_problems();
    println!(
        "scale: daemon log: {problems} unexpected warnings or errors{}",
        if distinct.is_empty() {
            String::new()
        } else {
            format!(": {}", distinct.join(" | "))
        }
    );
    drop(watch);
    let stop = daemon.stop()?;
    let db = file_len(&env.hub_db());
    let index = index_bytes(env);
    let wal_left = hub_bytes(env).saturating_sub(db);
    println!(
        "scale: stopped in {:.0} ms; hub.db {:.1} MiB ({} bytes of write-ahead log left), runner index {:.1} MiB",
        ms(stop),
        bytes_mib(db),
        wal_left,
        bytes_mib(index)
    );

    let grown = db.saturating_sub(empty_db);
    let sessions = discovered.max(1);
    emit(
        "first_scan",
        ms(scan),
        ms(scan),
        "ms",
        &format!(
            "{total} transcripts, {rev} events, {:.1} s of cpu, {}",
            ticks_scan as f64 / 100.0,
            if dropped {
                "read from disk (page cache dropped)"
            } else {
                "read from the page cache"
            }
        ),
    );
    if let Some(first) = first_session {
        let at = |n: u64| {
            marks
                .iter()
                .find(|(t, _)| *t == n)
                .map_or_else(|| "-".to_owned(), |(_, d)| format!("{:.0} ms", ms(*d)))
        };
        emit(
            "first_scan.first_session",
            ms(first),
            ms(first),
            "ms",
            &format!(
                "streamed: 1 session after {}, 100 after {}, 1000 after {}",
                at(1),
                at(100),
                at(1000)
            ),
        );
    }
    emit(
        "rss.scan_peak",
        mib(steady.peak),
        mib(steady.peak),
        "MiB",
        "VmHWM over the first scan and the idle after it",
    );
    emit(
        "rss.scan_steady",
        mib(steady.rss),
        mib(steady.rss),
        "MiB",
        &format!(
            "VmRSS after {:.0} s idle; heap {:.1}, mapped files {:.1}",
            options.idle.as_secs_f64(),
            mib(steady.anon),
            mib(steady.file)
        ),
    );
    emit(
        "db.after_scan",
        bytes_mib(db),
        bytes_mib(db),
        "MiB",
        &format!(
            "hub.db after a clean stop; {rev_after} events, {:.0} bytes an event",
            db.saturating_sub(empty_db) as f64 / rev_after.max(1) as f64
        ),
    );
    emit(
        "db.per_session",
        grown as f64 / 1024.0 / sessions as f64,
        grown as f64 / 1024.0 / sessions as f64,
        "KiB",
        &format!(
            "hub.db growth over an empty store / {sessions} sessions ({:.0} events a session)",
            rev_after as f64 / sessions as f64
        ),
    );
    emit(
        "index.after_scan",
        bytes_mib(index),
        bytes_mib(index),
        "MiB",
        &format!(
            "the runner's own index, {:.1} KiB a transcript",
            index as f64 / 1024.0 / total as f64
        ),
    );
    Ok(Scanned { db_bytes: db })
}

/// Cold starts with the index present, then one that stays up to read the settled memory.
fn cold_starts(options: &Options, env: &Env, total: u64) -> Result<()> {
    let mut times = Vec::new();
    let mut at_ready = Vec::new();
    for n in 0..options.starts {
        let mut d = Daemon::start(env, Tmux::AsShipped, 10 + n)?;
        times.push(ms(d.ready_in));
        at_ready.push(procfs::memory(d.pid).map_or(0.0, |m| mib(m.rss)));
        d.stop()?;
    }
    let mut no_tmux = Vec::new();
    for n in 0..options.starts_no_tmux {
        let mut d = Daemon::start(env, Tmux::Refused, 30 + n)?;
        no_tmux.push(ms(d.ready_in));
        d.stop()?;
    }
    // After the caches are dropped, if that is allowed: a first start after boot.
    let cold_cache = if drop_caches() {
        let mut d = Daemon::start(env, Tmux::AsShipped, 40)?;
        let t = ms(d.ready_in);
        d.stop()?;
        Some(t)
    } else {
        None
    };

    let times = sorted(times);
    let (median, best) = (percentile(&times, 0.5), times[0]);
    println!(
        "scale: cold starts with the index present (ms): {}; resident at the ready line {:.1} MiB",
        times
            .iter()
            .map(|t| format!("{t:.0}"))
            .collect::<Vec<_>>()
            .join(" "),
        at_ready.iter().copied().fold(0.0, f64::max)
    );
    emit(
        "cold_start",
        median,
        best,
        "ms",
        &format!(
            "spawn to the ready line, median of {}, {total} sessions indexed",
            times.len()
        ),
    );
    if !no_tmux.is_empty() {
        let no_tmux = sorted(no_tmux);
        emit(
            "cold_start.no_tmux",
            percentile(&no_tmux, 0.5),
            no_tmux[0],
            "ms",
            &format!("tmux refused, median of {}", no_tmux.len()),
        );
    }
    if let Some(t) = cold_cache {
        println!("scale: first start after dropping the page cache: ready in {t:.0} ms");
    }

    // One that stays: memory once it has caught up and gone quiet.
    let mut d = Daemon::start(env, Tmux::AsShipped, 50)?;
    let quiet = wait_idle(d.pid, options.idle);
    let m = procfs::memory(d.pid).unwrap_or_default();
    let busiest = busiest(&procfs::threads(d.pid), 4);
    println!(
        "scale: restarted and left {:.0} s ({}): resident {:.1} MiB (heap {:.1}, mapped files {:.1}), peak {:.1} MiB; \
         cpu since start {}",
        options.idle.as_secs_f64(),
        if quiet { "quiet" } else { "still busy" },
        mib(m.rss),
        mib(m.anon),
        mib(m.file),
        mib(m.peak),
        busiest
    );
    emit(
        "rss.restart_peak",
        mib(m.peak),
        mib(m.peak),
        "MiB",
        "VmHWM from the start through its catch-up and idle",
    );
    emit(
        "rss.restart_steady",
        mib(m.rss),
        mib(m.rss),
        "MiB",
        &format!(
            "VmRSS after {:.0} s idle; heap {:.1}, mapped files {:.1}",
            options.idle.as_secs_f64(),
            mib(m.anon),
            mib(m.file)
        ),
    );
    d.stop()?;
    Ok(())
}

/// Hooks for one session, each timed until the stream's frame for it arrives. Returns the
/// database's size after the daemon stopped.
fn hooks(options: &Options, env: &Env, native: &str, total: u64) -> Result<u64> {
    let mut d = Daemon::start(env, Tmux::AsShipped, 60)?;
    let token = d.token(env)?;
    wait_idle(d.pid, options.idle);
    let mut ws = Ws::connect(d.port, "/v1/stream", &token).map_err(|e| format!("stream: {e}"))?;
    let hello = ws
        .next_text(Duration::from_secs(10))
        .map_err(|e| format!("no hello: {e}"))?
        .unwrap_or_default();
    if !hello.contains("\"hello\"") {
        return Err(format!("the stream opened with {hello:.200}"));
    }
    // A thread stamps each frame when it arrives.
    let (frames, arrived) = mpsc::channel::<(Instant, String)>();
    thread::spawn(move || {
        while let Ok(Some(text)) = ws.next_text(Duration::from_secs(600)) {
            if frames.send((Instant::now(), text)).is_err() {
                break;
            }
        }
    });

    let events = ["UserPromptSubmit", "Stop"];
    let mut times = Vec::new();
    let mut posts = Vec::new();
    for n in 0..options.warmup + options.probes {
        thread::sleep(Duration::from_millis(400));
        while arrived.try_recv().is_ok() {}
        let event = events[n % 2];
        let body = format!(r#"{{"session_id":"{native}","hook_event_name":"{event}"}}"#);
        let sent = Instant::now();
        let reply = client::request(
            d.port,
            "POST",
            &format!("/v1/hooks/claude/{event}"),
            Some(&token),
            Some(&body),
        )
        .map_err(|e| format!("hook: {e}"))?;
        let answered = sent.elapsed();
        if reply.status != 202 {
            return Err(format!(
                "the hook answered {}: {}",
                reply.status, reply.body
            ));
        }
        let frame = loop {
            let (at, text) = arrived.recv_timeout(Duration::from_secs(10)).map_err(|_| {
                format!("no frame for hook {n} ({event}) in 10 s:\n{}", d.log_tail())
            })?;
            if text.contains("\"events\"") && text.contains("session_state_changed") {
                break at;
            }
        };
        if n >= options.warmup {
            times.push(ms(frame.duration_since(sent)));
            posts.push(ms(answered));
        }
    }
    let times = sorted(times);
    let posts = sorted(posts);
    println!(
        "scale: hook to frame with {total} sessions (ms): min {:.0}, p50 {:.0}, p95 {:.0}, max {:.0} over {}; \
         the hook's own answer p50 {:.1} ms",
        times[0],
        percentile(&times, 0.5),
        percentile(&times, 0.95),
        times[times.len() - 1],
        times.len(),
        percentile(&posts, 0.5)
    );
    emit(
        "hook_to_frame",
        percentile(&times, 0.5),
        times[0],
        "ms",
        &format!(
            "POST /v1/hooks to the events frame, p50 of {} (p95 {:.0} ms, max {:.0} ms), {total} sessions",
            times.len(),
            percentile(&times, 0.95),
            times[times.len() - 1]
        ),
    );
    drop(arrived);
    d.stop()?;
    Ok(file_len(&env.hub_db()))
}

/// Writes more turns to the live sessions and sizes what the events they become add.
fn growth(options: &Options, env: &Env, made: &mut Generated, before: u64) -> Result<()> {
    let mut d = Daemon::start(env, Tmux::AsShipped, 70)?;
    wait_idle(d.pid, Duration::from_secs(3));
    let mut watch = Watch::new(&env.state);
    let rev0 = watch.rev().ok_or("cannot read the log")?;
    let mut written = 0u64;
    let mut rounds = 0;
    let mut rev = rev0;
    while rev - rev0 < options.growth_events && rounds < 8 {
        rounds += 1;
        for live in &mut made.live {
            written += live
                .append_turns(options.growth_turns)
                .map_err(|e| format!("cannot append to {}: {e}", live.path.display()))?;
        }
        // The runner reads on a change; wait until the log stops growing.
        let mut still = 0;
        let mut last = rev;
        while still < 12 {
            thread::sleep(Duration::from_millis(250));
            rev = watch.rev().unwrap_or(last);
            still = if rev == last { still + 1 } else { 0 };
            last = rev;
        }
    }
    let events = rev - rev0;
    let types = watch.event_types();
    drop(watch);
    d.stop()?;
    let after = file_len(&env.hub_db());
    if events == 0 {
        return Err("the live sessions grew but the log did not".to_owned());
    }
    let grown = after.saturating_sub(before);
    let per_thousand = grown as f64 / 1024.0 / (events as f64 / 1000.0);
    let mut by_type = String::new();
    for (t, n) in types.iter().take(6) {
        let _ = write!(by_type, "{t} {n}, ");
    }
    println!(
        "scale: wrote {:.1} MiB of turns to {} live sessions in {rounds} rounds: {events} new events; \
         hub.db {:.2} -> {:.2} MiB",
        bytes_mib(written),
        made.live.len(),
        bytes_mib(before),
        bytes_mib(after)
    );
    emit(
        "db.per_1000_events",
        per_thousand,
        per_thousand,
        "KiB",
        &format!("hub.db growth over {events} events from live transcripts"),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_have_names() {
        for (name, stage) in [
            ("scan", Stage::Scan),
            ("start", Stage::Start),
            ("hook", Stage::Hook),
            ("growth", Stage::Growth),
            ("all", Stage::All),
        ] {
            assert_eq!(Stage::parse(name), Some(stage));
        }
        assert_eq!(Stage::parse("nope"), None);
        assert!(Stage::All.starts() && Stage::All.hooks() && Stage::All.grows());
        assert!(!Stage::Scan.starts() && !Stage::Scan.hooks() && !Stage::Scan.grows());
    }

    #[test]
    fn percentiles_pick_from_the_sorted_list() {
        let v = sorted(vec![5.0, 1.0, 3.0, 2.0, 4.0]);
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 1.0), 5.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn reads_free_space_from_df() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                   /dev/vda 263174212 22106788 19927592 53% /\n";
        assert_eq!(parse_df(out), Some(19_927_592));
        assert_eq!(parse_df("nothing"), None);
    }

    #[test]
    fn a_metric_line_is_what_the_report_reads() {
        // `emit` prints this shape; `external` must read it back.
        let line = "scale rss.scan_peak: value 61.20 best 61.20 MiB   (VmHWM)";
        let found = crate::external::parse(line);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "scale.rss.scan_peak");
    }
}
