//! The watcher: discovers transcripts, notices changes, reads from stored cursors, and turns items
//! into events for the sink.
//!
//! One thread owns all transcript state. Notifications only mark paths dirty (a map bounded by the
//! number of paths), so a slow sink stalls the reader, not memory: the watcher blocks on the
//! bounded channel, and changes that arrive meanwhile coalesce into one read per file.

use crate::config::{EngineHome, PollMode, Timing};
use crate::derive::{self, Derived, Facts};
use crate::fsinfo::{self, FileStat};
use crate::sink::Batch;
use crate::store::{Commit, Row, Store, path_text};
use notify::event::{EventKind, MetadataKind, ModifyKind};
use notify::{RecursiveMode, Watcher as _};
use pitcrew_interfaces::source::{Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptRef};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MachineId, MemberId, SessionId, WorkspaceId};
use pitcrew_protocol::model::{Engine, Session, TimestampMs};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
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

/// Signals from notifications and the handle to the watcher thread.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    signals: Mutex<Signals>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct Signals {
    /// Changed paths, each with when to read it.
    dirty: HashMap<PathBuf, Instant>,
    /// Too many dirty paths: check every transcript instead.
    overflow: bool,
    rediscover_at: Option<Instant>,
    stop: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Signals> {
        self.signals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn mark(&self, paths: Vec<PathBuf>, due: Instant) {
        let mut s = self.lock();
        for p in paths {
            if s.dirty.len() >= MAX_DIRTY_PATHS && !s.dirty.contains_key(&p) {
                s.overflow = true;
                continue;
            }
            // Keep the earliest due time: a stream of writes is read every `debounce`, not starved.
            s.dirty.entry(p).or_insert(due);
        }
        drop(s);
        self.cv.notify_one();
    }

    pub fn rescan(&self) {
        self.lock().rediscover_at = Some(Instant::now());
        self.cv.notify_one();
    }

    pub fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_one();
    }
}

/// The notification callback: it only marks paths.
pub(crate) fn notify_handler(
    shared: Arc<Shared>,
    debounce: Duration,
) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
    move |res| match res {
        // Reads (ours included) and access-time updates are not changes.
        Ok(ev)
            if matches!(
                ev.kind,
                EventKind::Access(_)
                    | EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
            ) => {}
        Ok(ev) => shared.mark(ev.paths, Instant::now() + debounce),
        Err(e) => tracing::warn!(error = %e, "file watcher error"),
    }
}

/// A CLI home and how it is watched.
struct Home {
    home: EngineHome,
    adapter: Arc<dyn SourceAdapter>,
    polled: bool,
}

/// One transcript. `row` is the in-memory state, ahead of the store until the sink accepts.
struct Tracked {
    row: Row,
    tref: TranscriptRef,
    home: usize,
    hot: bool,
    watched: Vec<PathBuf>,
    poll_every: Duration,
    next_poll: Instant,
}

/// Why the watcher stopped early.
struct Hangup;

pub(crate) struct Watcher {
    workspace: WorkspaceId,
    machine: MachineId,
    owner: MemberId,
    timing: Timing,
    max_batch: usize,
    homes: Vec<Home>,
    store: Arc<Mutex<Store>>,
    tx: SyncSender<Batch>,
    shared: Arc<Shared>,
    notify: Option<notify::RecommendedWatcher>,
    tracked: Vec<Tracked>,
    by_key: HashMap<(PathBuf, Option<String>), usize>,
    by_path: HashMap<PathBuf, Vec<usize>>,
    dirs: HashMap<PathBuf, usize>,
    next_sweep: Instant,
    next_rediscover: Instant,
    last_rediscover: Option<Instant>,
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
    pub store: Arc<Mutex<Store>>,
    pub tx: SyncSender<Batch>,
    pub shared: Arc<Shared>,
}

