//! The watcher: discovers transcripts, notices changes, reads from stored cursors, and turns items
//! into events for the sink.
//!
//! One thread owns all transcript state. Notifications only mark paths dirty (a map bounded by the
//! number of paths), so a slow sink stalls the reader, not memory: the watcher blocks on the
//! bounded channel, and changes that arrive meanwhile coalesce into one read per file.
//!
//! Transcripts are keyed by their canonical path, so a home reached through a symlink, or one that
//! did not exist yet at the first start, keeps its session ids across restarts.
//!
//! On a local filesystem these folders are watched:
//! - each home and its first-level folders (a new `projects` folder, the first session ever);
//! - the folders up to [`ALWAYS_WATCH_DEPTH`] levels below a home that lead to a transcript (for
//!   Claude, `projects/<project>`), so a new session in a quiet project appears at once;
//! - while a transcript is hot, its folder and that folder's parent (sub-agent and day folders).
//!
//! A slow sweep re-checks every transcript by size and mtime. Homes on network filesystems are
//! polled instead, and swept and rediscovered [`NETWORK_SLOWDOWN`] times less often; a hook for
//! an unknown session never makes them look sooner.
//!
//! The same thread applies states reported by hooks and the runtime ([`Signal`]s), so they and
//! the transcripts agree (see `derive::report`), and links sessions to workstreams. It also
//! decides, by the session's agent, whether a hook's sender may change the session (the rule is
//! on [`RunnerHooks`](crate::RunnerHooks)).

use crate::agents::{SessionAgent, SessionAgents};
use crate::config::{EngineHome, PollMode, Timing};
use crate::derive::{self, Derived, Facts, Reported};
use crate::fsinfo::{self, FileStat};
use crate::held::Held;
use crate::hooks::{self, Sender};
use crate::link::{self, Locations, WorkstreamLocation};
use crate::sink::Batch;
use crate::store::{Commit, Row, Store, path_text};
use notify::event::{EventKind, MetadataKind, ModifyKind};
use notify::{RecursiveMode, Watcher as _};
use pitcrew_interfaces::source::{Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptRef};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MachineId, MemberId, SessionId, TerminalId, WorkspaceId};
use pitcrew_protocol::model::{Engine, LinkBasis, Session, SessionState, TimestampMs};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use ulid::Ulid;

/// Reads of one transcript per wake-up before others get a turn.
const MAX_READS_PER_REFRESH: usize = 4096;
/// Dirty paths held before the watcher falls back to checking everything.
const MAX_DIRTY_PATHS: usize = 10_000;
/// Least time between rediscoveries triggered by unknown files.
const REDISCOVER_GAP: Duration = Duration::from_secs(1);
/// Folders this many levels below a home are always watched when they lead to a transcript.
const ALWAYS_WATCH_DEPTH: usize = 2;
/// Polled network homes are swept and rediscovered this many times less often, to spare shared
/// metadata servers.
const NETWORK_SLOWDOWN: u32 = 4;
/// Least time between two "file watcher error" warnings.
const NOTIFY_WARN_GAP: Duration = Duration::from_secs(60);
/// The identity saved for a transcript whose file was deleted. It matches no real file, so a file
/// that appears at the path later is read from the start, even if it reuses the old inode.
const GONE: &str = "gone";
/// Reported states waiting for the watcher; past this the sender with the most waiting loses
/// its oldest.
const MAX_SIGNALS: usize = 1024;
/// Least time between two "too many reported states" warnings.
const SIGNALS_WARN_GAP: Duration = Duration::from_secs(60);

/// A state reported outside the transcripts, for one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signal {
    pub target: Target,
    pub report: Reported,
    pub origin: Origin,
}

/// Who reported a [`Signal`], and so whether it is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The runner itself (a command it ran): applied as is.
    Runner,
    /// An agent hook: applied only if its sender may change the session.
    Hook(Sender),
}

impl Origin {
    /// Who to count it against when signals pile up: the hook's member, or the runner.
    fn member(self) -> Option<MemberId> {
        match self {
            Self::Runner => None,
            Self::Hook(sender) => Some(sender.member()),
        }
    }
}

/// Which session a [`Signal`] is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// By the CLI's own session id (hooks).
    Native { engine: Engine, native_id: String },
    /// By the runner's id (commands).
    Session(SessionId),
}

/// Signals from notifications, hooks, commands and the handle to the watcher thread.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    signals: Mutex<Signals>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct Signals {
    /// Changed paths.
    dirty: HashMap<PathBuf, Dirty>,
    /// Too many dirty paths, or lost events: check every transcript instead.
    overflow: bool,
    /// When to run discovery in every home (asked for, or events were lost).
    rediscover_at: Option<Instant>,
    /// When to look for new transcripts in the homes that are not slow (network homes keep their
    /// own, rarer schedule): a new file appeared, or a hook named an unknown session.
    look_at: Option<Instant>,
    stop: bool,
    notify_warned_at: Option<Instant>,
    /// Reported states, in arrival order.
    reports: VecDeque<Signal>,
    signals_warned_at: Option<Instant>,
    /// Workstream locations changed: link every session again.
    relink: bool,
}

#[derive(Clone, Copy, Debug)]
struct Dirty {
    /// When to read it.
    due: Instant,
    /// Created or renamed into place: if nothing tracks the path, it may be a new transcript.
    created: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Signals> {
        self.signals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn mark(&self, paths: Vec<PathBuf>, due: Instant, created: bool) {
        let mut s = self.lock();
        for p in paths {
            if let Some(d) = s.dirty.get_mut(&p) {
                // Keep the earliest due time: a stream of writes is read every `debounce`, not
                // starved.
                d.created |= created;
                continue;
            }
            if s.dirty.len() >= MAX_DIRTY_PATHS {
                s.overflow = true;
                if created {
                    s.look_at = Some(s.look_at.map_or(due, |r| r.min(due)));
                }
                continue;
            }
            s.dirty.insert(p, Dirty { due, created });
        }
        drop(s);
        self.cv.notify_one();
    }

    /// Events were lost (inotify queue overflow, FSEvents "must scan subdirectories"): check
    /// every transcript and discover again.
    fn lost_events(&self) {
        let mut s = self.lock();
        s.overflow = true;
        s.rediscover_at = Some(Instant::now());
        drop(s);
        self.cv.notify_one();
    }

    fn watcher_error(&self, e: &notify::Error) {
        let now = Instant::now();
        let mut s = self.lock();
        if s.notify_warned_at
            .is_none_or(|t| now.duration_since(t) >= NOTIFY_WARN_GAP)
        {
            s.notify_warned_at = Some(now);
            drop(s);
            tracing::warn!(error = %e, "file watcher error");
        }
    }

    pub fn rescan(&self) {
        self.lock().rediscover_at = Some(Instant::now());
        self.cv.notify_one();
    }

    /// Hands a reported state to the watcher. It never blocks: a hook must not wait. When too
    /// many wait, the sender with the most waiting loses its oldest, so a flood from one sender
    /// costs only that sender.
    pub fn signal(&self, signal: Signal) {
        let mut s = self.lock();
        if s.reports.len() >= MAX_SIGNALS {
            drop_one(&mut s.reports);
            let now = Instant::now();
            if s.signals_warned_at
                .is_none_or(|t| now.duration_since(t) >= SIGNALS_WARN_GAP)
            {
                s.signals_warned_at = Some(now);
                tracing::warn!(
                    "too many reported states waiting; dropping the oldest of the sender with the most"
                );
            }
        }
        s.reports.push_back(signal);
        drop(s);
        self.cv.notify_one();
    }

    pub fn relink(&self) {
        self.lock().relink = true;
        self.cv.notify_one();
    }

    pub fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_one();
    }

    fn stopping(&self) -> bool {
        self.lock().stop
    }
}

/// The notification callback: it only marks paths.
pub(crate) fn notify_handler(
    shared: Arc<Shared>,
    debounce: Duration,
) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
    move |res| match res {
        Ok(ev) if ev.need_rescan() => shared.lost_events(),
        // Reads (ours included) and access-time updates are not changes.
        Ok(ev)
            if matches!(
                ev.kind,
                EventKind::Access(_)
                    | EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
            ) => {}
        Ok(ev) => {
            let created = matches!(
                ev.kind,
                EventKind::Create(_)
                    | EventKind::Modify(ModifyKind::Name(_))
                    | EventKind::Any
                    | EventKind::Other
            );
            shared.mark(ev.paths, after(Instant::now(), debounce), created);
        }
        Err(e) => shared.watcher_error(&e),
    }
}

/// A CLI home and how it is watched.
struct Home {
    engine: Engine,
    /// As configured.
    configured: PathBuf,
    /// Its canonical form, refreshed on each rediscovery; the configured path while it does not
    /// exist.
    path: PathBuf,
    adapter: Arc<dyn SourceAdapter>,
    polled: bool,
    /// Polled as a network filesystem: sweeps and rediscoveries are rarer.
    slow: bool,
    /// The home and its first-level folders, while watched.
    watched: Vec<PathBuf>,
    next_sweep: Instant,
    next_rediscover: Instant,
    /// Discovery is failing; warned once until it works again.
    discover_failing: bool,
}

