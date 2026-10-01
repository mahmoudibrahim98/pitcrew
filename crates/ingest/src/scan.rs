//! The machine scan: counts and suggested projects/workstreams, for onboarding and "scan again".
//!
//! [`scan`] discovers every engine's transcripts under the given [`ScanHome`]s (each adapter's
//! own `discover`), then reads each one's **light** session facts: a bounded prefix of a JSONL
//! transcript, or one indexed row of an OpenCode store. It never reads a full transcript, and
//! never copies prompt text — only paths, branches and counts (a title is not collected here).
//!
//! Discovery is sequential (each adapter's `discover` is already a bounded directory walk that
//! does not follow symlinks); the light reads run on a bounded pool of threads, claimed
//! dynamically from a shared queue so one huge OpenCode store does not stall the others.
//! `progress` always runs on the caller's own thread, at most every 100 ms.
//!
//! Because the light reads only look at a transcript's first bytes, a `cwd` or `branch` reflects
//! the session's **start**, not necessarily a later change (the Claude adapter's own cursor-based
//! `branch` is "latest seen"; here it is "first seen"). That is an accepted trade-off for a fast,
//! bounded scan; a full import (stream D) sees the final value.

use crate::claude::{self, ClaudeAdapter};
use crate::codex::{self, CodexAdapter};
use crate::opencode::{self, OpenCodeAdapter};
use pitcrew_interfaces::source::{SourceAdapter, TranscriptRef};
use pitcrew_protocol::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How much of a transcript's start the scan reads for its session facts: enough for the first
/// few records (session id, cwd, branch, start time), never the rest of the file.
const SCAN_PREFIX_BYTES: u64 = 64 * 1024;
/// OpenCode sessions looked up per store connection, so one huge store is still split into units
/// the thread pool can share out.
const OPENCODE_BATCH: usize = 256;
/// Branch names that do not count as a workstream of their own.
const DEFAULT_BRANCHES: &[&str] = &["main", "master", "trunk", "develop", "head"];
/// Non-git cwds sharing a parent become one suggested project only once this many of them exist;
/// a lone one is its own project (named after itself, not its parent).
const MIN_GROUPED_SIBLINGS: usize = 2;
const DAY_MS: i64 = 86_400_000;

// ─── Input ───────────────────────────────────────────────────────────────────────────────────

/// One engine's home folder to scan: an account's `~/.claude`, `~/.codex` or OpenCode's data
/// folder (or a `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `XDG_DATA_HOME` equivalent). Two homes for
/// the same engine are two accounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanHome {
    /// Which CLI.
    pub engine: Engine,
    /// Its home folder on this machine.
    pub home: PathBuf,
}

/// The engine homes this machine would scan by default, skipping an engine whose home cannot be
/// determined (no `HOME`/`USERPROFILE`). A caller with several accounts per engine builds its own
/// list instead.
#[must_use]
pub fn default_homes() -> Vec<ScanHome> {
    [
        (Engine::Claude, ClaudeAdapter::default_home()),
        (Engine::Codex, CodexAdapter::default_home()),
        (Engine::OpenCode, OpenCodeAdapter::default_home()),
    ]
    .into_iter()
    .filter_map(|(engine, home)| home.map(|home| ScanHome { engine, home }))
    .collect()
}

/// Options for [`scan`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanOptions {
    /// Now, for ranking by recent activity (sessions in the last 30 and 90 days). Tests pass a
    /// fixed time so rankings are deterministic.
    pub now: TimestampMs,
    /// Worker threads for the light reads; `None` picks the machine's available parallelism.
    pub threads: Option<usize>,
}

// ─── Output ──────────────────────────────────────────────────────────────────────────────────

/// One progress tick, sent at most every 100 ms.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanProgress {
    /// Transcripts looked at so far.
    pub scanned: usize,
    /// Total transcripts discovered. Always `Some` in this implementation: discovery (a directory
    /// walk) finishes before the first tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    /// A path recently finished, for a "scanning …" line. Best-effort: a tick can land between
    /// messages and carry nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Sessions found for one engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineCount {
    /// The CLI.
    pub engine: Engine,
    /// Sessions found.
    pub count: usize,
}

/// Sessions found under one account home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeCount {
    /// The CLI.
    pub engine: Engine,
    /// The home folder, as given in [`ScanHome`].
    pub home: String,
    /// Sessions found.
    pub count: usize,
}

