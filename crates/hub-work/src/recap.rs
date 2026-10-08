//! The recap index: activity blocks with their lines, and day paragraphs, as
//! `GET /v1/recaps/blocks` and `GET /v1/recaps/days` serve them (`docs/build/contracts/api-v1.md`,
//! "Recaps").
//!
//! - [`Recaps`] keeps the recap engine's [`BlockBuilder`] fed with events in log order, holds every
//!   block it has made (open and closed), indexed by their links, and caches day paragraphs.
//!   [`Recaps::blocks`] and [`Recaps::days`] answer the two queries.
//! - [`RecapIndex`] is the seam for the API layer, like [`crate::EventRefs`]: [`WorkService`]
//!   implements it over its store, and the daemon adapts it to the API's recap source.
//!
//! # Kept current from the log
//!
//! The service's index starts empty and reads the log from revision 1. **Every query first reads
//! the log from the last revision the index has seen** (pages of [`Store::since`]), so a page
//! reflects every event appended before the query began, by this process or another, and no event
//! is missed or applied twice. This is the back office's "from the first revision not looked at"
//! without the subscription: nothing needs to announce appends, and a client that refetches after
//! a stream frame always gets the event the frame carried. [`WorkService::sync_recaps`] does the
//! same reading eagerly, e.g. to build the index at start rather than on the first request.
//!
//! What the engine knows about the workspace (its [`Directory`]: members, sessions, tasks,
//! workstreams, dispatches, asks) comes from the same events, read from the start: the log holds
//! everything the projections were built from, so the directory is the projections' state as of
//! each event, the links "as they were when the events happened" that the contract asks for.
//! Seeding it with the projections as they are now would put today's links in front of
//! yesterday's events, and an index built yesterday and kept current would then differ from one
//! rebuilt today.
//!
//! One directory serves both roles (grouping events into blocks, and naming things in lines and
//! paragraphs): [`Recaps`] keeps no second copy. [`Directory::names_version`] says when a name a
//! paragraph may show has changed, including past the directory's bound (see "Day paragraphs are
//! cached" below).
//!
//! A `task_created` that the tasks projection refused (its key was taken, see "One writer") is not
//! activity here either: it is left out, so a task the hub never had never shows in a recap. So
//! the index reads no further than the tasks projection has applied; when another process appended
//! without the work model's projections, the rest waits until this store's next append catches
//! them up.
//!
//! # Day paragraphs are cached
//!
//! By scope, `tz`, date and workstream, with the blocks each paragraph covers: their ids and last
//! events. A query recomputes an entry only when its blocks changed (a block grew, began, or moved
//! to another day or workstream), or when a name it could show changed. At most a set number of
//! entries are kept ([`DAY_CACHE_ENTRIES`]); past that, the one used longest ago goes.
//!
//! So a query reads only its blocks' heads (id, start, workstream, last event) to find its dates
//! and check each cached paragraph, and reads and decodes the bodies of a paragraph's blocks only
//! when it writes that paragraph again.
//!
//! [`Store::since`]: pitcrew_store::Store::since

use crate::codec::sql_rev;
use crate::error::{Result, WorkError};
use crate::projection::Tasks;
use crate::recap_db::{self, BlockDb, Head, Place, Scope};
use crate::service::WorkService;
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{EventId, MemberId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{Date, TimestampMs};
use pitcrew_protocol::recap::{
    BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT, Block, BlocksPage, DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT,
    DayRecap, DaysPage, FactKind, MAX_TZ_MINUTES, RecapBlock,
};
use pitcrew_recap::{
    BlockBuilder, Config, Directory, RuleSummarizer, block_line, date_of, day_recaps,
};
use pitcrew_store::sql::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{MutexGuard, PoisonError};

/// The most day paragraphs a [`Recaps`] keeps by default.
pub const DAY_CACHE_ENTRIES: usize = 2_048;

/// Events read from the log at a time while catching up.
const SYNC_PAGE: usize = 1_000;

const DAY_MS: i64 = 86_400_000;

/// Which blocks to find: those linked to **all** of the given session, task, workstream and
/// project (`GET /v1/recaps/blocks?session=&task=&workstream=&project=`). No field: every block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockFilter {
    /// The block's session.
    pub session: Option<SessionId>,
    /// One of the block's tasks (its first 8, as the block lists them).
    pub task: Option<TaskId>,
    /// The block's workstream.
    pub workstream: Option<WorkstreamId>,
    /// The block's project.
    pub project: Option<ProjectId>,
}

impl BlockFilter {
    /// Whether `block` is linked to everything the filter names.
    #[must_use]
    pub fn matches(&self, block: &Block) -> bool {
        self.session.is_none_or(|s| block.session == Some(s))
            && self.task.is_none_or(|t| block.tasks.contains(&t))
            && self.workstream.is_none_or(|w| block.workstream == Some(w))
            && self.project.is_none_or(|p| block.project == Some(p))
    }
}

/// Whose days `GET /v1/recaps/days` pages through: exactly one workstream or one project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DaysScope {
    /// The paragraphs over the blocks whose workstream it is, one per day.
    Workstream(WorkstreamId),
    /// From the blocks whose project it is, one paragraph per workstream per day, plus one per
    /// day, without a workstream, for the project's work outside any workstream.
    Project(ProjectId),
}

/// The recap queries, for the API layer's recap routes. [`WorkService`] implements it; the daemon
/// passes its one `Arc<WorkService>` to the API through a small adapter, as it does for
/// [`crate::EventRefs`].
///
/// Both calls block (they may read the log, see the [module docs](self)): call them on the
/// blocking pool. Neither panics on any input; what the contract calls `400 invalid` that reaches
/// them is an `invalid` error.
pub trait RecapIndex: Send + Sync + std::fmt::Debug {
    /// Blocks matching **all** of `filter`, newest first by id, each with its line.
    ///
    /// - `before` is a block id (any event id), exclusive.
    /// - `limit`: `None` is [`BLOCKS_DEFAULT_LIMIT`]; more than [`BLOCKS_MAX_LIMIT`] counts as
    ///   that.
    /// - `at_start` is true exactly when no older matching block exists, so a page that is not at
    ///   the start holds at least one block. An id nothing is linked to gives an empty page with
    ///   `at_start`.
    ///
    /// # Errors
    ///
    /// `invalid` for a `limit` of 0; reading the log (an internal error).
    fn recap_blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: Option<usize>,
    ) -> Result<BlocksPage>;

    /// Day paragraphs of `scope` at `tz_minutes` east of UTC (a block belongs to the day its start
    /// falls on there), newest date first; within a date, the entry without a workstream first,
    /// then by workstream id.
    ///
    /// - `before` is a date, exclusive.
    /// - `limit` counts dates: `None` is [`DAYS_DEFAULT_LIMIT`]; more than [`DAYS_MAX_LIMIT`]
    ///   counts as that. A page holds every entry of its dates.
    /// - `at_start` is true exactly when no older date has an entry, so a page that is not at the
    ///   start holds at least one date. An unknown workstream or project gives an empty page with
    ///   `at_start`.
    ///
    /// # Errors
    ///
    /// `invalid` for a `tz_minutes` beyond [`MAX_TZ_MINUTES`] either way, a `limit` of 0, or a
    /// `before` that is not `YYYY-MM-DD`; reading the log (an internal error).
    fn recap_days(
        &self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<&Date>,
        limit: Option<usize>,
    ) -> Result<DaysPage>;
}

