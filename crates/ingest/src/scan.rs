//! The machine scan: counts and suggested projects/workstreams, for onboarding and "scan again".
//!
//! [`scan`] discovers every engine's transcripts under the given [`ScanHome`]s (each adapter's
//! own `discover`), then reads each one's **light** session facts: a bounded prefix of a JSONL
//! transcript, or one indexed row of an OpenCode store. It never reads a full transcript, and
//! never copies prompt text — only paths, branches and counts (a title is not collected here).
//!
//! Discovery is sequential (each adapter's `discover` is already a bounded directory walk that
//! does not follow symlinks); the light reads run on a bounded pool of threads, claimed
//! dynamically from a shared queue so one huge OpenCode store does not stall the others. A panic
//! while reading one unit is caught and counted as unreadable rather than losing the rest of its
//! thread's results. `progress` always runs on the caller's own thread, at most every 100 ms.
//!
//! Because the light reads only look at a transcript's first bytes, a `cwd` or `branch` reflects
//! the session's **start**, not necessarily a later change (the Claude adapter's own cursor-based
//! `branch` is "latest seen"; here it is "first seen"). That is an accepted trade-off for a fast,
//! bounded scan; a full import (stream D) sees the final value.
//!
//! Grouping and exclusion (projects, workstreams, the home/system-folder guard) compare paths by
//! [`cmp_key`], which case-folds on a case-insensitive filesystem (Windows, and macOS's default):
//! two spellings of the same real folder are one project, never two, and the home directory is
//! excluded whatever case it is spelled in. Every grouped path still keeps one of its original
//! spellings for display.

use crate::claude::{self, ClaudeAdapter};
use crate::codex::{self, CodexAdapter};
use crate::opencode::{self, OpenCodeAdapter};
use pitcrew_interfaces::source::{Lineage, SourceAdapter, SourceError, TranscriptRef};
use pitcrew_protocol::model::{Engine, TimestampMs};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::panic::UnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// However many threads a caller asks for, this many worker threads at most.
const MAX_THREADS: usize = 64;
const DAY_MS: i64 = 86_400_000;

/// Whether this machine's filesystem compares paths case-insensitively: Windows, and macOS's
/// default APFS/HFS+ (both case-preserving but case-insensitive). Linux's common filesystems are
/// case-sensitive, so two differently-cased cwds there really are different folders.
const CASE_INSENSITIVE_PATHS: bool = cfg!(any(windows, target_os = "macos"));

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
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Now, for ranking by recent activity (sessions in the last 30 and 90 days). Tests pass a
    /// fixed time so rankings are deterministic; [`ScanOptions::default`] uses the real clock.
    pub now: TimestampMs,
    /// Worker threads for the light reads; `None` picks the machine's available parallelism.
    /// Capped at [`MAX_THREADS`] either way.
    pub threads: Option<usize>,
    /// Set to stop scheduling work between homes, files and suggestion directories.
    pub cancel: Arc<AtomicBool>,
    /// Maximum time for the scan, including suggestions; in-flight filesystem operations must finish.
    pub budget: Duration,
}

impl Default for ScanOptions {
    /// `now` is the real clock, not the epoch: a caller that does not set it still gets correct
    /// recency ranking, rather than every session silently looking infinitely old (`now = 0`
    /// would make every real timestamp come out *after* "now", so nothing is ever "recent").
    fn default() -> Self {
        Self {
            now: now_ms(),
            threads: None,
            cancel: Arc::new(AtomicBool::new(false)),
            budget: Duration::from_secs(600),
        }
    }
}

/// The current time in Unix milliseconds, for [`ScanOptions::default`].
fn now_ms() -> TimestampMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| TimestampMs::try_from(d.as_millis()).unwrap_or(TimestampMs::MAX))
        .unwrap_or(0)
}

// ─── Output ──────────────────────────────────────────────────────────────────────────────────

// The scan's results are wire types (`POST /v1/machines/{id}/scan`), so they live in the protocol;
// they are re-exported here for this crate's callers.
pub use pitcrew_protocol::scan::{
    EngineCount, FolderCount, HomeCount, MonthCount, ScanCounts, ScanProgress, ScanReport,
    Suggestion, WorkstreamSuggestion, WorkstreamSuggestionKind,
};

// ─── The scan ────────────────────────────────────────────────────────────────────────────────

/// A read-only scan of a machine's agent history: discovers every engine's transcripts under
/// `homes`, reads each one's light session facts, and returns counts plus suggested projects and
/// workstreams. `progress` is called on the caller's own thread only, at most every 100 ms, and
/// at least once at the end; a partial scan may have `scanned < total`.
pub fn scan(
    homes: &[ScanHome],
    options: &ScanOptions,
    progress: impl FnMut(ScanProgress),
) -> ScanReport {
    scan_with_resolver(homes, options, progress, git_root)
}

