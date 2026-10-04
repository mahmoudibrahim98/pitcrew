//! The watcher: discovers transcripts, notices changes, reads from stored cursors, and turns items
//! into events for the sink.
//!
//! One thread owns all transcript state. Notifications only mark paths dirty (a map bounded by the
//! number of paths), so a slow sink stalls the reader, not memory: the watcher blocks on the
//! bounded channel, and changes that arrive meanwhile coalesce into one read per file.
//!
//! Transcripts are keyed by their canonical path, so a home reached through a symlink, or one that
//! did not exist yet at the first start, keeps its session ids across restarts. Only the folder is
//! resolved: a transcript's own name never is, so one swapped for a link is not tracked at, or
//! moved to, the link's target (`transcript_key`).
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
use crate::derive::{self, Derived, Facts, Parent, Reported};
use crate::discovery::Layout;
use crate::fsinfo::{self, FileStat};
use crate::held::Held;
use crate::hooks::{self, Sender};
use crate::link::{self, Locations, WorkstreamLocation};
use crate::pages::Source;
use crate::pages::Watched;
use crate::sink::Batch;
use crate::store::{self, Claim, Commit, Indexed, Row, Store, native_id, path_text};
use crate::terminals::{RunnerTerminals, WeakTerminals};
use notify::event::{EventKind, MetadataKind, ModifyKind};
use notify::{RecursiveMode, Watcher as _};
use pitcrew_interfaces::source::{Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptRef};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MachineId, MemberId, SessionId, TerminalId, WorkspaceId};
use pitcrew_protocol::model::{Engine, LinkBasis, Session, SessionState, TimestampMs};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use ulid::Ulid;

/// Reads of one transcript per wake-up before others get a turn.
const MAX_READS_PER_REFRESH: usize = 4096;
/// Recently used hot rows retained after saving; cold history still lives in the index.
const HOT_ROW_CACHE: usize = 64;
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
    pub(crate) busy: Arc<AtomicUsize>,
    /// Starts for sessions the hub named whose terminal is not recorded yet (see [`Pending`]).
    pending: Mutex<Vec<Pending>>,
    /// Commands starting a session the hub named, from when they are run until they return: how
    /// many for each session (see [`Shared::under_way`]).
    under_way: Mutex<HashMap<SessionId, usize>>,
    /// The terminals the runner's commands start CLIs in, once there are any: asked whether a
    /// terminal's program still runs (see [`Shared::has_ended`]).
    terminals: Mutex<Option<WeakTerminals>>,
}

/// Keeps a start for a session the hub named known as under way ([`Shared::under_way`]) until
/// dropped.
pub(crate) struct UnderWay<'a> {
    shared: &'a Shared,
    session: SessionId,
}

impl Drop for UnderWay<'_> {
    fn drop(&mut self) {
        let mut under_way = self
            .shared
            .under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(n) = under_way.get_mut(&self.session) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                under_way.remove(&self.session);
            }
        }
    }
}

/// A start for a session the hub named, from just before its program starts until its terminal
/// is recorded in the index. A CLI may write its transcript in between, and the watcher find it
/// then: it still takes the name (and its terminal is linked when it is recorded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub session: SessionId,
    pub engine: Engine,
    /// The CLI's id, when the runner chose it (Claude's `--session-id`); else it is matched by
    /// folder and start time, as a recorded terminal is.
    pub native_id: Option<String>,
    pub cwd: String,
    pub started_at: TimestampMs,
}

/// Keeps a [`Pending`] start known to the watcher until dropped.
pub(crate) struct Starting<'a> {
    shared: &'a Shared,
    session: SessionId,
}

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        self.shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|p| p.session != self.session);
    }
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
    /// An exited terminal gets one final discovery before it stops accepting transcripts.
    exit_scans: Vec<(TerminalId, SyncSender<bool>)>,
    exhausted: HashSet<TerminalId>,
}

#[derive(Clone, Copy, Debug)]
struct Dirty {
    /// When to read it.
    due: Instant,
    /// Created or renamed into place: if nothing tracks the path, it may be a new transcript.
    created: bool,
}

/// A read or delivery in flight; it ends with the work, never with a timer.
pub(crate) struct Busy(Arc<AtomicUsize>);

impl Busy {
    pub(crate) fn enter(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(count))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Signals> {
        self.signals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn mark(&self, paths: Vec<PathBuf>, due: Instant, created: bool) {
        let mut s = self.lock();
        Self::mark_locked(&mut s, paths, due, created);
        drop(s);
        self.cv.notify_one();
    }

    fn notification(
        &self,
        paths: Vec<PathBuf>,
        now: Instant,
        debounce: Duration,
        coalesce: bool,
        created: bool,
    ) {
        let mut s = self.lock();
        let due = if coalesce
            && (self.busy.load(Ordering::Acquire) > 0 || s.dirty.values().any(|d| d.due <= now))
        {
            now
        } else {
            after(now, debounce)
        };
        Self::mark_locked(&mut s, paths, due, created);
        drop(s);
        self.cv.notify_one();
    }

    fn mark_locked(s: &mut Signals, paths: Vec<PathBuf>, due: Instant, created: bool) {
        for p in paths {
            if let Some(d) = s.dirty.get_mut(&p) {
                // Keep the earliest due time: a stream of writes is read every `debounce`, not
                // starved.
                d.due = d.due.min(due);
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
        self.lost_events();
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

    /// Waits for the watcher to match transcripts already written by an exited CLI. No index
    /// or runtime lock may be held while waiting. A failed scan must not retire the terminal.
    pub fn scan_exit(&self, terminal: TerminalId) -> bool {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        {
            let mut s = self.lock();
            if s.exhausted.contains(&terminal) {
                return true;
            }
            if s.stop {
                return false;
            }
            s.exit_scans.push((terminal, tx));
            s.rediscover_at = Some(Instant::now());
        }
        self.cv.notify_one();
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or(false)
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

    /// A start for a session the hub named is under way: until the guard is dropped (once its
    /// terminal is recorded), a transcript that matches it takes its session.
    pub fn starting(&self, pending: Pending) -> Starting<'_> {
        let session = pending.session;
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pending);
        Starting {
            shared: self,
            session,
        }
    }

    /// A command starting `session`, a session the hub named, runs: until the guard is dropped
    /// (when the command returns, its terminal recorded or the start refused), the session's
    /// start is under way, and `RunnerCommands::started` answers it as running.
    pub fn under_way(&self, session: SessionId) -> UnderWay<'_> {
        *self
            .under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session)
            .or_insert(0) += 1;
        UnderWay {
            shared: self,
            session,
        }
    }

    /// Whether a command starting `session` runs now.
    pub fn is_under_way(&self, session: SessionId) -> bool {
        self.under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&session)
    }

    /// The runner's commands start CLIs in `terminals`: [`Shared::has_ended`] asks them.
    pub fn set_terminals(&self, terminals: &RunnerTerminals) {
        *self
            .terminals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(terminals.downgrade());
    }

    /// Whether `terminal`'s program has certainly ended: its runtime says so, or no longer has
    /// it. False while it runs, and when that cannot be told (no terminals yet, or a runtime that
    /// does not answer). Blocking: it asks the runtime, for at most its call timeout.
    pub fn has_ended(&self, terminal: TerminalId) -> bool {
        let terminals = self
            .terminals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(WeakTerminals::upgrade);
        terminals.is_some_and(|t| t.has_ended(terminal))
    }

    /// The session of a start under way whose CLI is `native` (an id the runner chose), or that
    /// started in `cwd` no later than `started` (with [`store::CLAIM_SLACK_MS`] of slack).
    fn pending_session(
        &self,
        engine: Engine,
        native: &str,
        cwd: Option<&str>,
        started: Option<TimestampMs>,
    ) -> Option<SessionId> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending
            .iter()
            .filter(|p| p.engine == engine)
            .find(|p| match &p.native_id {
                Some(id) => !native.is_empty() && id == native,
                None => {
                    cwd.is_some_and(|c| store::same_dir(c, &p.cwd))
                        && started.is_some_and(|s| {
                            s >= p.started_at.saturating_sub(store::CLAIM_SLACK_MS)
                        })
                }
            })
            .map(|p| p.session)
    }
}