/// Most ids [`Unnamed`] and the asks' descriptions hold. Ids come from untrusted events; past this,
/// every name learned counts as a change.
const MAX_NAMED: usize = 100_000;

/// Members, tasks and workstreams a block names while their names are not known. Lines and
/// paragraphs call them "someone", "a task" or "a workstream"; when one of them becomes known,
/// paragraphs already written may say it differently now.
#[derive(Debug, Default)]
struct Unnamed {
    members: HashSet<MemberId>,
    tasks: HashSet<TaskId>,
    workstreams: HashSet<WorkstreamId>,
    /// A set was full: any name learned may be one of them.
    overflow: bool,
}

impl Unnamed {
    fn add<K: Hash + Eq>(set: &mut HashSet<K>, overflow: &mut bool, id: K) {
        if set.len() < MAX_NAMED || set.contains(&id) {
            set.insert(id);
        } else {
            *overflow = true;
        }
    }

    fn member(&mut self, names: &Directory, id: MemberId) {
        if names.handle(id).is_none() {
            Self::add(&mut self.members, &mut self.overflow, id);
        }
    }

    fn task(&mut self, names: &Directory, id: TaskId) {
        if names.task_key(id).is_none() {
            Self::add(&mut self.tasks, &mut self.overflow, id);
        }
    }

    fn workstream(&mut self, names: &Directory, id: WorkstreamId) {
        if names.workstream_name(id).is_none() {
            Self::add(&mut self.workstreams, &mut self.overflow, id);
        }
    }

    /// Notes everything `block` names, in its facts or links, that `names` does not know.
    fn note(&mut self, names: &Directory, block: &Block) {
        for m in block.agent.iter().chain(&block.actors) {
            self.member(names, *m);
        }
        for t in &block.tasks {
            self.task(names, *t);
        }
        if let Some(w) = block.workstream {
            self.workstream(names, w);
        }
        for fact in &block.facts {
            self.member(names, fact.by);
            // Every kind, so that a new one is a compile error here until it is looked at.
            match &fact.kind {
                FactKind::SessionStarted { .. }
                | FactKind::SessionWaiting { .. }
                | FactKind::SessionEnded
                | FactKind::Checks { .. }
                | FactKind::JobDiverged { .. }
                | FactKind::DecisionRecorded { .. } => {}
                FactKind::SessionLinked { workstream, task } => {
                    if let Some(w) = workstream {
                        self.workstream(names, *w);
                    }
                    if let Some(t) = task {
                        self.task(names, *t);
                    }
                }
                FactKind::DispatchStarted { task, agent } => {
                    self.task(names, *task);
                    self.member(names, *agent);
                }
                FactKind::DispatchFinished { task, .. } => {
                    if let Some(t) = task {
                        self.task(names, *t);
                    }
                }
                FactKind::TaskCreated { task }
                | FactKind::TaskMoved { task, .. }
                | FactKind::PlanUpdated { task, .. } => self.task(names, *task),
                FactKind::TaskAssigned { task, assignee } => {
                    self.task(names, *task);
                    if let Some(a) = assignee {
                        self.member(names, *a);
                    }
                }
                FactKind::AskRaised { to, .. } => self.member(names, *to),
                FactKind::AskAnswered { ask } => {
                    if let Some((_, from)) = names.ask(*ask) {
                        self.member(names, from);
                    }
                }
                FactKind::Commented {
                    task,
                    workstream,
                    mentions,
                } => {
                    if let Some(t) = task {
                        self.task(names, *t);
                    }
                    if let Some(w) = workstream {
                        self.workstream(names, *w);
                    }
                    for m in mentions {
                        self.member(names, *m);
                    }
                }
                FactKind::WorkstreamCreated { workstream }
                | FactKind::WorkstreamChanged { workstream, .. } => {
                    self.workstream(names, *workstream);
                }
                FactKind::BriefAccepted { target, .. } => {
                    if let BriefTarget::Workstream(w) = target {
                        self.workstream(names, *w);
                    }
                }
            }
        }
    }
}

/// Runs a call into the recap engine. The engine is not meant to panic on any input; if it does,
/// that is an internal error for this request, not a poisoned index that every request rebuilds.
fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f))
        .map_err(|_| WorkError::internal(format!("the recap engine panicked {what}")))
}

/// One cached paragraph's place.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DayKey {
    scope: DaysScope,
    tz: i32,
    date: Date,
    workstream: Option<WorkstreamId>,
}

#[derive(Debug)]
struct CachedDay {
    /// `(id, last)` of each block covered, in order: what the paragraph was written from.
    covers: Vec<(EventId, EventId)>,
    /// The names generation it was written with.
    names: u64,
    recaps: Vec<DayRecap>,
    used: u64,
}

/// Day paragraphs, bounded, least recently used out first.
#[derive(Debug)]
struct DayCache {
    entries: HashMap<DayKey, CachedDay>,
    capacity: usize,
    clock: u64,
    made: u64,
}

impl DayCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            clock: 0,
            made: 0,
        }
    }

    /// The paragraph for the blocks `covers` names (`(id, last)` of one workstream's blocks on
    /// one date, in order), from the cache when it was written from the same blocks and names;
    /// otherwise written from the blocks `load` reads (those, in that order).
    fn get_or_make(
        &mut self,
        key: DayKey,
        covers: Vec<(EventId, EventId)>,
        names: &Directory,
        names_gen: u64,
        load: impl FnOnce() -> Result<Vec<Block>>,
    ) -> Result<Vec<DayRecap>> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(hit) = self.entries.get_mut(&key)
            && hit.covers == covers
            && hit.names == names_gen
        {
            hit.used = self.clock;
            return Ok(hit.recaps.clone());
        }
        let owned = load()?;
        if !owned
            .iter()
            .map(|b| (b.id, b.last))
            .eq(covers.iter().copied())
        {
            return Err(WorkError::internal(
                "the recap index's blocks do not match their heads",
            ));
        }
        let recaps = guarded("writing a day recap", || {
            day_recaps(&owned, names, key.tz, &RuleSummarizer)
        })?
        .map_err(|e| WorkError::internal(format!("writing a day recap: {e}")))?;
        self.made = self.made.saturating_add(1);
        if self.capacity == 0 {
            return Ok(recaps);
        }
        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            self.evict();
        }
        self.entries.insert(
            key,
            CachedDay {
                covers,
                names: names_gen,
                recaps: recaps.clone(),
                used: self.clock,
            },
        );
        Ok(recaps)
    }

    fn evict(&mut self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, c)| c.used)
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest {
            self.entries.remove(&key);
        }
    }
}