fn scan_with_resolver(
    homes: &[ScanHome],
    options: &ScanOptions,
    mut progress: impl FnMut(ScanProgress),
    mut resolver: impl FnMut(&Path, &dyn Fn() -> bool) -> Option<Repo>,
) -> ScanReport {
    let started = Instant::now();
    let stopped = || options.cancel.load(Ordering::Relaxed) || started.elapsed() >= options.budget;
    let mut units: Vec<Unit> = Vec::new();
    let mut unreadable = 0u64;
    for home in homes {
        if stopped() {
            break;
        }
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
    let threads = options
        .threads
        .unwrap_or_else(default_parallelism)
        .clamp(1, MAX_THREADS);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    // Carries which unit (by index) a worker just finished, so the main thread can build its
    // label lazily -- only once, right before a progress tick actually needs it -- instead of
    // every worker formatting a path string that may just be dropped because the channel is full.
    let (tx, rx) = mpsc::sync_channel::<usize>(64);

    let facts: Vec<Option<SessionFacts>> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let next = &next;
            let done = &done;
            let units = &units;
            let tx = tx.clone();
            let stopped = &stopped;
            handles.push(scope.spawn(move || {
                let mut out = Vec::new();
                loop {
                    if stopped() {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(unit) = units.get(i) else {
                        break;
                    };
                    let results = run_unit(unit);
                    done.fetch_add(results.len(), Ordering::Relaxed);
                    let _ = tx.try_send(i);
                    out.extend(results);
                }
                out
            }));
        }
        drop(tx);

        let mut last_emit = Instant::now();
        let mut last_index: Option<usize> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(i) => last_index = Some(i),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if last_emit.elapsed() >= Duration::from_millis(100) {
                progress(ScanProgress {
                    scanned: done.load(Ordering::Relaxed),
                    total: Some(total),
                    path: last_index.take().map(|i| units[i].label()),
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
    settle_subagents(&mut session_facts);

    let counts = aggregate_counts(&session_facts);
    let suggestions = build_suggestions(
        &session_facts,
        homes,
        options.now,
        CASE_INSENSITIVE_PATHS,
        &stopped,
        &mut resolver,
    );
    ScanReport {
        partial: stopped().then_some(true),
        counts,
        suggestions,
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

/// Runs one unit, turning a panic into `unit.len()` `None`s (counted as unreadable by the
/// caller) instead of losing everything else its worker thread already collected. No adapter is
/// supposed to panic on untrusted transcript bytes, but transcripts are attacker-controllable
/// text, so one bad file must not erase a thread's other results.
fn run_unit(unit: &Unit) -> Vec<Option<SessionFacts>> {
    run_catching_panics(unit.len(), || unit.run())
}

/// The mechanism behind [`run_unit`], generic so it is testable with a closure that panics on
/// purpose rather than needing a real adapter bug.
fn run_catching_panics(
    len: usize,
    f: impl FnOnce() -> Vec<Option<SessionFacts>> + UnwindSafe,
) -> Vec<Option<SessionFacts>> {
    std::panic::catch_unwind(f).unwrap_or_else(|_| {
        tracing::warn!(
            count = len,
            reason = "worker panicked",
            "scan sessions counted as unreadable"
        );
        vec![None; len]
    })
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
    /// A sub-agent whose parent the scan found (see [`settle_subagents`]): nested under it, so
    /// counted apart and left out of suggestions. Before settling, any sub-agent.
    is_subagent: bool,
    /// The CLI's own id for the session, as its adapter names it.
    native_id: Option<String>,
    /// For a sub-agent, its parent's own id, where the transcript names it.
    parent: Option<String>,
}

/// A sub-agent stays one only if its parent was scanned too, in the same home: the hub nests a
/// sub-agent under its parent only when it finds the parent, and shows any other as a session of
/// its own, so the scan counts it as one.
fn settle_subagents(facts: &mut [SessionFacts]) {
    let known: HashSet<(Engine, &Path, &str)> = facts
        .iter()
        .filter_map(|f| Some((f.engine, f.home.as_path(), f.native_id.as_deref()?)))
        .collect();
    let nested: Vec<bool> = facts
        .iter()
        .map(|f| {
            f.is_subagent
                && f.parent
                    .as_deref()
                    .is_some_and(|p| known.contains(&(f.engine, f.home.as_path(), p)))
        })
        .collect();
    for (f, nested) in facts.iter_mut().zip(nested) {
        f.is_subagent = nested;
    }
}

fn claude_light(home: &ScanHome, t: &TranscriptRef) -> Option<SessionFacts> {
    let data = read_prefix(&t.path, SCAN_PREFIX_BYTES)?;
    let mut cwd = None;
    let mut branch = None;
    let mut started = None;
    let mut sidechain = None;
    let mut session_id = None;
    let mut agent_id = None;
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
            if session_id.is_none() {
                session_id = f.session_id;
            }
            if agent_id.is_none() {
                agent_id = f.agent_id;
            }
        }
    }
    // Matches `ClaudeAdapter`'s own rules: a transcript under a `subagents` folder is a sub-agent
    // session even on CLI versions that do not also set `isSidechain`; a sub-agent is named by its
    // agent id, and the session id its records carry (or its session folder) names its parent.
    let folder = claude::subagent_session_folder(&t.path);
    let is_subagent = folder.is_some() || sidechain.unwrap_or(false);
    let stem = t.path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let (native_id, parent) = if is_subagent {
        (agent_id.or(stem), session_id.or(folder))
    } else {
        (session_id.or(stem), None)
    };
    let parent = parent.filter(|p| Some(p) != native_id.as_ref());
    Some(SessionFacts {
        engine: Engine::Claude,
        home: home.home.clone(),
        cwd,
        branch,
        started,
        last_activity: t.modified,
        is_subagent,
        native_id,
        parent,
    })
}

/// A Claude or Codex transcript's [`Lineage`], from the same head the scan reads: the
/// adapters' `SourceAdapter::lineage`. `Err` when the head cannot be read.
pub(crate) fn head_lineage(t: &TranscriptRef) -> Result<Option<Lineage>, SourceError> {
    let home = ScanHome {
        engine: t.engine,
        home: PathBuf::new(),
    };
    let facts = match t.engine {
        Engine::Claude => claude_light(&home, t),
        Engine::Codex => codex_light(&home, t),
        _ => return Ok(None),
    };
    let facts = facts.ok_or_else(|| SourceError::Unreadable {
        path: t.path.clone(),
        reason: "its head cannot be read".into(),
    })?;
    Ok(Some(Lineage {
        is_subagent: facts.is_subagent,
        parent: facts.parent.filter(|_| facts.is_subagent),
    }))
}

fn codex_light(home: &ScanHome, t: &TranscriptRef) -> Option<SessionFacts> {
    let data = read_prefix(&t.path, SCAN_PREFIX_BYTES)?;
    let mut cwd = None;
    let mut branch = None;
    let mut started = None;
    let mut subagent = None;
    let mut session_id = None;
    let mut parent = None;
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
            if session_id.is_none() {
                session_id = f.session_id;
            }
            if parent.is_none() {
                parent = f.parent;
            }
        }
    }
    let is_subagent = subagent.unwrap_or(false);
    let native_id = session_id.or_else(|| {
        t.path
            .file_stem()
            .map(|s| codex::id_from_stem(&s.to_string_lossy()).to_owned())
    });
    let parent = parent.filter(|p| is_subagent && Some(p) != native_id.as_ref());
    Some(SessionFacts {
        engine: Engine::Codex,
        home: home.home.clone(),
        cwd,
        branch,
        started,
        last_activity: t.modified,
        is_subagent,
        native_id,
        parent,
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
            let id = r.inner_id.as_deref()?;
            let m = metas.get(id)?;
            Some(SessionFacts {
                engine: Engine::OpenCode,
                home: home.home.clone(),
                cwd: m.cwd.clone(),
                branch: None,
                started: m.started,
                last_activity: r.modified,
                is_subagent: m.is_subagent,
                native_id: Some(id.to_owned()),
                parent: m.parent.clone(),
            })
        })
        .collect()
}

/// The first `max` bytes of `path`; `None` if it cannot be opened, or is no longer a regular file
/// (see [`crate::open`]).
fn read_prefix(path: &Path, max: u64) -> Option<Vec<u8>> {
    let file = crate::open::open_transcript(path).ok()?;
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
    // Keyed by cmp_key so two spellings of one real folder count together; the value keeps one
    // original spelling (the first seen) for display.
    let mut by_folder: HashMap<String, (String, usize)> = HashMap::new();
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
            let key = cmp_key(Path::new(cwd), CASE_INSENSITIVE_PATHS);
            let entry = by_folder.entry(key).or_insert_with(|| (cwd.clone(), 0));
            entry.1 += 1;
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
        .into_values()
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

// ─── Comparison keys (grouping and exclusion, never display) ───────────────────────────────────

/// A path's comparison key, never for display: separators normalised to `/`, and case-folded
/// when `case_insensitive`. Real call sites pass [`CASE_INSENSITIVE_PATHS`]; tests pass an
/// explicit value, so Windows-style grouping and exclusion are checked on any host, not only one
/// actually running on a case-insensitive filesystem.
fn cmp_key(path: &Path, case_insensitive: bool) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if case_insensitive {
        s.to_ascii_lowercase()
    } else {
        s
    }
}

/// The first path component of `cwd` below `root`, keeping its original spelling, found by
/// comparing components with `case_insensitive` folding rather than a literal `strip_prefix` (a
/// session's cwd need not match the suggestion's canonical root spelling exactly). `None` if
/// `cwd` is not under `root`.
fn first_segment_below(cwd: &Path, root: &Path, case_insensitive: bool) -> Option<String> {
    let mut cwd_parts = cwd.components();
    for root_part in root.components() {
        let cwd_part = cwd_parts.next()?;
        let same = if case_insensitive {
            cwd_part
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&root_part.as_os_str().to_string_lossy())
        } else {
            cwd_part == root_part
        };
        if !same {
            return None;
        }
    }
    cwd_parts
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
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

/// How much of a `.git` file, a `commondir` or a `HEAD` the scan reads: each holds one line.
const GIT_FILE_BYTES: u64 = 4096;

/// Where a folder is in git, as [`git_root`] found it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Repo {
    /// The repository's main worktree: the folder holding its `.git` (for a bare repository,
    /// the repository's own folder).
    root: PathBuf,
    /// The linked worktree the folder is in, when it is not in the main one.
    worktree: Option<PathBuf>,
    /// The branch checked out in the main worktree, when its `HEAD` names one.
    root_branch: Option<String>,
    /// The branch checked out in the linked worktree, when its `HEAD` names one.
    worktree_branch: Option<String>,
}

impl Repo {
    /// A repository rooted at `root`, with nothing more known about it.
    #[cfg(test)]
    fn at(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            worktree: None,
            root_branch: None,
            worktree_branch: None,
        }
    }
}