/// The notification callback: it only marks paths.
pub(crate) fn notify_handler(
    shared: Arc<Shared>,
    debounce: Duration,
    window: Duration,
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
            shared.notification(
                ev.paths,
                Instant::now(),
                debounce,
                !window.is_zero(),
                created,
            );
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
    layout: Option<Layout>,
    /// The home and its first-level folders, while watched.
    watched: Vec<Arc<Path>>,
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

/// One transcript. Every tracked transcript has one, so it holds only what tells a change (size,
/// mtime and identity when last read), where the transcript is, how it is watched, and what routes
/// hooks to its session. The rest of its row (the cursor, the session's facts and metadata) is
/// read from the index while it is in use, and let go once the index has saved it ([`Loaded`]).
struct Tracked {
    session: SessionId,
    engine: Engine,
    /// Shared with the keys that find it, and with the transcript pages.
    path: Arc<Path>,
    inner_id: Option<Arc<str>>,
    size: u64,
    mtime: TimestampMs,
    identity: Option<Box<str>>,
    /// Reading had reached the end of the file at `size` and `mtime`.
    caught_up: bool,
    /// Its `session_discovered` was sent.
    discovered: bool,
    /// A sub-agent's transcript.
    subagent: bool,
    /// The parent its hooks are judged by, once looked up (`Facts::parent`).
    parent: Option<Parent>,
    home: usize,
    hot: bool,
    /// Folders this transcript holds a watch on, shared with `Watcher::dirs`.
    watched: Vec<Arc<Path>>,
    poll_every: Duration,
    next_poll: Instant,
    /// Size and mtime at which reading failed: not read again until the file changes. Boxed:
    /// rare, and every transcript has the field.
    failed_at: Option<Box<(u64, TimestampMs)>>,
    /// A read failure was logged as a warning; later ones are quieter until a read works.
    warned: bool,
    /// The whole row, while in use or not saved yet.
    loaded: Option<Box<Loaded>>,
}

/// A transcript's whole row in memory: ahead of the index until the sink accepts what was read and
/// the index saves it.
struct Loaded {
    row: Row,
    /// Batches sent with this row's changes that are not saved yet (see `sink::Batch`). At zero
    /// the index holds this row, so it can go.
    unsaved: Arc<AtomicUsize>,
    /// Changed since the last batch that saves it (a re-index before a read that failed, a report
    /// that moved `reported_at` but not the state): kept until a batch carries it, as every row
    /// was before the index saved only what batches carry.
    dirty: bool,
}

impl Loaded {
    fn new(row: Row) -> Box<Self> {
        Box::new(Self {
            row,
            unsaved: Arc::default(),
            dirty: false,
        })
    }

    /// The row is in a batch about to be sent: what the batch counts against.
    fn sending(&mut self) -> Arc<AtomicUsize> {
        self.dirty = false;
        Arc::clone(&self.unsaved)
    }

    /// Whether the index holds all of it, so it can go. A row in the middle of a replay after a
    /// crash (`accepted` not empty) stays: the index forgets the accepted items at the first full
    /// save, and the rest of the replay still needs them.
    fn saved(&self) -> bool {
        !self.dirty && self.row.accepted.is_empty() && self.unsaved.load(Ordering::Acquire) == 0
    }
}

impl Tracked {
    fn new(row: &Indexed, path: Arc<Path>, home: usize, timing: &Timing) -> Self {
        Self {
            session: row.session,
            engine: row.engine,
            path,
            inner_id: row.inner_id.as_deref().map(Arc::from),
            size: row.size,
            mtime: row.mtime,
            identity: row.identity.as_deref().map(Box::from),
            caught_up: row.caught_up,
            discovered: row.discovered,
            subagent: row.subagent,
            parent: row.parent,
            home,
            hot: false,
            watched: Vec::new(),
            poll_every: timing.poll_min,
            next_poll: Instant::now(),
            failed_at: None,
            warned: false,
            loaded: None,
        }
    }

    /// The whole row, if it is in memory.
    fn row(&mut self) -> Option<&mut Row> {
        self.loaded.as_deref_mut().map(|l| &mut l.row)
    }

    /// Takes in what the whole row now says.
    fn sync(&mut self) {
        let Some(l) = self.loaded.as_deref() else {
            return;
        };
        let row = &l.row;
        self.size = row.size;
        self.mtime = row.mtime;
        if self.identity.as_deref() != row.identity.as_deref() {
            self.identity = row.identity.as_deref().map(Box::from);
        }
        self.caught_up = row.caught_up;
        self.discovered = row.discovered;
        self.subagent = row.meta.as_ref().is_some_and(|m| m.is_subagent);
        self.parent = row.facts.parent;
    }

    /// The transcript as the adapter reads it, at size `size` and modification time `modified`.
    fn tref(&self, size: u64, modified: TimestampMs) -> TranscriptRef {
        TranscriptRef {
            engine: self.engine,
            path: self.path.to_path_buf(),
            inner_id: self.inner_id.as_deref().map(str::to_owned),
            size,
            modified,
        }
    }
}

/// A transcript's `(path, inner id)`.
type Key = (Arc<Path>, Option<Arc<str>>);

/// A transcript's `(path, inner id)` as discovered.
type RawKey = (PathBuf, Option<String>);

fn key(path: &Path, inner_id: Option<&str>) -> Key {
    (Arc::from(path), inner_id.map(Arc::from))
}

/// The tracked transcripts by id, in id order: a sorted vector. Ids only grow (`next_id`), so a
/// new one goes at the end, and one leaves only when its transcript is deleted. Every transcript
/// has an entry, and a `BTreeMap` fed growing keys leaves its nodes about half full: this holds
/// them close to their size, growing by an eighth at a time.
struct IdMap<T> {
    entries: Vec<(u64, T)>,
}

impl<T> Default for IdMap<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<T> IdMap<T> {
    fn find(&self, id: u64) -> Result<usize, usize> {
        // Most histories have contiguous ids. Deletion gaps keep the sorted-vector fallback.
        if let Ok(i) = usize::try_from(id)
            && self.entries.get(i).is_some_and(|(key, _)| *key == id)
        {
            return Ok(i);
        }
        self.entries.binary_search_by_key(&id, |(k, _)| *k)
    }

    fn get(&self, id: &u64) -> Option<&T> {
        self.find(*id).ok().map(|i| &self.entries[i].1)
    }

    fn get_mut(&mut self, id: &u64) -> Option<&mut T> {
        self.find(*id).ok().map(|i| &mut self.entries[i].1)
    }

    fn insert(&mut self, id: u64, value: T) {
        match self.find(id) {
            Ok(i) => self.entries[i].1 = value,
            Err(i) => {
                if self.entries.len() == self.entries.capacity() {
                    self.entries.reserve_exact(self.entries.len() / 8 + 16);
                }
                self.entries.insert(i, (id, value));
            }
        }
    }

    fn remove(&mut self, id: &u64) -> Option<T> {
        self.find(*id).ok().map(|i| self.entries.remove(i).1)
    }

    fn iter(&self) -> impl Iterator<Item = (&u64, &T)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    fn keys(&self) -> impl Iterator<Item = &u64> {
        self.entries.iter().map(|(k, _)| k)
    }

    fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|(_, v)| v)
    }
}

#[cfg(test)]
impl<T> std::ops::Index<&u64> for IdMap<T> {
    type Output = T;

    fn index(&self, id: &u64) -> &T {
        self.get(id).expect("tracked")
    }
}

/// The tracked transcripts at one path: one, or several in a store of many sessions (OpenCode's).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ids {
    One(u64),
    Many(Vec<u64>),
}

impl Ids {
    fn push(&mut self, id: u64) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, id]),
            Self::Many(ids) => ids.push(id),
        }
    }

    /// Takes `id` out. False once none is left.
    fn remove(&mut self, id: u64) -> bool {
        match self {
            Self::One(only) => *only != id,
            Self::Many(ids) => {
                ids.retain(|i| *i != id);
                !ids.is_empty()
            }
        }
    }

    fn to_vec(&self) -> Vec<u64> {
        match self {
            Self::One(id) => vec![*id],
            Self::Many(ids) => ids.clone(),
        }
    }
}

/// Why the watcher stopped early.
struct Hangup;

pub(crate) struct Watcher {
    byte_file_cursors: bool,
    cache_file_discovery: bool,
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
    rows: Vec<Indexed>,
    tracked: IdMap<Tracked>,
    next_id: u64,
    /// Rows in use or not saved yet. Saved cache entries are checked only when used again.
    loaded: Vec<u64>,
    cached: VecDeque<u64>,
    /// Canonical key → tracked transcript.
    by_key: HashMap<Key, u64>,
    /// Key as discovered through a symlink → tracked transcript, so each discovered path is
    /// canonicalized once. A path discovered as it is (canonical) is found in `by_key` instead.
    by_raw: HashMap<RawKey, u64>,
    by_path: HashMap<Arc<Path>, Ids>,
    /// Watched folders and how many holders each has.
    dirs: HashMap<Arc<Path>, usize>,
    /// Folders that could not be watched: warned once, retried on rediscovery.
    watch_failed: HashSet<PathBuf>,
    /// Discovered transcripts that cannot be indexed: warned once.
    skipped: HashSet<RawKey>,
    /// Folders outside their home that hold transcripts: warned once.
    outside: HashSet<PathBuf>,
    last_rediscover: Option<Instant>,
    /// Workstream locations, to link sessions to.
    locations: Option<Arc<dyn Locations>>,
    /// The CLI's session id → tracked transcript, for sessions; sub-agents have their own map,
    /// looked up after it (see `map_native`).
    by_native: HashMap<(Engine, Box<str>), u64>,
    /// A sub-agent's CLI id → tracked transcript.
    by_sub_native: HashMap<(Engine, Box<str>), u64>,
    by_session: HashMap<SessionId, u64>,
    /// Hooks for sessions not indexed yet.
    held: Held,
    /// Who runs each session; without it every hook is refused.
    agents: Option<Arc<dyn SessionAgents>>,
    /// The agent lookup panicked; warned once.
    lookup_panicked: bool,
    /// The tracked transcripts by session, for transcript pages.
    watched: Arc<Watched>,
}

pub(crate) struct Setup {
    pub notification_window: Duration,
    pub byte_file_cursors: bool,
    pub cache_file_discovery: bool,
    pub workspace: WorkspaceId,
    pub machine: MachineId,
    pub owner: MemberId,
    pub timing: Timing,
    pub poll: PollMode,
    pub max_batch: usize,
    pub homes: Vec<EngineHome>,
    pub adapters: Vec<Arc<dyn SourceAdapter>>,
    pub rows: Vec<Indexed>,
    pub store: Arc<Mutex<Store>>,
    pub tx: SyncSender<Batch>,
    pub shared: Arc<Shared>,
    pub locations: Option<Arc<dyn Locations>>,
    pub agents: Option<Arc<dyn SessionAgents>>,
    pub watched: Arc<Watched>,
}