impl Watcher {
    pub fn new(s: Setup) -> Result<Self, notify::Error> {
        let adapter_for = |engine: Engine| s.adapters.iter().find(|a| a.engine() == engine);
        let mut homes = Vec::new();
        for h in s.homes {
            let Some(adapter) = adapter_for(h.engine) else {
                tracing::warn!(engine = ?h.engine, home = %h.path.display(), "no adapter for this home; skipping it");
                continue;
            };
            let path = h.path.canonicalize().unwrap_or(h.path);
            let polled = match s.poll {
                PollMode::Always => true,
                PollMode::Never => false,
                PollMode::Auto => fsinfo::is_network_fs(&path),
            };
            if polled {
                tracing::info!(home = %path.display(), "polling this home (network filesystem or forced)");
            }
            homes.push(Home {
                home: EngineHome {
                    engine: h.engine,
                    path,
                },
                adapter: Arc::clone(adapter),
                polled,
            });
        }
        let notify = if homes.iter().any(|h| !h.polled) {
            Some(notify::recommended_watcher(notify_handler(
                Arc::clone(&s.shared),
                s.timing.debounce,
            ))?)
        } else {
            None
        };
        let now = Instant::now();
        Ok(Self {
            workspace: s.workspace,
            machine: s.machine,
            owner: s.owner,
            next_sweep: now + s.timing.cold_interval,
            next_rediscover: now + s.timing.rediscover_interval,
            timing: s.timing,
            max_batch: s.max_batch.max(1),
            homes,
            store: s.store,
            tx: s.tx,
            shared: s.shared,
            notify,
            tracked: Vec::new(),
            by_key: HashMap::new(),
            by_path: HashMap::new(),
            dirs: HashMap::new(),
            last_rediscover: None,
        })
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

    /// Loads the index, catches up on changes made while the runner was down, then discovers.
    fn start(&mut self) -> Result<(), Hangup> {
        let rows = match self.store_lock().load_all() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(error = %e, "cannot load the runner index");
                Vec::new()
            }
        };
        for row in rows {
            let Some(home) = self.home_for(row.engine, &row.path) else {
                continue;
            };
            let tref = TranscriptRef {
                engine: row.engine,
                path: row.path.clone(),
                inner_id: row.inner_id.clone(),
                size: row.size,
                modified: row.mtime,
            };
            self.track(row, tref, home);
        }
        for i in 0..self.tracked.len() {
            self.check(i)?;
        }
        self.rediscover()
    }

    /// Sleeps until something is due. Returns what woke it, or `None` to stop.
    fn wait(&mut self) -> Option<Wake> {
        let mut deadline = self.next_sweep.min(self.next_rediscover);
        for t in &self.tracked {
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
            if let Some(r) = s.rediscover_at {
                next = next.min(r);
            }
            if let Some(d) = s.dirty.values().min() {
                next = next.min(*d);
            }
            if s.overflow || next <= now {
                let mut due = Vec::new();
                s.dirty.retain(|p, d| {
                    if *d <= now {
                        due.push(p.clone());
                        false
                    } else {
                        true
                    }
                });
                let rediscover = s.rediscover_at.is_some_and(|r| r <= now);
                if rediscover {
                    s.rediscover_at = None;
                }
                let overflow = std::mem::take(&mut s.overflow);
                return Some(Wake {
                    due,
                    overflow,
                    rediscover,
                });
            }
            s = shared
                .cv
                .wait_timeout(s, next - now)
                .map_or_else(|e| e.into_inner().0, |(g, _)| g);
        }
    }

    fn handle(&mut self, wake: Wake) -> Result<(), Hangup> {
        let now = Instant::now();
        let mut unknown = false;
        for path in wake.due {
            match self.by_path.get(&path).cloned() {
                Some(ids) => {
                    for i in ids {
                        self.check(i)?;
                    }
                }
                None => unknown = true,
            }
        }
        if unknown {
            // A new file or folder next to a watched transcript: maybe a new session.
            let at = self
                .last_rediscover
                .map_or(now, |l| l + REDISCOVER_GAP)
                .max(now + self.timing.debounce);
            let mut s = self.shared.lock();
            s.rediscover_at = Some(s.rediscover_at.map_or(at, |r| r.min(at)));
        }
        if wake.rediscover || now >= self.next_rediscover {
            self.rediscover()?;
        }
        if wake.overflow || now >= self.next_sweep {
            self.next_sweep = now + self.timing.cold_interval;
            for i in 0..self.tracked.len() {
                self.check(i)?;
            }
        }
        for i in 0..self.tracked.len() {
            let t = &self.tracked[i];
            if t.hot && self.homes[t.home].polled && t.next_poll <= now {
                let changed = self.check(i)?;
                let t = &mut self.tracked[i];
                t.poll_every = if changed {
                    self.timing.poll_min
                } else {
                    (t.poll_every * 2).min(self.timing.poll_max)
                };
                t.next_poll = Instant::now() + t.poll_every;
            }
        }
        Ok(())
    }

    fn rediscover(&mut self) -> Result<(), Hangup> {
        let now = Instant::now();
        self.last_rediscover = Some(now);
        self.next_rediscover = now + self.timing.rediscover_interval;
        for h in 0..self.homes.len() {
            let home = &self.homes[h];
            let found = match home.adapter.discover(&home.home.path) {
                Ok(found) => found,
                Err(e) => {
                    tracing::warn!(home = %home.home.path.display(), error = %e, "discovery failed");
                    continue;
                }
            };
            for tref in found {
                let key = (tref.path.clone(), tref.inner_id.clone());
                if self.by_key.contains_key(&key) {
                    continue;
                }
                let row = Row {
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
                    emitted_through: None,
                    meta: None,
                    facts: Facts::default(),
                };
                if path_text(&row.path).is_err() {
                    tracing::warn!(path = %row.path.display(), "skipping a transcript whose path is not Unicode");
                    continue;
                }
                // The session id is saved before anything is sent, so it never changes.
                if let Err(e) = self.store_lock().insert(&row) {
                    tracing::error!(path = %row.path.display(), error = %e, "cannot index a transcript");
                    continue;
                }
                tracing::debug!(path = %row.path.display(), session = %row.session, "new transcript");
                let i = self.track(row, tref, h);
                self.check(i)?;
            }
        }
        Ok(())
    }

    fn track(&mut self, row: Row, tref: TranscriptRef, home: usize) -> usize {
        let i = self.tracked.len();
        self.by_key
            .insert((row.path.clone(), row.inner_id.clone()), i);
        self.by_path.entry(row.path.clone()).or_default().push(i);
        self.tracked.push(Tracked {
            row,
            tref,
            home,
            hot: false,
            watched: Vec::new(),
            poll_every: self.timing.poll_min,
            next_poll: Instant::now(),
        });
        i
    }

    fn home_for(&self, engine: Engine, path: &Path) -> Option<usize> {
        let same = |h: &Home| h.home.engine == engine;
        self.homes
            .iter()
            .position(|h| same(h) && path.starts_with(&h.home.path))
            .or_else(|| self.homes.iter().position(same))
    }

    /// Stats one transcript and reads it if it changed. Returns whether it changed.
    fn check(&mut self, i: usize) -> Result<bool, Hangup> {
        let st = match fsinfo::stat(&self.tracked[i].row.path) {
            Ok(st) => st,
            Err(e) => {
                tracing::debug!(path = %self.tracked[i].row.path.display(), error = %e, "transcript not readable");
                return Ok(false);
            }
        };
        self.classify(i, st.mtime);
        let row = &self.tracked[i].row;
        if row.caught_up
            && row.size == st.size
            && row.mtime == st.mtime
            && fsinfo::same_file(row.identity.as_deref(), st.identity.as_deref())
        {
            return Ok(false);
        }
        self.refresh(i, st)?;
        Ok(true)
    }

    /// Hot transcripts get their folder (and its parent, where new session folders appear)
    /// watched; cold ones are left to the sweep.
    fn classify(&mut self, i: usize, mtime: TimestampMs) {
        let age = fsinfo::millis(SystemTime::now()).saturating_sub(mtime);
        let hot = u128::try_from(age).unwrap_or(0) < self.timing.hot_window.as_millis();
        let t = &mut self.tracked[i];
        if t.hot == hot {
            return;
        }
        t.hot = hot;
        t.next_poll = Instant::now();
        if self.homes[t.home].polled {
            return;
        }
        if hot {
            let home = &self.homes[t.home].home.path;
            let dirs: Vec<PathBuf> = t
                .row
                .path
                .ancestors()
                .skip(1)
                .take(2)
                .filter(|d| d.starts_with(home))
                .map(Path::to_path_buf)
                .collect();
            for d in &dirs {
                self.watch_dir(d);
            }
            self.tracked[i].watched = dirs;
        } else {
            for d in std::mem::take(&mut t.watched) {
                self.unwatch_dir(&d);
            }
        }
    }

    fn watch_dir(&mut self, dir: &Path) {
        let count = self.dirs.entry(dir.to_path_buf()).or_insert(0);
        *count += 1;
        if *count == 1
            && let Some(w) = self.notify.as_mut()
            && let Err(e) = w.watch(dir, RecursiveMode::NonRecursive)
        {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot watch a folder; the sweep still covers it");
        }
    }

    fn unwatch_dir(&mut self, dir: &Path) {
        let Some(count) = self.dirs.get_mut(dir) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.dirs.remove(dir);
            if let Some(w) = self.notify.as_mut() {
                let _ = w.unwatch(dir);
            }
        }
    }

    /// Reads from the stored cursor until the adapter has nothing more.
    fn refresh(&mut self, i: usize, st: FileStat) -> Result<(), Hangup> {
        if needs_reindex(&self.tracked[i].row, &st) {
            self.reindex(i, &st);
        }
        let adapter = Arc::clone(&self.homes[self.tracked[i].home].adapter);
        {
            let t = &mut self.tracked[i];
            t.tref.size = st.size;
            t.tref.modified = st.mtime;
        }
        let mut retried = false;
        for n in 0..=MAX_READS_PER_REFRESH {
            if n == MAX_READS_PER_REFRESH {
                // Let other transcripts have a turn; this one is read again next wake-up.
                let path = self.tracked[i].row.path.clone();
                self.shared.mark(vec![path], Instant::now());
                break;
            }
            let t = &self.tracked[i];
            let cursor = t.row.cursor.clone();
            match adapter.read_from(&t.tref, &cursor) {
                Ok(chunk) => {
                    // The last read saves `caught_up`, even when it found nothing new.
                    let more = !chunk.items.is_empty() && chunk.cursor.offset != cursor.offset;
                    self.process(i, chunk, &st, !more)?;
                    if !more {
                        break;
                    }
                }
                Err(SourceError::Unreadable { reason, .. })
                    if !retried && t.row.inner_id.is_none() && cursor.offset > st.size =>
                {
                    tracing::debug!(reason, "adapter reports a shorter file");
                    retried = true;
                    self.reindex(i, &st);
                }
                Err(e) => {
                    tracing::warn!(path = %t.row.path.display(), error = %e, "cannot read transcript");
                    break;
                }
            }
        }
        Ok(())
    }

    fn reindex(&mut self, i: usize, st: &FileStat) {
        let row = &mut self.tracked[i].row;
        tracing::warn!(
            path = %row.path.display(),
            session = %row.session,
            was = row.size,
            now = st.size,
            "transcript was truncated or replaced; re-indexing it from the start"
        );
        row.cursor = Cursor::default();
        row.generation += 1;
        row.emitted_through = None;
        row.facts.open_calls.clear();
        row.facts.last_key = None;
        row.facts.seq = 0;
    }

    /// Turns one chunk into events and queues them; the store is updated once they are accepted.
    fn process(
        &mut self,
        i: usize,
        chunk: ParseChunk,
        st: &FileStat,
        caught_up: bool,
    ) -> Result<(), Hangup> {
        let (workspace, owner, machine) = (self.workspace, self.owner, self.machine);
        let t = &mut self.tracked[i];
        let session = t.row.session;
        if let Some(meta) = chunk.meta {
            t.row.meta = Some(meta);
        }
        let cwd = t.row.meta.as_ref().and_then(|m| m.cwd.clone());
        let emit_states = t.row.discovered;
        let skip_through = t.row.emitted_through;

        let mut derived: Vec<Derived> = Vec::new();
        for item in &chunk.items {
            let before = derived.len();
            derive::apply(
                &mut t.row.facts,
                session,
                cwd.as_deref(),
                item,
                emit_states,
                &mut derived,
            );
            // Already accepted before a crash: fold the item, don't send it again.
            if skip_through.is_some_and(|s| item.offset() <= s) {
                derived.truncate(before);
            }
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
        if !t.row.discovered {
            let s = session_of(&t.row, &t.tref, machine);
            let id = event_id(session, 0, None, 0, s.started);
            events.push((
                None,
                event(id, s.started, EventBody::SessionDiscovered { session: s }),
            ));
            t.row.discovered = true;
        }
        for d in derived {
            let seq = next_seq(&mut t.row.facts, d.key);
            let id = event_id(session, t.row.generation, Some(d.key), seq, d.at);
            t.row.emitted_through = Some(t.row.emitted_through.map_or(d.key, |e| e.max(d.key)));
            events.push((Some(d.key), event(id, d.at, d.body)));
        }
        t.row.cursor = chunk.cursor;
        t.row.size = st.size;
        t.row.mtime = st.mtime;
        t.row.identity.clone_from(&st.identity);
        t.row.caught_up = caught_up;

        let batches = split(events, self.max_batch, skip_through, &t.row);
        for b in batches {
            self.tx.send(b).map_err(|_| Hangup)?;
        }
        Ok(())
    }

    fn store_lock(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct Wake {
    due: Vec<PathBuf>,
    overflow: bool,
    rediscover: bool,
}

/// A smaller file, or a different file (inode) at the same path, means it was truncated or
/// replaced. Multi-session stores (`inner_id`) change size for other reasons and are not judged
/// this way.
fn needs_reindex(row: &Row, st: &FileStat) -> bool {
    let read_before = row.cursor != Cursor::default();
    let replaced = !fsinfo::same_file(row.identity.as_deref(), st.identity.as_deref());
    row.inner_id.is_none() && read_before && (st.size < row.size || replaced)
}

/// Splits events into batches of about `max`, never splitting one offset's events, so a partial
/// commit's `emitted_through` covers whole records. The last batch saves the full row.
fn split(
    events: Vec<(Option<u64>, Event)>,
    max: usize,
    mut through: Option<u64>,
    row: &Row,
) -> Vec<Batch> {
    let mut out = Vec::new();
    let mut current: Vec<Event> = Vec::new();
    let mut last_key: Option<u64> = None;
    for (key, ev) in events {
        if current.len() >= max && key.is_some() && key != last_key {
            out.push(Batch {
                events: std::mem::take(&mut current),
                commit: Commit::Partial {
                    session: row.session,
                    emitted_through: through,
                    discovered: true,
                },
            });
        }
        if let Some(k) = key {
            through = Some(through.map_or(k, |t| t.max(k)));
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

fn next_seq(facts: &mut Facts, key: u64) -> u32 {
    if facts.last_key == Some(key) {
        facts.seq += 1;
    } else {
        facts.last_key = Some(key);
        facts.seq = 0;
    }
    facts.seq
}

/// A ULID whose time is the event's and whose random part is a hash of where it came from, so an
/// event sent again after a crash has the same id.
fn event_id(
    session: SessionId,
    generation: u32,
    key: Option<u64>,
    seq: u32,
    at: TimestampMs,
) -> EventId {
    let mut h = Sha256::new();
    h.update(session.0.to_bytes());
    h.update(generation.to_le_bytes());
    match key {
        Some(k) => {
            h.update([1]);
            h.update(k.to_le_bytes());
        }
        None => h.update([0]),
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

fn session_of(row: &Row, tref: &TranscriptRef, machine: MachineId) -> Session {
    let meta = row.meta.clone().unwrap_or_default();
    let native_id = Some(meta.native_id)
        .filter(|n| !n.is_empty())
        .or_else(|| row.inner_id.clone())
        .or_else(|| {
            row.path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let last_activity = if row.facts.last_activity > 0 {
        row.facts.last_activity
    } else {
        tref.modified
    };
    Session {
        id: row.session,
        engine: row.engine,
        native_id,
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
        terminal: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            emitted_through: None,
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
        let mut fresh = row();
        fresh.cursor = Cursor::default();
        assert!(!needs_reindex(&fresh, &stat(5, "1:2")));
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
        let a = event_id(s, 0, Some(10), 0, 1000);
        assert_eq!(a, event_id(s, 0, Some(10), 0, 1000));
        assert_ne!(a, event_id(s, 0, Some(10), 1, 1000));
        assert_ne!(a, event_id(s, 1, Some(10), 0, 1000));
        assert_ne!(a, event_id(s, 0, None, 0, 1000));
        assert_eq!(a.0.timestamp_ms(), 1000);
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
        let batches = split(vec![ev(1), ev(1), ev(1), ev(2), ev(3)], 2, None, &r);
        let sizes: Vec<usize> = batches.iter().map(|b| b.events.len()).collect();
        assert_eq!(sizes, [3, 2]);
        assert!(matches!(
            batches[0].commit,
            Commit::Partial {
                emitted_through: Some(1),
                ..
            }
        ));
        assert!(matches!(batches[1].commit, Commit::Full(_)));
    }
}