/// Sessions found with one working directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCount {
    /// The `cwd`.
    pub path: String,
    /// Sessions found.
    pub count: usize,
}

/// Sessions started in one calendar month, UTC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthCount {
    /// `YYYY-MM`.
    pub month: String,
    /// Sessions found.
    pub count: usize,
}

/// Counts from a scan. `by_engine`, `by_home`, `by_folder` and `by_month` cover ordinary sessions
/// only: a sub-agent session shares its parent's folder and month, so folding it in would double
/// those buckets without adding information. Sub-agent sessions are counted once, separately, in
/// `subagent_sessions`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCounts {
    /// Ordinary (non-sub-agent) sessions found.
    pub sessions: usize,
    /// Sub-agent sessions found, counted separately.
    pub subagent_sessions: usize,
    /// Per engine.
    pub by_engine: Vec<EngineCount>,
    /// Per account home.
    pub by_home: Vec<HomeCount>,
    /// Per folder, busiest first.
    pub by_folder: Vec<FolderCount>,
    /// Per month, most recent first.
    pub by_month: Vec<MonthCount>,
    /// The earliest session start found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_activity: Option<TimestampMs>,
    /// The most recent activity found (a transcript's modification time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<TimestampMs>,
}

/// A suggested workstream inside a [`Suggestion`]'s project: either an active sub-folder
/// (`branch: None`, named after the folder) or a non-default branch (`branch: Some`, named after
/// it). A project can suggest both kinds, and a session can count toward one of each.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkstreamSuggestion {
    /// Stable within its project.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Set for a branch-based suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Sessions in it.
    pub session_count: usize,
    /// Of those, in the last 30 days.
    pub recent_30d: usize,
    /// Of those, in the last 90 days.
    pub recent_90d: usize,
}

/// A suggested project: a repository root (the nearest ancestor with a `.git`), or, for cwds with
/// no `.git` above them, a folder shared by several of them. Never the user's home directory, one
/// of the scanned [`ScanHome`]s, or a well-known system folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    /// Stable across a re-scan: the root's path.
    pub id: String,
    /// Display name (the root folder's name).
    pub name: String,
    /// The root folder.
    pub path: String,
    /// Whether a `.git` was found at or above it (versus a grouped non-git folder).
    pub is_git: bool,
    /// Sessions under it (any depth), excluding sub-agents.
    pub session_count: usize,
    /// Of those, in the last 30 days.
    pub recent_30d: usize,
    /// Of those, in the last 90 days.
    pub recent_90d: usize,
    /// Suggested workstreams inside it, ranked the same way as projects.
    pub workstreams: Vec<WorkstreamSuggestion>,
}

/// The result of [`scan`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    /// Counts.
    pub counts: ScanCounts,
    /// Suggested projects, most recently active first.
    pub suggestions: Vec<Suggestion>,
    /// Folders or transcripts skipped because they could not be read (permission denied, a
    /// vanished file, a locked store). Not fatal: the rest of the scan still ran.
    pub unreadable: u64,
}

// ─── The scan ────────────────────────────────────────────────────────────────────────────────