impl Watcher {
    pub fn new(s: Setup) -> Self {
        let notify = if s.poll == PollMode::Always {
            None
        } else {
            match notify::recommended_watcher(notify_handler(
                Arc::clone(&s.shared),
                s.timing.debounce,
                s.notification_window,
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
                layout: None,
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
            cache_file_discovery: s.cache_file_discovery,
            byte_file_cursors: s.byte_file_cursors,
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
            tracked: IdMap::default(),
            next_id: 0,
            loaded: Vec::new(),
            cached: VecDeque::new(),
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
            by_sub_native: HashMap::new(),
            by_session: HashMap::new(),
            held: Held::default(),
            agents: s.agents,
            lookup_panicked: false,
            watched: s.watched,
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
        rows.sort_by_key(|row| std::cmp::Reverse(row.mtime));
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
                .contains_key(&key(&row.path, row.inner_id.as_deref()))
            {
                tracing::warn!(path = %row.path.display(), session = %row.session, "a second index row for one transcript; ignoring it");
                continue;
            }
            let (size, modified) = (row.size, row.mtime);
            self.track(&row, None, home, size, modified);
        }
        let ids: Vec<u64> = self.tracked.keys().copied().collect();
        for id in ids {
            self.serve_due()?;
            self.check(id)?;
        }
        let all: Vec<usize> = (0..self.homes.len()).collect();
        self.rediscover(&all, true)
    }

    /// A stored path whose canonical form changed (a folder above it became a symlink) moves to
    /// the new form, so the transcript keeps its session. A transcript that is now a link, or
    /// anything else but a regular file, is never moved to where it points: the row stays, and
    /// reads refuse the path until a regular file is back. False if the row can't be used.
    fn follow_canonical(&self, row: &mut Indexed) -> bool {
        let canonical = match transcript_key(&row.path) {
            Ok(canonical) => canonical,
            Err(e) => {
                if e.kind() == io::ErrorKind::InvalidInput {
                    tracing::warn!(path = %row.path.display(), "a transcript is no longer a regular file (a link?); not following it");
                }
                return true;
            }
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
        if self.homes.iter().any(|h| h.polled) {
            for t in self.tracked.values() {
                if t.hot && self.homes[t.home].polled {
                    deadline = deadline.min(t.next_poll);
                }
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
                    _busy: (!due.is_empty()).then(|| Busy::enter(&self.shared.busy)),
                    due,
                    overflow,
                    rediscover,
                    look,
                    reports: s.reports.drain(..).collect(),
                    relink: std::mem::take(&mut s.relink),
                    exit_scans: std::mem::take(&mut s.exit_scans),
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
            match self.by_path.get(path.as_path()).map(Ids::to_vec) {
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
            self.rediscover(&due, wake.rediscover || wake.look || wake.overflow)?;
        }
        for h in 0..self.homes.len() {
            if wake.overflow || self.homes[h].next_sweep <= now {
                self.sweep(h)?;
            }
        }
        let polls: Vec<u64> = if self.homes.iter().any(|h| h.polled) {
            self.tracked
                .iter()
                .filter(|(_, t)| t.hot && self.homes[t.home].polled && t.next_poll <= now)
                .map(|(id, _)| *id)
                .collect()
        } else {
            Vec::new()
        };
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
        if !wake.exit_scans.is_empty() {
            for (terminal, reply) in wake.exit_scans {
                let closed = self.exit_scan_complete(terminal)
                    && self.store_lock().close_folder_claim(terminal).is_ok();
                if closed {
                    self.shared.lock().exhausted.insert(terminal);
                }
                let _ = reply.send(closed);
            }
        }
        self.let_go();
        Ok(())
    }

    /// Only this terminal's engine and possible transcripts can hold its final scan open.
    fn exit_scan_complete(&mut self, terminal: TerminalId) -> bool {
        let terminals = match self.store_lock().terminals() {
            Ok(terminals) => terminals,
            Err(e) => {
                tracing::warn!(%terminal, error = %e, "cannot look up the terminal's final scan");
                return false;
            }
        };
        let Some(terminal) = terminals.into_iter().find(|t| t.terminal == terminal) else {
            return true;
        };
        let Some(engine) = terminal.engine else {
            return true;
        };
        if self
            .homes
            .iter()
            .any(|h| h.engine == engine && h.discover_failing)
        {
            return false;
        }
        let unread: Vec<u64> = self
            .tracked
            .iter()
            .filter(|(_, t)| t.engine == engine && !t.discovered && !t.subagent)
            .map(|(id, _)| *id)
            .collect();
        let now = crate::now_ms();
        for id in unread {
            if !self.load(id) {
                return false;
            }
            let Some(t) = self.tracked.get_mut(&id) else {
                return false;
            };
            let caught_up = t.caught_up;
            let Some(row) = t.row() else {
                return false;
            };
            if terminal.session == Some(row.session) {
                return false;
            }
            let native = native_id(row);
            if let Some(expected) = &terminal.native_id {
                if *expected == native {
                    return false;
                }
                continue;
            }
            // A successful read with no metadata cannot be claimed by folder. A failed or
            // unfinished read may still reveal this terminal's folder on the next attempt.
            let Some(meta) = &row.meta else {
                if !caught_up {
                    return false;
                }
                continue;
            };
            let found = store::Found {
                session: row.session,
                engine,
                native_id: &native,
                cwd: meta.cwd.as_deref(),
                started: meta.started.unwrap_or(now),
            };
            if store::by_folder(&terminal, &found, now) {
                return false;
            }
        }
        true
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
        self.let_go();
        let now = Instant::now();
        let (due, reports): (Vec<PathBuf>, Vec<Signal>) = {
            let mut s = self.shared.lock();
            if s.stop {
                return Err(Hangup);
            }
            let mut due = Vec::new();
            s.dirty.retain(|p, d| {
                if d.due <= now && self.by_path.contains_key(p.as_path()) {
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
            for id in self
                .by_path
                .get(path.as_path())
                .map(Ids::to_vec)
                .unwrap_or_default()
            {
                self.check(id)?;
            }
        }
        Ok(())
    }

    /// Runs discovery for some homes and indexes the new transcripts, newest first.
    fn rediscover(&mut self, which: &[usize], force: bool) -> Result<(), Hangup> {
        let now = Instant::now();
        self.last_rediscover = Some(now);
        let mut found: Vec<(TimestampMs, usize, RawKey, TranscriptRef, PathBuf)> = Vec::new();
        for &h in which {
            self.refresh_home(h);
            let home = &mut self.homes[h];
            home.next_rediscover = after(now, home.every(self.timing.rediscover_interval));
            let cache = self.cache_file_discovery
                && !home.polled
                && (matches!(home.engine, Engine::Claude | Engine::Codex)
                    || (cfg!(unix) && home.engine == Engine::OpenCode))
                && self.watch_failed.is_empty();
            if cache
                && !force
                && !home.discover_failing
                && home
                    .layout
                    .as_ref()
                    .is_some_and(|layout| layout.unchanged(&home.path))
            {
                continue;
            }
            // Capture before discovery, so mutations during its walk are noticed next time.
            home.layout = cache
                .then(|| {
                    if home.engine == Engine::OpenCode {
                        Layout::capture_databases(&home.path)
                    } else {
                        Layout::capture(&home.path)
                    }
                })
                .flatten();
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
                // Tracked already, as discovered: through its canonical path, or through a link.
                if self
                    .by_key
                    .contains_key(&key(&tref.path, tref.inner_id.as_deref()))
                {
                    continue;
                }
                let raw = (tref.path.clone(), tref.inner_id.clone());
                if self.by_raw.contains_key(&raw) || self.skipped.contains(&raw) {
                    continue;
                }
                // Keys are canonical: the file exists now, so this is when to resolve it (its
                // folder only: a transcript swapped for a link since discovery is not tracked).
                let canonical = match transcript_key(&tref.path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::debug!(path = %tref.path.display(), error = %e, "a discovered transcript is gone, or no longer a regular file");
                        continue;
                    }
                };
                if let Some(&id) = self.by_key.get(&key(&canonical, tref.inner_id.as_deref())) {
                    self.note_raw(raw, &canonical, id);
                    continue;
                }
                let mtime = fsinfo::stat(&canonical).map_or(tref.modified, |s| s.mtime);
                found.push((mtime, h, raw, tref, canonical));
            }
        }
        // Newest first: live sessions are indexed before a backlog of old ones.
        found.sort_by_key(|f| std::cmp::Reverse(f.0));
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
        raw: RawKey,
        tref: TranscriptRef,
        canonical: PathBuf,
    ) -> Result<(), Hangup> {
        if let Some(&id) = self.by_key.get(&key(&canonical, tref.inner_id.as_deref())) {
            self.note_raw(raw, &canonical, id);
            return Ok(());
        }
        if path_text(&canonical).is_err() {
            tracing::warn!(path = %canonical.display(), "skipping a transcript whose path is not Unicode");
            self.skipped.insert(raw);
            return Ok(());
        }
        let earlier = self.store_lock().find(&canonical, tref.inner_id.as_deref());
        let row = match earlier {
            Ok(Some(row)) => {
                tracing::debug!(path = %canonical.display(), session = %row.session, "a transcript seen before; resuming its row");
                row
            }
            Ok(None) => {
                let mut row = new_row(&TranscriptRef {
                    path: canonical.clone(),
                    ..tref.clone()
                });
                if let Some(named) =
                    self.named_session(tref.engine, &canonical, tref.inner_id.as_deref())
                {
                    row.session = named;
                }
                if let Err(e) = self.store_lock().insert(&row) {
                    tracing::error!(path = %row.path.display(), error = %e, "cannot index a transcript");
                    return Ok(());
                }
                tracing::debug!(path = %row.path.display(), session = %row.session, "new transcript");
                row
            }
            Err(e) => {
                tracing::error!(path = %canonical.display(), error = %e, "cannot look up a transcript");
                return Ok(());
            }
        };
        let id = self.track(&Indexed::of(&row), Some(row), h, tref.size, tref.modified);
        self.note_raw(raw, &canonical, id);
        self.check(id)?;
        Ok(())
    }

    /// Remembers a transcript discovered through a link by the path it was discovered at. One
    /// discovered at its canonical path is found by that.
    fn note_raw(&mut self, raw: RawKey, canonical: &Path, id: u64) {
        if raw.0 != canonical {
            self.by_raw.insert(raw, id);
        }
    }

    /// Starts tracking a transcript, with its whole row if it is at hand. `size` and `modified`
    /// are what the transcript pages are told (the adapter's `TranscriptRef`).
    fn track(
        &mut self,
        entry: &Indexed,
        row: Option<Row>,
        home: usize,
        size: u64,
        modified: TimestampMs,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        // Transcripts in one file (OpenCode's) share one path.
        let path: Arc<Path> = self
            .by_path
            .get_key_value(entry.path.as_path())
            .map_or_else(|| Arc::from(entry.path.as_path()), |(p, _)| Arc::clone(p));
        let mut t = Tracked::new(entry, Arc::clone(&path), home, &self.timing);
        if let Some(row) = row {
            t.loaded = Some(Loaded::new(row));
            self.loaded.push(id);
        }
        self.by_key
            .insert((Arc::clone(&path), t.inner_id.clone()), id);
        match self.by_path.entry(Arc::clone(&path)) {
            std::collections::hash_map::Entry::Occupied(mut ids) => ids.get_mut().push(id),
            std::collections::hash_map::Entry::Vacant(none) => {
                none.insert(Ids::One(id));
            }
        }
        self.by_session.insert(entry.session, id);
        if let Some(native) = &entry.native {
            self.map_native(
                id,
                entry.session,
                entry.engine,
                native,
                entry.subagent,
                true,
            );
        }
        self.watched.insert(
            entry.session,
            Source {
                adapter: Arc::clone(&self.homes[home].adapter),
                engine: entry.engine,
                path: Arc::clone(&path),
                inner_id: t.inner_id.clone(),
                size,
                modified,
            },
        );
        self.note_outside(&path, home);
        self.tracked.insert(id, t);
        self.sync_watches(id);
        id
    }

    /// Reads a tracked transcript's whole row into memory, unless it is there. False if it cannot
    /// be read (logged).
    fn load(&mut self, id: u64) -> bool {
        let Some(t) = self.tracked.get(&id) else {
            return false;
        };
        if t.loaded.is_some() {
            if let Some(at) = self.cached.iter().position(|cached| *cached == id) {
                self.cached.remove(at);
                self.loaded.push(id);
            }
            return true;
        }
        let session = t.session;
        let found = self.store_lock().load(session);
        let mut row = match found {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::error!(%session, "a tracked transcript has no row in the runner's index");
                return false;
            }
            Err(e) => {
                tracing::error!(%session, error = %e, "cannot read a transcript's row from the runner's index");
                return false;
            }
        };
        let Some(t) = self.tracked.get_mut(&id) else {
            return false;
        };
        // A parent looked up since the row was saved is saved with it next time.
        if row.facts.parent.is_none() {
            row.facts.parent = t.parent;
        }
        t.loaded = Some(Loaded::new(row));
        self.loaded.push(id);
        true
    }

    /// Keeps at most a small cache of saved hot rows, plus every row ahead of the index.
    fn let_go(&mut self) {
        let tracked = &mut self.tracked;
        let cached = &mut self.cached;
        self.loaded.retain(|id| {
            let Some(t) = tracked.get_mut(id) else {
                return false;
            };
            let saved = t.loaded.as_deref().is_none_or(Loaded::saved);
            if saved && t.hot && t.loaded.is_some() {
                cached.push_back(*id);
                return false;
            }
            if saved {
                t.loaded = None;
            }
            !saved
        });
        while self.cached.len() > HOT_ROW_CACHE {
            if let Some(id) = self.cached.pop_front()
                && let Some(t) = self.tracked.get_mut(&id)
            {
                t.loaded = None;
            }
        }
    }

    /// Hands a batch to the sink thread, counting it against its row until it is saved.
    fn send(&self, batch: Batch) -> Result<(), Hangup> {
        if let Some(unsaved) = &batch.unsaved {
            unsaved.fetch_add(1, Ordering::AcqRel);
        }
        self.tx.send(batch).map_err(|_| Hangup)
    }

    /// Stops tracking a deleted transcript. Its row stays, so if a file comes back at the path it
    /// keeps its session; the row is marked [`GONE`] (in order with the commits already queued),
    /// so that file is read from the start.
    fn drop_deleted(&mut self, id: u64) -> Result<(), Hangup> {
        self.load(id);
        let Some(t) = self.tracked.remove(&id) else {
            return Ok(());
        };
        tracing::info!(path = %t.path.display(), session = %t.session, "transcript deleted; no longer watching it");
        self.by_key
            .remove(&(Arc::clone(&t.path), t.inner_id.clone()));
        let empty = self
            .by_path
            .get_mut(&*t.path)
            .is_some_and(|ids| !ids.remove(id));
        if empty {
            self.by_path.remove(&*t.path);
        }
        self.by_raw.retain(|_, i| *i != id);
        self.by_native.retain(|_, i| *i != id);
        self.by_sub_native.retain(|_, i| *i != id);
        self.by_session.remove(&t.session);
        self.watched.remove(t.session);
        for d in &t.watched {
            self.unwatch_dir(d);
        }
        self.loaded.retain(|i| *i != id);
        let Some(mut l) = t.loaded else {
            tracing::warn!(session = %t.session, "cannot mark a deleted transcript's row; a file back at its path is read from its cursor");
            return Ok(());
        };
        l.row.identity = Some(GONE.to_owned());
        l.row.caught_up = false;
        self.send(Batch {
            events: Vec::new(),
            commit: Commit::Full(Box::new(l.row)),
            unsaved: None,
        })
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

    /// The home a stored transcript belongs to: one of its engine's homes that holds it. A row
    /// outside every home (a home that moved, or a path that should never have been stored) is
    /// not tracked; discovery finds the transcript again where it is now.
    fn home_for(&self, engine: Engine, path: &Path) -> Option<usize> {
        self.homes
            .iter()
            .position(|h| h.engine == engine && path.starts_with(&h.path))
    }

    /// Stats one transcript and reads it if it changed. Returns whether it changed.
    fn check(&mut self, id: u64) -> Result<bool, Hangup> {
        let Some(t) = self.tracked.get(&id) else {
            return Ok(false);
        };
        let st = match fsinfo::stat(&t.path) {
            Ok(st) => st,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.drop_deleted(id)?;
                return Ok(false);
            }
            Err(e) => {
                tracing::debug!(path = %t.path.display(), error = %e, "transcript not readable");
                return Ok(false);
            }
        };
        self.classify(id, st.mtime);
        let Some(t) = self.tracked.get(&id) else {
            return Ok(false);
        };
        if t.failed_at.as_deref() == Some(&(st.size, st.mtime))
            || (t.caught_up
                && t.size == st.size
                && t.mtime == st.mtime
                && fsinfo::same_file(t.identity.as_deref(), st.identity.as_deref()))
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
        if !hot && let Some(at) = self.cached.iter().position(|cached| *cached == id) {
            self.cached.remove(at);
            self.loaded.push(id);
        }
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
            wanted_dirs(&t.path, &home.path, t.hot)
        };
        let have = std::mem::take(&mut t.watched);
        let kept = self.reconcile(have, want);
        if let Some(t) = self.tracked.get_mut(&id) {
            t.watched = kept;
        }
    }

    /// Moves a holder's watches from `have` to `want`; returns the folders it now holds.
    fn reconcile(&mut self, have: Vec<Arc<Path>>, want: Vec<PathBuf>) -> Vec<Arc<Path>> {
        let mut kept = Vec::with_capacity(want.len());
        for d in have {
            if want.iter().any(|w| *w == *d) {
                kept.push(d);
            } else {
                self.unwatch_dir(&d);
            }
        }
        for d in want {
            if !kept.iter().any(|k| **k == *d)
                && let Some(watched) = self.watch_dir(&d)
            {
                kept.push(watched);
            }
        }
        kept
    }

    /// Adds a holder to a folder's watch, starting it if needed, and returns the folder as the
    /// watches share it. Only a working watch counts.
    fn watch_dir(&mut self, dir: &Path) -> Option<Arc<Path>> {
        if let Some((shared, n)) = self.dirs.get_key_value(dir) {
            let shared = Arc::clone(shared);
            let n = n + 1;
            self.dirs.insert(Arc::clone(&shared), n);
            return Some(shared);
        }
        let w = self.notify.as_mut()?;
        match w.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                let shared: Arc<Path> = Arc::from(dir);
                self.dirs.insert(Arc::clone(&shared), 1);
                self.watch_failed.remove(dir);
                Some(shared)
            }
            Err(e) if is_not_found(&e) => {
                tracing::debug!(dir = %dir.display(), "folder is gone; not watching it");
                None
            }
            Err(e) => {
                if self.watch_failed.insert(dir.to_path_buf()) {
                    tracing::warn!(dir = %dir.display(), error = %e, "cannot watch a folder; the sweep still covers it");
                }
                None
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
        let _busy = Busy::enter(&self.shared.busy);
        if !self.load(id) {
            return Ok(());
        }
        let Some(t) = self.tracked.get_mut(&id) else {
            return Ok(());
        };
        let tref = t.tref(st.size, st.mtime);
        let adapter = Arc::clone(&self.homes[t.home].adapter);
        let byte_cursor = self.byte_file_cursors
            && t.inner_id.is_none()
            && matches!(t.engine, Engine::Claude | Engine::Codex);
        if let Some(l) = t.loaded.as_deref_mut()
            && needs_reindex(&l.row, &st)
        {
            reindex(&mut l.row, &st);
            l.dirty = true;
        }
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
                    .mark(vec![t.path.to_path_buf()], Instant::now(), false);
                break;
            }
            let Some(l) = t.loaded.as_deref_mut() else {
                return Ok(());
            };
            let cursor = l.row.cursor.clone();
            match guard(|| adapter.read_from(&tref, &cursor)) {
                Ok(chunk) => {
                    // Any cursor change is progress; OpenCode's lives in `state`, not `offset`.
                    // The last read saves `caught_up`, even when it found nothing new.
                    let more = chunk.cursor != *cursor;
                    let at_end = byte_cursor && chunk.cursor.offset == st.size;
                    t.failed_at = None;
                    t.warned = false;
                    self.process(id, chunk, &st, !more || at_end)?;
                    if !more || at_end {
                        break;
                    }
                }
                Err(AdapterError::Source(SourceError::Unreadable { reason, .. }))
                    if !retried && l.row.inner_id.is_none() && cursor.offset > st.size =>
                {
                    tracing::debug!(reason, "adapter reports a shorter file");
                    retried = true;
                    reindex(&mut l.row, &st);
                    l.dirty = true;
                }
                Err(e) => {
                    // Not read again until the file changes, and warned about once.
                    t.failed_at = Some(Box::new((st.size, st.mtime)));
                    if t.warned {
                        tracing::debug!(path = %t.path.display(), error = %e, "cannot read transcript");
                    } else {
                        tracing::warn!(path = %t.path.display(), error = %e, "cannot read transcript; trying again when it changes");
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
        let (path, home) = (Arc::clone(&t.path), t.home);
        let Some(row) = t.row() else {
            return Ok(());
        };
        let mut session = row.session;
        let place = |row: &Row| row.meta.as_ref().map(|m| (m.cwd.clone(), m.branch.clone()));
        let was = place(row);
        if let Some(meta) = chunk.meta {
            row.meta = Some(meta);
        }
        let first = !row.discovered;
        let moved = !first && place(row) != was;
        let cwd = row.meta.as_ref().and_then(|m| m.cwd.clone());
        let engine = row.engine;
        let native = native_id(row);
        let subagent = row.meta.as_ref().is_some_and(|m| m.is_subagent);
        let started = row.meta.as_ref().and_then(|m| m.started);

        // The terminal the runner started a new session in, claimed before anything names the
        // session: one started for a session the hub named makes the transcript adopt that id.
        // So does such a start still under way (its terminal not recorded yet): the session is
        // then reported without its terminal, which is linked when it is recorded.
        let terminal = if first && !subagent {
            let claim = self.claim_terminal(session, engine, &native, cwd.as_deref(), started);
            let named = match claim {
                Some(c) => c.adopted,
                None => self
                    .shared
                    .pending_session(engine, &native, cwd.as_deref(), started)
                    .filter(|named| *named != session && self.move_row(session, *named)),
            };
            if let Some(named) = named {
                self.adopt(id, session, named);
                session = named;
            }
            claim.map(|c| c.terminal)
        } else {
            None
        };

        let Some(row) = self.tracked.get_mut(&id).and_then(Tracked::row) else {
            return Ok(());
        };
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
            derive::apply(&mut row.facts, &ctx, item, key, &mut derived);
            // Accepted before a crash: fold the item, don't send it again.
            if row.accepted.contains(&key) {
                derived.truncate(before);
            }
        }
        self.map_native(id, session, engine, &native, subagent, first);

        // Facts from elsewhere: the parent of a sub-agent, hooks that came before the transcript,
        // and workstream locations.
        let (parent, held) = if first {
            let parent = if subagent {
                self.parent_of(engine, &path, home)
            } else {
                Ok(Parent::None)
            };
            let held = self.allowed_held(session, parent, engine, &native);
            (parent, held)
        } else {
            (Ok(Parent::None), Vec::new())
        };
        let places = (first || moved).then(|| self.places(session)).flatten();

        let Some(t) = self.tracked.get_mut(&id) else {
            return Ok(());
        };
        let Some(l) = t.loaded.as_deref_mut() else {
            return Ok(());
        };
        let row = &mut l.row;
        // Kept with the row, so its hooks are judged by the parent its discovery names, after a
        // restart too. A failed lookup is not kept: it is tried again at the next hook.
        if first
            && subagent
            && let Ok(found) = parent
        {
            row.facts.parent = Some(found);
        }
        let parent = parent.ok().and_then(Parent::session);
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
                changed |= derive::report(&mut row.facts, r).is_some();
            }
            let ended = changed && row.facts.state == SessionState::Ended;
            let s = session_of(row, st.mtime, machine, parent, terminal);
            let at = discovered_id_time(row);
            events.push((
                None,
                event(
                    event_id(session, 0, Cause::Discovered, 0, at),
                    s.started,
                    EventBody::SessionDiscovered { session: s },
                ),
            ));
            if ended {
                row.facts.reports += 1;
                let n = row.facts.reports;
                let at = row.facts.reported_at.unwrap_or(at);
                events.push((
                    None,
                    event(
                        event_id(session, row.generation, Cause::Report(n), 0, at),
                        at,
                        EventBody::SessionEnded { session },
                    ),
                ));
            }
            row.discovered = true;
        }
        if let Some((locations, stands)) = &places {
            let at = if first {
                discovered_id_time(row)
            } else {
                crate::now_ms()
            };
            if let Some(e) = link_event(row, machine, locations, *stands, at) {
                events.push((None, event(e.0, e.1, e.2)));
            }
        }
        let mut last: Option<u64> = None;
        let mut seq = 0u32;
        for d in derived {
            seq = if last == Some(d.key) { seq + 1 } else { 0 };
            last = Some(d.key);
            let id = event_id(session, row.generation, Cause::Item(d.key), seq, d.at);
            events.push((Some(d.key), event(id, d.at, d.body)));
        }
        row.cursor = Arc::new(chunk.cursor);
        row.size = st.size;
        row.mtime = st.mtime;
        row.identity.clone_from(&st.identity);
        row.caught_up = caught_up;
        if caught_up {
            // The replay after a crash is over.
            row.accepted.clear();
        }

        let batches = split(events, self.max_batch, row);
        let unsaved = l.sending();
        t.sync();
        for mut b in batches {
            b.unsaved = Some(Arc::clone(&unsaved));
            self.send(b)?;
        }
        Ok(())
    }

    /// Applies reported states: at once for indexed sessions or, for a hook, held until the
    /// session's transcript is found. A hook applies only if its sender may change the session.
    fn apply_reports(&mut self, reports: Vec<Signal>) -> Result<(), Hangup> {
        for s in reports {
            let id = match &s.target {
                Target::Native { engine, native_id } => self.find_native(*engine, native_id),
                Target::Session(session) => self.by_session.get(session).copied(),
            };
            let indexed = id.and_then(|id| {
                self.tracked
                    .get(&id)
                    .filter(|t| t.discovered)
                    .map(|t| (id, t.session))
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
    /// parent's agent is known.
    ///
    /// It fails closed: the session's own answer, when it is an agent or unknown, stands (the
    /// parent never overrides it); the parent's answer is taken as it is, unknown included; and
    /// a sub-agent whose parent could not be looked up is unknown.
    fn runs_as(
        &mut self,
        session: SessionId,
        parent: Result<Parent, LookupFailed>,
    ) -> SessionAgent {
        match (self.agent_of(session), parent) {
            (SessionAgent::NoAgent, Ok(Parent::Session(parent))) => self.agent_of(parent),
            (SessionAgent::NoAgent, Err(LookupFailed)) => SessionAgent::Unknown,
            (own, _) => own,
        }
    }

    /// The parent of a tracked session: the one kept with its row, or, for a sub-agent indexed
    /// before parents were kept, looked up now and kept. A failed lookup is not kept.
    fn parent_for(&mut self, id: u64) -> Result<Parent, LookupFailed> {
        let Some(t) = self.tracked.get(&id) else {
            return Ok(Parent::None);
        };
        if let Some(parent) = t.parent {
            return Ok(parent);
        }
        if !t.subagent {
            return Ok(Parent::None);
        }
        let (engine, path, home) = (t.engine, Arc::clone(&t.path), t.home);
        let parent = self.parent_of(engine, &path, home)?;
        if let Some(t) = self.tracked.get_mut(&id) {
            // Saved with the row's next change (see `load`).
            t.parent = Some(parent);
            if let Some(row) = t.row() {
                row.facts.parent = Some(parent);
            }
        }
        Ok(parent)
    }

    /// The hooks held for a newly discovered session whose senders may apply them, oldest
    /// first. The others are dropped. A sub-agent's are judged as its parent's (see `runs_as`).
    fn allowed_held(
        &mut self,
        session: SessionId,
        parent: Result<Parent, LookupFailed>,
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
        if !self.load(id) {
            return Ok(());
        }
        let Some(t) = self.tracked.get_mut(&id) else {
            return Ok(());
        };
        let Some(l) = t.loaded.as_deref_mut() else {
            return Ok(());
        };
        let row = &mut l.row;
        let was = row.facts.clone();
        let Some(from) = derive::report(&mut row.facts, r) else {
            // The state stands, but when it was reported, or its status line, may have moved.
            if row.facts != was {
                l.dirty = true;
            }
            return Ok(());
        };
        row.facts.reports += 1;
        let (session, generation, n) = (row.session, row.generation, row.facts.reports);
        let events = derive::reported_events(session, from, &row.facts)
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
        let commit = Commit::Full(Box::new(row.clone()));
        let unsaved = Some(l.sending());
        t.sync();
        self.send(Batch {
            events,
            commit,
            unsaved,
        })
    }

    /// Maps tracked transcript `id` (session `session`) by its CLI id, for the hooks that name
    /// it. Sessions and sub-agents have separate maps, and a hook is matched to a session first:
    /// a sub-agent whose id equals a session's (its `agentId` is whatever its transcript says)
    /// never takes that session's hooks, whichever of the two was found first.
    ///
    /// Among sessions the last one found takes an id, as before (one session seen in two homes).
    /// Among sub-agents the one indexed first keeps it, by its session id, so the same one after
    /// a restart too (which finds the newest first); a later one is refused it (`loud`: said as
    /// a warning).
    fn map_native(
        &mut self,
        id: u64,
        session: SessionId,
        engine: Engine,
        native: &str,
        subagent: bool,
        loud: bool,
    ) {
        if native.is_empty() {
            return;
        }
        let key = (engine, Box::<str>::from(native));
        if !subagent {
            self.by_native.insert(key, id);
            return;
        }
        let holder = match self.by_sub_native.get(&key) {
            None => None,
            Some(&held) if held == id => return,
            Some(held) => self.tracked.get(held).map(|t| t.session),
        };
        if holder.is_some_and(|first| first < session) {
            if loud {
                tracing::warn!(engine = ?engine, id = native, %session, "two sub-agents name the same id; its hooks go to the one indexed first");
            } else {
                tracing::debug!(engine = ?engine, id = native, %session, "two sub-agents name the same id; its hooks go to the one indexed first");
            }
            return;
        }
        self.by_sub_native.insert(key, id);
    }

    /// The tracked transcript a hook's CLI id names: a session's before a sub-agent's.
    fn find_native(&self, engine: Engine, native: &str) -> Option<u64> {
        let key = (engine, Box::<str>::from(native));
        self.by_native
            .get(&key)
            .or_else(|| self.by_sub_native.get(&key))
            .copied()
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
            if !self.tracked.get(&id).is_some_and(|t| t.discovered) || !self.load(id) {
                continue;
            }
            let Some(l) = self
                .tracked
                .get_mut(&id)
                .and_then(|t| t.loaded.as_deref_mut())
            else {
                continue;
            };
            let stands = locations.link_of(l.row.session);
            let Some((eid, at, body)) =
                link_event(&mut l.row, machine, &all, stands, crate::now_ms())
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
            let commit = Commit::Full(Box::new(l.row.clone()));
            let unsaved = Some(l.sending());
            self.send(Batch {
                events: vec![event],
                commit,
                unsaved,
            })?;
        }
        Ok(())
    }

    /// For a Claude sub-agent's transcript (at `path`, canonical, in home `home`), its parent's
    /// session. The parent's transcript must be a regular file, not a link, inside the same home
    /// (see [`parent_file`]); otherwise the sub-agent has no parent. A parent not indexed yet
    /// gets its session id now, which its discovery then keeps. An index or I/O error is a failed
    /// lookup, never "no parent".
    fn parent_of(&self, engine: Engine, path: &Path, home: usize) -> Result<Parent, LookupFailed> {
        let Some(parent) = parent_transcript(path) else {
            return Ok(Parent::None);
        };
        match parent_file(&parent, path, &self.homes[home].path) {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(path = %parent.display(), "a sub-agent's parent is not a plain file in its home; it has no parent");
                return Ok(Parent::None);
            }
            Err(e) => {
                tracing::warn!(path = %parent.display(), error = %e, "cannot look at a sub-agent's parent; its hooks are refused for now");
                return Err(LookupFailed);
            }
        }
        if let Some(t) = self
            .by_key
            .get(&key(&parent, None))
            .and_then(|id| self.tracked.get(id))
        {
            return Ok(Parent::Session(t.session));
        }
        let found = self.store_lock().find(&parent, None);
        match found {
            Ok(Some(row)) => Ok(Parent::Session(row.session)),
            Ok(None) => {
                // A parent started for a session the hub named is that session already.
                let named = self.named_session(engine, &parent, None);
                let mut row = new_row(&TranscriptRef {
                    engine,
                    path: parent,
                    inner_id: None,
                    size: 0,
                    modified: 0,
                });
                if let Some(named) = named {
                    row.session = named;
                }
                let store = self.store_lock();
                match store.insert(&row) {
                    Ok(()) => Ok(Parent::Session(row.session)),
                    Err(e) => {
                        tracing::warn!(path = %row.path.display(), error = %e, "cannot index a sub-agent's parent; its hooks are refused for now");
                        Err(LookupFailed)
                    }
                }
            }
            Err(e) => {
                tracing::warn!(path = %parent.display(), error = %e, "cannot look up a sub-agent's parent; its hooks are refused for now");
                Err(LookupFailed)
            }
        }
    }

    /// The terminal the runner started a newly discovered session in, if it did, and the session
    /// the hub named for it, which the transcript then adopts (its row is moved in the index).
    ///
    /// An exited terminal can still own an unread transcript. It stops accepting folder matches
    /// only after its final scan has completed; the runtime is asked without the index locked.
    fn claim_terminal(
        &self,
        session: SessionId,
        engine: Engine,
        native: &str,
        cwd: Option<&str>,
        started: Option<TimestampMs>,
    ) -> Option<Claim> {
        let now = crate::now_ms();
        let found = store::Found {
            session,
            engine,
            native_id: native,
            cwd,
            started: started.unwrap_or(now),
        };
        let candidates = self
            .store_lock()
            .folder_candidates(&found, now)
            .unwrap_or_else(|e| {
                tracing::warn!(%session, error = %e, "cannot look up the terminals started in the session's folder");
                Vec::new()
            });
        let ended: Vec<TerminalId> = candidates
            .into_iter()
            .filter(|t| self.shared.has_ended(*t) && self.shared.lock().exhausted.contains(t))
            .collect();
        if !ended.is_empty() {
            tracing::debug!(%session, ?ended, "terminals in the folder whose program ended are not matched");
        }
        self.store_lock()
            .claim_terminal(&found, now, &ended)
            .unwrap_or_else(|e| {
                tracing::warn!(%session, error = %e, "cannot look up the session's terminal");
                None
            })
    }

    /// Transcript `id`, found as `from`, adopts `to`, the session the hub named for the terminal
    /// it runs in (the index has moved its row): everything that finds it by session follows. No
    /// event has named `from`: this happens at its discovery.
    fn adopt(&mut self, id: u64, from: SessionId, to: SessionId) {
        tracing::debug!(%from, %to, "a new transcript takes the session the hub named for its terminal");
        if let Some(t) = self.tracked.get_mut(&id) {
            t.session = to;
            if let Some(row) = t.row() {
                row.session = to;
            }
        }
        if self.by_session.get(&from) == Some(&id) {
            self.by_session.remove(&from);
        }
        self.by_session.insert(to, id);
        self.watched.rekey(from, to);
    }

    /// The session a transcript not indexed yet takes: the one the hub named for a terminal the
    /// runner started (or is starting) with the CLI id the transcript is named by (Claude's
    /// `<id>.jsonl`, from its `--session-id`), if one waits; else none, and it gets a new one.
    fn named_session(
        &self,
        engine: Engine,
        path: &Path,
        inner_id: Option<&str>,
    ) -> Option<SessionId> {
        let native = match inner_id {
            Some(inner) => inner.to_owned(),
            None => path.file_stem()?.to_str()?.to_owned(),
        };
        let recorded = self
            .store_lock()
            .named_session(engine, &native)
            .unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "cannot look up a session the hub named; the transcript gets an id of its own");
                None
            });
        recorded.or_else(|| self.shared.pending_session(engine, &native, None, None))
    }

    /// Moves the index's row of a transcript found as `from` to `to`, a session the hub named
    /// whose start is under way. False (logged) if it cannot.
    fn move_row(&self, from: SessionId, to: SessionId) -> bool {
        match self.store_lock().move_row(from, to) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(%from, %to, error = %e, "cannot give a transcript the session the hub named; it keeps its own");
                false
            }
        }
    }

