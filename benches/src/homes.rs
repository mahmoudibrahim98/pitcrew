//! Synthetic agent homes at scale: thousands of transcripts across Claude Code, Codex and
//! OpenCode, for the measurements that need a person's whole history (`scale`).
//!
//! Everything is generated from a seed and written under a folder the caller gives (a temp dir);
//! nothing is read from a real home, and nothing here comes from a real machine. The layout is
//! what `pitcrewd serve --homes <root>` expects:
//!
//! | Path under the root | Content |
//! |---|---|
//! | `.claude/projects/<folder>/<session>.jsonl` | a Claude Code session |
//! | `.claude/projects/<folder>/<session>/subagents/agent-<id>.jsonl` | a sub-agent of that session |
//! | `.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl` | a Codex rollout |
//! | `.local/share/opencode/opencode.db` | OpenCode's SQLite store, one row per session |
//!
//! # Shape
//!
//! The records follow the fixtures (`crates/fixtures/data/transcripts`) and [`crate::inputs`]: a
//! prompt, a plan, tool calls with their results (file reads, shell output, edits with a
//! structured patch, searches), a closing message and a turn duration. What varies, as in a real
//! history:
//!
//! - **Length.** Most sessions are one or two turns, a few are long (a mix: 45% of 1-2 turns, 30%
//!   of 3-10, 20% of 11-40, 5% of 41-250), so the mean is about fifteen turns and the largest
//!   files run to several MiB.
//! - **Tool results.** Log-normal sizes, from a few hundred bytes to 48 KiB for a file read.
//! - **Folders.** About one project folder per 80 transcripts, with a few popular ones holding
//!   most sessions (and a few git worktrees of the same project).
//! - **Months.** Starts spread over the 270 days before 2026-09-28, more of them recent. File
//!   times match the end of each session, except for [`HOT`] live sessions, which were written in
//!   the last hours: those are the ones a watcher keeps a close eye on.
//! - **Sub-agents.** One transcript in ten is a Claude sub-agent of a longer session.
//! - **Mix.** 60% Claude sessions, 10% Claude sub-agents, 20% Codex rollouts, 10% OpenCode
//!   sessions.
//!
//! Timestamps inside records are fixed dates (before any real run), so the content depends on the
//! seed alone; only the file times of the hot sessions depend on the clock.

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The number of transcripts the budgets are stated for.
pub const TRANSCRIPTS: usize = 10_000;

/// Claude sessions written in the last hours, and kept so they can be written to again.
pub const HOT: usize = 24;

/// 2026-09-28T00:00:00Z: the newest instant any generated record carries.
const ANCHOR_MS: i64 = 1_790_553_600_000;
const DAY_MS: i64 = 86_400_000;
/// How far back the history reaches.
const HISTORY_DAYS: i64 = 270;

// ─── Random numbers ─────────────────────────────────────────────────────────────────────────────