/// The recaps of a log: the recap engine's blocks, kept current as events are pushed, with their
/// lines and day paragraphs. See the [module docs](self).
///
/// It reads no store and no clock. [`WorkService`] keeps one current from its store; build one
/// directly to recap a slice of a log, e.g. the demo fixture's.
///
/// **Where the blocks are.** The engine keeps what it needs to go on (its directory and the blocks
/// still open); every block made so far, open or closed, is in a SQLite database of the index's
/// own, in memory ([`Recaps::new`]) or in a file ([`Recaps::in_file`]), so a long history need not
/// be held in memory. Queries read the blocks they answer with from there.
///
/// **Broken.** If storing a batch of blocks fails (a full disk), the database is behind the engine
/// and cannot catch up: the index counts as broken ([`Recaps::is_broken`]), every query answers an
/// internal error, and [`WorkService`] builds a new one from the log.
pub struct Recaps {
    /// The one directory, for both grouping events into blocks and naming things in lines and
    /// paragraphs: `builder.directory()`.
    builder: BlockBuilder,
    /// Changes whenever a name a paragraph may show changes, so cached paragraphs are written
    /// again: a member, task or workstream renamed, an ask re-stated as another kind or by
    /// another asker, a name dropped for the directory's bound, or a name learned for something a
    /// block already named. Driven by [`Directory::names_version`], which moves on past the
    /// bound too, plus the "named while unknown" check below, which it does not cover.
    names_gen: u64,
    /// What blocks name that the directory does not know yet.
    unnamed: Unnamed,
    /// Events the engine panicked on, left out.
    failed: u64,
    /// Every block, indexed by its links; `None` if it could not be opened.
    db: Option<BlockDb>,
    /// Why the blocks are not what the engine made (see "Broken" above).
    broken: Option<String>,
    cache: DayCache,
}

impl std::fmt::Debug for Recaps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recaps")
            .field("blocks", &self.len())
            .field("cached_days", &self.cache.entries.len())
            .field("broken", &self.broken)
            .finish_non_exhaustive()
    }
}

impl Recaps {
    /// Recaps with the engine's default settings (the contract's gap and caps). `seed` is what was
    /// known before the first event pushed: `None` for a log read from its start, or the
    /// projections' lists for a slice of one. The blocks are kept in memory.
    #[must_use]
    pub fn new(seed: Option<Directory>) -> Self {
        Self::with_config(Config::default(), seed)
    }

    /// Recaps with other engine settings.
    #[must_use]
    pub fn with_config(config: Config, seed: Option<Directory>) -> Self {
        let (db, broken) = match BlockDb::memory() {
            Ok(db) => (Some(db), None),
            Err(e) => (
                None,
                Some(format!("cannot open the recap blocks' database: {e}")),
            ),
        };
        Self {
            builder: BlockBuilder::new(config, seed.unwrap_or_default()),
            names_gen: 0,
            unnamed: Unnamed::default(),
            failed: 0,
            db,
            broken,
            cache: DayCache::new(DAY_CACHE_ENTRIES),
        }
    }

    /// Keeps the blocks in a database file at `path` instead of memory: a cache, replaced if
    /// something is there already, and removed when these recaps are dropped. Call it before the
    /// first [`Recaps::push`]. The file is private to the user (0600 on Unix).
    ///
    /// # Errors
    ///
    /// `invalid` if blocks were pushed already; an internal error if the file cannot be made.
    pub fn in_file(mut self, path: &Path) -> Result<Self> {
        self.use_file(path)?;
        Ok(self)
    }

    /// [`Recaps::in_file`] in place; on an error the blocks stay in memory.
    fn use_file(&mut self, path: &Path) -> Result<()> {
        self.use_db(|| BlockDb::file(path))
    }

    /// Keeps the blocks in a cache file meant for `path`, on a local disk ([`BlockDb::local`]: at
    /// `path`, in a private local folder when `path` is on a network filesystem, or in memory);
    /// on an error the blocks stay in memory.
    fn use_local_file(&mut self, path: &Path) -> Result<()> {
        self.use_db(|| BlockDb::local(path))
    }

    /// Moves the (empty) index to the database `open` makes.
    fn use_db(&mut self, open: impl FnOnce() -> Result<BlockDb>) -> Result<()> {
        if !self.is_empty() {
            return Err(WorkError::invalid(
                "the recap index already has blocks in memory",
            ));
        }
        self.db = Some(open()?);
        self.broken = None;
        Ok(())
    }

    /// Keeps at most `entries` day paragraphs (0: none) instead of [`DAY_CACHE_ENTRIES`].
    #[must_use]
    pub fn with_day_cache(mut self, entries: usize) -> Self {
        self.cache = DayCache::new(entries);
        self
    }

    /// Adds events, in log order after those pushed before.
    ///
    /// An event the engine panics on is left out (and logged, and counted in
    /// [`Recaps::failed_events`]) rather than taking the index down; a rebuild leaves it out the
    /// same way. If the blocks cannot be stored, the index is broken ([`Recaps::is_broken`]).
    pub fn push(&mut self, events: &[Event]) {
        for event in events {
            if matches!(event.body, EventBody::CursorMoved { .. }) {
                continue;
            }
            let taken = catch_unwind(AssertUnwindSafe(|| {
                let was_named = self.named_before(event);
                let names_before = self.builder.directory().names_version();
                #[cfg(test)]
                tests::fail_here(event.id);
                self.builder.push(event);
                self.builder.directory().names_version() != names_before
                    || self.newly_named(event, was_named)
            }));
            match taken {
                Ok(changed) => {
                    if changed {
                        self.names_gen = self.names_gen.wrapping_add(1);
                    }
                }
                Err(_) => {
                    self.failed = self.failed.saturating_add(1);
                    tracing::error!(
                        event = %event.id,
                        "the recap engine failed on an event: recaps leave it out"
                    );
                }
            }
        }
        let changes = self.builder.take_changes();
        let mut blocks = changes.closed;
        blocks.extend(changes.open);
        for block in &blocks {
            self.unnamed.note(self.builder.directory(), block);
        }
        if self.broken.is_some() {
            return;
        }
        #[cfg(test)]
        let stored = tests::store_fails().map_or_else(|| self.store(&blocks), Err);
        #[cfg(not(test))]
        let stored = self.store(&blocks);
        if let Err(e) = stored {
            tracing::error!(error = %e, "the recap index cannot store its blocks; it is rebuilt");
            self.broken = Some(e.to_string());
        }
    }

    /// Stores blocks that began or changed.
    fn store(&mut self, blocks: &[Block]) -> Result<()> {
        self.db_mut()?.put(blocks)
    }

    /// Whether the blocks could not be stored, so the index no longer matches the log (see
    /// "Broken" on [`Recaps`]).
    #[must_use]
    pub fn is_broken(&self) -> bool {
        self.broken.is_some()
    }

