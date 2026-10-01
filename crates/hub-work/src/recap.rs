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
//! What the engine knows about the workspace (its [`Directory`]: sessions, tasks, workstreams,
//! dispatches, asks) comes from the same events, read from the start: the log holds everything the
//! projections were built from, so the directory is the projections' state as of each event, the
//! links "as they were when the events happened" that the contract asks for. Seeding it with the
//! projections as they are now would put today's links in front of yesterday's events, and an
//! index built yesterday and kept current would then differ from one rebuilt today. Members, which
//! the engine's directory does not follow, are learned from `member_added` for the names in lines
//! and paragraphs, so they are always the current names.
//!
//! A `task_created` that the tasks projection refused (its key was taken, see "One writer") is not
//! activity here either: it is left out, so a task the hub never had never shows in a recap.
//!
//! # Day paragraphs are cached
//!
//! By scope, `tz`, date and workstream, with the blocks each paragraph covers: their ids and last
//! events. A query recomputes an entry only when its blocks changed (a block grew, began, or moved
//! to another day or workstream), or when a name it could show changed. At most a set number of
//! entries are kept ([`DAY_CACHE_ENTRIES`]); past that, the one used longest ago goes.
//!
//! [`Store::since`]: pitcrew_store::Store::since

use crate::codec::sql_rev;
use crate::error::{Result, WorkError};
use crate::service::WorkService;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{Date, TimestampMs};
use pitcrew_protocol::recap::{
    BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT, Block, BlocksPage, DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT,
    DayRecap, DaysPage, MAX_TZ_MINUTES, RecapBlock,
};
use pitcrew_recap::{
    BlockBuilder, Config, Directory, RuleSummarizer, block_line, date_of, day_recaps,
};
use pitcrew_store::sql::{Connection, params};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;
use std::ops::Bound;
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

/// The blocks linked to one workstream or project: by id, for filters, and by start, for days.
#[derive(Debug, Default)]
struct Linked {
    ids: BTreeSet<EventId>,
    starts: BTreeSet<(TimestampMs, EventId)>,
}

impl Linked {
    fn add(&mut self, id: EventId, start: TimestampMs) {
        self.ids.insert(id);
        self.starts.insert((start, id));
    }

    fn remove(&mut self, id: EventId, start: TimestampMs) {
        self.ids.remove(&id);
        self.starts.remove(&(start, id));
    }

    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// What a block is indexed by.
#[derive(Debug, PartialEq, Eq)]
struct Links {
    start: TimestampMs,
    session: Option<SessionId>,
    tasks: Vec<TaskId>,
    workstream: Option<WorkstreamId>,
    project: Option<ProjectId>,
}

impl Links {
    fn of(block: &Block) -> Self {
        Self {
            start: block.start,
            session: block.session,
            tasks: block.tasks.clone(),
            workstream: block.workstream,
            project: block.project,
        }
    }
}

fn add_id<K: Hash + Eq>(map: &mut HashMap<K, BTreeSet<EventId>>, key: K, id: EventId) {
    map.entry(key).or_default().insert(id);
}

fn remove_id<K: Hash + Eq>(map: &mut HashMap<K, BTreeSet<EventId>>, key: K, id: EventId) {
    if let Some(set) = map.get_mut(&key) {
        set.remove(&id);
        if set.is_empty() {
            map.remove(&key);
        }
    }
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

    /// The paragraph for `blocks` (one workstream's blocks on one date, in order), from the cache
    /// when it was written from the same blocks and names.
    fn get_or_make(
        &mut self,
        key: DayKey,
        blocks: &[&Block],
        names: &Directory,
        names_gen: u64,
    ) -> Result<Vec<DayRecap>> {
        self.clock = self.clock.wrapping_add(1);
        let covers: Vec<(EventId, EventId)> = blocks.iter().map(|b| (b.id, b.last)).collect();
        if let Some(hit) = self.entries.get_mut(&key)
            && hit.covers == covers
            && hit.names == names_gen
        {
            hit.used = self.clock;
            return Ok(hit.recaps.clone());
        }
        let owned: Vec<Block> = blocks.iter().map(|b| (*b).clone()).collect();
        let recaps = day_recaps(&owned, names, key.tz, &RuleSummarizer)
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
/// Pure and in memory: it reads no store and no clock. [`WorkService`] keeps one current from its
/// store; build one directly to recap a slice of a log, e.g. the demo fixture's.
pub struct Recaps {
    builder: BlockBuilder,
    /// Names for lines and paragraphs: the directory as events keep it, plus members.
    names: Directory,
    /// Changes whenever a name may have changed, so cached paragraphs are written again.
    names_gen: u64,
    blocks: BTreeMap<EventId, Block>,
    sessions: HashMap<SessionId, BTreeSet<EventId>>,
    tasks: HashMap<TaskId, BTreeSet<EventId>>,
    workstreams: HashMap<WorkstreamId, Linked>,
    projects: HashMap<ProjectId, Linked>,
    cache: DayCache,
}

impl std::fmt::Debug for Recaps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recaps")
            .field("blocks", &self.blocks.len())
            .field("cached_days", &self.cache.entries.len())
            .finish_non_exhaustive()
    }
}

impl Recaps {
    /// Recaps with the engine's default settings (the contract's gap and caps). `seed` is what was
    /// known before the first event pushed: empty for a log read from its start, or the
    /// projections' lists for a slice of one.
    #[must_use]
    pub fn new(seed: Directory) -> Self {
        Self::with_config(Config::default(), seed)
    }