/// A read-only scan of a machine's agent history: discovers every engine's transcripts under
/// `homes`, reads each one's light session facts, and returns counts plus suggested projects and
/// workstreams. `progress` is called on the caller's own thread only, at most every 100 ms, and
/// at least once at the end with `scanned == total`.
pub fn scan(
    homes: &[ScanHome],
    options: &ScanOptions,
    mut progress: impl FnMut(ScanProgress),
) -> ScanReport {
    let mut units: Vec<Unit> = Vec::new();
    let mut unreadable = 0u64;
    for home in homes {
        let found = match home.engine {
            Engine::Claude => ClaudeAdapter.discover(&home.home),
            Engine::Codex => CodexAdapter.discover(&home.home),
            Engine::OpenCode => OpenCodeAdapter.discover(&home.home),
            _ => Ok(Vec::new()),
        };
        match found {
            Ok(refs) => extend_units(&mut units, home, refs),
            Err(_) => unreadable += 1,
        }
    }

    let total: usize = units.iter().map(Unit::len).sum();
    let threads = options.threads.unwrap_or_else(default_parallelism).max(1);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let (tx, rx) = mpsc::sync_channel::<String>(64);

    let facts: Vec<Option<SessionFacts>> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let next = &next;
            let done = &done;
            let units = &units;
            let tx = tx.clone();
            handles.push(scope.spawn(move || {
                let mut out = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(unit) = units.get(i) else {
                        break;
                    };
                    let results = unit.run();
                    done.fetch_add(results.len(), Ordering::Relaxed);
                    let _ = tx.try_send(unit.label());
                    out.extend(results);
                }
                out
            }));
        }
        drop(tx);

        let mut last_emit = Instant::now();
        let mut last_path: Option<String> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(p) => last_path = Some(p),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if last_emit.elapsed() >= Duration::from_millis(100) {
                progress(ScanProgress {
                    scanned: done.load(Ordering::Relaxed),
                    total: Some(total),
                    path: last_path.take(),
                });
                last_emit = Instant::now();
            }
        }

        let mut facts = Vec::with_capacity(total);
        for h in handles {
            facts.extend(h.join().unwrap_or_default());
        }
        facts
    });

    progress(ScanProgress {
        scanned: done.load(Ordering::Relaxed),
        total: Some(total),
        path: None,
    });

    let mut session_facts = Vec::with_capacity(facts.len());
    for f in facts {
        match f {
            Some(f) => session_facts.push(f),
            None => unreadable += 1,
        }
    }

    ScanReport {
        counts: aggregate_counts(&session_facts),
        suggestions: build_suggestions(&session_facts, homes, options.now),
        unreadable,
    }
}

fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

// ─── Work units ──────────────────────────────────────────────────────────────────────────────

/// One chunk of work for the thread pool: one Claude or Codex transcript, or a batch of OpenCode
/// sessions from the same store (so the store is opened once per batch, not once per session).
enum Unit {
    File {
        home: ScanHome,
        t: TranscriptRef,
    },
    OpenCode {
        home: ScanHome,
        path: PathBuf,
        refs: Vec<TranscriptRef>,
    },
}

impl Unit {
    fn len(&self) -> usize {
        match self {
            Self::File { .. } => 1,
            Self::OpenCode { refs, .. } => refs.len(),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::File { t, .. } => t.path.to_string_lossy().into_owned(),
            Self::OpenCode { path, .. } => path.to_string_lossy().into_owned(),
        }
    }

    fn run(&self) -> Vec<Option<SessionFacts>> {
        match self {
            Self::File { home, t } => vec![match home.engine {
                Engine::Claude => claude_light(home, t),
                Engine::Codex => codex_light(home, t),
                _ => None,
            }],
            Self::OpenCode { home, path, refs } => opencode_batch(home, path, refs),
        }
    }
}

/// Splits `refs` into work units: one per Claude/Codex transcript, or OpenCode sessions grouped
/// by store and chunked so one store's sessions can still be shared across threads.
fn extend_units(units: &mut Vec<Unit>, home: &ScanHome, refs: Vec<TranscriptRef>) {
    if home.engine == Engine::OpenCode {
        let mut by_path: HashMap<PathBuf, Vec<TranscriptRef>> = HashMap::new();
        for r in refs {
            by_path.entry(r.path.clone()).or_default().push(r);
        }
        for (path, rs) in by_path {
            for chunk in rs.chunks(OPENCODE_BATCH) {
                units.push(Unit::OpenCode {
                    home: home.clone(),
                    path: path.clone(),
                    refs: chunk.to_vec(),
                });
            }
        }
    } else {
        for t in refs {
            units.push(Unit::File {
                home: home.clone(),
                t,
            });
        }
    }
}

// ─── Light reads ─────────────────────────────────────────────────────────────────────────────

/// What one session contributes to the scan. Never the prompt text, only paths, times and counts.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionFacts {
    engine: Engine,
    home: PathBuf,
    cwd: Option<String>,
    branch: Option<String>,
    /// The session's own first-seen start time, when a light read found one.
    started: Option<TimestampMs>,
    /// The transcript's modification time (OpenCode: the session's `time_updated`); always known
    /// from discovery, so it stands in for "last activity" even when `started` is not.
    last_activity: TimestampMs,
    is_subagent: bool,
}