    /// The blocks' database, unless the index is broken.
    fn db(&self) -> Result<&BlockDb> {
        usable(self.db.as_ref(), self.broken.as_deref())
    }

    fn db_mut(&mut self) -> Result<&mut BlockDb> {
        match (&mut self.db, &self.broken) {
            (Some(db), None) => Ok(db),
            (_, broken) => Err(WorkError::internal(format!(
                "the recap index is broken: {}",
                broken.as_deref().unwrap_or("no database")
            ))),
        }
    }

    /// Whether the member, task or workstream `event` names was already known, before the event
    /// is applied: for events of no other kind, there is nothing to check (`true`, so
    /// [`Recaps::newly_named`] is skipped for them).
    fn named_before(&self, event: &Event) -> bool {
        let names = self.builder.directory();
        match &event.body {
            EventBody::MemberAdded { member } => names.handle(member.id).is_some(),
            EventBody::TaskCreated { task } => names.task_key(task.id).is_some(),
            EventBody::WorkstreamCreated { workstream } => {
                names.workstream_name(workstream.id).is_some()
            }
            _ => true,
        }
    }

    /// Whether `event` just made a member, task or workstream known for the first time, and a
    /// block already named it while it was unknown: if so, every cached paragraph that may have
    /// said "someone", "a task" or "a workstream" for it is written again. A name becoming known
    /// does not by itself move [`Directory::names_version`] (see its docs), so this is tracked
    /// here from what blocks have named (`Recaps::unnamed`).
    fn newly_named(&mut self, event: &Event, was_named: bool) -> bool {
        if was_named {
            return false;
        }
        let names = self.builder.directory();
        match &event.body {
            EventBody::MemberAdded { member } => {
                names.handle(member.id).is_some()
                    && (self.unnamed.overflow || self.unnamed.members.remove(&member.id))
            }
            EventBody::TaskCreated { task } => {
                names.task_key(task.id).is_some()
                    && (self.unnamed.overflow || self.unnamed.tasks.remove(&task.id))
            }
            EventBody::WorkstreamCreated { workstream } => {
                names.workstream_name(workstream.id).is_some()
                    && (self.unnamed.overflow || self.unnamed.workstreams.remove(&workstream.id))
            }
            _ => false,
        }
    }

    /// How many events the engine failed on (see [`Recaps::push`]).
    #[must_use]
    pub fn failed_events(&self) -> u64 {
        self.failed
    }

    /// How many blocks there are, open and closed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.db.as_ref().map_or(0, BlockDb::len)
    }

    /// Whether there are no blocks yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many day paragraphs are cached.
    #[must_use]
    pub fn cached_days(&self) -> usize {
        self.cache.entries.len()
    }

    /// How many day paragraphs have been written so far: each query's cache misses.
    #[must_use]
    pub fn days_written(&self) -> u64 {
        self.cache.made
    }

    /// The names lines and paragraphs use: what the seed and the events pushed so far say.
    #[must_use]
    pub fn names(&self) -> &Directory {
        self.builder.directory()
    }

    /// `GET /v1/recaps/blocks`; see [`RecapIndex::recap_blocks`].
    ///
    /// # Errors
    ///
    /// `invalid` for a `limit` of 0.
    pub fn blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: Option<usize>,
    ) -> Result<BlocksPage> {
        let limit = page_limit(limit, BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT)?;
        let (found, more) = self.db()?.page(filter, before, limit)?;
        let mut blocks = Vec::with_capacity(found.len());
        for block in found {
            let line = guarded("writing a block's line", || {
                block_line(&block, self.builder.directory())
            })?;
            blocks.push(RecapBlock { block, line });
        }
        Ok(BlocksPage {
            blocks,
            at_start: !more,
        })
    }

    /// `GET /v1/recaps/days`; see [`RecapIndex::recap_days`].
    ///
    /// # Errors
    ///
    /// `invalid` for a `tz_minutes` beyond [`MAX_TZ_MINUTES`] either way, a `limit` of 0, or a
    /// `before` that is not `YYYY-MM-DD`; an internal error if the engine's paragraph fails its
    /// own check.
    pub fn days(
        &mut self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<&Date>,
        limit: Option<usize>,
    ) -> Result<DaysPage> {
        if !(-MAX_TZ_MINUTES..=MAX_TZ_MINUTES).contains(&tz_minutes) {
            return Err(WorkError::invalid(format!(
                "tz must be from -{MAX_TZ_MINUTES} to {MAX_TZ_MINUTES} minutes."
            )));
        }
        let limit = page_limit(limit, DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT)?;
        if let Some(date) = before
            && !date.is_well_formed()
        {
            return Err(WorkError::invalid("before must be a date, YYYY-MM-DD."));
        }
        // The fields apart, so the cache can read bodies from the database while it changes.
        let db = usable(self.db.as_ref(), self.broken.as_deref())?;
        let linked = match scope {
            DaysScope::Workstream(w) => Scope::Workstream(recap_db::key(w.0)),
            DaysScope::Project(p) => Scope::Project(recap_db::key(p.0)),
        };
        // Blocks that start before `before` begins at this offset fall on earlier days (except
        // where `date_of` clamps far-off times, which the date check below catches).
        let nil = EventId(ulid::Ulid::nil());
        let mut upper = before
            .and_then(|d| day_start(d, tz_minutes))
            .map(|ms| (ms, nil));
        // A date at a time, newest first: the latest start below `upper` names the date, and
        // every block from that date's first millisecond up to it falls on it (a block's day only
        // grows with its start). Where `date_of` clamps far-off times, a block at a time.
        // Each date with the range of places its blocks span, lowest first, and their heads.
        let mut dates: Vec<(Date, Place, Place, Vec<Head>)> = Vec::new();
        let mut at_start = true;
        while let Some((start, id)) = db.latest(linked, upper)? {
            let date = date_of(start, tz_minutes);
            let lowest = day_start(&date, tz_minutes)
                .filter(|ms| *ms <= start && date_of(*ms, tz_minutes) == date)
                .map_or((start, id), |ms| (ms, nil));
            upper = Some(lowest);
            if before.is_some_and(|b| date >= *b) {
                continue;
            }
            let same = dates.last().is_some_and(|(last, ..)| *last == date);
            if !same && dates.len() >= limit {
                at_start = false;
                break;
            }
            if !same {
                dates.push((date, lowest, (start, id), Vec::new()));
            }
            let window = db.window(linked, lowest, (start, id))?;
            if let Some((_, low, _, heads)) = dates.last_mut() {
                // Windows of one date follow on from each other, downwards.
                *low = lowest;
                heads.extend(window);
            }
        }
        let mut days = Vec::new();
        for (date, low, high, heads) in dates {
            // The entry without a workstream first (`None` sorts first), then by workstream id.
            let mut groups: BTreeMap<Option<WorkstreamId>, Vec<Head>> = BTreeMap::new();
            for head in heads {
                groups.entry(head.workstream).or_default().push(head);
            }
            for (workstream, mut group) in groups {
                group.sort_by_key(|h| (h.start, h.id));
                let key = DayKey {
                    scope,
                    tz: tz_minutes,
                    date: date.clone(),
                    workstream,
                };
                let covers = group.iter().map(|h| (h.id, h.last)).collect();
                days.extend(self.cache.get_or_make(
                    key,
                    covers,
                    self.builder.directory(),
                    self.names_gen,
                    || db.bodies(linked, low, high, workstream),
                )?);
            }
        }
        Ok(DaysPage { days, at_start })
    }
}