    /// Recaps with other engine settings.
    #[must_use]
    pub fn with_config(config: Config, seed: Directory) -> Self {
        Self {
            builder: BlockBuilder::new(config, seed.clone()),
            names: seed,
            names_gen: 0,
            blocks: BTreeMap::new(),
            sessions: HashMap::new(),
            tasks: HashMap::new(),
            workstreams: HashMap::new(),
            projects: HashMap::new(),
            cache: DayCache::new(DAY_CACHE_ENTRIES),
        }
    }

    /// Keeps at most `entries` day paragraphs (0: none) instead of [`DAY_CACHE_ENTRIES`].
    #[must_use]
    pub fn with_day_cache(mut self, entries: usize) -> Self {
        self.cache = DayCache::new(entries);
        self
    }

    /// Adds events, in log order after those pushed before.
    pub fn push(&mut self, events: &[Event]) {
        for event in events {
            self.learn_names(event);
            self.builder.push(event);
        }
        let changes = self.builder.take_changes();
        for block in changes.closed.into_iter().chain(changes.open) {
            self.put(block);
        }
    }

    /// How many blocks there are, open and closed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether there are no blocks yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
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
        &self.names
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
        let empty = || BlocksPage {
            blocks: Vec::new(),
            at_start: true,
        };
        // Walk the smallest set of blocks one of the filters names; with none, every block.
        let named = [
            filter.session.map(|s| self.sessions.get(&s)),
            filter.task.map(|t| self.tasks.get(&t)),
            filter
                .workstream
                .map(|w| self.workstreams.get(&w).map(|l| &l.ids)),
            filter
                .project
                .map(|p| self.projects.get(&p).map(|l| &l.ids)),
        ];
        let mut smallest: Option<&BTreeSet<EventId>> = None;
        for set in named.into_iter().flatten() {
            let Some(set) = set else {
                return Ok(empty());
            };
            if smallest.is_none_or(|s| set.len() < s.len()) {
                smallest = Some(set);
            }
        }
        let below = (
            Bound::Unbounded,
            before.map_or(Bound::Unbounded, Bound::Excluded),
        );
        let ids: Box<dyn Iterator<Item = &EventId> + '_> = match smallest {
            Some(set) => Box::new(set.range(below).rev()),
            None => Box::new(self.blocks.range(below).map(|(id, _)| id).rev()),
        };
        let mut blocks = Vec::new();
        for id in ids {
            let Some(block) = self.blocks.get(id) else {
                continue;
            };
            if !filter.matches(block) {
                continue;
            }
            if blocks.len() == limit {
                return Ok(BlocksPage {
                    blocks,
                    at_start: false,
                });
            }
            blocks.push(RecapBlock {
                block: block.clone(),
                line: block_line(block, &self.names),
            });
        }
        Ok(BlocksPage {
            blocks,
            at_start: true,
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
        let linked = match scope {
            DaysScope::Workstream(w) => self.workstreams.get(&w),
            DaysScope::Project(p) => self.projects.get(&p),
        };
        let Some(linked) = linked else {
            return Ok(DaysPage {
                days: Vec::new(),
                at_start: true,
            });
        };
        // Blocks that start before `before` begins at this offset fall on earlier days (except
        // where `date_of` clamps far-off times, which the date check below catches).
        let nil = EventId(ulid::Ulid::nil());
        let mut upper = before
            .and_then(|d| day_start(d, tz_minutes))
            .map_or(Bound::Unbounded, |ms| Bound::Excluded((ms, nil)));
        // A date at a time, newest first: the latest start below `upper` names the date, and
        // every block from that date's first millisecond up to it falls on it (a block's day only
        // grows with its start). Where `date_of` clamps far-off times, a block at a time.
        let mut dates: Vec<(Date, Vec<&Block>)> = Vec::new();
        let mut at_start = true;
        while let Some(&(start, id)) = linked.starts.range((Bound::Unbounded, upper)).next_back() {
            let date = date_of(start, tz_minutes);
            let lowest = day_start(&date, tz_minutes)
                .filter(|ms| *ms <= start && date_of(*ms, tz_minutes) == date)
                .map_or((start, id), |ms| (ms, nil));
            let window = linked
                .starts
                .range((Bound::Included(lowest), Bound::Included((start, id))));
            upper = Bound::Excluded(lowest);
            if before.is_some_and(|b| date >= *b) {
                continue;
            }
            let same = dates.last().is_some_and(|(last, _)| *last == date);
            if !same && dates.len() >= limit {
                at_start = false;
                break;
            }
            if !same {
                dates.push((date, Vec::new()));
            }
            if let Some((_, blocks)) = dates.last_mut() {
                blocks.extend(window.rev().filter_map(|(_, id)| self.blocks.get(id)));
            }
        }
        let mut days = Vec::new();
        for (date, blocks) in dates {
            // The entry without a workstream first (`None` sorts first), then by workstream id.
            let mut groups: BTreeMap<Option<WorkstreamId>, Vec<&Block>> = BTreeMap::new();
            for block in blocks {
                groups.entry(block.workstream).or_default().push(block);
            }
            for (workstream, mut group) in groups {
                group.sort_by_key(|b| (b.start, b.id));
                let key = DayKey {
                    scope,
                    tz: tz_minutes,
                    date: date.clone(),
                    workstream,
                };
                days.extend(
                    self.cache
                        .get_or_make(key, &group, &self.names, self.names_gen)?,
                );
            }
        }
        Ok(DaysPage { days, at_start })
    }

    /// Keeps the names current. Paragraphs written before a name changed are written again.
    fn learn_names(&mut self, event: &Event) {
        let changed = match &event.body {
            EventBody::MemberAdded { member } => {
                let before = self.names.handle(member.id).map(str::to_owned);
                self.names.add_member(member);
                before.as_deref() != self.names.handle(member.id)
            }
            EventBody::TaskCreated { task } => {
                let before = self.names.task_key(task.id).map(str::to_owned);
                self.names.observe(event);
                before.as_deref() != self.names.task_key(task.id)
            }
            EventBody::WorkstreamCreated { workstream } => {
                let before = self.names.workstream_name(workstream.id).map(str::to_owned);
                self.names.observe(event);
                before.as_deref() != self.names.workstream_name(workstream.id)
            }
            // An answer is described by its ask's kind and asker, which the directory does not
            // give back to compare: any ask counts as a change.
            EventBody::AskRaised { .. } => {
                self.names.observe(event);
                true
            }
            _ => {
                self.names.observe(event);
                false
            }
        };
        if changed {
            self.names_gen = self.names_gen.wrapping_add(1);
        }
    }

    /// Stores a block that began or changed, and re-indexes it if its links or start moved.
    fn put(&mut self, block: Block) {
        let links = Links::of(&block);
        match self.blocks.get(&block.id).map(Links::of) {
            Some(old) if old == links => {}
            Some(old) => {
                self.unlink(block.id, old);
                self.link(block.id, &links);
            }
            None => self.link(block.id, &links),
        }
        self.blocks.insert(block.id, block);
    }

    fn link(&mut self, id: EventId, links: &Links) {
        if let Some(s) = links.session {
            add_id(&mut self.sessions, s, id);
        }
        for t in &links.tasks {
            add_id(&mut self.tasks, *t, id);
        }
        if let Some(w) = links.workstream {
            self.workstreams.entry(w).or_default().add(id, links.start);
        }
        if let Some(p) = links.project {
            self.projects.entry(p).or_default().add(id, links.start);
        }
    }

    fn unlink(&mut self, id: EventId, links: Links) {
        if let Some(s) = links.session {
            remove_id(&mut self.sessions, s, id);
        }
        for t in links.tasks {
            remove_id(&mut self.tasks, t, id);
        }
        if let Some(w) = links.workstream
            && let Some(linked) = self.workstreams.get_mut(&w)
        {
            linked.remove(id, links.start);
            if linked.is_empty() {
                self.workstreams.remove(&w);
            }
        }
        if let Some(p) = links.project
            && let Some(linked) = self.projects.get_mut(&p)
        {
            linked.remove(id, links.start);
            if linked.is_empty() {
                self.projects.remove(&p);
            }
        }
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

/// The `task_created` events between `from` and `to` (inclusive) that the tasks projection
/// refused because another task held their key.
fn refused_task_creations(conn: &Connection, from: u64, to: u64) -> Result<HashSet<u64>> {
    let mut stmt =
        conn.prepare_cached("SELECT rev FROM work_task_clashes WHERE rev BETWEEN ?1 AND ?2")?;
    let rows = stmt.query_map(params![sql_rev(from), sql_rev(to)], |r| r.get::<_, i64>(0))?;
    let mut out = HashSet::new();
    for rev in rows {
        if let Ok(rev) = u64::try_from(rev?) {
            out.insert(rev);
        }
    }
    Ok(out)
}

/// A service's recap index and the last revision it has read.
#[derive(Debug)]
pub(crate) struct RecapSync {
    recaps: Recaps,
    rev: u64,
}

impl Default for RecapSync {
    fn default() -> Self {
        Self {
            recaps: Recaps::new(Directory::new()),
            rev: 0,
        }
    }
}

impl RecapSync {
    /// Reads the log after `rev`, a page at a time, into the index. A page is applied whole or
    /// not at all, so an error leaves the index where it was, ready to read the same page again.
    fn catch_up(&mut self, work: &WorkService) -> Result<()> {
        loop {
            let page = work.store().since(self.rev, SYNC_PAGE)?;
            let (Some(first), Some(last)) = (page.first(), page.last()) else {
                return Ok(());
            };
            let (first, last) = (first.rev, last.rev);
            let refused = work.read(|c| refused_task_creations(c, first, last))?;
            let full = page.len() >= SYNC_PAGE;
            let events: Vec<Event> = page
                .into_iter()
                .filter(|e| !refused.contains(&e.rev))
                .map(|e| e.event)
                .collect();
            self.recaps.push(&events);
            self.rev = last;
            if !full {
                return Ok(());
            }
        }
    }
}

impl WorkService {
    /// Brings the recap index up to date with the log and returns the last revision it reflects.
    /// The first call builds it from the whole log.
    ///
    /// Queries do this themselves (see the [module docs](crate::recap)); call it on the blocking
    /// pool at start to build the index before the first request, rather than during it.
    ///
    /// # Errors
    ///
    /// Reading the log or the tasks projection (internal errors). The index keeps every page it
    /// read before the error, and the next call goes on from there.
    pub fn sync_recaps(&self) -> Result<u64> {
        Ok(self.recap_state()?.rev)
    }

    /// The recap index, caught up with the log.
    fn recap_state(&self) -> Result<MutexGuard<'_, RecapSync>> {
        let lock = self.recap_lock();
        let mut state = lock.lock().unwrap_or_else(|poisoned: PoisonError<_>| {
            // A panic part-way through an update may have left the index half-changed: start
            // again from the log.
            let mut state = poisoned.into_inner();
            *state = RecapSync::default();
            lock.clear_poison();
            state
        });
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