    fn store_lock(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct Wake {
    _busy: Option<Busy>,
    /// Due paths, each with whether it may be a new file.
    due: Vec<(PathBuf, bool)>,
    overflow: bool,
    /// Discover in every home.
    rediscover: bool,
    /// Discover in the homes that are not slow.
    look: bool,
    reports: Vec<Signal>,
    relink: bool,
    exit_scans: Vec<(TerminalId, SyncSender<bool>)>,
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
pub(crate) enum AdapterError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("the source adapter panicked: {0}")]
    Panic(String),
}

/// Runs an adapter call, turning a panic into an error: one bad transcript must not stop the
/// watcher.
pub(crate) fn guard<T>(call: impl FnOnce() -> Result<T, SourceError>) -> Result<T, AdapterError> {
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
        cursor: Arc::default(),
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
    let read_before = *row.cursor != Cursor::default();
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
    row.cursor = Arc::default();
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
                unsaved: None,
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
        unsaved: None,
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

fn session_of(
    row: &Row,
    modified: TimestampMs,
    machine: MachineId,
    parent: Option<SessionId>,
    terminal: Option<TerminalId>,
) -> Session {
    let meta = row.meta.clone().unwrap_or_default();
    let last_activity = if row.facts.last_activity > 0 {
        row.facts.last_activity
    } else {
        modified
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

/// A sub-agent's parent could not be looked up (the index or the filesystem failed). Its hooks
/// are refused, as for an unknown agent, and the lookup is tried again at the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LookupFailed;

/// A transcript's key: its folder resolved (so a home reached through a symlink keeps its
/// sessions), joined with its own name, which is never resolved. A transcript swapped for a link
/// must not be tracked, or have its row moved, at the link's target: reads would then open that
/// target, a regular file, and the ingest's own check (`pitcrew_ingest`'s `NotRegularFile`) would
/// pass. `InvalidInput` if the path is not a regular file now (a link, a folder, a pipe); the
/// I/O error if it, or its folder, is gone.
fn transcript_key(path: &Path) -> io::Result<PathBuf> {
    let Some(name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a transcript path with no file name",
        ));
    };
    let folder = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let key = folder.canonicalize()?.join(name);
    if std::fs::symlink_metadata(&key)?.file_type().is_file() {
        Ok(key)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a transcript that is not a regular file",
        ))
    }
}