/// Climbs from `cwd` to the nearest ancestor containing a `.git` (directory or file). Uses
/// `symlink_metadata` so a `.git` that is itself a symlink is not followed, matching how the
/// adapters treat symlinks elsewhere in this crate.
///
/// A `.git` **file** whose `gitdir:` names a folder with a `commondir` is a linked worktree
/// (`git worktree add`, or a CLI's own under `.claude/worktrees/`): its repository's root is the
/// main worktree, the folder holding the common `.git`. A `.git` file without a `commondir` (a
/// submodule, a `--separate-git-dir` checkout) is a repository of its own.
fn git_root(cwd: &Path, stopped: &dyn Fn() -> bool) -> Option<Repo> {
    for ancestor in cwd.ancestors() {
        if stopped() {
            break;
        }
        let dot_git = ancestor.join(".git");
        let Ok(meta) = fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.is_file()
            && let Some(gitdir) = read_gitdir(ancestor, &dot_git)
        {
            if let Some(common) = common_dir(&gitdir) {
                let root = if common.file_name() == Some(OsStr::new(".git")) {
                    common
                        .parent()
                        .map_or_else(|| common.clone(), Path::to_path_buf)
                } else {
                    common.clone()
                };
                return Some(Repo {
                    root,
                    worktree: Some(ancestor.to_path_buf()),
                    root_branch: head_branch(&common),
                    worktree_branch: head_branch(&gitdir),
                });
            }
            return Some(Repo {
                root: ancestor.to_path_buf(),
                worktree: None,
                root_branch: head_branch(&gitdir),
                worktree_branch: None,
            });
        }
        let root_branch = meta.is_dir().then(|| head_branch(&dot_git)).flatten();
        return Some(Repo {
            root: ancestor.to_path_buf(),
            worktree: None,
            root_branch,
            worktree_branch: None,
        });
    }
    None
}