fn claude_light(home: &ScanHome, t: &TranscriptRef) -> Option<SessionFacts> {
    let data = read_prefix(&t.path, SCAN_PREFIX_BYTES)?;
    let mut cwd = None;
    let mut branch = None;
    let mut started = None;
    let mut sidechain = None;
    for raw in data.split(|&b| b == b'\n') {
        let line = strip_cr(raw);
        if line.is_empty() {
            continue;
        }
        if let Ok(rec) = claude::parse_line(line, 0) {
            let f = rec.facts;
            if cwd.is_none() {
                cwd = f.cwd;
            }
            if branch.is_none() {
                branch = f.branch;
            }
            if started.is_none() {
                started = f.timestamp;
            }
            if sidechain.is_none() {
                sidechain = f.is_sidechain;
            }
        }
    }
    // Matches `ClaudeAdapter`'s own rule: a transcript under a `subagents` folder is a sub-agent
    // session even on CLI versions that do not also set `isSidechain`.
    let in_subagents = t.path.parent().and_then(Path::file_name) == Some(OsStr::new("subagents"));
    Some(SessionFacts {
        engine: Engine::Claude,
        home: home.home.clone(),
        cwd,
        branch,
        started,
        last_activity: t.modified,
        is_subagent: in_subagents || sidechain.unwrap_or(false),
    })
}

fn codex_light(home: &ScanHome, t: &TranscriptRef) -> Option<SessionFacts> {
    let data = read_prefix(&t.path, SCAN_PREFIX_BYTES)?;
    let mut cwd = None;
    let mut branch = None;
    let mut started = None;
    let mut subagent = None;
    for raw in data.split(|&b| b == b'\n') {
        let line = strip_cr(raw);
        if line.is_empty() {
            continue;
        }
        if let Ok(rec) = codex::parse_line(line, 0) {
            let f = rec.facts;
            if cwd.is_none() {
                cwd = f.cwd;
            }
            if branch.is_none() {
                branch = f.branch;
            }
            if started.is_none() {
                started = f.timestamp;
            }
            if subagent.is_none() {
                subagent = f.is_subagent;
            }
        }
    }
    Some(SessionFacts {
        engine: Engine::Codex,
        home: home.home.clone(),
        cwd,
        branch,
        started,
        last_activity: t.modified,
        is_subagent: subagent.unwrap_or(false),
    })
}

fn opencode_batch(
    home: &ScanHome,
    path: &Path,
    refs: &[TranscriptRef],
) -> Vec<Option<SessionFacts>> {
    let ids: Vec<&str> = refs.iter().filter_map(|r| r.inner_id.as_deref()).collect();
    let metas = opencode::light_meta(path, &ids).unwrap_or_default();
    refs.iter()
        .map(|r| {
            let m = metas.get(r.inner_id.as_deref()?)?;
            Some(SessionFacts {
                engine: Engine::OpenCode,
                home: home.home.clone(),
                cwd: m.cwd.clone(),
                branch: None,
                started: m.started,
                last_activity: r.modified,
                is_subagent: m.is_subagent,
            })
        })
        .collect()
}