/// A small deterministic generator (SplitMix64): the same seed always gives the same stream.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator starting at `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A generator for item `index` of kind `tag`, independent of the others of the same seed.
    #[must_use]
    pub fn for_item(seed: u64, tag: u64, index: usize) -> Self {
        let mut mix = Self(seed ^ tag.wrapping_mul(0xD6E8_FEB8_6659_FD93));
        mix.0 = mix
            .0
            .wrapping_add((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        // Run it once so neighbouring indices do not start with neighbouring states.
        let first = mix.next_u64();
        Self(first)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n` (`0` when `n` is 0).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi.saturating_sub(lo) + 1)
    }

    /// A number in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// True `percent` times in a hundred.
    pub fn percent(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// One of `items` (which must not be empty).
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.below(items.len() as u64)).unwrap_or(0)]
    }

    /// A log-normal number with this median, clamped to `min..=max`.
    pub fn lognormal(&mut self, median: f64, sigma: f64, min: f64, max: f64) -> f64 {
        let (u1, u2) = (self.unit().max(1e-12), self.unit());
        let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
        (median * (sigma * z).exp()).clamp(min, max)
    }

    /// `digits` lower-case hex digits.
    pub fn hex(&mut self, digits: usize) -> String {
        let mut out = String::with_capacity(digits + 16);
        while out.len() < digits {
            let _ = write!(out, "{:016x}", self.next_u64());
        }
        out.truncate(digits);
        out
    }

    /// `digits` letters and digits, as OpenCode's ids end.
    pub fn alnum(&mut self, digits: usize) -> String {
        const SET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        (0..digits)
            .map(|_| char::from(SET[usize::try_from(self.below(62)).unwrap_or(0)]))
            .collect()
    }

    /// A version-4 shaped UUID.
    pub fn uuid(&mut self) -> String {
        let h = self.hex(32);
        let variant = ['8', '9', 'a', 'b'][usize::try_from(self.below(4)).unwrap_or(0)];
        format!(
            "{}-{}-4{}-{}{}-{}",
            &h[0..8],
            &h[8..12],
            &h[13..16],
            variant,
            &h[17..20],
            &h[20..32]
        )
    }
}

// ─── Time ───────────────────────────────────────────────────────────────────────────────────────

/// An RFC 3339 time with milliseconds, from Unix milliseconds.
#[must_use]
pub fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// The calendar date of a day count since 1970-01-01 (the proleptic Gregorian calendar).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn system_time(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

// ─── What to generate ───────────────────────────────────────────────────────────────────────────

/// How many transcripts of each kind to write, and the seed they come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The seed: the same spec always writes the same bytes.
    pub seed: u64,
    /// Claude Code sessions.
    pub claude: usize,
    /// Claude Code sub-agent transcripts.
    pub subagents: usize,
    /// Codex rollouts.
    pub codex: usize,
    /// OpenCode sessions (rows of one store).
    pub opencode: usize,
}

impl Spec {
    /// `total` transcripts in the usual mix: 60% Claude sessions, 10% sub-agents, 20% Codex and
    /// the rest OpenCode.
    #[must_use]
    pub fn new(seed: u64, total: usize) -> Self {
        let claude = total * 6 / 10;
        let subagents = total / 10;
        let codex = total * 2 / 10;
        Self {
            seed,
            claude,
            subagents,
            codex,
            opencode: total - claude - subagents - codex,
        }
    }

    /// The history the budgets are stated for: [`TRANSCRIPTS`] transcripts.
    #[must_use]
    pub fn standard(seed: u64) -> Self {
        Self::new(seed, TRANSCRIPTS)
    }

    /// Every transcript, sub-agents and OpenCode sessions included.
    #[must_use]
    pub fn total(&self) -> usize {
        self.claude + self.subagents + self.codex + self.opencode
    }

    /// Project folders: about one per 80 transcripts.
    fn projects(&self) -> usize {
        (self.total() / 80).max(4)
    }
}

/// What was written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Claude Code session files.
    pub claude: usize,
    /// Claude Code sub-agent files.
    pub subagents: usize,
    /// Codex rollout files.
    pub codex: usize,
    /// OpenCode sessions.
    pub opencode: usize,
    /// Project folders (distinct working directories).
    pub projects: usize,
    /// Bytes on disk: every transcript file and OpenCode's store.
    pub bytes: u64,
    /// JSONL lines written, and OpenCode parts.
    pub records: u64,
    /// The size of the largest transcript file.
    pub largest: u64,
}

impl Stats {
    /// Transcripts the runner is expected to index.
    #[must_use]
    pub fn transcripts(&self) -> usize {
        self.claude + self.subagents + self.codex + self.opencode
    }
}

/// A hook a watcher can be sent for a generated session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The CLI's own id of a Claude session, as a hook names it.
    pub native_id: String,
}

/// A hot Claude session that can be written to again, as a live one is.
#[derive(Debug)]
pub struct Live {
    /// The transcript file.
    pub path: PathBuf,
    writer: ClaudeWriter,
    rng: Rng,
    pool: Arc<Pool>,
}

impl Live {
    /// Appends `turns` more turns to the transcript, as the CLI writes them. Returns the bytes
    /// written.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn append_turns(&mut self, turns: usize) -> io::Result<u64> {
        let mut buf = Vec::new();
        for _ in 0..turns {
            self.writer.turn(&mut self.rng, &self.pool, &mut buf);
        }
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        file.write_all(&buf)?;
        file.flush()?;
        Ok(buf.len() as u64)
    }
}

/// The result of [`generate`].
#[derive(Debug)]
pub struct Generated {
    /// What was written.
    pub stats: Stats,
    /// The hot Claude sessions, newest first.
    pub live: Vec<Live>,
    /// A session to send hooks to.
    pub probe: Option<Probe>,
}

/// The three homes under a root, as `pitcrewd --homes <root>` takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Homes {
    /// The root, laid out like a user's home folder.
    pub root: PathBuf,
}

impl Homes {
    /// Homes under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Claude Code's home.
    #[must_use]
    pub fn claude(&self) -> PathBuf {
        self.root.join(".claude")
    }

    /// Codex's home.
    #[must_use]
    pub fn codex(&self) -> PathBuf {
        self.root.join(".codex")
    }

    /// OpenCode's data folder.
    #[must_use]
    pub fn opencode(&self) -> PathBuf {
        self.root.join(".local").join("share").join("opencode")
    }
}

/// Writes the history `spec` describes into `homes`. The hot sessions' file times are set
/// relative to `now`.
///
/// # Errors
///
/// I/O errors, and a failure writing OpenCode's store.
pub fn generate(spec: &Spec, homes: &Homes, now: SystemTime) -> io::Result<Generated> {
    let pool = Arc::new(Pool::new());
    let projects = Project::table(spec);
    let now_ms = i64::try_from(
        now.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(ANCHOR_MS);
    let mut stats = Stats {
        projects: projects.iter().map(|p| p.cwds.len()).sum(),
        ..Stats::default()
    };
    let mut live = Vec::new();
    let mut probe = None;
    let mut parents: Vec<Parent> = Vec::new();
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);

    // Claude sessions. The first HOT are the live ones.
    for i in 0..spec.claude {
        let mut rng = Rng::for_item(spec.seed, 1, i);
        let hot = i < HOT;
        let project = &projects[project_index(&mut rng, projects.len())];
        let cwd = rng.pick(&project.cwds).clone();
        let session = rng.uuid();
        let (start, turns) = if hot {
            (
                ANCHOR_MS - (i as i64) * 2 * 3_600_000,
                usize::try_from(rng.range(6, 14)).unwrap_or(6),
            )
        } else {
            (session_start(&mut rng), session_turns(&mut rng))
        };
        let branch = (*rng.pick(BRANCHES)).to_owned();
        let mut writer = ClaudeWriter::new(&session, &cwd, branch, &project.files, start, None);
        buf.clear();
        for _ in 0..turns {
            writer.turn(&mut rng, &pool, &mut buf);
        }
        let folder = folder_name(&cwd);
        let path = homes
            .claude()
            .join("projects")
            .join(&folder)
            .join(format!("{session}.jsonl"));
        let mtime = if hot {
            now_ms - (i as i64 + 1) * 9 * 60_000
        } else {
            writer.t_ms
        };
        write_file(&path, &buf, mtime, &mut stats)?;
        stats.claude += 1;
        if turns >= 8 {
            parents.push(Parent {
                folder,
                session: session.clone(),
                cwd,
                branch: writer.branch.clone(),
                files: project.files.clone(),
                start: writer.started_ms,
                end: writer.t_ms,
            });
        }
        if hot {
            if probe.is_none() {
                probe = Some(Probe {
                    native_id: session.clone(),
                });
            }
            live.push(Live {
                path,
                writer,
                rng: Rng::for_item(spec.seed, 5, i),
                pool: Arc::clone(&pool),
            });
        }
    }

    // Sub-agents, each next to a longer session.
    for j in 0..spec.subagents {
        let mut rng = Rng::for_item(spec.seed, 2, j);
        let Some(parent) = (if parents.is_empty() {
            None
        } else {
            Some(&parents[j * 7 % parents.len()])
        }) else {
            break;
        };
        let agent = rng.hex(7);
        let span = (parent.end - parent.start).max(120_000);
        let start = parent.start + (rng.unit() * span as f64) as i64;
        let turns = usize::try_from(rng.range(1, 5)).unwrap_or(1);
        let mut writer = ClaudeWriter::new(
            &parent.session,
            &parent.cwd,
            parent.branch.clone(),
            &parent.files,
            start,
            Some(&agent),
        );
        buf.clear();
        for _ in 0..turns {
            writer.turn(&mut rng, &pool, &mut buf);
        }
        let path = homes
            .claude()
            .join("projects")
            .join(&parent.folder)
            .join(&parent.session)
            .join("subagents")
            .join(format!("agent-{agent}.jsonl"));
        write_file(&path, &buf, writer.t_ms, &mut stats)?;
        stats.subagents += 1;
    }

    // Codex rollouts, in the date folders it uses.
    for i in 0..spec.codex {
        let mut rng = Rng::for_item(spec.seed, 3, i);
        let project = &projects[project_index(&mut rng, projects.len())];
        let cwd = rng.pick(&project.cwds).clone();
        let id = rng.uuid();
        let start = session_start(&mut rng);
        let turns = session_turns(&mut rng);
        let mut writer = CodexWriter::new(&id, &cwd, start);
        buf.clear();
        writer.meta(&mut buf);
        for _ in 0..turns {
            writer.turn(&mut rng, &pool, &mut buf);
        }
        let stamp = rfc3339(start);
        let (date, time) = stamp.split_at(10);
        let (y, rest) = date.split_at(4);
        let (m, d) = (&rest[1..3], &rest[4..6]);
        let name = format!("rollout-{date}T{}-{id}.jsonl", time[1..9].replace(':', "-"));
        let path = homes
            .codex()
            .join("sessions")
            .join(y)
            .join(m)
            .join(d)
            .join(name);
        write_file(&path, &buf, writer.t_ms, &mut stats)?;
        stats.codex += 1;
    }

    // OpenCode: one store, one row per session.
    if spec.opencode > 0 {
        let dir = homes.opencode();
        fs::create_dir_all(&dir)?;
        let path = dir.join("opencode.db");
        let parts = opencode_store(&path, spec, &projects, &pool).map_err(io::Error::other)?;
        let size = fs::metadata(&path)?.len();
        stats.bytes += size;
        stats.largest = stats.largest.max(size);
        stats.records += parts;
        stats.opencode = spec.opencode;
        File::options()
            .write(true)
            .open(&path)?
            .set_modified(system_time(now_ms - 3 * 3_600_000))?;
    }
    Ok(Generated { stats, live, probe })
}

fn write_file(path: &Path, body: &[u8], mtime_ms: i64, stats: &mut Stats) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = File::create(path)?;
    file.write_all(body)?;
    file.flush()?;
    file.set_modified(system_time(mtime_ms))?;
    stats.bytes += body.len() as u64;
    stats.largest = stats.largest.max(body.len() as u64);
    stats.records += body.iter().filter(|b| **b == b'\n').count() as u64;
    Ok(())
}

/// A longer session a sub-agent can belong to.
struct Parent {
    folder: String,
    session: String,
    cwd: String,
    branch: String,
    files: Vec<String>,
    start: i64,
    end: i64,
}

// ─── Projects and sessions ──────────────────────────────────────────────────────────────────────

const BRANCHES: &[&str] = &[
    "main",
    "main",
    "main",
    "develop",
    "feature/ingest-cache",
    "fix/flaky-test",
    "chore/deps",
    "release/1.4",
];

const FILES: &[&str] = &[
    "src/lib.rs",
    "src/main.rs",
    "src/parser.rs",
    "src/config.rs",
    "src/store/mod.rs",
    "src/store/migrate.rs",
    "src/api/routes.rs",
    "src/util.rs",
    "tests/integration.rs",
    "Cargo.toml",
    "README.md",
    "docs/design.md",
];

const PROMPTS: &[&str] = &[
    "Fix the failing test in the parser module",
    "Add a retry with backoff to the upload step",
    "Explain how the store migrates old rows",
    "Rename the config loader and update its callers",
    "Why does the build take so long on CI? Look for the slowest crate",
    "Write tests for the empty-input case",
    "Tidy the error types: one enum per module",
    "Check that the new endpoint rejects a missing token",
    "Summarise what changed since the last release",
    "Replace the hand-written loop with an iterator chain",
    "The logs show a timeout after thirty seconds, find where it is set",
    "Add a flag to skip the slow checks",
    "Move the helpers out of main into their own module",
    "Draft the release notes from the commit log",
    "Profile the start-up path and list what dominates it",
    "Make the watcher ignore temporary files",
    "Document the public functions that have no comment",
    "Bump the dependency and fix what breaks",
];

/// A project: the folders it has been worked in, and the files its sessions touch.
struct Project {
    cwds: Vec<String>,
    files: Vec<String>,
}

impl Project {
    fn table(spec: &Spec) -> Vec<Self> {
        let n = spec.projects();
        (0..n)
            .map(|p| {
                let mut rng = Rng::for_item(spec.seed, 9, p);
                let name = format!("p{p:03}");
                let mut cwds = vec![format!("/work/projects/{name}")];
                // One project in ten has git worktrees too.
                if rng.percent(10) {
                    for w in 1..=rng.range(1, 3) {
                        cwds.push(format!("/work/projects/{name}-wt{w}"));
                    }
                }
                let files = FILES
                    .iter()
                    .filter(|_| rng.percent(70))
                    .map(|f| format!("{}/{f}", cwds[0]))
                    .collect::<Vec<_>>();
                let files = if files.is_empty() {
                    vec![format!("{}/src/lib.rs", cwds[0])]
                } else {
                    files
                };
                Self { cwds, files }
            })
            .collect()
    }
}

/// Claude Code's folder for a working directory: separators become dashes.
fn folder_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A few popular projects hold most sessions.
fn project_index(rng: &mut Rng, n: usize) -> usize {
    ((rng.unit().powf(2.2) * n as f64) as usize).min(n - 1)
}

/// When a session started: spread over the history, more of them recent.
fn session_start(rng: &mut Rng) -> i64 {
    let back = (HISTORY_DAYS * DAY_MS) as f64 * rng.unit().powf(1.6);
    ANCHOR_MS - back as i64 - rng.below(DAY_MS as u64) as i64
}

/// How many turns a session has.
fn session_turns(rng: &mut Rng) -> usize {
    let bucket = rng.below(100);
    let (lo, hi) = match bucket {
        0..=44 => (1, 2),
        45..=74 => (3, 10),
        75..=94 => (11, 40),
        _ => (41, 250),
    };
    usize::try_from(rng.range(lo, hi)).unwrap_or(1)
}

// ─── Text to draw from ──────────────────────────────────────────────────────────────────────────

/// Source listings and command output, cut to size at line boundaries.
#[derive(Debug)]
struct Pool {
    listing: String,
    listing_lines: Vec<usize>,
    output: String,
    output_lines: Vec<usize>,
}

impl Pool {
    fn new() -> Self {
        let mut listing = String::new();
        let mut n = 1u32;
        while listing.len() < 160 * 1024 {
            let _ = writeln!(
                listing,
                "{n:>6}\tlet value_{n} = compute({n}); // generated line"
            );
            n += 1;
        }
        let mut output = String::new();
        let mut n = 1u32;
        while output.len() < 96 * 1024 {
            let _ = match n % 4 {
                0 => writeln!(output, "test module_{n}::case_{} ... ok", n % 7),
                1 => writeln!(
                    output,
                    "   Compiling bench-crate-{n} v0.{}.{} (/work/crates/c{n})",
                    n % 5,
                    n % 11
                ),
                2 => writeln!(
                    output,
                    "warning: unused variable `value_{n}` in src/generated_{}.rs:{}:9",
                    n % 13,
                    n % 200
                ),
                _ => writeln!(
                    output,
                    "    Finished `dev` profile [unoptimized] target(s) in {}.{}s",
                    n % 40,
                    n % 10
                ),
            };
            n += 1;
        }
        Self {
            listing_lines: line_starts(&listing),
            listing,
            output_lines: line_starts(&output),
            output,
        }
    }

    fn listing(&self, rng: &mut Rng, bytes: usize) -> &str {
        cut(&self.listing, &self.listing_lines, rng, bytes)
    }

    fn output(&self, rng: &mut Rng, bytes: usize) -> &str {
        cut(&self.output, &self.output_lines, rng, bytes)
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|i| *i < text.len())
        .collect()
}

/// About `bytes` bytes of whole lines from a random place in `text`.
fn cut<'a>(text: &'a str, starts: &[usize], rng: &mut Rng, bytes: usize) -> &'a str {
    let first = starts[usize::try_from(rng.below(starts.len() as u64)).unwrap_or(0)];
    let want = first + bytes.min(text.len());
    let end = starts
        .get(starts.partition_point(|s| *s < want))
        .copied()
        .unwrap_or(text.len());
    if end > first {
        &text[first..end]
    } else {
        &text[..bytes.min(text.len())]
    }
}

fn push_line(out: &mut Vec<u8>, record: &Value) {
    out.extend_from_slice(record.to_string().as_bytes());
    out.push(b'\n');
}

// ─── Claude Code ────────────────────────────────────────────────────────────────────────────────

/// Writes one Claude Code (or sub-agent) transcript, turn by turn.
#[derive(Clone, Debug)]
struct ClaudeWriter {
    session: String,
    short: String,
    cwd: String,
    branch: String,
    files: Vec<String>,
    agent: Option<String>,
    parent: Option<String>,
    k: u32,
    turn: u32,
    started_ms: i64,
    t_ms: i64,
}

impl ClaudeWriter {
    fn new(
        session: &str,
        cwd: &str,
        branch: String,
        files: &[String],
        start_ms: i64,
        agent: Option<&str>,
    ) -> Self {
        Self {
            session: session.to_owned(),
            short: agent.map_or_else(|| session[..8].to_owned(), str::to_owned),
            cwd: cwd.to_owned(),
            branch,
            files: files.to_vec(),
            agent: agent.map(str::to_owned),
            parent: None,
            k: 0,
            turn: 0,
            started_ms: start_ms,
            t_ms: start_ms,
        }
    }

    /// A record of `kind`, `gap_ms` after the one before, chained to it.
    fn record(&mut self, kind: &str, gap_ms: i64) -> Value {
        self.k += 1;
        self.t_ms += gap_ms;
        let uuid = format!("{}-{:05}", self.short, self.k);
        let mut record = json!({
            "parentUuid": self.parent,
            "isSidechain": self.agent.is_some(),
            "userType": "external",
            "cwd": self.cwd,
            "sessionId": self.session,
            "version": "2.1.0",
            "gitBranch": self.branch,
            "type": kind,
            "uuid": uuid,
            "timestamp": rfc3339(self.t_ms),
        });
        if let (Some(agent), Some(map)) = (&self.agent, record.as_object_mut()) {
            map.insert("agentId".to_owned(), json!(agent));
        }
        self.parent = Some(uuid);
        record
    }

    fn user(&mut self, gap_ms: i64, content: Value) -> Value {
        let mut r = self.record("user", gap_ms);
        r["message"] = json!({"role": "user", "content": content});
        r
    }

    fn assistant(&mut self, gap_ms: i64, content: Value, stop: &str) -> Value {
        let (turn, k) = (self.turn, self.k + 1);
        let mut r = self.record("assistant", gap_ms);
        r["message"] = json!({
            "id": format!("msg_{}_{turn}_{k}", self.short),
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-5-5",
            "content": content,
            "stop_reason": stop,
            "usage": {"input_tokens": 1200 + turn, "output_tokens": 180}
        });
        r
    }

    /// One turn: a prompt, an optional remark, one to five tool calls with their results, a
    /// closing message and the turn's duration.
    fn turn(&mut self, rng: &mut Rng, pool: &Pool, out: &mut Vec<u8>) {
        self.turn += 1;
        let turn = self.turn;
        let thinking = i64::try_from(rng.range(4_000, 900_000)).unwrap_or(4_000);
        let prompt = format!("{}. (turn {turn})", rng.pick(PROMPTS));
        let r = self.user(thinking, json!(prompt));
        push_line(out, &r);
        if rng.percent(55) {
            let r = self.assistant(
                3_000,
                json!([{"type": "text", "text": "I will read the code first, then make the change and run the tests."}]),
                "tool_use",
            );
            push_line(out, &r);
        }
        let tools = rng.range(1, 5);
        for n in 1..=tools {
            self.tool(rng, pool, n, out);
        }
        let reply = format!("Turn {turn} is done: the change is in and the checks pass.");
        let r = self.assistant(4_000, json!([{"type": "text", "text": reply}]), "end_turn");
        push_line(out, &r);
        let parent = self.parent.clone();
        let mut r = self.record("system", 500);
        r["subtype"] = json!("turn_duration");
        r["durationMs"] = json!(self.t_ms - self.started_ms);
        r["parentUuid"] = json!(parent);
        push_line(out, &r);
    }

    /// A tool call and its result.
    fn tool(&mut self, rng: &mut Rng, pool: &Pool, n: u64, out: &mut Vec<u8>) {
        let id = format!("toolu_{}_{}_{n}", self.short, self.turn);
        let file = rng.pick(&self.files).clone();
        let kind = rng.below(100);
        let secs = |rng: &mut Rng, lo, hi| i64::try_from(rng.range(lo, hi)).unwrap_or(1000);
        let (name, input, result, extra) = match kind {
            0..=34 => {
                let bytes = rng.lognormal(4096.0, 1.0, 200.0, 48.0 * 1024.0) as usize;
                let text = pool.listing(rng, bytes).to_owned();
                ("Read", json!({"file_path": file}), text, None)
            }
            35..=59 => {
                let bytes = rng.lognormal(700.0, 1.2, 40.0, 16.0 * 1024.0) as usize;
                let text = pool.output(rng, bytes).to_owned();
                (
                    "Bash",
                    json!({"command": "cargo test", "description": "Run the tests"}),
                    text,
                    None,
                )
            }
            60..=74 => {
                let (old, new) = (
                    format!("let value_{} = compute({});", self.turn, n),
                    format!("let value_{} = compute_checked({})?;", self.turn, n),
                );
                (
                    "Edit",
                    json!({"file_path": file, "old_string": old, "new_string": new}),
                    format!("The file {file} has been updated."),
                    Some(json!({"filePath": file, "structuredPatch": [{
                        "oldStart": 10, "oldLines": 1, "newStart": 10, "newLines": 1,
                        "lines": [format!("-{old}"), format!("+{new}")]
                    }]})),
                )
            }
            75..=84 => {
                let bytes = rng.lognormal(900.0, 1.0, 60.0, 8.0 * 1024.0) as usize;
                let text = pool.output(rng, bytes).to_owned();
                (
                    "Grep",
                    json!({"pattern": "compute", "path": self.cwd}),
                    text,
                    None,
                )
            }
            85..=89 => {
                let listing = self
                    .files
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                ("Glob", json!({"pattern": "src/**/*.rs"}), listing, None)
            }
            90..=94 => (
                "Write",
                json!({"file_path": file, "content": "pub fn compute(n: u32) -> u32 { n }\n"}),
                format!("File created successfully at: {file}"),
                Some(json!({"type": "create", "filePath": file,
                            "content": "pub fn compute(n: u32) -> u32 { n }\n", "structuredPatch": []})),
            ),
            _ => (
                "TodoWrite",
                json!({"todos": [
                    {"content": "Read the module", "status": "completed", "activeForm": "Reading"},
                    {"content": "Make the change", "status": "in_progress", "activeForm": "Changing"},
                    {"content": "Run the tests", "status": "pending", "activeForm": "Testing"}
                ]}),
                "Todos have been modified successfully.".to_owned(),
                None,
            ),
        };
        let r = self.assistant(
            secs(rng, 2_000, 12_000),
            json!([{"type": "tool_use", "id": id, "name": name, "input": input}]),
            "tool_use",
        );
        push_line(out, &r);
        let mut r = self.user(
            secs(rng, 200, 25_000),
            json!([{"tool_use_id": id, "type": "tool_result", "content": result}]),
        );
        if let Some(extra) = extra {
            r["toolUseResult"] = extra;
        }
        push_line(out, &r);
    }
}

// ─── Codex ──────────────────────────────────────────────────────────────────────────────────────

/// Writes one Codex rollout, turn by turn.
#[derive(Debug)]
struct CodexWriter {
    id: String,
    cwd: String,
    turn: u32,
    t_ms: i64,
    start_ms: i64,
}

impl CodexWriter {
    fn new(id: &str, cwd: &str, start_ms: i64) -> Self {
        Self {
            id: id.to_owned(),
            cwd: cwd.to_owned(),
            turn: 0,
            t_ms: start_ms,
            start_ms,
        }
    }

    fn record(&mut self, gap_ms: i64, kind: &str, payload: Value) -> Value {
        self.t_ms += gap_ms;
        json!({"timestamp": rfc3339(self.t_ms), "type": kind, "payload": payload})
    }

    fn meta(&mut self, out: &mut Vec<u8>) {
        let r = self.record(
            0,
            "session_meta",
            json!({
                "id": self.id, "timestamp": rfc3339(self.start_ms), "cwd": self.cwd,
                "originator": "codex_cli_rs", "cli_version": "0.50.0",
                "instructions": null, "git": {"branch": "main"}
            }),
        );
        push_line(out, &r);
    }

    fn turn(&mut self, rng: &mut Rng, pool: &Pool, out: &mut Vec<u8>) {
        self.turn += 1;
        let turn = self.turn;
        let gap = |rng: &mut Rng, lo, hi| i64::try_from(rng.range(lo, hi)).unwrap_or(1000);
        let prompt = format!("{}. (turn {turn})", rng.pick(PROMPTS));
        let reply = format!("Turn {turn} is done: all tests pass.");
        let tag = self.id[..8].to_owned();
        let call = |n: u64| format!("call_{tag}_{turn}_{n}");
        let g = gap(rng, 4_000, 600_000);
        let cwd = self.cwd.clone();
        let mut push = |this: &mut Self, gap_ms: i64, kind: &str, payload: Value| {
            let r = this.record(gap_ms, kind, payload);
            push_line(out, &r);
        };
        push(
            self,
            g,
            "turn_context",
            json!({"cwd": cwd, "approval_policy": "on-request",
                   "sandbox_policy": {"mode": "workspace-write"}, "model": "gpt-5-codex", "summary": "auto"}),
        );
        push(
            self,
            100,
            "response_item",
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": prompt}]}),
        );
        push(
            self,
            0,
            "event_msg",
            json!({"type": "user_message", "message": prompt, "kind": "plain"}),
        );
        if rng.percent(40) {
            push(
                self,
                3_000,
                "response_item",
                json!({"type": "function_call", "name": "update_plan", "call_id": call(0),
                       "arguments": json!({"plan": [
                           {"step": "Run the tests", "status": "in_progress"},
                           {"step": "Fix failures", "status": "pending"}]}).to_string()}),
            );
            push(
                self,
                100,
                "response_item",
                json!({"type": "function_call_output", "call_id": call(0), "output": "Plan updated"}),
            );
        }
        for n in 1..=rng.range(1, 4) {
            let bytes = rng.lognormal(1500.0, 1.2, 60.0, 32.0 * 1024.0) as usize;
            let output = pool.output(rng, bytes).to_owned();
            let g = gap(rng, 2_000, 12_000);
            push(
                self,
                g,
                "response_item",
                json!({"type": "function_call", "name": "shell", "call_id": call(n),
                       "arguments": json!({"command": ["bash", "-lc", "cargo test"], "workdir": cwd}).to_string()}),
            );
            let g = gap(rng, 300, 20_000);
            push(
                self,
                g,
                "response_item",
                json!({"type": "function_call_output", "call_id": call(n),
                       "output": json!({"output": output,
                                        "metadata": {"exit_code": 0, "duration_seconds": 1.5}}).to_string()}),
            );
        }
        if rng.percent(50) {
            let n = 9;
            push(
                self,
                2_000,
                "response_item",
                json!({"type": "custom_tool_call", "status": "completed", "call_id": call(n), "name": "apply_patch",
                       "input": format!("*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-let v = {turn};\n+let v = {turn} + 1;\n*** End Patch")}),
            );
            push(
                self,
                200,
                "response_item",
                json!({"type": "custom_tool_call_output", "call_id": call(n),
                       "output": json!({"output": "Success. Updated the following files:\nM src/lib.rs\n",
                                        "metadata": {"exit_code": 0, "duration_seconds": 0.0}}).to_string()}),
            );
        }
        push(
            self,
            3_000,
            "event_msg",
            json!({"type": "agent_message", "message": reply}),
        );
        push(
            self,
            0,
            "response_item",
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": reply}]}),
        );
        push(
            self,
            100,
            "event_msg",
            json!({"type": "token_count", "info": {"total_token_usage":
                {"input_tokens": 5200 + turn, "output_tokens": 410, "total_tokens": 5610 + turn}}}),
        );
        push(
            self,
            100,
            "event_msg",
            json!({"type": "task_complete", "last_agent_message": reply}),
        );
    }
}

// ─── OpenCode ───────────────────────────────────────────────────────────────────────────────────

const OPENCODE_SCHEMA: &str = "
CREATE TABLE session (
  id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT, directory TEXT NOT NULL,
  title TEXT NOT NULL, version TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
);
CREATE TABLE message (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES session(id),
  time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL
);
CREATE TABLE part (
  id TEXT PRIMARY KEY, message_id TEXT NOT NULL REFERENCES message(id),
  session_id TEXT NOT NULL REFERENCES session(id),
  time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL
);
CREATE INDEX message_session_time_created_id_idx ON message (session_id, time_created, id);
CREATE INDEX part_session_idx ON part (session_id);
CREATE INDEX part_message_id_id_idx ON part (message_id, id);
";

/// An OpenCode id: its prefix, 12 hex digits of the position (milliseconds times 4096 plus a
/// counter, low 48 bits) and 14 letters and digits.
fn opencode_id(prefix: &str, ms: i64, counter: u64, rng: &mut Rng) -> String {
    let position = u64::try_from(ms).unwrap_or(0) * 4096 + counter % 4096;
    format!(
        "{prefix}_{:012x}{}",
        position & ((1 << 48) - 1),
        rng.alnum(14)
    )
}

/// Writes the store with `spec.opencode` sessions. Returns the number of parts.
fn opencode_store(
    path: &Path,
    spec: &Spec,
    projects: &[Project],
    pool: &Pool,
) -> rusqlite::Result<u64> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA synchronous = OFF; PRAGMA foreign_keys = OFF;")?;
    conn.execute_batch(OPENCODE_SCHEMA)?;
    let mut parts_total = 0u64;
    let batch = 100;
    let mut i = 0;
    while i < spec.opencode {
        let tx = conn.transaction()?;
        {
            let mut session_stmt =
                tx.prepare("INSERT INTO session VALUES (?1, ?2, NULL, ?3, ?4, '1.18.0', ?5, ?6)")?;
            let mut message_stmt = tx.prepare("INSERT INTO message VALUES (?1, ?2, ?3, ?4, ?5)")?;
            let mut part_stmt = tx.prepare("INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
            for index in i..(i + batch).min(spec.opencode) {
                let mut rng = Rng::for_item(spec.seed, 4, index);
                let project = &projects[project_index(&mut rng, projects.len())];
                let cwd = rng.pick(&project.cwds).clone();
                let start = session_start(&mut rng);
                let turns = session_turns(&mut rng).min(60);
                let session = opencode_id("ses", start, 0, &mut rng);
                let mut t = start;
                let mut counter = 1u64;
                let mut parts = 0u64;
                let mut sessions_end = start;
                for turn in 1..=turns {
                    t += i64::try_from(rng.range(4_000, 900_000)).unwrap_or(4_000);
                    let user = opencode_id("msg", t, counter, &mut rng);
                    counter += 1;
                    message_stmt.execute(params![
                        user,
                        session,
                        t,
                        t,
                        json!({"role": "user", "time": {"created": t}}).to_string()
                    ])?;
                    let text = format!("{}. (turn {turn})", rng.pick(PROMPTS));
                    let part = opencode_id("prt", t, counter, &mut rng);
                    counter += 1;
                    part_stmt.execute(params![
                        part,
                        user,
                        session,
                        t,
                        t,
                        json!({"type": "text", "text": text}).to_string()
                    ])?;
                    parts += 1;

                    t += 3_000;
                    let assistant = opencode_id("msg", t, counter, &mut rng);
                    counter += 1;
                    let mut part_rows: Vec<(String, i64, i64, String)> = Vec::new();
                    for n in 0..rng.range(1, 3) {
                        t += 4_000;
                        let started = t;
                        t += i64::try_from(rng.range(300, 15_000)).unwrap_or(300);
                        let (tool, input, bytes) = match rng.below(3) {
                            0 => (
                                "read",
                                json!({"filePath": rng.pick(&project.files)}),
                                rng.lognormal(4096.0, 1.0, 200.0, 48.0 * 1024.0),
                            ),
                            1 => (
                                "bash",
                                json!({"command": "cargo test"}),
                                rng.lognormal(700.0, 1.2, 40.0, 16.0 * 1024.0),
                            ),
                            _ => ("edit", json!({"filePath": rng.pick(&project.files)}), 80.0),
                        };
                        let output = if tool == "read" {
                            pool.listing(&mut rng, bytes as usize).to_owned()
                        } else if tool == "bash" {
                            pool.output(&mut rng, bytes as usize).to_owned()
                        } else {
                            "Edited the file".to_owned()
                        };
                        let id = opencode_id("prt", started, counter, &mut rng);
                        counter += 1;
                        part_rows.push((
                            id,
                            started,
                            t,
                            json!({"type": "tool", "tool": tool, "callID": format!("call_{turn}_{n}"),
                                   "state": {"status": "completed", "input": input, "output": output,
                                             "time": {"start": started, "end": t}}})
                            .to_string(),
                        ));
                    }
                    t += 3_000;
                    let reply = opencode_id("prt", t, counter, &mut rng);
                    counter += 1;
                    part_rows.push((
                        reply,
                        t,
                        t,
                        json!({"type": "text", "text": format!("Turn {turn} is done.")})
                            .to_string(),
                    ));
                    let finish = opencode_id("prt", t + 1, counter, &mut rng);
                    counter += 1;
                    part_rows.push((
                        finish,
                        t + 1,
                        t + 1,
                        json!({"type": "step-finish", "reason": "stop"}).to_string(),
                    ));
                    message_stmt.execute(params![
                        assistant,
                        session,
                        t - 20_000,
                        t + 1,
                        json!({"role": "assistant", "modelID": "demo-model", "providerID": "demo",
                               "time": {"created": t - 20_000, "completed": t + 1}})
                        .to_string()
                    ])?;
                    for (id, created, updated, data) in &part_rows {
                        part_stmt
                            .execute(params![id, assistant, session, created, updated, data])?;
                        parts += 1;
                    }
                    sessions_end = t + 1;
                }
                session_stmt.execute(params![
                    session,
                    format!("prj_{}", &folder_name(&cwd)),
                    cwd,
                    format!("Session {index}"),
                    start,
                    sessions_end
                ])?;
                parts_total += parts;
            }
        }
        tx.commit()?;
        i += batch;
    }
    conn.execute_batch("PRAGMA optimize;")?;
    Ok(parts_total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_ingest::claude::ClaudeAdapter;
    use pitcrew_ingest::codex::CodexAdapter;
    use pitcrew_ingest::opencode::OpenCodeAdapter;
    use pitcrew_interfaces::source::{Cursor, SourceAdapter, TranscriptItem, TranscriptRef};
    use pitcrew_protocol::model::Engine;
    use std::collections::BTreeMap;

    fn small(seed: u64) -> Spec {
        Spec::new(seed, 100)
    }

    /// Every transcript `adapter` discovers in `home`, read from the start with nothing skipped.
    fn read_all(engine: Engine, home: &Path) -> Vec<(TranscriptRef, Vec<TranscriptItem>)> {
        let adapter: Box<dyn SourceAdapter> = match engine {
            Engine::Claude => Box::new(ClaudeAdapter::new()),
            Engine::Codex => Box::new(CodexAdapter::new()),
            _ => Box::new(OpenCodeAdapter::new()),
        };
        adapter
            .discover(home)
            .unwrap()
            .into_iter()
            .map(|t| {
                let items = match engine {
                    Engine::Claude => {
                        let r = ClaudeAdapter::new().read(&t, &Cursor::default()).unwrap();
                        assert_eq!(r.skipped_total, 0, "{}", t.path.display());
                        r.chunk.items
                    }
                    Engine::Codex => {
                        let r = CodexAdapter::new().read(&t, &Cursor::default()).unwrap();
                        assert_eq!(r.skipped_total, 0, "{}", t.path.display());
                        r.chunk.items
                    }
                    _ => {
                        let r = OpenCodeAdapter::new().read(&t, &Cursor::default()).unwrap();
                        assert_eq!(r.skipped_total, 0, "{}", t.path.display());
                        r.chunk.items
                    }
                };
                (t, items)
            })
            .collect()
    }

    #[test]
    fn times_are_rfc3339() {
        assert_eq!(rfc3339(1_790_756_400_000), "2026-09-30T08:20:00.000Z");
        assert_eq!(rfc3339(ANCHOR_MS), "2026-09-28T00:00:00.000Z");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(951_782_400_123), "2000-02-29T00:00:00.123Z");
    }

    #[test]
    fn the_mix_adds_up() {
        let s = Spec::standard(1);
        assert_eq!(s.total(), TRANSCRIPTS);
        assert_eq!(
            (s.claude, s.subagents, s.codex, s.opencode),
            (6000, 1000, 2000, 1000)
        );
        assert_eq!(Spec::new(1, 7).total(), 7);
    }

    #[test]
    fn ids_are_shaped_and_streams_are_independent() {
        let mut r = Rng::new(7);
        let id = r.uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(id.matches('-').count(), 4);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_eq!(Rng::new(7).uuid(), id);
        assert_ne!(
            Rng::for_item(1, 1, 0).next_u64(),
            Rng::for_item(1, 1, 1).next_u64()
        );
        assert_ne!(
            Rng::for_item(1, 1, 0).next_u64(),
            Rng::for_item(1, 2, 0).next_u64()
        );
        let mut r = Rng::new(3);
        for _ in 0..1000 {
            let n = r.lognormal(1000.0, 1.0, 100.0, 5000.0);
            assert!((100.0..=5000.0).contains(&n));
            assert!(r.unit() < 1.0);
            assert!(r.range(3, 5) >= 3);
        }
    }

    #[test]
    fn every_transcript_parses_and_discovery_finds_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path());
        let spec = small(42);
        let made = generate(&spec, &homes, SystemTime::now()).unwrap();
        assert_eq!(made.stats.transcripts(), spec.total());
        assert!(made.stats.bytes > 0 && made.stats.records > 0);

        let claude = read_all(Engine::Claude, &homes.claude());
        assert_eq!(claude.len(), spec.claude + spec.subagents);
        let subs = claude
            .iter()
            .filter(|(t, _)| {
                t.path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "subagents")
            })
            .count();
        assert_eq!(subs, spec.subagents);
        let codex = read_all(Engine::Codex, &homes.codex());
        assert_eq!(codex.len(), spec.codex);
        let opencode = read_all(Engine::OpenCode, &homes.opencode());
        assert_eq!(opencode.len(), spec.opencode);
        for (t, items) in claude.iter().chain(&codex).chain(&opencode) {
            assert!(!items.is_empty(), "{}", t.path.display());
            assert!(
                items
                    .iter()
                    .any(|i| matches!(i, TranscriptItem::TurnEnded { .. })),
                "{} {:?}",
                t.path.display(),
                t.inner_id
            );
        }
        // Tool calls and edits are among the items, as the runner derives its events from them.
        let all = claude
            .iter()
            .chain(&codex)
            .chain(&opencode)
            .flat_map(|(_, i)| i);
        assert!(
            all.clone()
                .any(|i| matches!(i, TranscriptItem::ToolUse { .. }))
        );
        assert!(
            all.clone()
                .any(|i| matches!(i, TranscriptItem::FileEdit { .. }))
        );
    }

    #[test]
    fn the_same_seed_writes_the_same_bytes_and_another_does_not() {
        fn digest(root: &Path) -> BTreeMap<String, u64> {
            let mut out = BTreeMap::new();
            let mut stack = vec![root.to_path_buf()];
            while let Some(dir) = stack.pop() {
                for entry in fs::read_dir(dir).unwrap().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|e| e == "jsonl") {
                        let bytes = fs::read(&path).unwrap();
                        let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
                        });
                        out.insert(path.strip_prefix(root).unwrap().display().to_string(), h);
                    }
                }
            }
            out
        }
        let run = |seed| {
            let dir = tempfile::tempdir().unwrap();
            generate(&small(seed), &Homes::new(dir.path()), SystemTime::now()).unwrap();
            digest(dir.path())
        };
        let (a, b, c) = (run(5), run(5), run(6));
        assert!(a.len() > 50);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn hot_sessions_are_recent_and_can_grow() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path());
        let mut made = generate(&Spec::new(9, 400), &homes, SystemTime::now()).unwrap();
        assert_eq!(made.live.len(), HOT);
        let probe = made.probe.clone().unwrap();
        assert!(
            made.live[0]
                .path
                .to_string_lossy()
                .contains(&probe.native_id)
        );
        let before = fs::metadata(&made.live[0].path).unwrap();
        assert!(before.modified().unwrap().elapsed().unwrap() < Duration::from_secs(3600));
        let added = made.live[0].append_turns(3).unwrap();
        assert!(added > 0);
        assert_eq!(
            fs::metadata(&made.live[0].path).unwrap().len(),
            before.len() + added
        );
        let report = ClaudeAdapter::new()
            .read(
                &TranscriptRef {
                    engine: Engine::Claude,
                    path: made.live[0].path.clone(),
                    inner_id: None,
                    size: before.len() + added,
                    modified: 0,
                },
                &Cursor::default(),
            )
            .unwrap();
        assert_eq!(report.skipped_total, 0);
        // An older session is not recent.
        let old = fs::read_dir(homes.claude().join("projects"))
            .unwrap()
            .flatten()
            .flat_map(|d| {
                fs::read_dir(d.path())
                    .unwrap()
                    .flatten()
                    .collect::<Vec<_>>()
            })
            .filter(|e| e.path().is_file())
            .map(|e| e.metadata().unwrap().modified().unwrap().elapsed().unwrap())
            .max()
            .unwrap();
        assert!(old > Duration::from_secs(30 * 86_400), "{old:?}");
    }

    #[test]
    fn sizes_have_a_long_tail_and_folders_are_spread() {
        let dir = tempfile::tempdir().unwrap();
        let homes = Homes::new(dir.path());
        let spec = Spec::new(11, 1000);
        let made = generate(&spec, &homes, SystemTime::now()).unwrap();
        assert!(made.stats.projects >= 10, "{:?}", made.stats);
        let folders = fs::read_dir(homes.claude().join("projects"))
            .unwrap()
            .count();
        assert!(folders >= 8, "{folders} folders");
        let mean = made.stats.bytes / spec.total() as u64;
        assert!(
            made.stats.largest > 10 * mean,
            "{:?} mean {mean}",
            made.stats
        );
    }
}