/// The first line of a small git file (`.git`, `commondir`, `HEAD`), trimmed; `None` if it is not
/// a regular file (a link is not followed) or not UTF-8.
fn git_line(path: &Path) -> Option<String> {
    let data = read_prefix(path, GIT_FILE_BYTES)?;
    let text = std::str::from_utf8(&data).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// The folder a `.git` file's `gitdir:` names, resolved against the worktree holding it.
fn read_gitdir(worktree: &Path, dot_git: &Path) -> Option<PathBuf> {
    let line = git_line(dot_git)?;
    let target = line.strip_prefix("gitdir:")?.trim();
    (!target.is_empty()).then(|| resolve_git_path(worktree, target))
}

/// A linked worktree's common folder (the repository's `.git`), from its git folder's
/// `commondir`; `None` when there is none (the git folder is a whole repository's).
fn common_dir(gitdir: &Path) -> Option<PathBuf> {
    let line = git_line(&gitdir.join("commondir"))?;
    Some(resolve_git_path(gitdir, &line))
}

/// The branch a git folder's `HEAD` names (`ref: refs/heads/<branch>`); `None` when it is
/// detached, unreadable or names something else.
fn head_branch(gitdir: &Path) -> Option<String> {
    let line = git_line(&gitdir.join("HEAD"))?;
    let branch = line
        .strip_prefix("ref:")?
        .trim()
        .strip_prefix("refs/heads/")?;
    (!branch.is_empty()).then(|| branch.to_owned())
}

/// A path a git file names: absolute as it is, or relative to `base`; `.` and `..` resolved
/// lexically (nothing is followed). Git writes `/` on every platform; a Windows path keeps its
/// drive (`C:/Users/sam/repo/.git`), which `Path` reads as absolute there.
fn resolve_git_path(base: &Path, named: &str) -> PathBuf {
    let named = Path::new(named);
    let joined = if named.is_absolute() || named.has_root() {
        named.to_path_buf()
    } else {
        base.join(named)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push(part);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// What [`resolve_roots`] found for one cwd.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Resolved {
    /// The project root.
    root: PathBuf,
    /// Whether a `.git` was found at or above it.
    is_git: bool,
    /// The linked worktree the cwd is in, and the branch checked out there.
    worktree: Option<(PathBuf, Option<String>)>,
    /// The branch checked out in the project's main checkout.
    root_branch: Option<String>,
}

/// Resolves every distinct cwd to a project root and whether it is a git root, grouping by
/// [`cmp_key`] so two spellings of the same folder (a different drive-letter case, or `\` vs `/`)
/// are not treated as different roots. The returned root keeps one of the original spellings, not
/// a normalised one: a root found by climbing a cwd keeps that cwd's spelling, and a linked
/// worktree's (spelled as git wrote its `gitdir:`) takes it when they are the same folder. Keyed
/// by `cmp_key(cwd)`.
fn resolve_roots(
    cwds: &[PathBuf],
    case_insensitive: bool,
    stopped: &dyn Fn() -> bool,
    resolver: &mut impl FnMut(&Path, &dyn Fn() -> bool) -> Option<Repo>,
) -> HashMap<String, Resolved> {
    let mut out: HashMap<String, Resolved> = HashMap::with_capacity(cwds.len());
    let mut non_git: Vec<&PathBuf> = Vec::new();
    for cwd in cwds {
        if stopped() {
            break;
        }
        match resolver(cwd, stopped) {
            Some(repo) => {
                out.insert(
                    cmp_key(cwd, case_insensitive),
                    Resolved {
                        root: repo.root,
                        is_git: true,
                        worktree: repo.worktree.map(|w| (w, repo.worktree_branch)),
                        root_branch: repo.root_branch,
                    },
                );
            }
            None if stopped() => break,
            None => non_git.push(cwd),
        }
    }
    // One spelling per git root: a root found by climbing a cwd is spelled as that cwd was.
    let mut spelling: HashMap<String, PathBuf> = HashMap::new();
    for r in out.values().filter(|r| r.worktree.is_none()) {
        spelling
            .entry(cmp_key(&r.root, case_insensitive))
            .or_insert_with(|| r.root.clone());
    }
    for r in out.values_mut() {
        if let Some(root) = spelling.get(&cmp_key(&r.root, case_insensitive)) {
            r.root.clone_from(root);
        }
    }
    // Count non-git siblings by their parent's key, keeping one spelling of the parent.
    let mut by_parent_key: HashMap<String, (PathBuf, usize)> = HashMap::new();
    for cwd in &non_git {
        if let Some(parent) = cwd.parent() {
            let key = cmp_key(parent, case_insensitive);
            let entry = by_parent_key
                .entry(key)
                .or_insert_with(|| (parent.to_path_buf(), 0));
            entry.1 += 1;
        }
    }
    for cwd in non_git {
        let root = match cwd.parent() {
            Some(parent) => {
                let key = cmp_key(parent, case_insensitive);
                match by_parent_key.get(&key) {
                    Some((canonical, n)) if *n >= MIN_GROUPED_SIBLINGS => canonical.clone(),
                    _ => cwd.clone(),
                }
            }
            None => cwd.clone(),
        };
        out.insert(
            cmp_key(cwd, case_insensitive),
            Resolved {
                root,
                is_git: false,
                worktree: None,
                root_branch: None,
            },
        );
    }
    out
}

/// A root never worth suggesting: the user's home, a scanned engine home, a filesystem/drive
/// root, or a well-known OS folder directly below one. The home and scanned-home checks compare
/// by [`cmp_key`], so a differently-cased spelling of the same real folder is still excluded.
fn is_excluded_root(path: &Path, homes: &[ScanHome], case_insensitive: bool) -> bool {
    let key = cmp_key(path, case_insensitive);
    if crate::user_home().is_some_and(|home| cmp_key(&home, case_insensitive) == key) {
        return true;
    }
    if homes
        .iter()
        .any(|h| cmp_key(&h.home, case_insensitive) == key)
    {
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
    case_insensitive: bool,
    stopped: &dyn Fn() -> bool,
    resolver: &mut impl FnMut(&Path, &dyn Fn() -> bool) -> Option<Repo>,
) -> Vec<Suggestion> {
    if stopped() {
        return Vec::new();
    }
    let with_cwd: Vec<&SessionFacts> = facts
        .iter()
        .filter(|f| !f.is_subagent && f.cwd.is_some())
        .collect();

    // Distinct cwds, deduped by comparison key so grouping sees one spelling per real folder.
    let mut distinct: HashMap<String, PathBuf> = HashMap::new();
    for f in &with_cwd {
        if let Some(cwd) = f.cwd.as_deref() {
            let p = PathBuf::from(cwd);
            distinct.entry(cmp_key(&p, case_insensitive)).or_insert(p);
        }
    }
    let mut distinct_cwds: Vec<PathBuf> = distinct.into_values().collect();
    distinct_cwds.sort();
    let roots = resolve_roots(&distinct_cwds, case_insensitive, stopped, resolver);

    let mut by_root: HashMap<String, Project<'_>> = HashMap::new();
    for f in &with_cwd {
        let Some(cwd) = f.cwd.as_deref().map(Path::new) else {
            continue;
        };
        let Some(resolved) = roots.get(&cmp_key(cwd, case_insensitive)) else {
            continue;
        };
        if is_excluded_root(&resolved.root, homes, case_insensitive) {
            continue;
        }
        let key = cmp_key(&resolved.root, case_insensitive);
        let project = by_root.entry(key).or_insert_with(|| Project {
            root: resolved.root.clone(),
            is_git: resolved.is_git,
            root_branch: None,
            sessions: Vec::new(),
        });
        if project.root_branch.is_none() {
            project.root_branch.clone_from(&resolved.root_branch);
        }
        project.sessions.push(Placed {
            facts: f,
            worktree: resolved.worktree.as_ref(),
        });
    }

    let mut suggestions: Vec<Suggestion> = by_root
        .into_values()
        .map(|project| {
            let all: Vec<&SessionFacts> = project.sessions.iter().map(|p| p.facts).collect();
            let (recent_30d, recent_90d) = recency_counts(&all, now);
            let root = &project.root;
            Suggestion {
                id: root.to_string_lossy().into_owned(),
                name: folder_name(root),
                path: root.to_string_lossy().into_owned(),
                is_git: project.is_git,
                session_count: all.len(),
                recent_30d,
                recent_90d,
                workstreams: build_workstreams(&project, now, case_insensitive),
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

/// One suggested project while it is being built: its root and its sessions, each with the linked
/// worktree it is in, if any.
struct Project<'a> {
    root: PathBuf,
    is_git: bool,
    /// The branch checked out in the main checkout, as its `HEAD` says.
    root_branch: Option<String>,
    sessions: Vec<Placed<'a>>,
}

/// A session, and the linked worktree (with the branch checked out there) it is in, if any.
struct Placed<'a> {
    facts: &'a SessionFacts,
    worktree: Option<&'a (PathBuf, Option<String>)>,
}

/// A folder's own name, or the whole path for a root.
fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The branch most of `sessions` started on (ties: the alphabetically first), if any did. With
/// `default_first`, a default branch (`main`, `master`, …) wins over any other, as the main
/// checkout's usual one.
fn usual_branch(sessions: &[&SessionFacts], default_first: bool) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in sessions {
        if let Some(b) = f.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            *counts.entry(b).or_default() += 1;
        }
    }
    let is_default = |b: &str| DEFAULT_BRANCHES.contains(&b.to_ascii_lowercase().as_str());
    counts
        .into_iter()
        .max_by(|a, b| {
            (default_first && is_default(a.0))
                .cmp(&(default_first && is_default(b.0)))
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| b.0.cmp(a.0))
        })
        .map(|(b, _)| b.to_owned())
}

/// The workstreams of one project root:
/// - **the default one** (`main`), first, at the root, for the main checkout's sessions: named
///   after the branch checked out there (its `HEAD`, else the branch its sessions mostly started
///   on, else the folder), or `Main` for a folder without git;
/// - **one per linked worktree** with sessions, at its folder, named after its branch the same way;
/// - an active **sub-folder** of the main checkout (its first path segment under the root), and a
///   non-default **branch** of the main checkout. A main-checkout session can count toward one of
///   each; a worktree's sessions count toward the worktree only.
///
/// Sub-folder and worktree grouping use [`cmp_key`]-style case-folding when `case_insensitive`;
/// branch grouping never does, since git branch names are case-sensitive on every platform.
fn build_workstreams(
    project: &Project<'_>,
    now: TimestampMs,
    case_insensitive: bool,
) -> Vec<WorkstreamSuggestion> {
    let root = project.root.as_path();
    let main: Vec<&SessionFacts> = project
        .sessions
        .iter()
        .filter(|p| p.worktree.is_none())
        .map(|p| p.facts)
        .collect();
    // Keyed by the folded worktree path; the value keeps the first-seen spelling.
    let mut by_worktree: HashMap<String, (&PathBuf, Option<&str>, Vec<&SessionFacts>)> =
        HashMap::new();
    for p in &project.sessions {
        if let Some((path, branch)) = p.worktree {
            let entry = by_worktree
                .entry(cmp_key(path, case_insensitive))
                .or_insert_with(|| (path, branch.as_deref(), Vec::new()));
            entry.2.push(p.facts);
        }
    }

    let main_name = if project.is_git {
        project
            .root_branch
            .clone()
            .or_else(|| usual_branch(&main, true))
            .unwrap_or_else(|| folder_name(root))
    } else {
        "Main".to_owned()
    };
    let (recent_30d, recent_90d) = recency_counts(&main, now);
    let default = WorkstreamSuggestion {
        id: root.to_string_lossy().into_owned(),
        kind: WorkstreamSuggestionKind::Main,
        name: main_name.clone(),
        branch: None,
        session_count: main.len(),
        recent_30d,
        recent_90d,
    };

    let mut out: Vec<WorkstreamSuggestion> = by_worktree
        .into_values()
        .map(|(path, branch, sess)| {
            let (recent_30d, recent_90d) = recency_counts(&sess, now);
            WorkstreamSuggestion {
                id: path.to_string_lossy().into_owned(),
                kind: WorkstreamSuggestionKind::Worktree,
                name: branch
                    .map(str::to_owned)
                    .or_else(|| usual_branch(&sess, false))
                    .unwrap_or_else(|| folder_name(path)),
                branch: None,
                session_count: sess.len(),
                recent_30d,
                recent_90d,
            }
        })
        .collect();

    // Keyed by the folded folder name; the value keeps the first-seen original spelling.
    let mut by_folder: HashMap<String, (String, Vec<&SessionFacts>)> = HashMap::new();
    let mut by_branch: HashMap<String, Vec<&SessionFacts>> = HashMap::new();
    for &f in &main {
        if let Some(cwd) = f.cwd.as_deref().map(Path::new)
            && let Some(seg) = first_segment_below(cwd, root, case_insensitive)
        {
            let key = if case_insensitive {
                seg.to_ascii_lowercase()
            } else {
                seg.clone()
            };
            let entry = by_folder.entry(key).or_insert_with(|| (seg, Vec::new()));
            entry.1.push(f);
        }
        if let Some(branch) = f.branch.as_deref() {
            let trimmed = branch.trim();
            if !trimmed.is_empty()
                && !DEFAULT_BRANCHES.contains(&trimmed.to_ascii_lowercase().as_str())
                && !(project.is_git && trimmed == main_name)
            {
                by_branch.entry(trimmed.to_owned()).or_default().push(f);
            }
        }
    }

    let root_label = root.to_string_lossy().into_owned();
    out.extend(by_folder.into_values().map(|(folder, sess)| {
        let (recent_30d, recent_90d) = recency_counts(&sess, now);
        WorkstreamSuggestion {
            // The folder's own path, with this platform's separator.
            id: root.join(&folder).to_string_lossy().into_owned(),
            kind: WorkstreamSuggestionKind::Folder,
            name: folder,
            branch: None,
            session_count: sess.len(),
            recent_30d,
            recent_90d,
        }
    }));
    out.extend(by_branch.into_iter().map(|(branch, sess)| {
        let (recent_30d, recent_90d) = recency_counts(&sess, now);
        WorkstreamSuggestion {
            id: format!("{root_label}#{branch}"),
            kind: WorkstreamSuggestionKind::Branch,
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
    out.insert(0, default);
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
            native_id: None,
            parent: None,
        }
    }

    /// A project at `root` (git) holding `sessions`, all in its main checkout.
    fn project<'a>(root: &str, sessions: &'a [&'a SessionFacts]) -> Project<'a> {
        Project {
            root: PathBuf::from(root),
            is_git: true,
            root_branch: None,
            sessions: sessions
                .iter()
                .map(|f| Placed {
                    facts: f,
                    worktree: None,
                })
                .collect(),
        }
    }

    #[test]
    fn git_root_found_by_climbing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("proj");
        let sub = repo.join("crates").join("a");
        fs::create_dir_all(&sub).expect("mkdir");
        fs::create_dir(repo.join(".git")).expect("git dir");
        fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/trunk\n").expect("head");
        let found = git_root(&sub, &|| false).expect("a repository");
        assert_eq!(found.root, repo);
        assert_eq!(found.worktree, None);
        assert_eq!(found.root_branch.as_deref(), Some("trunk"));
        assert_eq!(git_root(&repo, &|| false).map(|r| r.root), Some(repo));
        assert_eq!(git_root(dir.path(), &|| false), None);
    }

    /// Lays out `<root>/repo` with a linked worktree at `worktree`, as `git worktree add` does:
    /// the worktree's `.git` file names `repo/.git/worktrees/<name>`, whose `commondir` is `../..`.
    fn linked_worktree(root: &Path, worktree: &Path, name: &str, branch: &str) -> PathBuf {
        let repo = root.join("repo");
        let git = repo.join(".git");
        let admin = git.join("worktrees").join(name);
        fs::create_dir_all(&admin).expect("mkdir");
        fs::create_dir_all(worktree).expect("mkdir");
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("head");
        fs::write(admin.join("HEAD"), format!("ref: refs/heads/{branch}\n")).expect("head");
        fs::write(admin.join("commondir"), "../..\n").expect("commondir");
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.to_str().expect("utf-8")),
        )
        .expect("dot git");
        repo
    }

    #[test]
    fn a_linked_worktree_belongs_to_its_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beside = dir.path().join("repo-fix");
        let repo = linked_worktree(dir.path(), &beside, "repo-fix", "fix/typos");
        let found = git_root(&beside.join("src"), &|| false).expect("a repository");
        assert_eq!(found.root, repo);
        assert_eq!(found.worktree.as_deref(), Some(beside.as_path()));
        assert_eq!(found.worktree_branch.as_deref(), Some("fix/typos"));
        assert_eq!(found.root_branch.as_deref(), Some("main"));

        // One inside the repository, as Claude Code makes them, with a relative `gitdir:`.
        let inside = repo.join(".claude").join("worktrees").join("feat");
        let admin = repo.join(".git").join("worktrees").join("feat");
        fs::create_dir_all(&admin).expect("mkdir");
        fs::create_dir_all(&inside).expect("mkdir");
        fs::write(admin.join("HEAD"), "ref: refs/heads/feat/x\n").expect("head");
        fs::write(admin.join("commondir"), "../..").expect("commondir");
        fs::write(inside.join(".git"), "gitdir: ../../../.git/worktrees/feat").expect("dot git");
        let found = git_root(&inside, &|| false).expect("a repository");
        assert_eq!(found.root, repo);
        assert_eq!(found.worktree.as_deref(), Some(inside.as_path()));
        assert_eq!(found.worktree_branch.as_deref(), Some("feat/x"));
    }

    #[test]
    fn a_git_file_without_a_common_dir_is_a_repository_of_its_own() {
        // A submodule: its `.git` file names the superproject's `modules/<name>`, no `commondir`.
        let dir = tempfile::tempdir().expect("tempdir");
        let modules = dir
            .path()
            .join("super")
            .join(".git")
            .join("modules")
            .join("lib");
        let sub = dir.path().join("super").join("lib");
        fs::create_dir_all(&modules).expect("mkdir");
        fs::create_dir_all(&sub).expect("mkdir");
        fs::write(
            modules.join("HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .expect("detached head");
        fs::write(sub.join(".git"), "gitdir: ../.git/modules/lib\n").expect("dot git");
        let found = git_root(&sub, &|| false).expect("a repository");
        assert_eq!(found.root, sub);
        assert_eq!(found.worktree, None);
        assert_eq!(found.root_branch, None, "a detached HEAD names no branch");

        // A `.git` file that is not git's: the folder is still a root, as before.
        let odd = dir.path().join("odd");
        fs::create_dir_all(&odd).expect("mkdir");
        fs::write(odd.join(".git"), "not git").expect("dot git");
        assert_eq!(git_root(&odd, &|| false).map(|r| r.root), Some(odd));
    }

    #[test]
    fn git_paths_resolve_lexically() {
        let base = Path::new("/w/repo/.git/worktrees/x");
        assert_eq!(
            resolve_git_path(base, "../.."),
            PathBuf::from("/w/repo/.git")
        );
        assert_eq!(
            resolve_git_path(base, "./../../"),
            PathBuf::from("/w/repo/.git")
        );
        assert_eq!(
            resolve_git_path(Path::new("/w/wt"), "/w/repo/.git/worktrees/wt"),
            PathBuf::from("/w/repo/.git/worktrees/wt")
        );
    }

    #[test]
    fn non_git_siblings_group_under_their_parent_but_a_lone_one_does_not() {
        let cwds = vec![
            PathBuf::from("/w/notes/a"),
            PathBuf::from("/w/notes/b"),
            PathBuf::from("/w/alone"),
        ];
        let roots = resolve_roots(&cwds, false, &|| false, &mut git_root);
        let key = |p: &str| cmp_key(Path::new(p), false);
        let root = |p: &str| (roots[&key(p)].root.clone(), roots[&key(p)].is_git);
        assert_eq!(root("/w/notes/a"), (PathBuf::from("/w/notes"), false));
        assert_eq!(root("/w/notes/b"), (PathBuf::from("/w/notes"), false));
        assert_eq!(root("/w/alone"), (PathBuf::from("/w/alone"), false));
    }

    #[test]
    fn a_worktree_root_takes_the_spelling_its_sessions_use() {
        // Git wrote the worktree's `gitdir:` with another case than the session's cwd has, as on
        // Windows; the project keeps the cwd's spelling.
        let cwds = vec![PathBuf::from("/W/Repo/src"), PathBuf::from("/w/repo-wt")];
        let mut resolver = |cwd: &Path, _: &dyn Fn() -> bool| {
            Some(if cwd.ends_with("repo-wt") {
                Repo {
                    root: PathBuf::from("/w/repo"),
                    worktree: Some(cwd.to_path_buf()),
                    root_branch: None,
                    worktree_branch: Some("wt".into()),
                }
            } else {
                Repo::at(Path::new("/W/Repo"))
            })
        };
        let roots = resolve_roots(&cwds, true, &|| false, &mut resolver);
        for r in roots.values() {
            assert_eq!(r.root, PathBuf::from("/W/Repo"), "{r:?}");
        }
    }

    #[test]
    fn sub_agents_nest_only_under_a_parent_the_scan_found() {
        let session = |native: &str| SessionFacts {
            native_id: Some(native.into()),
            ..facts("/w/p", None, 1000)
        };
        let sub = |native: &str, parent: Option<&str>| SessionFacts {
            is_subagent: true,
            parent: parent.map(str::to_owned),
            ..session(native)
        };
        let mut all = vec![
            session("parent"),
            sub("child", Some("parent")),
            sub("grandchild", Some("child")),
            sub("orphan", Some("gone")),
            sub("review", None),
            SessionFacts {
                home: PathBuf::from("/home/u/.claude-work"),
                ..sub("elsewhere", Some("parent"))
            },
        ];
        settle_subagents(&mut all);
        let nested: Vec<_> = all.iter().map(|f| f.is_subagent).collect();
        assert_eq!(nested, [false, true, true, false, false, false]);
        let counts = aggregate_counts(&all);
        assert_eq!((counts.sessions, counts.subagent_sessions), (4, 2));
    }

    #[test]
    fn home_and_system_paths_are_excluded() {
        let homes = [ScanHome {
            engine: Engine::Claude,
            home: PathBuf::from("/home/u/.claude"),
        }];
        assert!(is_excluded_root(Path::new("/"), &homes, false));
        assert!(is_excluded_root(Path::new("/usr"), &homes, false));
        assert!(is_excluded_root(
            Path::new("/home/u/.claude"),
            &homes,
            false
        ));
        assert!(!is_excluded_root(
            Path::new("/home/u/code/proj"),
            &homes,
            false
        ));
        if let Some(home) = crate::user_home() {
            assert!(is_excluded_root(&home, &homes, false));
        }
    }

    #[test]
    fn path_key_folds_case_and_separators_for_windows_style_paths() {
        // Windows-style spellings of the same folder, checked without needing to run on Windows:
        // `Path::components()` cannot split a `\`-separated string correctly on a Unix host, but
        // `cmp_key` is a pure string function and does not depend on that.
        let a = cmp_key(Path::new(r"C:\Work\Proj"), true);
        let b = cmp_key(Path::new("c:/WORK/proj"), true);
        assert_eq!(a, b);
        assert_eq!(a, "c:/work/proj");

        // Case-sensitively (as on Linux), two different spellings are two different keys.
        assert_ne!(
            cmp_key(Path::new("/Work/Proj"), false),
            cmp_key(Path::new("/work/proj"), false)
        );
    }

    #[test]
    fn home_and_scanned_homes_are_excluded_even_with_different_casing() {
        let homes = [ScanHome {
            engine: Engine::Claude,
            home: PathBuf::from("C:/Users/Sam/.claude"),
        }];
        // A differently-cased drive letter and folder name, same real place.
        assert!(is_excluded_root(
            Path::new("c:/users/sam/.claude"),
            &homes,
            true
        ));
        // Case-sensitively, as on Linux, a different spelling is a different path.
        assert!(!is_excluded_root(
            Path::new("c:/users/sam/.claude"),
            &homes,
            false
        ));
        // A real project a few levels under a differently-cased home is still fine.
        assert!(!is_excluded_root(
            Path::new("C:/Users/Sam/code/proj"),
            &homes,
            true
        ));
    }

    #[test]
    fn projects_group_differently_cased_spellings_of_one_cwd_only_when_case_insensitive() {
        let sessions = [
            facts("/w/Proj/a", Some("main"), 1000),
            facts("/w/proj/a", Some("main"), 1000), // the same real folder, spelled differently
        ];
        let homes: [ScanHome; 0] = [];

        let sensitive =
            build_suggestions(&sessions, &homes, 10_000, false, &|| false, &mut git_root);
        assert_eq!(
            sensitive.len(),
            2,
            "case-sensitively, as on Linux, these are two different paths: {sensitive:?}"
        );

        let insensitive =
            build_suggestions(&sessions, &homes, 10_000, true, &|| false, &mut git_root);
        assert_eq!(insensitive.len(), 1, "{insensitive:?}");
        assert_eq!(insensitive[0].session_count, 2);
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
        let ws = build_workstreams(&project("/w/proj", &refs), 10_000, false);
        // The default workstream comes first, at the root, named after the usual branch.
        assert_eq!(ws[0].kind, WorkstreamSuggestionKind::Main);
        assert_eq!(ws[0].id, "/w/proj");
        assert_eq!(ws[0].name, "main");
        assert_eq!(ws[0].session_count, 4);
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
    fn a_worktree_is_a_workstream_named_by_its_branch_and_nothing_else() {
        let sessions = [
            facts("/w/repo", Some("main"), 1000),
            facts("/w/repo/.claude/worktrees/feat", Some("feat/x"), 1000),
            facts("/w/repo/.claude/worktrees/feat/src", Some("feat/x"), 1000),
            facts("/w/repo-fix", Some("fix/typos"), 1000),
        ];
        let inside = (
            PathBuf::from("/w/repo/.claude/worktrees/feat"),
            Some("feat/x".to_owned()),
        );
        let beside = (PathBuf::from("/w/repo-fix"), None);
        let repo = Project {
            root: PathBuf::from("/w/repo"),
            is_git: true,
            root_branch: Some("trunk".into()),
            sessions: vec![
                Placed {
                    facts: &sessions[0],
                    worktree: None,
                },
                Placed {
                    facts: &sessions[1],
                    worktree: Some(&inside),
                },
                Placed {
                    facts: &sessions[2],
                    worktree: Some(&inside),
                },
                Placed {
                    facts: &sessions[3],
                    worktree: Some(&beside),
                },
            ],
        };
        let ws = build_workstreams(&repo, 10_000, false);
        let summary: Vec<_> = ws
            .iter()
            .map(|w| (w.kind, w.id.as_str(), w.name.as_str(), w.session_count))
            .collect();
        assert_eq!(
            summary,
            [
                (WorkstreamSuggestionKind::Main, "/w/repo", "trunk", 1),
                (
                    WorkstreamSuggestionKind::Worktree,
                    "/w/repo/.claude/worktrees/feat",
                    "feat/x",
                    2
                ),
                // No HEAD read: named after its sessions' branch.
                (
                    WorkstreamSuggestionKind::Worktree,
                    "/w/repo-fix",
                    "fix/typos",
                    1
                ),
            ],
            "no `.claude` folder and no branch suggestion come from worktree sessions"
        );

        // A folder without git: the default workstream is `Main`.
        let notes = [facts("/w/notes/a", None, 1000)];
        let refs: Vec<&SessionFacts> = notes.iter().collect();
        let plain = Project {
            is_git: false,
            ..project("/w/notes", &refs)
        };
        let ws = build_workstreams(&plain, 10_000, false);
        assert_eq!(
            (ws[0].kind, ws[0].id.as_str(), ws[0].name.as_str()),
            (WorkstreamSuggestionKind::Main, "/w/notes", "Main")
        );
        assert_eq!(ws[1].name, "a");
    }

    #[test]
    fn workstream_subfolders_group_case_insensitively_but_keep_a_spelling() {
        let sessions = [
            facts("/w/proj/Apps/web", Some("main"), 1000),
            facts("/w/proj/apps/api", Some("main"), 1000),
        ];
        let refs: Vec<&SessionFacts> = sessions.iter().collect();
        let ws = build_workstreams(&project("/w/proj", &refs), 10_000, true);
        assert_eq!(ws.len(), 2, "{ws:?}");
        assert_eq!(ws[0].kind, WorkstreamSuggestionKind::Main);
        assert_eq!(ws[1].session_count, 2);
        assert!(
            ["Apps", "apps"].contains(&ws[1].name.as_str()),
            "{}",
            ws[1].name
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

    #[test]
    fn scan_options_default_now_tracks_the_real_clock_not_the_epoch() {
        let before = now_ms();
        let got = ScanOptions::default().now;
        let after = now_ms();
        assert!(
            (before..=after).contains(&got),
            "{got} not in {before}..={after}"
        );
    }

    /// A panic while reading one unit is caught and counted as unreadable, not lost -- and does
    /// not take down anything else a worker thread already collected. This prints a "thread
    /// panicked" message to stderr (the default panic hook still runs before `catch_unwind`
    /// returns); that is expected noise from the one deliberate panic below, not a failure.
    #[test]
    fn a_panicking_unit_is_caught_and_counted_unreadable_not_lost() {
        let out = run_catching_panics(3, || panic!("synthetic panic for the test"));
        assert_eq!(out, vec![None, None, None]);
    }

    fn suggestion_test_home(root: &Path) -> ScanHome {
        let home = root.join("claude-home");
        let transcripts = home.join("projects").join("synthetic");
        fs::create_dir_all(&transcripts).unwrap();
        for name in ["a", "b", "c"] {
            let cwd = root.join("work").join(name);
            fs::create_dir_all(&cwd).unwrap();
            let line = serde_json::json!({
                "type": "user", "sessionId": name, "cwd": cwd,
                "timestamp": "2026-01-01T00:00:00Z",
                "message": {"role": "user", "content": "synthetic"}
            });
            fs::write(transcripts.join(format!("{name}.jsonl")), line.to_string()).unwrap();
        }
        ScanHome {
            engine: Engine::Claude,
            home,
        }
    }

    #[test]
    fn cancellation_during_suggestions_stops_remaining_resolutions() {
        let root = tempfile::tempdir().unwrap();
        let home = suggestion_test_home(root.path());
        let options = ScanOptions::default();
        let mut resolutions = 0;
        let report = scan_with_resolver(
            &[home],
            &options,
            |_| {},
            |cwd, _| {
                resolutions += 1;
                options.cancel.store(true, Ordering::Relaxed);
                Some(Repo::at(cwd))
            },
        );
        assert_eq!(
            resolutions, 1,
            "cancelled suggestions must not resolve remaining directories"
        );
        assert_eq!(report.partial, Some(true));
        assert_eq!(report.counts.sessions, 3);
        assert_eq!(report.suggestions.len(), 1);
        assert_eq!(report.suggestions[0].session_count, 1);
    }

    #[test]
    fn budget_exhausted_during_suggestions_marks_report_partial() {
        let root = tempfile::tempdir().unwrap();
        let home = suggestion_test_home(root.path());
        let options = ScanOptions {
            budget: Duration::from_secs(1),
            ..ScanOptions::default()
        };
        let mut resolutions = 0;
        let report = scan_with_resolver(
            &[home],
            &options,
            |_| {},
            |cwd, stopped| {
                resolutions += 1;
                while !stopped() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Some(Repo::at(cwd))
            },
        );
        assert_eq!(
            report.partial,
            Some(true),
            "budget exhausted during suggestions must mark the report partial"
        );
        assert_eq!(resolutions, 1);
        assert_eq!(report.counts.sessions, 3);
        assert_eq!(report.suggestions.len(), 1);
    }

    #[test]
    fn cancellation_and_zero_budget_return_partial_reports() {
        let home = tempfile::tempdir().unwrap();
        let homes = [ScanHome {
            engine: Engine::Claude,
            home: home.path().to_path_buf(),
        }];
        let cancelled = ScanOptions::default();
        cancelled.cancel.store(true, Ordering::Relaxed);
        let exhausted = ScanOptions {
            budget: Duration::ZERO,
            ..ScanOptions::default()
        };
        for options in [cancelled, exhausted] {
            let report = scan(&homes, &options, |_| {});
            assert_eq!(report.partial, Some(true));
            assert_eq!(report.counts.sessions, 0);
            assert_eq!(report.unreadable, 0);
        }
    }
}