/// The first `max` bytes of `path`; `None` if it cannot be opened.
fn read_prefix(path: &Path, max: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(max).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn strip_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

// ─── Counts ──────────────────────────────────────────────────────────────────────────────────

fn engine_rank(e: Engine) -> u8 {
    match e {
        Engine::Claude => 0,
        Engine::Codex => 1,
        Engine::OpenCode => 2,
        _ => 255,
    }
}

fn aggregate_counts(facts: &[SessionFacts]) -> ScanCounts {
    let mut sessions = 0usize;
    let mut subagent_sessions = 0usize;
    let mut by_engine: HashMap<Engine, usize> = HashMap::new();
    let mut by_home: HashMap<(Engine, String), usize> = HashMap::new();
    let mut by_folder: HashMap<String, usize> = HashMap::new();
    let mut by_month: HashMap<String, usize> = HashMap::new();
    let mut first_activity: Option<TimestampMs> = None;
    let mut last_activity: Option<TimestampMs> = None;

    for f in facts {
        let start = f.started.unwrap_or(f.last_activity);
        first_activity = Some(first_activity.map_or(start, |v| v.min(start)));
        last_activity = Some(last_activity.map_or(f.last_activity, |v| v.max(f.last_activity)));

        if f.is_subagent {
            subagent_sessions += 1;
            continue;
        }
        sessions += 1;
        *by_engine.entry(f.engine).or_default() += 1;
        *by_home
            .entry((f.engine, f.home.to_string_lossy().into_owned()))
            .or_default() += 1;
        if let Some(cwd) = &f.cwd {
            *by_folder.entry(cwd.clone()).or_default() += 1;
        }
        if let Some(started) = f.started {
            *by_month
                .entry(crate::time::year_month(started))
                .or_default() += 1;
        }
    }

    let mut by_engine: Vec<EngineCount> = by_engine
        .into_iter()
        .map(|(engine, count)| EngineCount { engine, count })
        .collect();
    by_engine.sort_by_key(|e| engine_rank(e.engine));

    let mut by_home: Vec<HomeCount> = by_home
        .into_iter()
        .map(|((engine, home), count)| HomeCount {
            engine,
            home,
            count,
        })
        .collect();
    by_home.sort_by(|a, b| {
        engine_rank(a.engine)
            .cmp(&engine_rank(b.engine))
            .then_with(|| a.home.cmp(&b.home))
    });

    let mut by_folder: Vec<FolderCount> = by_folder
        .into_iter()
        .map(|(path, count)| FolderCount { path, count })
        .collect();
    by_folder.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));

    let mut by_month: Vec<MonthCount> = by_month
        .into_iter()
        .map(|(month, count)| MonthCount { month, count })
        .collect();
    by_month.sort_by(|a, b| b.month.cmp(&a.month));

    ScanCounts {
        sessions,
        subagent_sessions,
        by_engine,
        by_home,
        by_folder,
        by_month,
        first_activity,
        last_activity,
    }
}

// ─── Suggestions ─────────────────────────────────────────────────────────────────────────────

fn recency_counts(sessions: &[&SessionFacts], now: TimestampMs) -> (usize, usize) {
    let within = |days: i64| {
        sessions
            .iter()
            .filter(|f| {
                now.saturating_sub(f.last_activity) <= days * DAY_MS && f.last_activity <= now
            })
            .count()
    };
    (within(30), within(90))
}

/// Climbs from `cwd` to the nearest ancestor containing a `.git` (directory or file).
fn git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|a| fs::metadata(a.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// Resolves every distinct cwd to a project root and whether it is a git root. A cwd with no
/// `.git` above it is grouped with its siblings under their shared parent once there are at least
/// [`MIN_GROUPED_SIBLINGS`] of them; a lone one is its own root.
fn resolve_roots(cwds: &[PathBuf]) -> HashMap<PathBuf, (PathBuf, bool)> {
    let mut out = HashMap::with_capacity(cwds.len());
    let mut non_git: Vec<&PathBuf> = Vec::new();
    for cwd in cwds {
        match git_root(cwd) {
            Some(root) => {
                out.insert(cwd.clone(), (root, true));
            }
            None => non_git.push(cwd),
        }
    }
    let mut by_parent: HashMap<&Path, usize> = HashMap::new();
    for cwd in &non_git {
        if let Some(parent) = cwd.parent() {
            *by_parent.entry(parent).or_default() += 1;
        }
    }
    for cwd in non_git {
        let root = match cwd.parent() {
            Some(parent)
                if by_parent
                    .get(parent)
                    .is_some_and(|n| *n >= MIN_GROUPED_SIBLINGS) =>
            {
                parent.to_path_buf()
            }
            _ => cwd.clone(),
        };
        out.insert(cwd.clone(), (root, false));
    }
    out
}

/// A root never worth suggesting: the user's home, a scanned engine home, a filesystem/drive
/// root, or a well-known OS folder directly below one.
fn is_excluded_root(path: &Path, homes: &[ScanHome]) -> bool {
    if crate::user_home().is_some_and(|home| path == home) {
        return true;
    }
    if homes.iter().any(|h| path == h.home) {
        return true;
    }
    let Some(parent) = path.parent() else {
        return true; // a filesystem or drive root
    };
    if parent.parent().is_none() {
        const SYSTEM_DIRS: &[&str] = &[
            "usr",
            "etc",
            "var",
            "tmp",
            "proc",
            "sys",
            "bin",
            "sbin",
            "opt",
            "root",
            "home",
            "windows",
            "program files",
            "program files (x86)",
            "programdata",
            "users",
        ];
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase());
        if name.is_some_and(|n| SYSTEM_DIRS.contains(&n.as_str())) {
            return true;
        }
    }
    false
}