impl Home {
    fn every(&self, interval: Duration) -> Duration {
        if self.slow {
            interval.saturating_mul(NETWORK_SLOWDOWN)
        } else {
            interval
        }
    }
}

/// One transcript. `row` is the in-memory state, ahead of the store until the sink accepts.
struct Tracked {
    row: Row,
    tref: TranscriptRef,
    home: usize,
    /// For a sub-agent, its parent's session (`Some(None)`: it names none); `None` until looked
    /// up, at its discovery or its first hook.
    parent: Option<Option<SessionId>>,
    hot: bool,
    /// Folders this transcript holds a watch on.
    watched: Vec<PathBuf>,
    poll_every: Duration,
    next_poll: Instant,
    /// Size and mtime at which reading failed: not read again until the file changes.
    failed_at: Option<(u64, TimestampMs)>,
    /// A read failure was logged as a warning; later ones are quieter until a read works.
    warned: bool,
}

/// A transcript's `(path, inner id)`.
type Key = (PathBuf, Option<String>);

/// Why the watcher stopped early.
struct Hangup;

pub(crate) struct Watcher {
    workspace: WorkspaceId,
    machine: MachineId,
    owner: MemberId,
    timing: Timing,
    poll: PollMode,
    max_batch: usize,
    homes: Vec<Home>,
    store: Arc<Mutex<Store>>,
    tx: SyncSender<Batch>,
    shared: Arc<Shared>,
    notify: Option<notify::RecommendedWatcher>,
    /// The index as loaded by `start()`; tracked by the watcher's own start.
    rows: Vec<Row>,
    tracked: BTreeMap<u64, Tracked>,
    next_id: u64,
    /// Canonical key → tracked transcript.
    by_key: HashMap<Key, u64>,
    /// Key as discovered (perhaps through a symlink) → tracked transcript, so each discovered
    /// path is canonicalized once.
    by_raw: HashMap<Key, u64>,
    by_path: HashMap<PathBuf, Vec<u64>>,
    /// Watched folders and how many holders each has.
    dirs: HashMap<PathBuf, usize>,
    /// Folders that could not be watched: warned once, retried on rediscovery.
    watch_failed: HashSet<PathBuf>,
    /// Discovered transcripts that cannot be indexed: warned once.
    skipped: HashSet<Key>,
    /// Folders outside their home that hold transcripts: warned once.
    outside: HashSet<PathBuf>,
    last_rediscover: Option<Instant>,
    /// Workstream locations, to link sessions to.
    locations: Option<Arc<dyn Locations>>,
    /// The CLI's session id → tracked transcript.
    by_native: HashMap<(Engine, String), u64>,
    by_session: HashMap<SessionId, u64>,
    /// Hooks for sessions not indexed yet.
    held: Held,
    /// Who runs each session; without it every hook is refused.
    agents: Option<Arc<dyn SessionAgents>>,
    /// The agent lookup panicked; warned once.
    lookup_panicked: bool,
}

pub(crate) struct Setup {
    pub workspace: WorkspaceId,
    pub machine: MachineId,
    pub owner: MemberId,
    pub timing: Timing,
    pub poll: PollMode,
    pub max_batch: usize,
    pub homes: Vec<EngineHome>,
    pub adapters: Vec<Arc<dyn SourceAdapter>>,
    pub rows: Vec<Row>,
    pub store: Arc<Mutex<Store>>,
    pub tx: SyncSender<Batch>,
    pub shared: Arc<Shared>,
    pub locations: Option<Arc<dyn Locations>>,
    pub agents: Option<Arc<dyn SessionAgents>>,
}

impl Watcher {
    pub fn new(s: Setup) -> Self {
        let notify = if s.poll == PollMode::Always {
            None
        } else {
            match notify::recommended_watcher(notify_handler(
                Arc::clone(&s.shared),
                s.timing.debounce,
            )) {
                Ok(w) => Some(w),
                Err(e) => {
                    tracing::warn!(error = %e, "file notifications are unavailable; polling every home instead");
                    None
                }
            }
        };
        if s.agents.is_none() {
            tracing::info!("no session agents configured: every hook will be refused");
        }
        let now = Instant::now();
        let mut homes = Vec::new();
        for h in s.homes {
            let Some(adapter) = s.adapters.iter().find(|a| a.engine() == h.engine) else {
                tracing::warn!(engine = ?h.engine, home = %h.path.display(), "no adapter for this home; skipping it");
                continue;
            };
            let path = h.path.canonicalize().unwrap_or_else(|_| h.path.clone());
            let (polled, slow) = poll_decision(s.poll, &path, notify.is_some());
            if polled {
                tracing::info!(home = %path.display(), "polling this home (network filesystem or forced)");
            }
            let mut home = Home {
                engine: h.engine,
                configured: h.path,
                path,
                adapter: Arc::clone(adapter),
                polled,
                slow,
                watched: Vec::new(),
                next_sweep: now,
                next_rediscover: now,
                discover_failing: false,
            };
            home.next_sweep = after(now, home.every(s.timing.cold_interval));
            home.next_rediscover = after(now, home.every(s.timing.rediscover_interval));
            homes.push(home);
        }
        Self {
            workspace: s.workspace,
            machine: s.machine,
            owner: s.owner,
            timing: s.timing,
            poll: s.poll,
            max_batch: s.max_batch.max(1),
            homes,
            store: s.store,
            tx: s.tx,
            shared: s.shared,
            notify,
            rows: s.rows,
            tracked: BTreeMap::new(),
            next_id: 0,
            by_key: HashMap::new(),
            by_raw: HashMap::new(),
            by_path: HashMap::new(),
            dirs: HashMap::new(),
            watch_failed: HashSet::new(),
            skipped: HashSet::new(),
            outside: HashSet::new(),
            last_rediscover: None,
            locations: s.locations,
            by_native: HashMap::new(),
            by_session: HashMap::new(),
            held: Held::default(),
            agents: s.agents,
            lookup_panicked: false,
        }
    }

    pub fn run(mut self) {
        if self.start().is_err() {
            return;
        }
        while let Some(wake) = self.wait() {
            if self.handle(wake).is_err() {
                return;
            }
        }
    }

    /// Tracks the loaded index, catches up on changes made while the runner was down (newest
    /// first), then discovers.
    fn start(&mut self) -> Result<(), Hangup> {
        let mut rows = std::mem::take(&mut self.rows);
        rows.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        for mut row in rows {
            if self.shared.stopping() {
                return Err(Hangup);
            }
            // Deleted earlier: discovery resumes the row if the file comes back.
            if row.identity.as_deref() == Some(GONE) || !self.follow_canonical(&mut row) {
                continue;
            }
            let Some(home) = self.home_for(row.engine, &row.path) else {
                continue;
            };
            if self
                .by_key
                .contains_key(&(row.path.clone(), row.inner_id.clone()))
            {
                tracing::warn!(path = %row.path.display(), session = %row.session, "a second index row for one transcript; ignoring it");
                continue;
            }
            let tref = TranscriptRef {
                engine: row.engine,
                path: row.path.clone(),
                inner_id: row.inner_id.clone(),
                size: row.size,
                modified: row.mtime,
            };
            self.track(row, tref, home);
        }
        let ids: Vec<u64> = self.tracked.keys().copied().collect();
        for id in ids {
            self.serve_due()?;
            self.check(id)?;
        }
        let all: Vec<usize> = (0..self.homes.len()).collect();
        self.rediscover(&all)
    }

    /// A stored path whose canonical form changed (a folder above it became a symlink) moves to
    /// the new form, so the transcript keeps its session. False if the row can't be used.
    fn follow_canonical(&self, row: &mut Row) -> bool {
        let Ok(canonical) = row.path.canonicalize() else {
            return true;
        };
        if canonical == row.path {
            return true;
        }
        match self.store_lock().set_path(row.session, &canonical) {
            Ok(()) => {
                tracing::info!(was = %row.path.display(), now = %canonical.display(), "a transcript's canonical path changed; following it");
                row.path = canonical;
                true
            }
            Err(e) => {
                tracing::warn!(path = %row.path.display(), error = %e, "cannot move a transcript to its canonical path; ignoring its row");
                false
            }
        }
    }