/// `db`, unless the index is `broken` (or has no database).
fn usable<'a>(db: Option<&'a BlockDb>, broken: Option<&str>) -> Result<&'a BlockDb> {
    match (db, broken) {
        (Some(db), None) => Ok(db),
        (_, broken) => Err(WorkError::internal(format!(
            "the recap index is broken: {}",
            broken.unwrap_or("no database")
        ))),
    }
}

/// A page's size: the default when absent, at most `max`, and never 0.
fn page_limit(limit: Option<usize>, default: usize, max: usize) -> Result<usize> {
    match limit {
        None => Ok(default),
        Some(0) => Err(WorkError::invalid("limit must be at least 1.")),
        Some(n) => Ok(n.min(max)),
    }
}

/// The first millisecond of `date` at `tz_minutes` east of UTC, or `None` when it is not a date.
fn day_start(date: &Date, tz_minutes: i32) -> Option<TimestampMs> {
    let text = date.0.as_str();
    let part = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = text.get(r)?;
        if s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (part(0..4)?, part(5..7)?, part(8..10)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let offset = i64::from(tz_minutes.clamp(-MAX_TZ_MINUTES, MAX_TZ_MINUTES)) * 60_000;
    Some(
        days_from_civil(y, m, d)
            .saturating_mul(DAY_MS)
            .saturating_sub(offset),
    )
}

/// Days from 1970-01-01 to a proleptic Gregorian date: Howard Hinnant's `days_from_civil`, the
/// inverse of the `civil_from_days` that `date_of` uses. A day past the month's end counts on into
/// the next month.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// What the tasks projection says about revisions `from..=to`, in one snapshot: the revision it
/// has applied up to, and the `task_created` events up to there that it refused because another
/// task held their key. Past that revision it has not decided yet.
fn task_clashes(conn: &Connection, from: u64, to: u64) -> Result<(u64, HashSet<u64>)> {
    let reached: Option<i64> = conn
        .prepare_cached("SELECT rev FROM projection_state WHERE name = ?1")?
        .query_row([Tasks::NAME], |r| r.get(0))
        .optional()?;
    let reached = reached.and_then(|r| u64::try_from(r).ok()).unwrap_or(0);
    let mut stmt =
        conn.prepare_cached("SELECT rev FROM work_task_clashes WHERE rev BETWEEN ?1 AND ?2")?;
    let rows = stmt.query_map(params![sql_rev(from), sql_rev(to.min(reached))], |r| {
        r.get::<_, i64>(0)
    })?;
    let mut refused = HashSet::new();
    for rev in rows {
        if let Ok(rev) = u64::try_from(rev?) {
            refused.insert(rev);
        }
    }
    Ok((reached, refused))
}

/// A service's recap index and the last revision it has read.
#[derive(Debug)]
pub(crate) struct RecapSync {
    recaps: Recaps,
    rev: u64,
    /// Where the blocks go: a file (see [`WorkService::with_recap_file`]), or memory.
    file: Option<PathBuf>,
    /// The file is still to be opened, before the next read of the log.
    to_open: bool,
    /// The directory's bound, for tests ([`WorkService::with_recap_directory_limit`]).
    limit: Option<usize>,
    import_choice: pitcrew_protocol::import::ImportChoice,
}

impl Default for RecapSync {
    fn default() -> Self {
        Self {
            recaps: Recaps::new(None),
            rev: 0,
            file: None,
            to_open: false,
            limit: None,
            import_choice: Default::default(),
        }
    }
}

impl RecapSync {
    /// Starts again from the log: an empty index, with the same file (opened afresh at the next
    /// read) and bound.
    fn reset(&mut self) {
        // The old index, and its file, go now; the file is made again by `open`.
        self.recaps = Recaps::new(self.limit.map(Directory::with_limit));
        self.rev = 0;
        self.to_open = self.file.is_some();
    }

    /// Opens the file, when there is one still to open: on a local disk ([`BlockDb::local`]). If
    /// it cannot be made, the blocks stay in memory (logged): recaps still answer, at the
    /// memory's cost.
    fn open(&mut self) {
        if !std::mem::take(&mut self.to_open) {
            return;
        }
        let Some(path) = self.file.clone() else {
            return;
        };
        if let Err(e) = self.recaps.use_local_file(&path) {
            tracing::warn!(
                file = %path.display(),
                error = %e,
                "the recap index keeps its blocks in memory"
            );
        }
    }

    /// Reads the log after `rev`, a page at a time, into the index. A page is applied whole or
    /// not at all, so an error reading it leaves the index where it was, ready to read the same
    /// page again.
    ///
    /// Only up to the revision the tasks projection has applied: before that, whether a
    /// `task_created` was refused is not known (another process may append without the work
    /// model's projections; this store's next append catches them up). The rest waits for a later
    /// query, so the index never takes in an event a rebuild would leave out.
    fn catch_up(&mut self, work: &WorkService) -> Result<()> {
        loop {
            let page = work.store().since(self.rev, SYNC_PAGE)?;
            let (Some(first), Some(last)) = (page.first(), page.last()) else {
                return Ok(());
            };
            let (first, last) = (first.rev, last.rev);
            let (reached, refused) = work.read(|c| task_clashes(c, first, last))?;
            let upto = last.min(reached);
            if upto < first {
                tracing::debug!(
                    rev = self.rev,
                    tasks = reached,
                    "the tasks projection is behind the log; recaps wait for it"
                );
                return Ok(());
            }
            let more = page.len() >= SYNC_PAGE && upto == last;
            let events: Vec<Event> = page
                .into_iter()
                .take_while(|e| e.rev <= upto)
                .filter(|e| !refused.contains(&e.rev))
                .map(|e| e.event)
                .collect();
            let events = events
                .into_iter()
                .map(|e| {
                    work.event_included_for(&e, &self.import_choice)
                        .map(|visible| visible.then_some(e))
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            self.recaps.push(&events);
            if self.recaps.is_broken() {
                return Err(WorkError::internal(
                    "the recap index could not store its blocks; the next query builds it again",
                ));
            }
            self.rev = upto;
            if !more {
                return Ok(());
            }
        }
    }
}

impl WorkService {
    /// Brings the recap index up to date with the log and returns the last revision it reflects:
    /// the log's newest, or the tasks projection's when that lags (see the
    /// [module docs](crate::recap)). The first call builds it from the whole log.
    ///
    /// Queries do this themselves (see the [module docs](crate::recap)); call it on the blocking
    /// pool at start to build the index before the first request, rather than during it.
    ///
    /// # Errors
    ///
    /// Reading the log or the tasks projection (internal errors). The index keeps every page it
    /// read before the error, and the next call goes on from there. If the blocks cannot be
    /// stored, the next call builds the index again from the log.
    pub fn sync_recaps(&self) -> Result<u64> {
        Ok(self.recap_state()?.rev)
    }

    /// Keeps the recap index's blocks in a file at `path` rather than in memory, so they cost
    /// disk, not memory, however long the log. A cache: whatever is at `path` is replaced when the
    /// index is first built (and whenever it is built again), and the file is removed when the
    /// service is dropped; it is never read from one run to the next. The file is private to the
    /// user (0600 on Unix). If it cannot be made, or the disk later refuses blocks (the index is
    /// then built again), the blocks stay in memory from then on (logged).
    ///
    /// **On a local disk.** When the folder of `path` is on a network filesystem (detected as the
    /// store detects its own; a filesystem not recognised counts as one), the file goes in a
    /// private folder (0700 on Unix) in the temporary folder, or else the runtime directory
    /// (`$XDG_RUNTIME_DIR`), instead, removed with it; when neither can be had, the blocks stay in
    /// memory. Either is logged. On Unix that folder is named after `path` and the user, so the
    /// one a hard kill left behind is used again, and its file replaced, at the next start on
    /// `path`; on Windows its name is random, and a hard kill leaves it behind.
    ///
    /// One file per service: two services (or processes) must not share a path.
    #[must_use]
    pub fn with_recap_file(self, path: impl Into<PathBuf>) -> Self {
        let mut state = self
            .recap_lock()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.file = Some(path.into());
        state.reset();
        drop(state);
        self
    }

    /// Rebuilds the recap index with a directory bounded to `limit` entries of each kind
    /// ([`Directory::with_limit`]), instead of the default 100,000. For tests that exercise the
    /// bound through the store; call it before the first query.
    #[must_use]
    pub fn with_recap_directory_limit(self, limit: usize) -> Self {
        let mut state = self
            .recap_lock()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.limit = Some(limit);
        state.reset();
        drop(state);
        self
    }

    /// The recap index, caught up with the log.
    fn recap_state(&self) -> Result<MutexGuard<'_, RecapSync>> {
        let lock = self.recap_lock();
        let mut state = lock.lock().unwrap_or_else(|poisoned: PoisonError<_>| {
            // A panic part-way through an update may have left the index half-changed: start
            // again from the log.
            let mut state = poisoned.into_inner();
            state.reset();
            lock.clear_poison();
            state
        });
        if state.recaps.is_broken() {
            // The disk refused the blocks (full, or failing): building them there again would
            // likely fail the same way, at the cost of reading the whole log each time.
            if let Some(file) = state.file.take() {
                tracing::warn!(
                    file = %file.display(),
                    "building the recap index again from the log, with its blocks in memory"
                );
            } else {
                tracing::warn!("building the recap index again from the log");
            }
            state.reset();
        }
        let choice = self.import_choice();
        if state.import_choice != choice {
            state.reset();
            state.import_choice = choice;
        }
        state.open();
        state.catch_up(self)?;
        Ok(state)
    }
}

impl RecapIndex for WorkService {
    fn recap_blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: Option<usize>,
    ) -> Result<BlocksPage> {
        self.recap_state()?.recaps.blocks(filter, before, limit)
    }

    fn recap_days(
        &self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<&Date>,
        limit: Option<usize>,
    ) -> Result<DaysPage> {
        self.recap_state()?
            .recaps
            .days(scope, tz_minutes, before, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::ids::WorkspaceId;
    use pitcrew_protocol::model::Receipt;
    use std::cell::Cell;

    thread_local! {
        static FAIL_ON: Cell<Option<EventId>> = const { Cell::new(None) };
        static STORE_FAILS: Cell<bool> = const { Cell::new(false) };
    }

    /// Makes storing blocks fail, on this thread, while set.
    pub(super) fn store_fails() -> Option<WorkError> {
        STORE_FAILS
            .with(Cell::get)
            .then(|| WorkError::internal("storing blocks fails, for the test"))
    }

    /// Makes the engine "panic" on one event, on this thread.
    pub(super) fn fail_here(id: EventId) {
        if FAIL_ON.with(Cell::get) == Some(id) {
            panic!("the recap engine fails on this event, for the test");
        }
    }

    fn tool_run(n: u128, minute: i64) -> Event {
        let session = SessionId(ulid::Ulid::from(7u128));
        Event {
            id: EventId(ulid::Ulid::from(n)),
            at: 1_790_755_200_000 + minute * 60_000,
            workspace: WorkspaceId(ulid::Ulid::from(2u128)),
            author: MemberId(ulid::Ulid::from(4u128)),
            on_behalf_of: None,
            body: EventBody::ToolRan {
                session,
                tool: "Bash".into(),
                target: "cargo test".into(),
                outcome: "ok".into(),
                failed: false,
                receipt: Receipt::Transcript { session, offset: 0 },
            },
        }
    }

    #[test]
    fn an_event_the_engine_fails_on_is_left_out_as_a_rebuild_leaves_it_out() {
        let events: Vec<Event> = (1..=4).map(|n| tool_run(n, n as i64)).collect();
        FAIL_ON.with(|f| f.set(Some(events[1].id)));
        let mut live = Recaps::new(None);
        for e in &events {
            live.push(std::slice::from_ref(e));
        }
        let mut rebuilt = Recaps::new(None);
        rebuilt.push(&events);
        FAIL_ON.with(|f| f.set(None));
        assert_eq!(live.failed_events(), 1);
        assert_eq!(rebuilt.failed_events(), 1);
        let page = live
            .blocks(&BlockFilter::default(), None, None)
            .expect("still answers");
        assert_eq!(page.blocks.len(), 1);
        assert_eq!(page.blocks[0].block.counts.tools_run, 3);
        assert_eq!(
            rebuilt
                .blocks(&BlockFilter::default(), None, None)
                .expect("blocks"),
            page
        );
    }

    /// A batch of blocks that cannot be stored (a full disk) fails the query that read it, and
    /// the next one builds the index again from the log, in memory: what was half-written is
    /// never served.
    #[test]
    fn a_failed_write_is_rebuilt_from_the_log() {
        use pitcrew_protocol::model::Workspace;
        use pitcrew_store::{Store, StoreOptions};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            Store::open_with(
                dir.path().join("hub.db"),
                StoreOptions::default(),
                crate::projections(),
            )
            .expect("store"),
        );
        let workspace = Workspace {
            id: WorkspaceId(ulid::Ulid::from(2u128)),
            name: "W".into(),
        };
        let file = dir.path().join("recaps.sqlite3");
        let work = WorkService::new(Arc::clone(&store), workspace.clone()).with_recap_file(&file);
        assert!(!file.exists(), "made when the index is first built");
        store
            .append(&[tool_run(1, 1), tool_run(2, 2)])
            .expect("append");
        assert_eq!(work.sync_recaps().expect("sync"), 2);
        assert!(file.exists());

        // Two more events, the second one opening a new block, cannot be stored.
        store
            .append(&[tool_run(3, 3), tool_run(4, 60)])
            .expect("append");
        STORE_FAILS.with(|f| f.set(true));
        assert!(work.sync_recaps().is_err());
        assert!(
            work.recap_blocks(&BlockFilter::default(), None, None)
                .is_err(),
            "a broken index answers nothing until it is built again"
        );
        STORE_FAILS.with(|f| f.set(false));

        let page = work
            .recap_blocks(&BlockFilter::default(), None, None)
            .expect("built again");
        assert!(
            !file.exists(),
            "built again in memory, not on the disk that failed"
        );
        let fresh = WorkService::new(Arc::clone(&store), workspace);
        assert_eq!(
            page,
            fresh
                .recap_blocks(&BlockFilter::default(), None, None)
                .expect("fresh")
        );
        assert_eq!(page.blocks.len(), 2);
        assert_eq!(page.blocks[1].block.counts.tools_run, 3);
    }

    /// The file is the service's: made when the index is first built, removed when the service
    /// is dropped.
    #[test]
    fn the_file_lives_with_the_service() {
        use pitcrew_protocol::model::Workspace;
        use pitcrew_store::{Store, StoreOptions};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            Store::open_with(
                dir.path().join("hub.db"),
                StoreOptions::default(),
                crate::projections(),
            )
            .expect("store"),
        );
        let workspace = Workspace {
            id: WorkspaceId(ulid::Ulid::from(2u128)),
            name: "W".into(),
        };
        let file = dir.path().join("recaps.sqlite3");
        let work = WorkService::new(Arc::clone(&store), workspace).with_recap_file(&file);
        assert!(!file.exists(), "made when the index is first built");
        store.append(&[tool_run(1, 1)]).expect("append");
        assert_eq!(work.sync_recaps().expect("sync"), 1);
        assert!(file.exists());
        drop(work);
        assert!(!file.exists(), "removed with the service");
    }

    /// A file left at the path (a crash, another log) is replaced, never read: the index answers
    /// from the log alone. The file is private.
    #[test]
    fn a_leftover_file_is_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("recaps.sqlite3");
        let events: Vec<Event> = (1..=3).map(|n| tool_run(n, n as i64)).collect();
        // What a process that died would leave: another index's blocks.
        let mut old = Recaps::new(None)
            .in_file(&dir.path().join("old.sqlite3"))
            .expect("file");
        old.push(&[tool_run(9, 500)]);
        std::fs::copy(dir.path().join("old.sqlite3"), &file).expect("copy");
        drop(old);
        assert!(file.exists());

        let mut recaps = Recaps::new(None).in_file(&file).expect("file");
        recaps.push(&events);
        let mut memory = Recaps::new(None);
        memory.push(&events);
        let all = |r: &Recaps| {
            r.blocks(&BlockFilter::default(), None, None)
                .expect("blocks")
        };
        assert_eq!(all(&recaps), all(&memory));
        assert_eq!(recaps.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Garbage there is replaced too.
        drop(recaps);
        std::fs::write(&file, b"not a database").expect("write");
        let mut recaps = Recaps::new(None).in_file(&file).expect("file");
        recaps.push(&events);
        assert_eq!(all(&recaps), all(&memory));

        // Only an empty index moves to a file.
        assert!(memory.in_file(&dir.path().join("other")).is_err());
    }

    /// A project with two workstreams, a session in each, and a task outside them; then four
    /// days of work in both sessions, and a move of the task each day: `(project, workstreams,
    /// sessions, events)`.
    fn linked_log() -> (ProjectId, [WorkstreamId; 2], [SessionId; 2], Vec<Event>) {
        use pitcrew_protocol::ids::{MachineId, ProjectKey, TaskKey};
        use pitcrew_protocol::model::{
            Engine, Health, Mover, Priority, Project, ProjectStatus, Session, SessionState, Task,
            TaskStatus, Workstream, WorkstreamStatus,
        };
        let id = |kind: u128, n: u128| ulid::Ulid::from((kind << 96) | n);
        let person = MemberId(id(4, 0));
        let project = ProjectId(id(3, 0));
        let streams = [WorkstreamId(id(5, 0)), WorkstreamId(id(5, 1))];
        let sessions = [SessionId(id(7, 0)), SessionId(id(7, 1))];
        let task = TaskId(id(6, 0));
        let mut n = 0u128;
        let mut event = |at: i64, body: EventBody| {
            n += 1;
            Event {
                id: EventId(id(1, n)),
                at,
                workspace: WorkspaceId(ulid::Ulid::from(2u128)),
                author: person,
                on_behalf_of: None,
                body,
            }
        };
        let t0 = 1_790_755_200_000; // 2026-09-30 08:00 UTC
        let mut events = vec![event(
            t0 - DAY_MS,
            EventBody::ProjectCreated {
                project: Project {
                    id: project,
                    key: ProjectKey::new("PAP").expect("key"),
                    name: "Paper".into(),
                    status: ProjectStatus::InProgress,
                    lead: person,
                    members: vec![person],
                    start: None,
                    due: None,
                    root: None,
                    external: vec![],
                },
            },
        )];
        for (i, w) in streams.iter().enumerate() {
            let workstream = Workstream {
                id: *w,
                project,
                name: format!("Stream {i}"),
                status: WorkstreamStatus::Active,
                health: Health::OnTrack,
                locations: vec![],
                external: vec![],
            };
            events.push(event(
                t0 - DAY_MS,
                EventBody::WorkstreamCreated { workstream },
            ));
        }
        let task_body = Task {
            id: task,
            key: TaskKey::new(ProjectKey::new("PAP").expect("key"), 1).expect("key"),
            title: "Outside the streams".into(),
            project,
            workstream: None,
            description: String::new(),
            status: TaskStatus::Todo,
            priority: Priority::None,
            assignee: None,
            labels: vec![],
            start: None,
            due: None,
            blocked_by: vec![],
            source: None,
            archived: false,
            accept_auto: false,
            subtasks: vec![],
        };
        events.push(event(
            t0 - DAY_MS,
            EventBody::TaskCreated { task: task_body },
        ));
        for (s, w) in sessions.iter().zip(streams) {
            let session = Session {
                id: *s,
                engine: Engine::Claude,
                native_id: "native".into(),
                machine: MachineId(ulid::Ulid::from(1u128)),
                cwd: "/work".into(),
                branch: None,
                title: None,
                agent: None,
                workstream: Some(w),
                task: None,
                link_basis: None,
                state: SessionState::Working,
                status_line: None,
                started: t0,
                last_activity: t0,
                terminal: None,
                parent: None,
            };
            events.push(event(t0 - DAY_MS, EventBody::SessionDiscovered { session }));
        }
        let statuses = [TaskStatus::Todo, TaskStatus::InProgress];
        for day in 0..4i64 {
            for (k, s) in sessions.iter().enumerate() {
                for minute in 0..3i64 {
                    let at = t0 + day * DAY_MS + (k as i64 * 10 + minute) * 60_000;
                    events.push(event(
                        at,
                        EventBody::ToolRan {
                            session: *s,
                            tool: "Bash".into(),
                            target: "cargo test".into(),
                            outcome: "ok".into(),
                            failed: false,
                            receipt: Receipt::Transcript {
                                session: *s,
                                offset: minute as u64,
                            },
                        },
                    ));
                }
            }
            let i = usize::try_from(day).unwrap_or(0) % 2;
            events.push(event(
                t0 + day * DAY_MS + 3_600_000,
                EventBody::TaskMoved {
                    task,
                    from: statuses[i],
                    to: statuses[1 - i],
                    mover: Mover::Person,
                },
            ));
        }
        (project, streams, sessions, events)
    }

    /// Day queries answered from the cache read no block's body, and answer what paragraphs
    /// written afresh say; new work on a day has only that day's paragraph written again, from
    /// its own blocks' bodies.
    #[test]
    fn days_from_the_cache_read_no_body_and_answer_as_written_afresh() {
        use crate::recap_db::tests::decoded;
        let (project, streams, sessions, events) = linked_log();
        let mut cached = Recaps::new(None);
        let mut fresh = Recaps::new(None).with_day_cache(0);
        cached.push(&events);
        fresh.push(&events);
        let scopes = [
            DaysScope::Project(project),
            DaysScope::Workstream(streams[0]),
            DaysScope::Workstream(streams[1]),
        ];
        let queries: Vec<(DaysScope, i32, Option<Date>, Option<usize>)> = scopes
            .iter()
            .flat_map(|scope| {
                [
                    (*scope, 0, None, None),
                    (*scope, 120, None, Some(2)),
                    (*scope, -300, Some(Date("2026-10-02".into())), Some(30)),
                ]
            })
            .collect();
        let ask = |r: &mut Recaps, (scope, tz, before, limit): &(DaysScope, i32, Option<Date>, Option<usize>)| {
            r.days(*scope, *tz, before.as_ref(), *limit).expect("days")
        };

        // Written once: the paragraphs read their blocks' bodies.
        let first: Vec<DaysPage> = queries.iter().map(|q| ask(&mut cached, q)).collect();
        for (q, page) in queries.iter().zip(&first) {
            assert_eq!(page, &ask(&mut fresh, q), "{q:?}");
        }
        // The day things were created, and four days of work.
        let whole = &first[0];
        assert_eq!(
            whole
                .days
                .iter()
                .map(|d| &d.date)
                .collect::<HashSet<_>>()
                .len(),
            5
        );
        for workstream in [None, Some(streams[0]), Some(streams[1])] {
            assert!(
                whole.days.iter().any(|d| d.workstream == workstream),
                "an entry for {workstream:?}"
            );
        }
        assert!(cached.days_written() > 0);

        // Again: every paragraph from the cache, and not one body read.
        let written = cached.days_written();
        for (q, page) in queries.iter().zip(&first) {
            let before = decoded();
            assert_eq!(&ask(&mut cached, q), page, "{q:?}");
            assert_eq!(decoded(), before, "{q:?} decoded a body");
        }
        assert_eq!(cached.days_written(), written);

        // More work in the first session on the last day: that paragraph alone is written again,
        // from the bodies of the blocks it covers and no others.
        let mut more = tool_run(1_000, 0);
        more.at = 1_790_755_200_000 + 3 * DAY_MS + 5 * 60_000;
        if let EventBody::ToolRan {
            session, receipt, ..
        } = &mut more.body
        {
            *session = sessions[0];
            *receipt = Receipt::Transcript {
                session: sessions[0],
                offset: 9,
            };
        }
        cached.push(std::slice::from_ref(&more));
        fresh.push(std::slice::from_ref(&more));
        let stream = (DaysScope::Workstream(streams[0]), 0, None, None);
        let want = ask(&mut fresh, &stream);
        let before = decoded();
        let got = ask(&mut cached, &stream);
        assert_eq!(got, want);
        assert_ne!(got, first[3], "the latest paragraph changed");
        assert_eq!(cached.days_written(), written + 1);
        assert_eq!(decoded() - before, got.days[0].blocks.len());
        // And the rest still from the cache.
        for q in &queries {
            let want = ask(&mut fresh, q);
            assert_eq!(ask(&mut cached, q), want, "{q:?}");
        }
    }

    #[test]
    fn day_start_is_the_inverse_of_date_of() {
        // Every 7 hours and a bit for about 30 years, at several offsets.
        let mut at = 2 * DAY_MS;
        while at < 30 * 365 * DAY_MS {
            for tz in [-MAX_TZ_MINUTES, -300, -1, 0, 1, 120, 330, MAX_TZ_MINUTES] {
                let date = date_of(at, tz);
                let start = day_start(&date, tz).expect("a date");
                assert!(start <= at && at < start + DAY_MS, "{at} {tz}");
                assert_eq!(date_of(start, tz), date, "{at} {tz}");
                assert!(date_of(start - 1, tz) < date, "{at} {tz}");
            }
            at += 25_237_123;
        }
    }

    #[test]
    fn day_start_takes_only_dates() {
        for text in [
            "",
            "2026-9-30",
            "2026-13-01",
            "2026-00-10",
            "2026-09-32",
            "abcd-ef-gh",
        ] {
            assert_eq!(day_start(&Date(text.into()), 0), None, "{text}");
        }
        assert_eq!(day_start(&Date("1970-01-01".into()), 0), Some(0));
        assert_eq!(
            day_start(&Date("1970-01-02".into()), 60),
            Some(DAY_MS - 3_600_000)
        );
        assert_eq!(
            day_start(&Date("2026-09-30".into()), 0),
            Some(1_790_726_400_000)
        );
        // Past the month's end counts on: the 31st of September is the 1st of October.
        assert_eq!(
            day_start(&Date("2026-09-31".into()), 0),
            day_start(&Date("2026-10-01".into()), 0)
        );
        assert!(day_start(&Date("0000-01-01".into()), MAX_TZ_MINUTES).is_some());
        assert!(day_start(&Date("9999-12-31".into()), -MAX_TZ_MINUTES).is_some());
    }

    #[test]
    fn limits() {
        assert_eq!(page_limit(None, 7, 30).ok(), Some(7));
        assert_eq!(page_limit(Some(1), 7, 30).ok(), Some(1));
        assert_eq!(page_limit(Some(31), 7, 30).ok(), Some(30));
        assert_eq!(page_limit(Some(usize::MAX), 7, 30).ok(), Some(30));
        assert!(page_limit(Some(0), 7, 30).is_err());
    }
}