/// Whether `parent`, the transcript a sub-agent at `child` names as its parent, may be taken as
/// one: a regular file (a link is not followed), inside `home`, and not the sub-agent itself.
/// `child` is canonical, so the folders above `parent` are real ones; only its last part could
/// be a link. A file that is not there is no parent; another I/O error is an error.
fn parent_file(parent: &Path, child: &Path, home: &Path) -> io::Result<bool> {
    if parent == child || !parent.starts_with(home) {
        return Ok(false);
    }
    match std::fs::symlink_metadata(parent) {
        Ok(meta) => Ok(meta.file_type().is_file()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
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
            cursor: Arc::new(Cursor {
                offset: 10,
                state: None,
            }),
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
    fn notifications_coalesce_only_after_work_is_due_or_while_it_is_running() {
        let shared = Shared::default();
        let now = Instant::now();
        let debounce = Duration::from_millis(100);
        let mark = |path: &str, at, enabled| {
            shared.notification(vec![path.into()], at, debounce, enabled, false);
        };
        let due = |path: &str| shared.lock().dirty[Path::new(path)].due;
        mark("first", now, true);
        assert_eq!(due("first"), now + debounce, "no delay beyond debounce");
        mark("before", now + Duration::from_millis(90), true);
        assert_eq!(due("before"), now + Duration::from_millis(190));
        mark("due", now + debounce, true);
        assert_eq!(due("due"), now + debounce);
        assert_eq!(
            due("before"),
            now + Duration::from_millis(190),
            "an earlier arrival retains its deadline"
        );
        shared.lock().dirty.clear();
        {
            let read = Busy::enter(&shared.busy);
            let delivery = Busy::enter(&shared.busy);
            drop(read);
            mark("during", now, true);
            assert_eq!(due("during"), now);
            mark("disabled", now, false);
            assert_eq!(due("disabled"), now + debounce);
            drop(delivery);
        }
        shared.lock().dirty.clear();
        mark("after", now, true);
        assert_eq!(
            due("after"),
            now + debounce,
            "the window ends with the work"
        );
        mark("after", now + Duration::from_millis(50), true);
        assert_eq!(
            due("after"),
            now + debounce,
            "repeated notifications cannot postpone work"
        );
        let _read = Busy::enter(&shared.busy);
        mark("after", now + Duration::from_millis(60), true);
        assert_eq!(
            due("after"),
            now + Duration::from_millis(60),
            "a new arrival can join in-flight work"
        );
    }

    #[test]
    fn the_id_map_keeps_ids_in_order() {
        let mut m = IdMap::default();
        for id in [3u64, 1, 7, 5] {
            m.insert(id, id * 10);
        }
        m.insert(5, 55);
        assert_eq!(m.keys().copied().collect::<Vec<_>>(), [1, 3, 5, 7]);
        assert_eq!(m.values().copied().collect::<Vec<_>>(), [10, 30, 55, 70]);
        assert_eq!(m.get(&7), Some(&70));
        assert_eq!(m.get(&4), None);
        if let Some(v) = m.get_mut(&1) {
            *v = 11;
        }
        assert_eq!(m.remove(&3), Some(30));
        assert_eq!(m.remove(&3), None);
        assert_eq!(m.keys().copied().collect::<Vec<_>>(), [1, 5, 7]);
        assert_eq!(m[&1], 11);
        // Growing by an eighth, not doubling.
        let mut big = IdMap::default();
        for id in 0..10_000u64 {
            big.insert(id, ());
        }
        assert!(big.entries.capacity() < 10_000 + 10_000 / 8 + 32);
    }

    #[test]
    fn ids_at_one_path() {
        let mut ids = Ids::One(4);
        assert_eq!(ids.to_vec(), [4]);
        ids.push(9);
        ids.push(2);
        assert_eq!(ids.to_vec(), [4, 9, 2]);
        assert!(ids.remove(9));
        assert!(ids.remove(4));
        assert!(!ids.remove(2), "none left");
        assert!(!Ids::One(1).remove(1));
        assert!(Ids::One(1).remove(2));
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
        fresh.cursor = Arc::default();
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
    fn a_parent_is_a_plain_file_in_the_sub_agents_home() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = home.join("projects").join("p");
        let child = project.join("s").join("subagents").join("a.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&child, b"{}\n").unwrap();
        let parent = project.join("s.jsonl");
        assert_eq!(parent_transcript(&child), Some(parent.clone()));

        // Not there yet: no parent. A regular file: the parent.
        assert!(!parent_file(&parent, &child, &home).unwrap());
        std::fs::write(&parent, b"{}\n").unwrap();
        assert!(parent_file(&parent, &child, &home).unwrap());
        // Never the sub-agent itself, nor a file outside its home, nor a folder.
        assert!(!parent_file(&child, &child, &home).unwrap());
        assert!(!parent_file(&parent, &child, &root.join("other-home")).unwrap());
        let folder = project.join("t.jsonl");
        std::fs::create_dir(&folder).unwrap();
        assert!(!parent_file(&folder, &child, &home).unwrap());
    }

    /// A transcript's key resolves its folder (a linked home included) but never its own name:
    /// a transcript that is now a link (to a file or a folder) has no key, so it is neither
    /// tracked nor moved to the link's target; neither has a folder or a missing file.
    #[cfg(unix)]
    #[test]
    fn a_transcript_key_resolves_the_folder_only() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        let home = root.join("home");
        symlink(&real, &home).unwrap();
        std::fs::write(real.join("a.jsonl"), b"{}\n").unwrap();
        assert_eq!(
            transcript_key(&home.join("a.jsonl")).unwrap(),
            real.join("a.jsonl")
        );

        let target = root.join("target.jsonl");
        std::fs::write(&target, b"{}\n").unwrap();
        symlink(&target, real.join("b.jsonl")).unwrap();
        symlink(&real, real.join("c.jsonl")).unwrap();
        std::fs::create_dir(real.join("d.jsonl")).unwrap();
        for name in ["b.jsonl", "c.jsonl", "d.jsonl"] {
            let err = transcript_key(&home.join(name)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        let gone = transcript_key(&home.join("e.jsonl")).unwrap_err();
        assert_eq!(gone.kind(), io::ErrorKind::NotFound);
    }

    /// A link is never followed: not to another session's transcript (which would lend the
    /// sub-agent that session's agent), nor back to the sub-agent itself.
    #[cfg(unix)]
    #[test]
    fn a_linked_parent_is_no_parent() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        let project = home.join("projects").join("p");
        let child = project.join("s").join("subagents").join("a.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&child, b"{}\n").unwrap();
        let other = project.join("other.jsonl");
        std::fs::write(&other, b"{}\n").unwrap();
        let parent = project.join("s.jsonl");
        for target in [&other, &child] {
            let _ = std::fs::remove_file(&parent);
            std::os::unix::fs::symlink(target, &parent).unwrap();
            assert!(!parent_file(&parent, &child, &home).unwrap(), "{target:?}");
        }
    }

    /// A watcher over one Claude home, with nothing tracked, and the agents it asks.
    fn bare_watcher(
        home: &Path,
        state: &Path,
    ) -> (
        Watcher,
        Arc<crate::MemoryAgents>,
        std::sync::mpsc::Receiver<Batch>,
    ) {
        watcher_with(
            home,
            state,
            Arc::new(pitcrew_interfaces::fake::FakeSource::new(
                Engine::Claude,
                Vec::new(),
                Vec::new(),
            )),
        )
    }

    /// A watcher over one Claude home read by `adapter`, with nothing tracked yet.
    fn watcher_with(
        home: &Path,
        state: &Path,
        adapter: Arc<dyn SourceAdapter>,
    ) -> (
        Watcher,
        Arc<crate::MemoryAgents>,
        std::sync::mpsc::Receiver<Batch>,
    ) {
        watcher_options(home, state, adapter, PollMode::Always, false)
    }

    fn watcher_options(
        home: &Path,
        state: &Path,
        adapter: Arc<dyn SourceAdapter>,
        poll: PollMode,
        cache_file_discovery: bool,
    ) -> (
        Watcher,
        Arc<crate::MemoryAgents>,
        std::sync::mpsc::Receiver<Batch>,
    ) {
        let agents = Arc::new(crate::MemoryAgents::new());
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let watcher = Watcher::new(Setup {
            notification_window: Duration::ZERO,
            byte_file_cursors: false,
            cache_file_discovery,
            workspace: WorkspaceId::new(),
            machine: MachineId::new(),
            owner: MemberId::new(),
            timing: Timing::default(),
            poll,
            max_batch: 64,
            homes: vec![EngineHome {
                engine: Engine::Claude,
                path: home.to_path_buf(),
            }],
            adapters: vec![adapter],
            rows: Vec::new(),
            store: Arc::new(Mutex::new(Store::open(state).unwrap())),
            tx,
            shared: Arc::default(),
            locations: None,
            agents: Some(agents.clone()),
            watched: Arc::default(),
        });
        (watcher, agents, rx)
    }

    /// A Claude adapter whose transcripts and items can grow: one item per read, the cursor's
    /// offset counting items.
    #[derive(Debug, Default)]
    struct Growing {
        discoveries: AtomicUsize,
        transcripts: Mutex<Vec<TranscriptRef>>,
        items: Mutex<Vec<pitcrew_interfaces::source::TranscriptItem>>,
    }

    impl SourceAdapter for Growing {
        fn engine(&self) -> Engine {
            Engine::Claude
        }

        fn discover(&self, _home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
            self.discoveries.fetch_add(1, Ordering::Relaxed);
            Ok(self.transcripts.lock().unwrap().clone())
        }

        fn read_from(
            &self,
            _t: &TranscriptRef,
            cursor: &Cursor,
        ) -> Result<ParseChunk, SourceError> {
            let items = self.items.lock().unwrap();
            let next: Vec<_> = items
                .get(usize::try_from(cursor.offset).unwrap())
                .cloned()
                .into_iter()
                .collect();
            Ok(ParseChunk {
                cursor: Cursor {
                    offset: cursor.offset + next.len() as u64,
                    state: None,
                },
                meta: None,
                items: next,
            })
        }

        fn read_page(
            &self,
            _t: &TranscriptRef,
            _before: Option<u64>,
            _limit: usize,
        ) -> Result<pitcrew_interfaces::source::TranscriptPage, SourceError> {
            Err(SourceError::Io(io::Error::other("not paged in this test")))
        }
    }

    #[test]
    fn only_unchanged_periodic_file_discovery_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let adapter = Arc::new(Growing::default());
        let (mut w, _agents, _rx) =
            watcher_options(&home, state.path(), adapter.clone(), PollMode::Never, true);
        assert!(
            w.notify.is_some(),
            "test needs the platform's native watcher"
        );
        assert!(w.rediscover(&[0], true).is_ok());
        assert!(w.rediscover(&[0], false).is_ok());
        assert_eq!(adapter.discoveries.load(Ordering::Relaxed), 1);
        std::fs::create_dir(home.join("new-project")).unwrap();
        assert!(w.rediscover(&[0], false).is_ok());
        assert_eq!(adapter.discoveries.load(Ordering::Relaxed), 2);
        // Notifications, explicit rescans and overflow ask for forced discovery.
        assert!(w.rediscover(&[0], true).is_ok());
        assert_eq!(adapter.discoveries.load(Ordering::Relaxed), 3);
        w.watch_failed.insert(home.join("failed-watch"));
        assert!(w.rediscover(&[0], false).is_ok());
        assert_eq!(adapter.discoveries.load(Ordering::Relaxed), 4);
        w.watch_failed.clear();
        let polled_state = tempfile::tempdir().unwrap();
        let (mut polled, _agents, _rx) = watcher_options(
            &home,
            polled_state.path(),
            adapter.clone(),
            PollMode::Always,
            true,
        );
        assert!(polled.rediscover(&[0], false).is_ok());
        assert!(polled.rediscover(&[0], false).is_ok());
        assert_eq!(
            adapter.discoveries.load(Ordering::Relaxed),
            6,
            "polled homes always discover"
        );
        w.cache_file_discovery = false;
        assert!(w.rediscover(&[0], false).is_ok());
        assert!(w.rediscover(&[0], false).is_ok());
        assert_eq!(
            adapter.discoveries.load(Ordering::Relaxed),
            8,
            "custom adapters keep their schedule"
        );
    }

    /// Hands every queued batch to the index, as the sink thread does once the sink accepts it.
    /// Returns the events.
    fn save_all(w: &Watcher, rx: &std::sync::mpsc::Receiver<Batch>) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(batch) = rx.try_recv() {
            w.store_lock().commit(&batch.commit).unwrap();
            if let Some(unsaved) = &batch.unsaved {
                unsaved.fetch_sub(1, Ordering::AcqRel);
            }
            events.extend(batch.events);
        }
        events
    }

    /// A transcript's whole row stays in memory only while it is in use or its changes are not
    /// saved; afterwards the watcher keeps only what tells a change, and the next change reads the
    /// row back and goes on from the saved cursor.
    #[test]
    fn a_saved_cold_row_is_let_go_and_read_back() {
        use pitcrew_interfaces::source::TranscriptItem;

        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = home.join("projects").join("p").join("s.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        let adapter = Arc::new(Growing::default());
        adapter.transcripts.lock().unwrap().push(TranscriptRef {
            engine: Engine::Claude,
            path: path.clone(),
            inner_id: None,
            size: 1,
            modified: 0,
        });
        let turn = |n: u64| TranscriptItem::TurnEnded {
            at: 1_790_755_200_000 + i64::try_from(n).unwrap(),
            offset: n,
        };
        adapter.items.lock().unwrap().extend([turn(0), turn(1)]);
        let (mut w, _agents, rx) = watcher_with(&home, state.path(), adapter.clone());
        w.timing.hot_window = Duration::ZERO;
        assert!(w.start().is_ok());
        let id = *w.tracked.keys().next().unwrap();
        w.let_go();
        assert_eq!(w.loaded, [id], "not saved yet: the row stays");
        assert!(w.tracked[&id].loaded.is_some());
        let first = save_all(&w, &rx);
        assert_eq!(first.len(), 3, "discovered and two turns: {first:?}");
        w.let_go();
        assert!(w.loaded.is_empty());
        assert!(w.tracked[&id].loaded.is_none());
        let t = &w.tracked[&id];
        assert!(t.discovered && t.caught_up);
        assert_eq!(t.size, 1);

        // Two more turns: the row is read back, and reading goes on from where it was saved.
        adapter.items.lock().unwrap().extend([turn(2), turn(3)]);
        std::fs::write(&path, b"xyz").unwrap();
        assert!(w.check(id).is_ok_and(|changed| changed));
        let more = save_all(&w, &rx);
        let offsets: Vec<u64> = more
            .iter()
            .map(|e| match &e.body {
                EventBody::TurnEnded {
                    receipt: pitcrew_protocol::model::Receipt::Transcript { offset, .. },
                    ..
                } => *offset,
                other => panic!("only the new turns: {other:?}"),
            })
            .collect();
        assert_eq!(offsets, [2, 3]);
        w.let_go();
        assert!(w.loaded.is_empty());
        let saved = w
            .store_lock()
            .load(w.tracked[&id].session)
            .unwrap()
            .unwrap();
        assert_eq!(saved.cursor.offset, 4);
        assert_eq!(saved.size, 3);
        assert_eq!(w.tracked[&id].size, 3);

        // Unchanged: nothing is read, and nothing is loaded.
        assert!(w.check(id).is_ok_and(|changed| !changed));
        assert!(w.loaded.is_empty());
    }

    #[test]
    fn hot_row_cache_is_bounded_and_never_evicts_unsaved_changes() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (mut w, _agents, _rx) = bare_watcher(home.path(), state.path());
        let mut row = Row {
            session: SessionId::new(),
            engine: Engine::Claude,
            path: home.path().join("s.jsonl"),
            inner_id: None,
            cursor: Arc::default(),
            size: 1,
            mtime: 0,
            identity: None,
            caught_up: true,
            generation: 0,
            discovered: true,
            accepted: HashSet::new(),
            meta: None,
            facts: Facts::default(),
        };
        let mut ids = Vec::new();
        for i in 0..HOT_ROW_CACHE + 3 {
            row.session = SessionId::new();
            row.path = home.path().join(format!("s{i}.jsonl"));
            w.store_lock().insert(&row).unwrap();
            let id = w.track(&Indexed::of(&row), Some(row.clone()), 0, 1, 0);
            w.tracked.get_mut(&id).unwrap().hot = true;
            ids.push(id);
        }
        // Both kinds of unsaved state must survive, even beyond the cache's bound.
        w.tracked
            .get_mut(&ids[0])
            .unwrap()
            .loaded
            .as_mut()
            .unwrap()
            .dirty = true;
        w.tracked[&ids[1]]
            .loaded
            .as_ref()
            .unwrap()
            .unsaved
            .store(1, Ordering::Release);
        w.let_go();
        assert_eq!(w.cached.len(), HOT_ROW_CACHE);
        assert_eq!(w.loaded.len(), 2);
        assert!(w.tracked[&ids[0]].loaded.is_some());
        assert!(w.tracked[&ids[1]].loaded.is_some());
        assert!(
            w.tracked[&ids[2]].loaded.is_none(),
            "oldest saved row evicted"
        );
        assert!(w.load(ids[2]), "evicted row can be read back");
        assert_eq!(
            w.tracked[&ids[2]].loaded.as_ref().unwrap().row,
            row_at(&w, ids[2])
        );
        let last = *ids.last().unwrap();
        w.store_lock().hide_transcripts(true).unwrap();
        assert!(w.load(last), "cache hit does not query the index");
        assert!(
            !w.cached.contains(&last),
            "in-use row leaves the saved cache"
        );
        w.tracked[&last]
            .loaded
            .as_ref()
            .unwrap()
            .unsaved
            .store(1, Ordering::Release);
        w.let_go();
        assert!(w.loaded.contains(&last), "changed cached row stays pending");
        assert!(!w.cached.contains(&last));
        w.tracked[&last]
            .loaded
            .as_ref()
            .unwrap()
            .unsaved
            .store(0, Ordering::Release);
        w.let_go();
        assert!(w.cached.contains(&last));
        w.store_lock().hide_transcripts(false).unwrap();
        // A saved row that cools down leaves the cache, even when space is available.
        w.timing.hot_window = Duration::ZERO;
        w.classify(last, 0);
        w.let_go();
        assert!(w.tracked[&last].loaded.is_none());
    }

    fn row_at(w: &Watcher, id: u64) -> Row {
        w.store_lock()
            .load(w.tracked[&id].session)
            .unwrap()
            .unwrap()
    }

    /// A change no batch carries yet keeps the whole row in memory until one does, as before:
    /// a report that repeats the state still moves when the state was last reported, and a late
    /// hook is judged by that.
    #[test]
    fn a_change_not_saved_yet_keeps_the_row() {
        use pitcrew_interfaces::source::TranscriptItem;

        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = home.join("projects").join("p").join("s.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        let adapter = Arc::new(Growing::default());
        adapter.transcripts.lock().unwrap().push(TranscriptRef {
            engine: Engine::Claude,
            path: path.clone(),
            inner_id: None,
            size: 1,
            modified: 0,
        });
        let base = 1_790_755_200_000;
        let turn = |n: u64| TranscriptItem::TurnEnded {
            at: base + i64::try_from(n).unwrap(),
            offset: n,
        };
        adapter.items.lock().unwrap().push(turn(0));
        let (mut w, _agents, rx) = watcher_with(&home, state.path(), adapter.clone());
        w.timing.hot_window = Duration::ZERO;
        assert!(w.start().is_ok());
        let id = *w.tracked.keys().next().unwrap();
        save_all(&w, &rx);
        w.let_go();
        assert!(w.loaded.is_empty());
        let session = w.tracked[&id].session;
        let now = w.store_lock().load(session).unwrap().unwrap().facts.state;

        // The same state, reported later with a status line: no event, nothing to save yet.
        let same = Reported {
            at: base + 100,
            to: now,
            status_line: Some("thinking".into()),
        };
        assert!(w.apply_report(id, &same).is_ok());
        assert!(rx.try_recv().is_err(), "no batch for a state that stands");
        w.let_go();
        assert_eq!(
            w.loaded,
            [id],
            "the report's time is not saved yet: the row stays"
        );

        // A hook from before that report is late, and changes nothing.
        let other = if now == SessionState::Working {
            SessionState::Waiting
        } else {
            SessionState::Working
        };
        let late = Reported {
            at: base + 50,
            to: other,
            status_line: None,
        };
        assert!(w.apply_report(id, &late).is_ok());
        assert!(rx.try_recv().is_err(), "a late hook moves nothing");

        // The next read carries the row, report time and all; then it goes.
        adapter.items.lock().unwrap().push(turn(1));
        std::fs::write(&path, b"xy").unwrap();
        assert!(w.check(id).is_ok_and(|changed| changed));
        save_all(&w, &rx);
        w.let_go();
        assert!(w.loaded.is_empty());
        let saved = w.store_lock().load(session).unwrap().unwrap();
        assert_eq!(saved.facts.reported_at, Some(base + 100));
        assert_eq!(saved.cursor.offset, 2);
    }

    /// When the index cannot say who a sub-agent's parent is, its hooks are refused (unknown),
    /// and the lookup is not kept: once the index answers, the parent is found and kept.
    #[test]
    fn a_failed_parent_lookup_refuses_and_is_tried_again() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let project = home.join("projects").join("p");
        let child = project.join("s").join("subagents").join("a.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&child, b"{}\n").unwrap();
        std::fs::write(project.join("s.jsonl"), b"{}\n").unwrap();
        let (mut w, agents, _rx) = bare_watcher(&home, state.path());

        let tref = TranscriptRef {
            engine: Engine::Claude,
            path: child.clone(),
            inner_id: None,
            size: 0,
            modified: 0,
        };
        let mut row = new_row(&tref);
        row.discovered = true;
        row.meta = Some(pitcrew_interfaces::source::SessionMeta {
            native_id: "a".into(),
            is_subagent: true,
            ..Default::default()
        });
        let sub = row.session;
        let id = w.track(&Indexed::of(&row), Some(row), 0, tref.size, tref.modified);

        w.store_lock().hide_transcripts(true).unwrap();
        assert_eq!(w.parent_for(id), Err(LookupFailed));
        assert_eq!(w.runs_as(sub, Err(LookupFailed)), SessionAgent::Unknown);
        assert_eq!(w.tracked[&id].parent, None, "a failure is not kept");
        // The sub-agent's own answer, when it has one, still stands.
        let own = SessionAgent::Agent {
            agent: MemberId::new(),
            owner: None,
        };
        agents.set(sub, own);
        assert_eq!(w.runs_as(sub, Err(LookupFailed)), own);
        agents.forget(sub);

        w.store_lock().hide_transcripts(false).unwrap();
        let Ok(Parent::Session(parent)) = w.parent_for(id) else {
            panic!("the parent is found once the index answers");
        };
        assert_eq!(w.tracked[&id].parent, Some(Parent::Session(parent)));
        let row = w.tracked.get_mut(&id).and_then(Tracked::row);
        assert_eq!(
            row.map(|r| r.facts.parent),
            Some(Some(Parent::Session(parent))),
            "kept with the row in memory, to be saved with it"
        );
        let writer = SessionAgent::Agent {
            agent: MemberId::new(),
            owner: Some(MemberId::new()),
        };
        agents.set(parent, writer);
        let found = w.parent_for(id);
        assert_eq!(w.runs_as(sub, found), writer);
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
        let mut handler = notify_handler(
            Arc::clone(&shared),
            Duration::from_millis(100),
            Duration::ZERO,
        );
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
        let mut handler = notify_handler(
            Arc::clone(&shared),
            Duration::from_millis(100),
            Duration::ZERO,
        );
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
    fn a_watcher_error_forces_discovery_and_a_full_sweep() {
        let shared = Shared::default();
        shared.watcher_error(&notify::Error::generic("synthetic failure"));
        let signals = shared.lock();
        assert!(signals.overflow);
        assert!(signals.rediscover_at.is_some());
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
            layout: None,
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