    /// Sleeps until something is due. Returns what woke it, or `None` to stop.
    fn wait(&mut self) -> Option<Wake> {
        let mut deadline = self
            .homes
            .iter()
            .map(|h| h.next_sweep.min(h.next_rediscover))
            .min()
            .unwrap_or_else(|| after(Instant::now(), Duration::MAX));
        for t in self.tracked.values() {
            if t.hot && self.homes[t.home].polled {
                deadline = deadline.min(t.next_poll);
            }
        }
        let shared = Arc::clone(&self.shared);
        let mut s = shared.lock();
        loop {
            if s.stop {
                return None;
            }
            let now = Instant::now();
            let mut next = deadline;
            for r in [s.rediscover_at, s.look_at].into_iter().flatten() {
                next = next.min(r);
            }
            if let Some(d) = s.dirty.values().map(|d| d.due).min() {
                next = next.min(d);
            }
            if s.overflow || next <= now || !s.reports.is_empty() || s.relink {
                let mut due = Vec::new();
                s.dirty.retain(|p, d| {
                    if d.due <= now {
                        due.push((p.clone(), d.created));
                        false
                    } else {
                        true
                    }
                });
                let rediscover = s.rediscover_at.is_some_and(|r| r <= now);
                if rediscover {
                    s.rediscover_at = None;
                }
                let look = s.look_at.is_some_and(|r| r <= now);
                if look {
                    s.look_at = None;
                }
                let overflow = std::mem::take(&mut s.overflow);
                return Some(Wake {
                    due,
                    overflow,
                    rediscover,
                    look,
                    reports: s.reports.drain(..).collect(),
                    relink: std::mem::take(&mut s.relink),
                });
            }
            s = shared
                .cv
                .wait_timeout(s, next - now)
                .map_or_else(|e| e.into_inner().0, |(g, _)| g);
        }
    }

    fn handle(&mut self, wake: Wake) -> Result<(), Hangup> {
        // Reports first: beating the transcripts is what they are for.
        self.apply_reports(wake.reports)?;
        if wake.relink {
            self.relink_all()?;
        }
        let now = Instant::now();
        let mut maybe_new = false;
        for (path, created) in wake.due {
            match self.by_path.get(&path).cloned() {
                Some(ids) => {
                    for id in ids {
                        self.serve_due()?;
                        self.check(id)?;
                    }
                }
                None => maybe_new |= created,
            }
        }
        if maybe_new {
            // A new file or folder in a watched folder: maybe a new session.
            self.look_soon(now);
        }
        let due: Vec<usize> = (0..self.homes.len())
            .filter(|&h| {
                let home = &self.homes[h];
                wake.rediscover || (wake.look && !home.slow) || home.next_rediscover <= now
            })
            .collect();
        if !due.is_empty() {
            self.rediscover(&due)?;
        }
        for h in 0..self.homes.len() {
            if wake.overflow || self.homes[h].next_sweep <= now {
                self.sweep(h)?;
            }
        }
        let polls: Vec<u64> = self
            .tracked
            .iter()
            .filter(|(_, t)| t.hot && self.homes[t.home].polled && t.next_poll <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in polls {
            let changed = self.check(id)?;
            let (poll_min, poll_max) = (self.timing.poll_min, self.timing.poll_max);
            if let Some(t) = self.tracked.get_mut(&id) {
                t.poll_every = if changed {
                    poll_min
                } else {
                    t.poll_every.saturating_mul(2).min(poll_max)
                };
                t.next_poll = after(Instant::now(), t.poll_every);
            }
        }
        Ok(())
    }

    /// Checks every transcript of one home by size and mtime.
    fn sweep(&mut self, h: usize) -> Result<(), Hangup> {
        let home = &mut self.homes[h];
        home.next_sweep = after(Instant::now(), home.every(self.timing.cold_interval));
        let ids: Vec<u64> = self
            .tracked
            .iter()
            .filter(|(_, t)| t.home == h)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.serve_due()?;
            self.check(id)?;
        }
        Ok(())
    }

    /// Between transcripts of a long backfill or sweep: stops if asked, and reads the transcripts
    /// whose changes are due, so live sessions don't wait for the backfill.
    fn serve_due(&mut self) -> Result<(), Hangup> {
        let now = Instant::now();
        let (due, reports): (Vec<PathBuf>, Vec<Signal>) = {
            let mut s = self.shared.lock();
            if s.stop {
                return Err(Hangup);
            }
            let mut due = Vec::new();
            s.dirty.retain(|p, d| {
                if d.due <= now && self.by_path.contains_key(p) {
                    due.push(p.clone());
                    false
                } else {
                    true
                }
            });
            (due, s.reports.drain(..).collect())
        };
        self.apply_reports(reports)?;
        for path in due {
            for id in self.by_path.get(&path).cloned().unwrap_or_default() {
                self.check(id)?;
            }
        }
        Ok(())
    }