fn build_suggestions(
    facts: &[SessionFacts],
    homes: &[ScanHome],
    now: TimestampMs,
) -> Vec<Suggestion> {
    let with_cwd: Vec<&SessionFacts> = facts
        .iter()
        .filter(|f| !f.is_subagent && f.cwd.is_some())
        .collect();

    let mut distinct_cwds: Vec<PathBuf> = with_cwd
        .iter()
        .filter_map(|f| f.cwd.as_deref())
        .map(PathBuf::from)
        .collect();
    distinct_cwds.sort();
    distinct_cwds.dedup();
    let roots = resolve_roots(&distinct_cwds);

    let mut by_root: HashMap<PathBuf, Vec<&SessionFacts>> = HashMap::new();
    let mut is_git_root: HashMap<&PathBuf, bool> = HashMap::new();
    for f in &with_cwd {
        let Some(cwd) = f.cwd.as_deref().map(Path::new) else {
            continue;
        };
        let Some((root, is_git)) = roots.get(cwd) else {
            continue;
        };
        if is_excluded_root(root, homes) {
            continue;
        }
        is_git_root.entry(root).or_insert(*is_git);
        by_root.entry(root.clone()).or_default().push(f);
    }

    let mut suggestions: Vec<Suggestion> = by_root
        .into_iter()
        .map(|(root, sessions)| {
            let (recent_30d, recent_90d) = recency_counts(&sessions, now);
            Suggestion {
                id: root.to_string_lossy().into_owned(),
                name: root.file_name().map_or_else(
                    || root.to_string_lossy().into_owned(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                path: root.to_string_lossy().into_owned(),
                is_git: is_git_root.get(&root).copied().unwrap_or(false),
                session_count: sessions.len(),
                recent_30d,
                recent_90d,
                workstreams: build_workstreams(&root, &sessions, now),
            }
        })
        .collect();

    suggestions.sort_by(|a, b| {
        b.recent_30d
            .cmp(&a.recent_30d)
            .then_with(|| b.recent_90d.cmp(&a.recent_90d))
            .then_with(|| b.session_count.cmp(&a.session_count))
            .then_with(|| a.path.cmp(&b.path))
    });
    suggestions
}

/// Workstreams inside one project root: an active sub-folder (its first path segment under the
/// root) or a non-default branch. A session can count toward one of each.
fn build_workstreams(
    root: &Path,
    sessions: &[&SessionFacts],
    now: TimestampMs,
) -> Vec<WorkstreamSuggestion> {
    let mut by_folder: HashMap<String, Vec<&SessionFacts>> = HashMap::new();
    let mut by_branch: HashMap<String, Vec<&SessionFacts>> = HashMap::new();
    for &f in sessions {
        if let Some(cwd) = f.cwd.as_deref().map(Path::new)
            && let Ok(rel) = cwd.strip_prefix(root)
            && let Some(first) = rel.components().next()
        {
            by_folder
                .entry(first.as_os_str().to_string_lossy().into_owned())
                .or_default()
                .push(f);
        }
        if let Some(branch) = f.branch.as_deref() {
            let trimmed = branch.trim();
            if !trimmed.is_empty()
                && !DEFAULT_BRANCHES.contains(&trimmed.to_ascii_lowercase().as_str())
            {
                by_branch.entry(trimmed.to_owned()).or_default().push(f);
            }
        }
    }

    let root_label = root.to_string_lossy().into_owned();
    let mut out: Vec<WorkstreamSuggestion> = by_folder
        .into_iter()
        .map(|(folder, sess)| {
            let (recent_30d, recent_90d) = recency_counts(&sess, now);
            WorkstreamSuggestion {
                id: format!("{root_label}/{folder}"),
                name: folder,
                branch: None,
                session_count: sess.len(),
                recent_30d,
                recent_90d,
            }
        })
        .collect();
    out.extend(by_branch.into_iter().map(|(branch, sess)| {
        let (recent_30d, recent_90d) = recency_counts(&sess, now);
        WorkstreamSuggestion {
            id: format!("{root_label}#{branch}"),
            name: branch.clone(),
            branch: Some(branch),
            session_count: sess.len(),
            recent_30d,
            recent_90d,
        }
    }));

    out.sort_by(|a, b| {
        b.recent_30d
            .cmp(&a.recent_30d)
            .then_with(|| b.recent_90d.cmp(&a.recent_90d))
            .then_with(|| b.session_count.cmp(&a.session_count))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(cwd: &str, branch: Option<&str>, last_activity: TimestampMs) -> SessionFacts {
        SessionFacts {
            engine: Engine::Claude,
            home: PathBuf::from("/home/u/.claude"),
            cwd: Some(cwd.to_owned()),
            branch: branch.map(str::to_owned),
            started: Some(last_activity),
            last_activity,
            is_subagent: false,
        }
    }

    #[test]
    fn git_root_found_by_climbing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("proj");
        let sub = repo.join("crates").join("a");
        fs::create_dir_all(&sub).expect("mkdir");
        fs::create_dir(repo.join(".git")).expect("git dir");
        assert_eq!(git_root(&sub), Some(repo.clone()));
        assert_eq!(git_root(&repo), Some(repo));
        assert_eq!(git_root(dir.path()), None);
    }

    #[test]
    fn non_git_siblings_group_under_their_parent_but_a_lone_one_does_not() {
        let cwds = vec![
            PathBuf::from("/w/notes/a"),
            PathBuf::from("/w/notes/b"),
            PathBuf::from("/w/alone"),
        ];
        let roots = resolve_roots(&cwds);
        assert_eq!(
            roots[&PathBuf::from("/w/notes/a")],
            (PathBuf::from("/w/notes"), false)
        );
        assert_eq!(
            roots[&PathBuf::from("/w/notes/b")],
            (PathBuf::from("/w/notes"), false)
        );
        assert_eq!(
            roots[&PathBuf::from("/w/alone")],
            (PathBuf::from("/w/alone"), false)
        );
    }

    #[test]
    fn home_and_system_paths_are_excluded() {
        let homes = [ScanHome {
            engine: Engine::Claude,
            home: PathBuf::from("/home/u/.claude"),
        }];
        assert!(is_excluded_root(Path::new("/"), &homes));
        assert!(is_excluded_root(Path::new("/usr"), &homes));
        assert!(is_excluded_root(Path::new("/home/u/.claude"), &homes));
        assert!(!is_excluded_root(Path::new("/home/u/code/proj"), &homes));
        if let Some(home) = crate::user_home() {
            assert!(is_excluded_root(&home, &homes));
        }
    }

    #[test]
    fn workstreams_come_from_subfolders_and_non_default_branches() {
        let sessions = [
            facts("/w/proj", Some("main"), 1000),
            facts("/w/proj/apps/web", Some("main"), 1000),
            facts("/w/proj/apps/web", Some("feat/x"), 1000),
            facts("/w/proj", Some("feat/x"), 1000),
        ];
        let refs: Vec<&SessionFacts> = sessions.iter().collect();
        let ws = build_workstreams(Path::new("/w/proj"), &refs, 10_000);
        let folder = ws
            .iter()
            .find(|w| w.name == "apps")
            .expect("folder workstream");
        assert_eq!(folder.session_count, 2);
        assert!(folder.branch.is_none());
        let branch = ws
            .iter()
            .find(|w| w.branch.as_deref() == Some("feat/x"))
            .expect("branch workstream");
        assert_eq!(branch.session_count, 2);
        assert!(
            !ws.iter().any(|w| w.branch.as_deref() == Some("main")),
            "default branch is not a workstream"
        );
    }

    #[test]
    fn recency_counts_respect_the_window_and_never_look_into_the_future() {
        let now = 100 * DAY_MS;
        let sessions = [
            facts("/w/p", None, now),                // today
            facts("/w/p", None, now - 29 * DAY_MS),  // within 30d
            facts("/w/p", None, now - 60 * DAY_MS),  // within 90d only
            facts("/w/p", None, now - 200 * DAY_MS), // old
            facts("/w/p", None, now + DAY_MS),       // clock skew: not "recent"
        ];
        let refs: Vec<&SessionFacts> = sessions.iter().collect();
        assert_eq!(recency_counts(&refs, now), (2, 3));
    }

    #[test]
    fn engine_rank_orders_known_engines_before_future_ones() {
        assert!(engine_rank(Engine::Claude) < engine_rank(Engine::Codex));
        assert!(engine_rank(Engine::Codex) < engine_rank(Engine::OpenCode));
    }
}