    /// Runs discovery for some homes and indexes the new transcripts, newest first.
    fn rediscover(&mut self, which: &[usize]) -> Result<(), Hangup> {
        let now = Instant::now();
        self.last_rediscover = Some(now);
        let mut found: Vec<(TimestampMs, usize, Key, TranscriptRef, PathBuf)> = Vec::new();
        for &h in which {
            self.refresh_home(h);
            let home = &mut self.homes[h];
            home.next_rediscover = after(now, home.every(self.timing.rediscover_interval));
            let adapter = Arc::clone(&home.adapter);
            let list = match guard(|| adapter.discover(&home.path)) {
                Ok(list) => {
                    if home.discover_failing {
                        tracing::info!(home = %home.path.display(), "discovery works again");
                    }
                    home.discover_failing = false;
                    list
                }
                Err(e) => {
                    if !home.discover_failing {
                        tracing::warn!(home = %home.path.display(), error = %e, "discovery failed");
                    }
                    home.discover_failing = true;
                    continue;
                }
            };
            for tref in list {
                let raw = (tref.path.clone(), tref.inner_id.clone());
                if self.by_raw.contains_key(&raw) || self.skipped.contains(&raw) {
                    continue;
                }
                // Keys are canonical: the file exists now, so this is when to resolve it.
                let canonical = match tref.path.canonicalize() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::debug!(path = %tref.path.display(), error = %e, "a discovered transcript is gone");
                        continue;
                    }
                };
                if let Some(&id) = self.by_key.get(&(canonical.clone(), tref.inner_id.clone())) {
                    self.by_raw.insert(raw, id);
                    continue;
                }
                let mtime = fsinfo::stat(&canonical).map_or(tref.modified, |s| s.mtime);
                found.push((mtime, h, raw, tref, canonical));
            }
        }
        // Newest first: live sessions are indexed before a backlog of old ones.
        found.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, h, raw, tref, canonical) in found {
            self.serve_due()?;
            self.add(h, raw, tref, canonical)?;
        }
        if !self.watch_failed.is_empty() {
            let ids: Vec<u64> = self.tracked.keys().copied().collect();
            for id in ids {
                self.sync_watches(id);
            }
        }
        Ok(())
    }

    /// Re-resolves a home (it may have appeared, or its symlink may point elsewhere now), and
    /// keeps it and its first-level folders watched.
    fn refresh_home(&mut self, h: usize) {
        let home = &self.homes[h];
        let canonical = home
            .configured
            .canonicalize()
            .unwrap_or_else(|_| home.configured.clone());
        if canonical != home.path {
            tracing::info!(home = %home.configured.display(), folder = %canonical.display(), "the home resolves to a new folder");
            let (polled, slow) = poll_decision(self.poll, &canonical, self.notify.is_some());
            let home = &mut self.homes[h];
            home.path = canonical;
            home.polled = polled;
            home.slow = slow;
            let ids: Vec<u64> = self
                .tracked
                .iter()
                .filter(|(_, t)| t.home == h)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                self.sync_watches(id);
            }
        }
        let home = &mut self.homes[h];
        let want = if home.polled || self.notify.is_none() {
            Vec::new()
        } else {
            home_dirs(&home.path)
        };
        let have = std::mem::take(&mut home.watched);
        let kept = self.reconcile(have, want);
        self.homes[h].watched = kept;
    }

    /// Starts tracking a discovered transcript: with its row from an earlier sighting, or a new
    /// row with a new session id, saved before anything is read so the id never changes.
    fn add(
        &mut self,
        h: usize,
        raw: Key,
        mut tref: TranscriptRef,
        canonical: PathBuf,
    ) -> Result<(), Hangup> {
        if let Some(&id) = self.by_key.get(&(canonical.clone(), tref.inner_id.clone())) {
            self.by_raw.insert(raw, id);
            return Ok(());
        }
        if path_text(&canonical).is_err() {
            tracing::warn!(path = %canonical.display(), "skipping a transcript whose path is not Unicode");
            self.skipped.insert(raw);
            return Ok(());
        }
        tref.path = canonical;
        let earlier = self.store_lock().find(&tref.path, tref.inner_id.as_deref());
        let row = match earlier {
            Ok(Some(row)) => {
                tracing::debug!(path = %tref.path.display(), session = %row.session, "a transcript seen before; resuming its row");
                row
            }
            Ok(None) => {
                let row = new_row(&tref);
                if let Err(e) = self.store_lock().insert(&row) {
                    tracing::error!(path = %row.path.display(), error = %e, "cannot index a transcript");
                    return Ok(());
                }
                tracing::debug!(path = %row.path.display(), session = %row.session, "new transcript");
                row
            }
            Err(e) => {
                tracing::error!(path = %tref.path.display(), error = %e, "cannot look up a transcript");
                return Ok(());
            }
        };
        let id = self.track(row, tref, h);
        self.by_raw.insert(raw, id);
        self.check(id)?;
        Ok(())
    }

    fn track(&mut self, row: Row, tref: TranscriptRef, home: usize) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.by_key
            .insert((row.path.clone(), row.inner_id.clone()), id);
        self.by_path.entry(row.path.clone()).or_default().push(id);
        self.by_session.insert(row.session, id);
        if row.meta.is_some() {
            self.by_native.insert((row.engine, native_id(&row)), id);
        }
        self.note_outside(&row.path, home);
        self.tracked.insert(
            id,
            Tracked {
                row,
                tref,
                home,
                parent: None,
                hot: false,
                watched: Vec::new(),
                poll_every: self.timing.poll_min,
                next_poll: Instant::now(),
                failed_at: None,
                warned: false,
            },
        );
        self.sync_watches(id);
        id
    }

    /// Stops tracking a deleted transcript. Its row stays, so if a file comes back at the path it
    /// keeps its session; the row is marked [`GONE`] (in order with the commits already queued),
    /// so that file is read from the start.
    fn drop_deleted(&mut self, id: u64) -> Result<(), Hangup> {
        let Some(t) = self.tracked.remove(&id) else {
            return Ok(());
        };
        let mut row = t.row;
        tracing::info!(path = %row.path.display(), session = %row.session, "transcript deleted; no longer watching it");
        self.by_key
            .remove(&(row.path.clone(), row.inner_id.clone()));
        let empty = self.by_path.get_mut(&row.path).is_some_and(|ids| {
            ids.retain(|i| *i != id);
            ids.is_empty()
        });
        if empty {
            self.by_path.remove(&row.path);
        }
        self.by_raw.retain(|_, i| *i != id);
        self.by_native.retain(|_, i| *i != id);
        self.by_session.remove(&row.session);
        for d in &t.watched {
            self.unwatch_dir(d);
        }
        row.identity = Some(GONE.to_owned());
        row.caught_up = false;
        self.tx
            .send(Batch {
                events: Vec::new(),
                commit: Commit::Full(Box::new(row)),
            })
            .map_err(|_| Hangup)
    }

    /// A transcript outside its home (its folder is reached through a symlink) is still indexed,
    /// but only its own folder can be watched. Said once per folder.
    fn note_outside(&mut self, path: &Path, home: usize) {
        let home = &self.homes[home].path;
        let Some(dir) = path.parent() else {
            return;
        };
        if dir.starts_with(home) || self.outside.contains(dir) || !dir.exists() {
            return;
        }
        tracing::warn!(folder = %dir.display(), home = %home.display(), "transcripts outside their home (through a symlink); only their own folder is watched");
        self.outside.insert(dir.to_path_buf());
    }

    fn home_for(&self, engine: Engine, path: &Path) -> Option<usize> {
        let same = |h: &Home| h.engine == engine;
        self.homes
            .iter()
            .position(|h| same(h) && path.starts_with(&h.path))
            .or_else(|| self.homes.iter().position(same))
    }

    /// Stats one transcript and reads it if it changed. Returns whether it changed.
    fn check(&mut self, id: u64) -> Result<bool, Hangup> {
        let Some(t) = self.tracked.get(&id) else {
            return Ok(false);
        };
        let st = match fsinfo::stat(&t.row.path) {
            Ok(st) => st,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.drop_deleted(id)?;
                return Ok(false);
            }
            Err(e) => {
                tracing::debug!(path = %t.row.path.display(), error = %e, "transcript not readable");
                return Ok(false);
            }
        };
        self.classify(id, st.mtime);
        let Some(t) = self.tracked.get(&id) else {
            return Ok(false);
        };
        let row = &t.row;
        if t.failed_at == Some((st.size, st.mtime))
            || (row.caught_up
                && row.size == st.size
                && row.mtime == st.mtime
                && fsinfo::same_file(row.identity.as_deref(), st.identity.as_deref()))
        {
            return Ok(false);
        }
        self.refresh(id, st)?;
        Ok(true)
    }

    fn classify(&mut self, id: u64, mtime: TimestampMs) {
        let age = fsinfo::millis(SystemTime::now()).saturating_sub(mtime);
        let hot = u128::try_from(age).unwrap_or(0) < self.timing.hot_window.as_millis();
        let Some(t) = self.tracked.get_mut(&id) else {
            return;
        };
        if t.hot == hot {
            return;
        }
        t.hot = hot;
        t.next_poll = Instant::now();
        self.sync_watches(id);
    }

    /// Brings a transcript's folder watches in line with where it is and whether it is hot.
    fn sync_watches(&mut self, id: u64) {
        let Some(t) = self.tracked.get_mut(&id) else {
            return;
        };
        let home = &self.homes[t.home];
        let want = if home.polled || self.notify.is_none() {
            Vec::new()
        } else {
            wanted_dirs(&t.row.path, &home.path, t.hot)
        };
        let have = std::mem::take(&mut t.watched);
        let kept = self.reconcile(have, want);
        if let Some(t) = self.tracked.get_mut(&id) {
            t.watched = kept;
        }
    }

    /// Moves a holder's watches from `have` to `want`; returns the folders it now holds.
    fn reconcile(&mut self, have: Vec<PathBuf>, want: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut kept = Vec::with_capacity(want.len());
        for d in have {
            if want.contains(&d) {
                kept.push(d);
            } else {
                self.unwatch_dir(&d);
            }
        }
        for d in want {
            if !kept.contains(&d) && self.watch_dir(&d) {
                kept.push(d);
            }
        }
        kept
    }

    /// Adds a holder to a folder's watch, starting it if needed. Only a working watch counts.
    fn watch_dir(&mut self, dir: &Path) -> bool {
        if let Some(n) = self.dirs.get_mut(dir) {
            *n += 1;
            return true;
        }
        let Some(w) = self.notify.as_mut() else {
            return false;
        };
        match w.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                self.dirs.insert(dir.to_path_buf(), 1);
                self.watch_failed.remove(dir);
                true
            }
            Err(e) if is_not_found(&e) => {
                tracing::debug!(dir = %dir.display(), "folder is gone; not watching it");
                false
            }
            Err(e) => {
                if self.watch_failed.insert(dir.to_path_buf()) {
                    tracing::warn!(dir = %dir.display(), error = %e, "cannot watch a folder; the sweep still covers it");
                }
                false
            }
        }
    }

    fn unwatch_dir(&mut self, dir: &Path) {
        let Some(n) = self.dirs.get_mut(dir) else {
            return;
        };
        *n = n.saturating_sub(1);
        if *n == 0 {
            self.dirs.remove(dir);
            if let Some(w) = self.notify.as_mut() {
                let _ = w.unwatch(dir);
            }
        }
    }

    /// Reads from the stored cursor until the adapter has nothing more.
    fn refresh(&mut self, id: u64, st: FileStat) -> Result<(), Hangup> {
        let Some(t) = self.tracked.get_mut(&id) else {
            return Ok(());
        };
        if needs_reindex(&t.row, &st) {
            reindex(&mut t.row, &st);
        }
        t.tref.size = st.size;
        t.tref.modified = st.mtime;
        let adapter = Arc::clone(&self.homes[t.home].adapter);
        let mut retried = false;
        for n in 0..=MAX_READS_PER_REFRESH {
            if self.shared.stopping() {
                return Err(Hangup);
            }
            let Some(t) = self.tracked.get_mut(&id) else {
                return Ok(());
            };
            if n == MAX_READS_PER_REFRESH {
                // Let other transcripts have a turn; this one is read again next wake-up.
                self.shared
                    .mark(vec![t.row.path.clone()], Instant::now(), false);
                break;
            }
            let cursor = t.row.cursor.clone();
            match guard(|| adapter.read_from(&t.tref, &cursor)) {
                Ok(chunk) => {
                    // Any cursor change is progress; OpenCode's lives in `state`, not `offset`.
                    // The last read saves `caught_up`, even when it found nothing new.
                    let more = chunk.cursor != cursor;
                    t.failed_at = None;
                    t.warned = false;
                    self.process(id, chunk, &st, !more)?;
                    if !more {
                        break;
                    }
                }
                Err(AdapterError::Source(SourceError::Unreadable { reason, .. }))
                    if !retried && t.row.inner_id.is_none() && cursor.offset > st.size =>
                {
                    tracing::debug!(reason, "adapter reports a shorter file");
                    retried = true;
                    reindex(&mut t.row, &st);
                }
                Err(e) => {
                    // Not read again until the file changes, and warned about once.
                    t.failed_at = Some((st.size, st.mtime));
                    if t.warned {
                        tracing::debug!(path = %t.row.path.display(), error = %e, "cannot read transcript");
                    } else {
                        tracing::warn!(path = %t.row.path.display(), error = %e, "cannot read transcript; trying again when it changes");
                        t.warned = true;
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    /// Turns one chunk into events and queues them; the store is updated once they are accepted.
    fn process(
        &mut self,
        id: u64,
        chunk: ParseChunk,
        st: &FileStat,
        caught_up: bool,
    ) -> Result<(), Hangup> {
        let (workspace, owner, machine) = (self.workspace, self.owner, self.machine);
        let Some(t) = self.tracked.get_mut(&id) else {
            return Ok(());
        };
        let session = t.row.session;
        let place = |row: &Row| row.meta.as_ref().map(|m| (m.cwd.clone(), m.branch.clone()));
        let was = place(&t.row);
        if let Some(meta) = chunk.meta {
            t.row.meta = Some(meta);
        }
        let first = !t.row.discovered;
        let moved = !first && place(&t.row) != was;
        let cwd = t.row.meta.as_ref().and_then(|m| m.cwd.clone());
        let ctx = derive::Ctx {
            session,
            cwd: cwd.as_deref(),
            emit_states: !first,
        };

        let mut derived: Vec<Derived> = Vec::new();
        let mut seen: HashMap<u64, u32> = HashMap::new();
        for item in &chunk.items {
            let before = derived.len();
            let mut key = derive::item_key(item);
            let n = seen.entry(key).or_insert(0);
            if *n > 0 {
                key = derive::nth(key, *n);
            }
            *n += 1;
            derive::apply(&mut t.row.facts, &ctx, item, key, &mut derived);
            // Accepted before a crash: fold the item, don't send it again.
            if t.row.accepted.contains(&key) {
                derived.truncate(before);
            }
        }
        let engine = t.row.engine;
        let native = native_id(&t.row);
        let path = t.row.path.clone();
        let subagent = t.row.meta.as_ref().is_some_and(|m| m.is_subagent);
        let started = t.row.meta.as_ref().and_then(|m| m.started);
        if !native.is_empty() {
            self.by_native.insert((engine, native.clone()), id);
        }

        // Facts from elsewhere: the parent of a sub-agent, the terminal the runner started the
        // session in, hooks that came before the transcript, and workstream locations.
        let (parent, terminal, held) = if first {
            let parent = subagent.then(|| self.parent_of(engine, &path)).flatten();
            let terminal = if subagent {
                None
            } else {
                self.claim_terminal(session, engine, &native, cwd.as_deref(), started)
            };
            let held = self.allowed_held(session, parent, engine, &native);
            (parent, terminal, held)
        } else {
            (None, None, Vec::new())
        };
        let places = (first || moved).then(|| self.places(session)).flatten();

        let Some(t) = self.tracked.get_mut(&id) else {
            return Ok(());
        };
        if first && t.row.meta.is_some() {
            t.parent = Some(parent);
        }
        let event = |id, at, body| Event {
            id,
            at,
            workspace,
            author: owner,
            on_behalf_of: None,
            body,
        };
        let mut events: Vec<(Option<u64>, Event)> = Vec::new();
        if first {
            // Hooks that came first are folded in, oldest first, unless the transcript is newer.
            let mut changed = false;
            for r in &held {
                changed |= derive::report(&mut t.row.facts, r).is_some();
            }
            let ended = changed && t.row.facts.state == SessionState::Ended;
            let s = session_of(&t.row, &t.tref, machine, parent, terminal);
            let at = discovered_id_time(&t.row);
            events.push((
                None,
                event(
                    event_id(session, 0, Cause::Discovered, 0, at),
                    s.started,
                    EventBody::SessionDiscovered { session: s },
                ),
            ));
            if ended {
                t.row.facts.reports += 1;
                let n = t.row.facts.reports;
                let at = t.row.facts.reported_at.unwrap_or(at);
                events.push((
                    None,
                    event(
                        event_id(session, t.row.generation, Cause::Report(n), 0, at),
                        at,
                        EventBody::SessionEnded { session },
                    ),
                ));
            }
            t.row.discovered = true;
        }
        if let Some((locations, stands)) = &places {
            let at = if first {
                discovered_id_time(&t.row)
            } else {
                crate::now_ms()
            };
            if let Some(e) = link_event(&mut t.row, machine, locations, *stands, at) {
                events.push((None, event(e.0, e.1, e.2)));
            }
        }
        let mut last: Option<u64> = None;
        let mut seq = 0u32;
        for d in derived {
            seq = if last == Some(d.key) { seq + 1 } else { 0 };
            last = Some(d.key);
            let id = event_id(session, t.row.generation, Cause::Item(d.key), seq, d.at);
            events.push((Some(d.key), event(id, d.at, d.body)));
        }
        t.row.cursor = chunk.cursor;
        t.row.size = st.size;
        t.row.mtime = st.mtime;
        t.row.identity.clone_from(&st.identity);
        t.row.caught_up = caught_up;
        if caught_up {
            // The replay after a crash is over.
            t.row.accepted.clear();
        }

        let batches = split(events, self.max_batch, &t.row);
        for b in batches {
            self.tx.send(b).map_err(|_| Hangup)?;
        }
        Ok(())
    }

    /// Applies reported states: at once for indexed sessions or, for a hook, held until the
    /// session's transcript is found. A hook applies only if its sender may change the session.
    fn apply_reports(&mut self, reports: Vec<Signal>) -> Result<(), Hangup> {
        for s in reports {
            let id = match &s.target {
                Target::Native { engine, native_id } => {
                    self.by_native.get(&(*engine, native_id.clone())).copied()
                }
                Target::Session(session) => self.by_session.get(session).copied(),
            };
            let indexed = id.and_then(|id| {
                self.tracked
                    .get(&id)
                    .filter(|t| t.row.discovered)
                    .map(|t| (id, t.row.session))
            });
            tracing::debug!(target = ?s.target, report = ?s.report, origin = ?s.origin, tracked = ?id, "reported state");
            match (indexed, s.origin) {
                (Some((id, _)), Origin::Runner) => self.apply_report(id, &s.report)?,
                (Some((id, session)), Origin::Hook(sender)) => {
                    let parent = self.parent_for(id);
                    match hooks::refusal(&sender, &self.runs_as(session, parent)) {
                        None => self.apply_report(id, &s.report)?,
                        Some(reason) => {
                            tracing::debug!(%session, target = ?s.target, member = %sender.member(), reason, "hook refused; dropped");
                        }
                    }
                }
                (None, Origin::Hook(sender)) => match s.target {
                    Target::Native { engine, native_id } => {
                        self.hold(engine, native_id, sender, s.report);
                    }
                    Target::Session(session) => {
                        tracing::debug!(%session, "a hook for a session not indexed; ignored");
                    }
                },
                (None, Origin::Runner) => {
                    tracing::debug!(target = ?s.target, "a reported state for a session not indexed; ignored");
                }
            }
        }
        Ok(())
    }

    /// The session's agent, from the [`SessionAgents`]: unknown without them, or if they panic.
    /// Called on this thread, which is why it must never call back into the runner.
    fn agent_of(&mut self, session: SessionId) -> SessionAgent {
        let Some(agents) = self.agents.clone() else {
            return SessionAgent::Unknown;
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| agents.agent_of(session))) {
            Ok(agent) => agent,
            Err(panic) => {
                let message = panic_text(&*panic);
                if self.lookup_panicked {
                    tracing::debug!(%session, panic = message, "the session agent lookup panicked; the agent is unknown");
                } else {
                    tracing::warn!(%session, panic = message, "the session agent lookup panicked; the agent is unknown, and the hook refused");
                    self.lookup_panicked = true;
                }
                SessionAgent::Unknown
            }
        }
    }

    /// Who a session runs as, for its hooks: its own agent as the [`SessionAgents`] tell, or, for
    /// a sub-agent they say has none, its parent's. A sub-agent runs as its parent, and the hub
    /// may not have stored it yet: at its discovery, and until the sink's write lands, only its
    /// parent's agent is known. Any other answer (an agent, or unknown) stands.
    fn runs_as(&mut self, session: SessionId, parent: Option<SessionId>) -> SessionAgent {
        match (self.agent_of(session), parent) {
            (SessionAgent::NoAgent, Some(parent)) => self.agent_of(parent),
            (own, _) => own,
        }
    }

    /// The parent session of a tracked sub-agent, looked up once its transcript says it is one.
    fn parent_for(&mut self, id: u64) -> Option<SessionId> {
        let t = self.tracked.get(&id)?;
        if let Some(parent) = t.parent {
            return parent;
        }
        let meta = t.row.meta.as_ref()?;
        let parent = if meta.is_subagent {
            let (engine, path) = (t.row.engine, t.row.path.clone());
            self.parent_of(engine, &path)
        } else {
            None
        };
        if let Some(t) = self.tracked.get_mut(&id) {
            t.parent = Some(parent);
        }
        parent
    }

    /// The hooks held for a newly discovered session whose senders may apply them, oldest
    /// first. The others are dropped. A sub-agent's are judged as its parent's (see `runs_as`).
    fn allowed_held(
        &mut self,
        session: SessionId,
        parent: Option<SessionId>,
        engine: Engine,
        native: &str,
    ) -> Vec<Reported> {
        let held = self.held.take(engine, native, Instant::now());
        if held.is_empty() {
            return Vec::new();
        }
        let agent = self.runs_as(session, parent);
        held.into_iter()
            .filter_map(|(sender, report)| match hooks::refusal(&sender, &agent) {
                None => Some(report),
                Some(reason) => {
                    tracing::debug!(%session, member = %sender.member(), reason, "a held hook refused at discovery; dropped");
                    None
                }
            })
            .collect()
    }

    fn apply_report(&mut self, id: u64, r: &Reported) -> Result<(), Hangup> {
        let (workspace, owner) = (self.workspace, self.owner);
        let Some(t) = self.tracked.get_mut(&id) else {
            return Ok(());
        };
        let Some(from) = derive::report(&mut t.row.facts, r) else {
            return Ok(());
        };
        t.row.facts.reports += 1;
        let (session, generation, n) = (t.row.session, t.row.generation, t.row.facts.reports);
        let events = derive::reported_events(session, from, &t.row.facts)
            .into_iter()
            .zip(0u32..)
            .map(|(body, seq)| Event {
                id: event_id(session, generation, Cause::Report(n), seq, r.at),
                at: r.at,
                workspace,
                author: owner,
                on_behalf_of: None,
                body,
            })
            .collect();
        self.tx
            .send(Batch {
                events,
                commit: Commit::Full(Box::new(t.row.clone())),
            })
            .map_err(|_| Hangup)
    }

    /// Holds a hook for a session whose transcript is not indexed yet (see `held`), and looks
    /// for it soon, unless its sender asked for a look just now.
    fn hold(&mut self, engine: Engine, native_id: String, sender: Sender, report: Reported) {
        let now = Instant::now();
        if self.held.hold(engine, native_id, sender, report, now) {
            // Its transcript may have just appeared.
            self.look_soon(now);
        }
    }

    /// Looks for new transcripts soon in the homes that are not slow, at most once per
    /// [`REDISCOVER_GAP`]. Slow (network) homes keep their own schedule.
    fn look_soon(&self, now: Instant) {
        let at = self
            .last_rediscover
            .map_or(now, |l| after(l, REDISCOVER_GAP))
            .max(after(now, self.timing.debounce));
        let mut s = self.shared.lock();
        s.look_at = Some(s.look_at.map_or(at, |r| r.min(at)));
    }

    /// Workstream locations and the session's standing link, when linking is on.
    fn places(&self, session: SessionId) -> Option<(Vec<WorkstreamLocation>, Option<LinkBasis>)> {
        let locations = self.locations.as_ref()?;
        Some((locations.locations(), locations.link_of(session)))
    }

    /// Links every session again, after the locations changed.
    fn relink_all(&mut self) -> Result<(), Hangup> {
        let Some(locations) = self.locations.clone() else {
            return Ok(());
        };
        let all = locations.locations();
        let (workspace, owner, machine) = (self.workspace, self.owner, self.machine);
        let ids: Vec<u64> = self.tracked.keys().copied().collect();
        for id in ids {
            self.serve_due()?;
            let Some(t) = self.tracked.get_mut(&id) else {
                continue;
            };
            if !t.row.discovered {
                continue;
            }
            let stands = locations.link_of(t.row.session);
            let Some((eid, at, body)) =
                link_event(&mut t.row, machine, &all, stands, crate::now_ms())
            else {
                continue;
            };
            let event = Event {
                id: eid,
                at,
                workspace,
                author: owner,
                on_behalf_of: None,
                body,
            };
            self.tx
                .send(Batch {
                    events: vec![event],
                    commit: Commit::Full(Box::new(t.row.clone())),
                })
                .map_err(|_| Hangup)?;
        }
        Ok(())
    }

    /// For a Claude sub-agent's transcript, its parent's session. A parent not indexed yet gets
    /// its session id now, which its discovery then keeps.
    fn parent_of(&self, engine: Engine, path: &Path) -> Option<SessionId> {
        let parent = parent_transcript(path)?.canonicalize().ok()?;
        if let Some(t) = self
            .by_key
            .get(&(parent.clone(), None))
            .and_then(|id| self.tracked.get(id))
        {
            return Some(t.row.session);
        }
        let store = self.store_lock();
        match store.find(&parent, None) {
            Ok(Some(row)) => Some(row.session),
            Ok(None) => {
                let row = new_row(&TranscriptRef {
                    engine,
                    path: parent,
                    inner_id: None,
                    size: 0,
                    modified: 0,
                });
                match store.insert(&row) {
                    Ok(()) => Some(row.session),
                    Err(e) => {
                        tracing::warn!(path = %row.path.display(), error = %e, "cannot index a sub-agent's parent");
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(path = %parent.display(), error = %e, "cannot look up a sub-agent's parent");
                None
            }
        }
    }

    /// The terminal the runner started a newly discovered session in, if it did.
    fn claim_terminal(
        &self,
        session: SessionId,
        engine: Engine,
        native: &str,
        cwd: Option<&str>,
        started: Option<TimestampMs>,
    ) -> Option<TerminalId> {
        let now = crate::now_ms();
        self.store_lock()
            .claim_terminal(session, engine, native, cwd, started.unwrap_or(now), now)
            .unwrap_or_else(|e| {
                tracing::warn!(%session, error = %e, "cannot look up the session's terminal");
                None
            })
    }

    fn store_lock(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct Wake {
    /// Due paths, each with whether it may be a new file.
    due: Vec<(PathBuf, bool)>,
    overflow: bool,
    /// Discover in every home.
    rediscover: bool,
    /// Discover in the homes that are not slow.
    look: bool,
    reports: Vec<Signal>,
    relink: bool,
}

/// Whether a home is polled, and whether it is slow (a network filesystem, swept and rediscovered
/// less often). Without working notifications every home is polled.
fn poll_decision(mode: PollMode, home: &Path, notify_works: bool) -> (bool, bool) {
    match mode {
        PollMode::Always => (true, true),
        PollMode::Never => (!notify_works, false),
        PollMode::Auto => {
            let network = fsinfo::is_network_fs(home);
            (network || !notify_works, network)
        }
    }
}

/// The folders a transcript needs watched: those up to [`ALWAYS_WATCH_DEPTH`] levels below its
/// home that lead to it and, while it is hot, its own folder and that folder's parent. The home
/// itself is watched by its [`Home`]. Outside its home, only its own folder, while hot.
fn wanted_dirs(path: &Path, home: &Path, hot: bool) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    if !parent.starts_with(home) {
        return if hot {
            vec![parent.to_path_buf()]
        } else {
            Vec::new()
        };
    }
    let mut out = Vec::new();
    for (i, dir) in path.ancestors().skip(1).enumerate() {
        let Ok(below) = dir.strip_prefix(home) else {
            break;
        };
        let depth = below.components().count();
        if depth == 0 {
            break;
        }
        if depth <= ALWAYS_WATCH_DEPTH || (hot && i < 2) {
            out.push(dir.to_path_buf());
        }
    }
    out
}

/// A home and its first-level folders (e.g. `projects`), or nothing while it does not exist.
fn home_dirs(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    let mut dirs = vec![home.to_path_buf()];
    dirs.extend(
        entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path()),
    );
    dirs
}

fn is_not_found(e: &notify::Error) -> bool {
    match &e.kind {
        notify::ErrorKind::PathNotFound => true,
        notify::ErrorKind::Io(io) => io.kind() == io::ErrorKind::NotFound,
        _ => false,
    }
}

/// Drops the oldest waiting signal of the sender with the most waiting (of those tied, the
/// highest member id, a hook's before the runner's).
fn drop_one(reports: &mut VecDeque<Signal>) {
    let mut counts: HashMap<Option<MemberId>, usize> = HashMap::new();
    for s in reports.iter() {
        *counts.entry(s.origin.member()).or_default() += 1;
    }
    let Some(most) = counts
        .into_iter()
        .max_by_key(|(m, n)| (*n, *m))
        .map(|(m, _)| m)
    else {
        return;
    };
    if let Some(i) = reports.iter().position(|s| s.origin.member() == most) {
        reports.remove(i);
    }
}

/// `now + d`, without overflowing on absurd durations (a year stands in).
fn after(now: Instant, d: Duration) -> Instant {
    now.checked_add(d)
        .or_else(|| now.checked_add(Duration::from_secs(365 * 24 * 60 * 60)))
        .unwrap_or(now)
}

/// An adapter call that failed or panicked.
#[derive(Debug, thiserror::Error)]
enum AdapterError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("the source adapter panicked: {0}")]
    Panic(String),
}

/// Runs an adapter call, turning a panic into an error: one bad transcript must not stop the
/// watcher.
fn guard<T>(call: impl FnOnce() -> Result<T, SourceError>) -> Result<T, AdapterError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(result) => result.map_err(AdapterError::Source),
        Err(panic) => Err(AdapterError::Panic(panic_text(&*panic).to_owned())),
    }
}

/// The message of a caught panic (`panic!` gives a `&str` or a `String`).
fn panic_text(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

fn new_row(tref: &TranscriptRef) -> Row {
    Row {
        session: SessionId::new(),
        engine: tref.engine,
        path: tref.path.clone(),
        inner_id: tref.inner_id.clone(),
        cursor: Cursor::default(),
        size: 0,
        mtime: 0,
        identity: None,
        caught_up: false,
        generation: 0,
        discovered: false,
        accepted: HashSet::new(),
        meta: None,
        facts: Facts::default(),
    }
}

/// A smaller file, or a different file (inode) at the same path, means it was truncated or
/// replaced. Multi-session stores (`inner_id`, OpenCode's database) change size for other
/// reasons, such as a revert deleting parts, and are not judged this way; but a file that was
/// deleted and came back is new for everyone.
fn needs_reindex(row: &Row, st: &FileStat) -> bool {
    let read_before = row.cursor != Cursor::default();
    let gone = row.identity.as_deref() == Some(GONE);
    let replaced = !fsinfo::same_file(row.identity.as_deref(), st.identity.as_deref());
    let single = row.inner_id.is_none() && row.engine != Engine::OpenCode;
    read_before && (gone || (single && (st.size < row.size || replaced)))
}

fn reindex(row: &mut Row, st: &FileStat) {
    tracing::warn!(
        path = %row.path.display(),
        session = %row.session,
        was = row.size,
        now = st.size,
        "transcript was truncated or replaced; re-indexing it from the start"
    );
    row.cursor = Cursor::default();
    row.generation += 1;
    row.accepted.clear();
    row.facts.open_calls.clear();
}

/// Splits events into batches of about `max`, never splitting one item's events, so a partial
/// commit's accepted items are whole. The last batch saves the full row.
fn split(events: Vec<(Option<u64>, Event)>, max: usize, row: &Row) -> Vec<Batch> {
    let mut out = Vec::new();
    let mut current: Vec<Event> = Vec::new();
    let mut keys: Vec<u64> = Vec::new();
    let mut last_key: Option<u64> = None;
    for (key, ev) in events {
        if current.len() >= max && key.is_some() && key != last_key {
            out.push(Batch {
                events: std::mem::take(&mut current),
                commit: Commit::Partial {
                    session: row.session,
                    keys: std::mem::take(&mut keys),
                },
            });
        }
        if let Some(k) = key {
            if last_key != key {
                keys.push(k);
            }
            last_key = key;
        }
        current.push(ev);
    }
    out.push(Batch {
        events: current,
        commit: Commit::Full(Box::new(row.clone())),
    });
    out
}

/// What caused an event, for its id.
#[derive(Clone, Copy, Debug)]
enum Cause {
    /// The session's discovery.
    Discovered,
    /// A transcript item, by its key.
    Item(u64),
    /// The n-th reported state.
    Report(u32),
    /// The n-th link.
    Link(u32),
}

/// A ULID whose time is the event's and whose random part is a hash of what caused it, so an
/// event sent again after a crash has the same id.
fn event_id(
    session: SessionId,
    generation: u32,
    cause: Cause,
    seq: u32,
    at: TimestampMs,
) -> EventId {
    let mut h = Sha256::new();
    h.update(session.0.to_bytes());
    h.update(generation.to_le_bytes());
    match cause {
        Cause::Discovered => h.update([0]),
        Cause::Item(k) => {
            h.update([1]);
            h.update(k.to_le_bytes());
        }
        Cause::Report(n) => {
            h.update([2]);
            h.update(n.to_le_bytes());
        }
        Cause::Link(n) => {
            h.update([3]);
            h.update(n.to_le_bytes());
        }
    }
    h.update(seq.to_le_bytes());
    let digest = h.finalize();
    let mut random = [0u8; 16];
    random[6..].copy_from_slice(&digest[..10]);
    EventId(Ulid::from_parts(
        u64::try_from(at).unwrap_or(0),
        u128::from_be_bytes(random),
    ))
}

/// The time in a `session_discovered` id: the session's start or, when the transcript has none,
/// when the runner first saw it (its session id's time). Never how much of the file was read, so
/// a replay after a crash repeats the id.
fn discovered_id_time(row: &Row) -> TimestampMs {
    row.meta
        .as_ref()
        .and_then(|m| m.started)
        .unwrap_or_else(|| TimestampMs::try_from(row.session.0.timestamp_ms()).unwrap_or(0))
}

/// The CLI's id for the session: from its records, else the store's inner id, else the file name.
fn native_id(row: &Row) -> String {
    row.meta
        .as_ref()
        .map(|m| m.native_id.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| row.inner_id.clone())
        .or_else(|| {
            row.path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

fn session_of(
    row: &Row,
    tref: &TranscriptRef,
    machine: MachineId,
    parent: Option<SessionId>,
    terminal: Option<TerminalId>,
) -> Session {
    let meta = row.meta.clone().unwrap_or_default();
    let last_activity = if row.facts.last_activity > 0 {
        row.facts.last_activity
    } else {
        tref.modified
    };
    Session {
        id: row.session,
        engine: row.engine,
        native_id: native_id(row),
        machine,
        cwd: meta.cwd.unwrap_or_default(),
        branch: meta.branch,
        title: meta.title,
        agent: None,
        workstream: None,
        task: None,
        link_basis: None,
        state: row.facts.state,
        status_line: row.facts.status_line.clone(),
        started: meta.started.unwrap_or(last_activity),
        last_activity,
        terminal,
        parent,
    }
}

/// For a Claude sub-agent transcript `<project>/<session>/subagents/<agent>.jsonl`, its parent's
/// transcript `<project>/<session>.jsonl`.
fn parent_transcript(path: &Path) -> Option<PathBuf> {
    let subagents = path.parent()?;
    if subagents.file_name()? != "subagents" {
        return None;
    }
    let session_dir = subagents.parent()?;
    let mut name = session_dir.file_name()?.to_os_string();
    name.push(".jsonl");
    Some(session_dir.with_file_name(name))
}

/// The `session_linked` event for a new link, if the session should have one; records it in the
/// row. Its id's time is the session's discovery time, so a replay repeats the id.
fn link_event(
    row: &mut Row,
    machine: MachineId,
    locations: &[WorkstreamLocation],
    stands: Option<LinkBasis>,
    at: TimestampMs,
) -> Option<(EventId, TimestampMs, EventBody)> {
    let meta = row.meta.as_ref()?;
    let cwd = meta.cwd.as_deref()?;
    let linked = link::relink(
        machine,
        locations,
        stands,
        cwd,
        meta.branch.as_deref(),
        row.facts.linked,
    )?;
    row.facts.linked = Some(linked);
    row.facts.links += 1;
    let id = event_id(
        row.session,
        0,
        Cause::Link(row.facts.links),
        0,
        discovered_id_time(row),
    );
    Some((
        id,
        at,
        EventBody::SessionLinked {
            session: row.session,
            workstream: Some(linked.workstream),
            task: None,
            basis: linked.basis,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, DataChange, Flag};

    fn row() -> Row {
        Row {
            session: SessionId::new(),
            engine: Engine::Claude,
            path: "/t/a.jsonl".into(),
            inner_id: None,
            cursor: Cursor {
                offset: 10,
                state: None,
            },
            size: 10,
            mtime: 1,
            identity: Some("1:1".into()),
            caught_up: true,
            generation: 0,
            discovered: true,
            accepted: HashSet::new(),
            meta: None,
            facts: Facts::default(),
        }
    }

    fn stat(size: u64, identity: &str) -> FileStat {
        FileStat {
            size,
            mtime: 2,
            identity: Some(identity.into()),
        }
    }

    #[test]
    fn reindex_on_shrink_or_replacement_only() {
        let r = row();
        assert!(!needs_reindex(&r, &stat(20, "1:1")));
        assert!(needs_reindex(&r, &stat(5, "1:1")));
        assert!(needs_reindex(&r, &stat(20, "1:2")));
        let mut multi = row();
        multi.inner_id = Some("s".into());
        assert!(!needs_reindex(&multi, &stat(5, "1:2")));
        // OpenCode's database shrinks when a revert deletes parts: not a truncation.
        let mut opencode = row();
        opencode.engine = Engine::OpenCode;
        assert!(!needs_reindex(&opencode, &stat(5, "1:2")));
        let mut fresh = row();
        fresh.cursor = Cursor::default();
        assert!(!needs_reindex(&fresh, &stat(5, "1:2")));
        // Deleted and back, even with the old inode, and even for a multi-session store.
        let mut gone = row();
        gone.identity = Some(GONE.into());
        assert!(needs_reindex(&gone, &stat(20, "1:1")));
        multi.identity = Some(GONE.into());
        assert!(needs_reindex(&multi, &stat(20, "1:1")));
    }

    #[test]
    fn a_new_device_number_is_not_a_replacement() {
        // After a reboot or an NFS remount `st_dev` differs; same inode, same size.
        let r = row();
        assert!(!needs_reindex(&r, &stat(10, "7:1")));
        assert!(!needs_reindex(&r, &stat(20, "7:1")));
        // The shrink rule still applies.
        assert!(needs_reindex(&r, &stat(5, "7:1")));
    }

    #[test]
    fn event_ids_repeat_for_the_same_origin() {
        let s = SessionId::new();
        let a = event_id(s, 0, Cause::Item(10), 0, 1000);
        assert_eq!(a, event_id(s, 0, Cause::Item(10), 0, 1000));
        assert_ne!(a, event_id(s, 0, Cause::Item(10), 1, 1000));
        assert_ne!(a, event_id(s, 1, Cause::Item(10), 0, 1000));
        assert_ne!(a, event_id(s, 0, Cause::Discovered, 0, 1000));
        assert_ne!(a, event_id(s, 0, Cause::Report(10), 0, 1000));
        assert_ne!(
            event_id(s, 0, Cause::Report(1), 0, 1000),
            event_id(s, 0, Cause::Link(1), 0, 1000)
        );
        assert_eq!(a.0.timestamp_ms(), 1000);
    }

    #[test]
    fn a_sub_agent_transcript_names_its_parent() {
        let p = Path::new("/h/projects/-w/abc/subagents/agent-1.jsonl");
        assert_eq!(
            parent_transcript(p),
            Some(PathBuf::from("/h/projects/-w/abc.jsonl"))
        );
        assert_eq!(
            parent_transcript(Path::new("/h/projects/-w/abc.jsonl")),
            None
        );
    }

    #[test]
    fn a_discovered_id_without_a_start_time_uses_the_session_id_time() {
        let mut r = row();
        let first_seen = TimestampMs::try_from(r.session.0.timestamp_ms()).unwrap_or(0);
        assert_eq!(discovered_id_time(&r), first_seen);
        // How much was read (last activity) does not move it.
        r.facts.last_activity = 5;
        assert_eq!(discovered_id_time(&r), first_seen);
        r.meta = Some(pitcrew_interfaces::source::SessionMeta {
            started: Some(42),
            ..Default::default()
        });
        assert_eq!(discovered_id_time(&r), 42);
    }

    #[test]
    fn batches_split_between_offsets_only() {
        let r = row();
        let ev = |k: u64| {
            (
                Some(k),
                Event::now(
                    WorkspaceId::new(),
                    MemberId::new(),
                    EventBody::SessionEnded { session: r.session },
                ),
            )
        };
        // Keys are item keys, not offsets: they need not grow.
        let batches = split(vec![ev(9), ev(9), ev(9), ev(2), ev(3)], 2, &r);
        let sizes: Vec<usize> = batches.iter().map(|b| b.events.len()).collect();
        assert_eq!(sizes, [3, 2]);
        assert_eq!(
            batches[0].commit,
            Commit::Partial {
                session: r.session,
                keys: vec![9]
            }
        );
        assert!(matches!(batches[1].commit, Commit::Full(_)));
    }

    #[test]
    fn watched_folders_follow_depth_and_heat() {
        let home = Path::new("/h");
        let p = |s: &str| PathBuf::from(s);
        let top = Path::new("/h/projects/p/s.jsonl");
        let top_dirs = [p("/h/projects/p"), p("/h/projects")];
        assert_eq!(wanted_dirs(top, home, false), top_dirs);
        assert_eq!(wanted_dirs(top, home, true), top_dirs);
        // Sub-agent folders are deeper: watched only while hot.
        let sub = Path::new("/h/projects/p/s/subagents/a.jsonl");
        assert_eq!(wanted_dirs(sub, home, false), top_dirs);
        assert_eq!(
            wanted_dirs(sub, home, true),
            [
                p("/h/projects/p/s/subagents"),
                p("/h/projects/p/s"),
                p("/h/projects/p"),
                p("/h/projects")
            ]
        );
        // Directly in the home: the home's own watch covers it.
        assert!(wanted_dirs(Path::new("/h/s.jsonl"), home, true).is_empty());
        // Outside the home: its own folder, while hot.
        let out = Path::new("/scratch/p/s.jsonl");
        assert!(wanted_dirs(out, home, false).is_empty());
        assert_eq!(wanted_dirs(out, home, true), [p("/scratch/p")]);
    }

    #[test]
    fn a_flood_of_signals_costs_only_its_sender() {
        use pitcrew_protocol::api::{Caller, TokenScope};
        let sender = || {
            Sender::new(Caller {
                member: MemberId::new(),
                scope: TokenScope::Agent,
                on_behalf_of: Some(MemberId::new()),
            })
        };
        let signal = |native_id: &str, origin| Signal {
            target: Target::Native {
                engine: Engine::Claude,
                native_id: native_id.into(),
            },
            report: Reported {
                at: 1,
                to: SessionState::Ended,
                status_line: None,
            },
            origin,
        };
        let (person, flood) = (sender(), sender());
        let shared = Shared::default();
        shared.signal(signal("mine", Origin::Hook(person)));
        shared.signal(signal("ended", Origin::Runner));
        for i in 0..3 * MAX_SIGNALS {
            shared.signal(signal(&format!("f{i}"), Origin::Hook(flood)));
        }
        let s = shared.lock();
        assert_eq!(s.reports.len(), MAX_SIGNALS);
        let kept = |origin| s.reports.iter().filter(|r| r.origin == origin).count();
        assert_eq!(kept(Origin::Hook(person)), 1);
        assert_eq!(kept(Origin::Runner), 1);
        assert_eq!(kept(Origin::Hook(flood)), MAX_SIGNALS - 2);
        // The flood kept its newest.
        assert_eq!(
            s.reports.back().map(|r| r.target.clone()),
            Some(Target::Native {
                engine: Engine::Claude,
                native_id: format!("f{}", 3 * MAX_SIGNALS - 1)
            })
        );
    }

    #[test]
    fn a_tie_among_waiting_signals_drops_from_the_highest_member() {
        use pitcrew_protocol::api::{Caller, TokenScope};
        let signal = |n: u128, id: &str| Signal {
            target: Target::Native {
                engine: Engine::Claude,
                native_id: id.into(),
            },
            report: Reported {
                at: 1,
                to: SessionState::Idle,
                status_line: None,
            },
            origin: Origin::Hook(Sender::new(Caller {
                member: MemberId(Ulid::from_parts(1, n)),
                scope: TokenScope::Agent,
                on_behalf_of: None,
            })),
        };
        let left = |q: &VecDeque<Signal>| -> Vec<String> {
            q.iter()
                .map(|s| match &s.target {
                    Target::Native { native_id, .. } => native_id.clone(),
                    Target::Session(id) => id.to_string(),
                })
                .collect()
        };
        for _ in 0..20 {
            let mut q: VecDeque<Signal> = [
                signal(1, "a1"),
                signal(2, "b1"),
                signal(1, "a2"),
                signal(2, "b2"),
            ]
            .into();
            drop_one(&mut q);
            assert_eq!(left(&q), ["a1", "a2", "b2"]);
        }
    }

    #[test]
    fn lost_events_force_a_full_check() {
        let shared = Arc::new(Shared::default());
        let mut handler = notify_handler(Arc::clone(&shared), Duration::from_millis(100));
        handler(Ok(
            notify::Event::new(EventKind::Other).set_flag(Flag::Rescan)
        ));
        let s = shared.lock();
        assert!(s.overflow);
        assert!(s.rediscover_at.is_some());
    }

    #[test]
    fn only_creations_can_announce_new_files() {
        let shared = Arc::new(Shared::default());
        let mut handler = notify_handler(Arc::clone(&shared), Duration::from_millis(100));
        let ev = |kind, path: &str| Ok(notify::Event::new(kind).add_path(path.into()));
        handler(ev(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            "/h/a",
        ));
        handler(ev(EventKind::Create(CreateKind::File), "/h/b"));
        handler(ev(EventKind::Access(AccessKind::Any), "/h/c"));
        let s = shared.lock();
        let created = |p: &str| s.dirty.get(Path::new(p)).map(|d| d.created);
        assert_eq!(created("/h/a"), Some(false));
        assert_eq!(created("/h/b"), Some(true));
        assert_eq!(created("/h/c"), None);
        assert!(!s.overflow);
    }

    #[test]
    fn an_adapter_panic_becomes_an_error() {
        let r: Result<(), AdapterError> = guard(|| panic!("bad record"));
        assert!(matches!(r, Err(AdapterError::Panic(m)) if m == "bad record"));
    }

    #[test]
    fn network_homes_are_polled_and_swept_less_often() {
        let local = Path::new("/");
        assert_eq!(poll_decision(PollMode::Always, local, true), (true, true));
        assert_eq!(poll_decision(PollMode::Never, local, true), (false, false));
        // Without notifications every home is polled, but a local one is not slowed down.
        assert_eq!(poll_decision(PollMode::Never, local, false), (true, false));

        let now = Instant::now();
        let mut home = Home {
            engine: Engine::Claude,
            configured: "/h".into(),
            path: "/h".into(),
            adapter: Arc::new(pitcrew_interfaces::fake::FakeSource::new(
                Engine::Claude,
                Vec::new(),
                Vec::new(),
            )),
            polled: true,
            slow: false,
            watched: Vec::new(),
            next_sweep: now,
            next_rediscover: now,
            discover_failing: false,
        };
        let sweep = Duration::from_secs(30);
        assert_eq!(home.every(sweep), sweep);
        home.slow = true;
        assert_eq!(home.every(sweep), sweep * NETWORK_SLOWDOWN);
    }

    #[test]
    fn huge_intervals_do_not_overflow() {
        let now = Instant::now();
        assert!(after(now, Duration::MAX) > now);
        let home_every = Duration::MAX.saturating_mul(NETWORK_SLOWDOWN);
        assert_eq!(home_every, Duration::MAX);
    }
}
