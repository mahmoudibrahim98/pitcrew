//! `GET /v1/events?before=&limit=&project=&workstream=&task=&session=`: activity, paged back
//! through the event log.
//!
//! - Events come oldest first within a page; without `before`, the newest page. `before` is an
//!   exclusive revision. `limit` defaults to 100 and is capped at 500.
//! - The page is `pitcrew_protocol::api::EventsPage`. `from_rev` and `to_rev` are the revisions
//!   of the first and last events (both 0 for an empty page at the start). Pass `from_rev` as
//!   `before` for the previous page. **Only `at_start` ends paging**: it is true when nothing
//!   older matches.
//! - **Filters combine:** an event must match every filter given.
//!   - `session` and `task` match events whose body has, at any depth, a `session` (or `task`)
//!     field holding that id, either as the id itself or as an object with that `id`: the session
//!     a turn ran in, a discovered session, the task a comment is on, an ask's task, a receipt's
//!     session, and so on. The id elsewhere (a session's `parent`, a task's `blocked_by`, free
//!     text) does not match.
//!   - With the hub's activity index ([`EventRefs`], given with [`Activity::with_refs`]), they
//!     also match the events the index says are about the session or task, following the links
//!     as they were when each event happened. `task` then also catches the turns, tool runs and
//!     file edits of sessions linked to the task, and `dispatch_finished` and `ask_answered` of
//!     its dispatches and asks; `session` also catches `dispatch_finished` and `ask_answered` of
//!     its dispatches and asks. **Without the index those events are missed.**
//!   - `project` and `workstream` match only through the index: events about the project or
//!     workstream, its tasks', and their sessions'. Without an index they answer `400 invalid`
//!     with [`NEEDS_INDEX`].
//! - **Bounded work per request.** `project` and `workstream` on their own are answered by the
//!   index, which bounds its own search ([`EventRefs::revs_matching`]). Any filter with `session`
//!   or `task` scans back at most [`SCAN_BUDGET`] events of the log. Either way a page may hold
//!   fewer than `limit` events (even none) with `at_start` false; an empty one has `to_rev` 0 and
//!   `from_rev` where the search stopped, so the next request continues from there.

use crate::source::{EventSource, SourceError, StoredEvent};
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use pitcrew_auth::ErrorResponse;
use pitcrew_protocol::api::{ErrorCode, EventsPage};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::{ProjectId, SessionId, TaskId, WorkstreamId};
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// Events per page when `limit` is absent.
pub const DEFAULT_LIMIT: usize = 100;
/// The largest `limit`.
pub const MAX_LIMIT: usize = 500;
/// The most events one request filtered by `session` or `task` scans.
pub const SCAN_BUDGET: usize = 10_000;
/// The message for `project` and `workstream` filters on a hub without an activity index.
pub const NEEDS_INDEX: &str = "Filtering activity by project or workstream needs the hub's \
    activity index, which this hub does not have. Filter by task or session, or page unfiltered.";

const SCAN_PAGE: usize = 500;

/// How deep a filter looks into an event body. Today's bodies nest a few levels; the cap keeps a
/// future body carrying raw nested JSON from recursing deeply.
pub const MAX_DEPTH: usize = 32;

/// Which events to find: those about **all** of the given project, workstream, task and session.
///
/// It is both what a request filters by and what the route asks the index ([`EventRefs`]). It
/// mirrors `pitcrew_hub_work::RefFilter` field for field, so the daemon's adapter copies it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefFilter {
    /// Events about this project.
    pub project: Option<ProjectId>,
    /// Events about this workstream.
    pub workstream: Option<WorkstreamId>,
    /// Events about this task.
    pub task: Option<TaskId>,
    /// Events about this session.
    pub session: Option<SessionId>,
}

impl RefFilter {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether it has a field only the index can match.
    fn needs_index(&self) -> bool {
        self.project.is_some() || self.workstream.is_some()
    }

    /// Whether it has a field that also matches ids in event bodies.
    fn keyed(&self) -> bool {
        self.task.is_some() || self.session.is_some()
    }
}

/// The hub's activity index: which events are about a project, workstream, task or session.
///
/// "About" follows the links in force when each event happened: a turn in a session linked to a
/// task is about that task, its workstream and its project; `dispatch_finished` is about its
/// dispatch's task and session. The work model has the index (`pitcrew_hub_work::EventRefs`).
/// This crate does not depend on the work model, so the daemon adapts it to this trait, copying
/// [`RefFilter`] across.
pub trait EventRefs: Send + Sync + fmt::Debug + 'static {
    /// The newest revisions below `before_rev` (exclusive) of events about **all** of `filter`'s
    /// fields: at most `limit`, oldest first, with `scanned_to`, where the search stopped.
    ///
    /// - `scanned_to` is **0 only when the search reached the start of the log**, so nothing
    ///   older matches. A non-zero one says where the search stopped, not that anything older
    ///   matches; it is below `before_rev`.
    /// - When more than `limit` events match, `scanned_to` is the oldest revision returned.
    /// - A call may examine a bounded number of index entries. When that runs out first, fewer
    ///   than `limit` revisions come back (maybe none) and `scanned_to` is where it stopped.
    ///
    /// Blocking: the route calls it on the blocking pool. It never passes an empty filter or a
    /// `limit` of 0.
    ///
    /// # Errors
    /// The index cannot be read.
    fn revs_matching(
        &self,
        filter: &RefFilter,
        before_rev: u64,
        limit: usize,
    ) -> Result<(Vec<u64>, u64), SourceError>;
}

/// The activity route over an event log and, optionally, the activity index.
#[derive(Clone, Debug)]
pub struct Activity {
    source: Arc<dyn EventSource>,
    refs: Option<Arc<dyn EventRefs>>,
}

/// The activity route without an index (`project` and `workstream` answer 400). Mount it as a
/// **device** route (`RouterParts::device`). Same as `Activity::new(source).routes()`.
pub fn routes(source: Arc<dyn EventSource>) -> Router {
    Activity::new(source).routes()
}

impl Activity {
    /// Activity from `source`, without an index.
    #[must_use]
    pub fn new(source: Arc<dyn EventSource>) -> Self {
        Self { source, refs: None }
    }

    /// Answers `project` and `workstream` through `refs`, and widens `task` and `session` with
    /// it. `refs` must index the same log as the source.
    #[must_use]
    pub fn with_refs(mut self, refs: Arc<dyn EventRefs>) -> Self {
        self.refs = Some(refs);
        self
    }

    /// The route. Mount it as a **device** route (`RouterParts::device`).
    pub fn routes(self) -> Router {
        Router::new()
            .route("/v1/events", get(events))
            .with_state(self)
    }

    /// The newest `limit` (at least 1) events below `before` that match `filter`, as the route
    /// answers them. Blocking.
    ///
    /// # Errors
    /// The log or the index cannot be read, or the index answered outside its contract (or a
    /// `project` or `workstream` filter without an index).
    pub fn page(
        &self,
        before: u64,
        limit: usize,
        filter: &RefFilter,
    ) -> Result<EventsPage, SourceError> {
        let limit = limit.max(1);
        let source = &*self.source;
        if filter.is_empty() {
            return unfiltered(source, before, limit);
        }
        match &self.refs {
            None if filter.needs_index() => Err(NEEDS_INDEX.into()),
            Some(refs) if !filter.keyed() => indexed(source, &**refs, before, limit, filter),
            refs => scan(source, refs.as_deref(), before, limit, filter),
        }
    }
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    before: Option<u64>,
    limit: Option<usize>,
    project: Option<String>,
    workstream: Option<String>,
    task: Option<String>,
    session: Option<String>,
}

async fn events(
    State(activity): State<Activity>,
    query: Result<Query<EventsQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<EventsPage>, ErrorResponse> {
    let invalid = |m: String| ErrorResponse::new(ErrorCode::Invalid, m);
    let Query(query) = query.map_err(|e| invalid(format!("Bad query: {}", e.body_text())))?;
    if activity.refs.is_none() && (query.project.is_some() || query.workstream.is_some()) {
        return Err(invalid(NEEDS_INDEX.to_owned()));
    }
    let limit = match query.limit {
        None => DEFAULT_LIMIT,
        Some(0) => return Err(invalid("`limit` must be at least 1.".to_owned())),
        Some(n) => n.min(MAX_LIMIT),
    };
    let filter = RefFilter {
        project: id(query.project.as_deref(), "project")?,
        workstream: id(query.workstream.as_deref(), "workstream")?,
        task: id(query.task.as_deref(), "task")?,
        session: id(query.session.as_deref(), "session")?,
    };
    let before = query.before;
    // Failures are logged in full; clients only hear that the log could not be read (the detail
    // may be the store's or the index's, such as SQL text or a path).
    let failed = || ErrorResponse::new(ErrorCode::Internal, "Could not read the event log.");
    let page = tokio::task::spawn_blocking(move || {
        let before = match before {
            Some(before) => before,
            None => activity.source.latest_rev()?.saturating_add(1),
        };
        activity.page(before, limit, &filter)
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "reading activity panicked");
        failed()
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "reading activity failed");
        failed()
    })?;
    Ok(Json(page))
}

/// Parses an optional id from the query: `400 invalid` if it is not one.
fn id<T: FromStr>(value: Option<&str>, kind: &str) -> Result<Option<T>, ErrorResponse> {
    value
        .map(|value| {
            value.parse().map_err(|_| {
                ErrorResponse::new(ErrorCode::Invalid, format!("{value:?} is not a {kind} id."))
            })
        })
        .transpose()
}

/// No filter: the newest `limit` events below `before`.
fn unfiltered(
    source: &dyn EventSource,
    before: u64,
    limit: usize,
) -> Result<EventsPage, SourceError> {
    // One extra event tells whether older ones exist.
    let mut events = source.before(before, limit.saturating_add(1))?;
    let at_start = events.len() <= limit;
    if !at_start {
        events.remove(0);
    }
    Ok(page_of(events, at_start, 0))
}

/// `project` and/or `workstream` alone: the index's answer is the page.
fn indexed(
    source: &dyn EventSource,
    refs: &dyn EventRefs,
    before: u64,
    limit: usize,
    filter: &RefFilter,
) -> Result<EventsPage, SourceError> {
    let (revs, scanned_to) = refs.revs_matching(filter, before, limit)?;
    // Paging relies on these; an index that broke them could make a client loop forever.
    let ascending = revs.windows(2).all(|pair| pair[0] < pair[1]);
    let below = revs.last().is_none_or(|&last| last < before);
    let progress = scanned_to == 0 || scanned_to < before;
    if revs.len() > limit || !ascending || !below || !progress {
        return Err(format!(
            "the activity index answered outside its contract: {} revisions for {limit}, \
             below {before}, stopping at {scanned_to}",
            revs.len()
        )
        .into());
    }
    let events = events_at(source, &revs)?;
    Ok(EventsPage {
        events,
        from_rev: revs.first().copied().unwrap_or(scanned_to),
        to_rev: revs.last().copied().unwrap_or(0),
        at_start: scanned_to == 0,
    })
}

/// The events at `revs` (ascending), read in runs of consecutive revisions.
fn events_at(source: &dyn EventSource, revs: &[u64]) -> Result<Vec<Event>, SourceError> {
    let mut events = Vec::with_capacity(revs.len());
    let mut rest = revs;
    while let Some(&first) = rest.first() {
        let run = 1 + rest
            .windows(2)
            .take_while(|pair| pair[0].checked_add(1) == Some(pair[1]))
            .count();
        let read = match first.checked_sub(1) {
            Some(after) => source.since(after, run)?,
            None => Vec::new(),
        };
        if read.len() != run
            || read
                .iter()
                .zip(first..)
                .any(|(event, rev)| event.rev != rev)
        {
            return Err(format!(
                "the activity index names revisions from {first} that the log does not have"
            )
            .into());
        }
        events.extend(read.into_iter().map(|event| event.event));
        rest = rest.get(run..).unwrap_or_default();
    }
    Ok(events)
}

/// Any filter with `session` or `task` (and any filter without an index): scans back through
/// the log, at most [`SCAN_BUDGET`] events, asking the index about each window it reads.
fn scan(
    source: &dyn EventSource,
    refs: Option<&dyn EventRefs>,
    before: u64,
    limit: usize,
    filter: &RefFilter,
) -> Result<EventsPage, SourceError> {
    let matcher = Matcher::new(filter);
    let mut found: Vec<StoredEvent> = Vec::new(); // newest first
    let mut cursor = before;
    let mut scanned = 0;
    let at_start = 'scan: loop {
        if cursor <= 1 {
            break true;
        }
        if scanned >= SCAN_BUDGET {
            break false;
        }
        let chunk = source.before(cursor, SCAN_PAGE)?;
        let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
            break true;
        };
        let about = match refs {
            Some(refs) => About::within(refs, filter, first.rev, last.rev.saturating_add(1))?,
            None => About::default(),
        };
        cursor = first.rev;
        scanned += chunk.len();
        for event in chunk.into_iter().rev() {
            if matcher.matches(&event, &about) {
                found.push(event);
                if found.len() > limit {
                    found.pop();
                    break 'scan false;
                }
            }
        }
    };
    found.reverse();
    Ok(page_of(found, at_start, cursor))
}

fn page_of(events: Vec<StoredEvent>, at_start: bool, scanned_to: u64) -> EventsPage {
    let (from_rev, to_rev) = match (events.first(), events.last()) {
        (Some(first), Some(last)) => (first.rev, last.rev),
        // An empty page that ran out of scan budget: continue from where it stopped.
        _ if !at_start => (scanned_to, 0),
        _ => (0, 0),
    };
    EventsPage {
        events: events.into_iter().map(|e| e.event).collect(),
        from_rev,
        to_rev,
        at_start,
    }
}

/// What the index says about one window of the log: for each of the filter's fields, the
/// revisions in the window about it. All empty without an index.
#[derive(Debug, Default)]
struct About {
    project: HashSet<u64>,
    workstream: HashSet<u64>,
    task: HashSet<u64>,
    session: HashSet<u64>,
}

impl About {
    /// Asks `refs` about the revisions `lo..hi`, one field at a time.
    fn within(
        refs: &dyn EventRefs,
        filter: &RefFilter,
        lo: u64,
        hi: u64,
    ) -> Result<Self, SourceError> {
        let none = RefFilter::default();
        let ask = |one: Option<RefFilter>| match one {
            Some(one) => revs_within(refs, &one, lo, hi),
            None => Ok(HashSet::new()),
        };
        Ok(Self {
            project: ask(filter.project.map(|project| RefFilter {
                project: Some(project),
                ..none
            }))?,
            workstream: ask(filter.workstream.map(|workstream| RefFilter {
                workstream: Some(workstream),
                ..none
            }))?,
            task: ask(filter.task.map(|task| RefFilter {
                task: Some(task),
                ..none
            }))?,
            session: ask(filter.session.map(|session| RefFilter {
                session: Some(session),
                ..none
            }))?,
        })
    }
}

/// Every revision in `lo..hi` the index matches for `filter`.
///
/// No more than `hi - lo` revisions lie in the window, so asking for that many below `hi` covers
/// it in one call, unless the index stops early (its budget); then it asks again from where the
/// index stopped.
fn revs_within(
    refs: &dyn EventRefs,
    filter: &RefFilter,
    lo: u64,
    hi: u64,
) -> Result<HashSet<u64>, SourceError> {
    let mut within = HashSet::new();
    let mut before = hi;
    while before > lo {
        let limit = usize::try_from(before - lo).unwrap_or(usize::MAX);
        let (revs, scanned_to) = refs.revs_matching(filter, before, limit)?;
        within.extend(revs.iter().copied().filter(|rev| (lo..hi).contains(rev)));
        if scanned_to == 0 || scanned_to <= lo || revs.len() >= limit {
            break;
        }
        if scanned_to >= before {
            return Err(format!(
                "the activity index made no progress below revision {before} (it stopped at \
                 {scanned_to})"
            )
            .into());
        }
        before = scanned_to;
    }
    Ok(within)
}

/// Whether an event matches a filter, from the index's window ([`About`]) and the event's body.
#[derive(Debug)]
struct Matcher {
    filter: RefFilter,
    task: Option<KeyMatch>,
    session: Option<KeyMatch>,
}

impl Matcher {
    fn new(filter: &RefFilter) -> Self {
        Self {
            filter: *filter,
            task: filter.task.map(KeyMatch::task),
            session: filter.session.map(KeyMatch::session),
        }
    }

    fn matches(&self, event: &StoredEvent, about: &About) -> bool {
        let rev = event.rev;
        // The index first: it is cheap, and the only way `project` and `workstream` match.
        if self.filter.project.is_some() && !about.project.contains(&rev) {
            return false;
        }
        if self.filter.workstream.is_some() && !about.workstream.contains(&rev) {
            return false;
        }
        let mut body = None;
        for (key, indexed) in [(&self.task, &about.task), (&self.session, &about.session)] {
            let Some(key) = key else {
                continue;
            };
            if indexed.contains(&rev) {
                continue;
            }
            let value = body
                .get_or_insert_with(|| serde_json::to_value(&event.event.body).unwrap_or_default());
            if !key.matches(value) {
                return false;
            }
        }
        true
    }
}

/// Events with a `key` field, at any depth, that holds `id` or an object whose `id` is `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyMatch {
    key: &'static str,
    id: String,
}

impl KeyMatch {
    fn session(id: SessionId) -> Self {
        Self {
            key: "session",
            id: id.0.to_string(),
        }
    }

    fn task(id: TaskId) -> Self {
        Self {
            key: "task",
            id: id.0.to_string(),
        }
    }

    /// Whether `value` has a `key` field naming the id, at any depth up to [`MAX_DEPTH`].
    fn matches(&self, value: &serde_json::Value) -> bool {
        self.matches_within(value, MAX_DEPTH)
    }

    fn matches_within(&self, value: &serde_json::Value, depth: usize) -> bool {
        let Some(deeper) = depth.checked_sub(1) else {
            return false;
        };
        match value {
            serde_json::Value::Object(map) => map.iter().any(|(key, field)| {
                (key == self.key && self.names(field)) || self.matches_within(field, deeper)
            }),
            serde_json::Value::Array(items) => {
                items.iter().any(|item| self.matches_within(item, deeper))
            }
            _ => false,
        }
    }

    /// Whether `field` is the id, or an object with that `id`.
    fn names(&self, field: &serde_json::Value) -> bool {
        let id = match field {
            serde_json::Value::Object(map) => map.get("id"),
            other => Some(other),
        };
        id.and_then(serde_json::Value::as_str) == Some(self.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::model::Liveness;
    use pitcrew_protocol::{MachineId, MemberId, WorkspaceId};
    use std::sync::Mutex;

    fn filler(n: usize) -> Vec<Event> {
        (0..n)
            .map(|_| {
                Event::now(
                    WorkspaceId::new(),
                    MemberId::new(),
                    EventBody::MachineLiveness {
                        machine: MachineId::new(),
                        liveness: Liveness::Live,
                    },
                )
            })
            .collect()
    }

    fn log(n: usize) -> Arc<MemorySource> {
        let source = Arc::new(MemorySource::new("log", 4));
        source.append(filler(n));
        source
    }

    fn session(id: SessionId) -> RefFilter {
        RefFilter {
            session: Some(id),
            ..RefFilter::default()
        }
    }

    fn task(id: TaskId) -> RefFilter {
        RefFilter {
            task: Some(id),
            ..RefFilter::default()
        }
    }

    fn project(id: ProjectId) -> RefFilter {
        RefFilter {
            project: Some(id),
            ..RefFilter::default()
        }
    }

    #[test]
    fn unfiltered_pages_join_up() {
        let activity = Activity::new(log(1200));
        for limit in [100, 500, 7] {
            let mut revs = Vec::new();
            let mut before = 1201;
            loop {
                let page = activity.page(before, limit, &RefFilter::default()).unwrap();
                assert!(page.events.len() <= limit);
                let expected_first = page.to_rev + 1 - page.events.len() as u64;
                assert_eq!(page.from_rev, expected_first);
                revs.splice(0..0, page.from_rev..=page.to_rev);
                if page.at_start {
                    assert_eq!(page.from_rev, 1);
                    break;
                }
                before = page.from_rev;
            }
            assert_eq!(revs, (1..=1200).collect::<Vec<_>>(), "limit {limit}");
        }
    }

    #[test]
    fn before_is_exclusive_and_at_start_is_exact() {
        let activity = Activity::new(log(10));
        let all = RefFilter::default();
        let page = activity.page(11, 5, &all).unwrap();
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (6, 10, false));
        let page = activity.page(6, 5, &all).unwrap();
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (1, 5, true));
        let page = activity.page(1, 5, &all).unwrap();
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (0, 0, true));
    }

    #[test]
    fn a_filtered_scan_that_runs_out_of_budget_says_where_it_stopped() {
        let activity = Activity::new(log(SCAN_BUDGET + 1500));
        let filter = session(SessionId::new());
        let first = activity.page(u64::MAX, 10, &filter).unwrap();
        assert!(first.events.is_empty());
        assert!(!first.at_start);
        assert_eq!(first.from_rev, 1501);
        assert_eq!(first.to_rev, 0);
        let rest = activity.page(first.from_rev, 10, &filter).unwrap();
        assert!(rest.events.is_empty());
        assert!(rest.at_start);
    }

    #[test]
    fn project_without_an_index_is_an_error() {
        let activity = Activity::new(log(3));
        let error = activity
            .page(u64::MAX, 10, &project(ProjectId::new()))
            .unwrap_err();
        assert_eq!(error.to_string(), NEEDS_INDEX);
    }

    #[test]
    fn the_walk_stops_at_max_depth() {
        let id = SessionId::new();
        let filter = KeyMatch::session(id);
        // The object holding `session` at `level`, 1 being the body itself.
        let nested = |level: usize| {
            let mut value = serde_json::json!({ "session": id.0.to_string() });
            for _ in 1..level {
                value = serde_json::json!({ "inner": [value] });
            }
            value
        };
        // Arrays count as a level too: `{"inner": [..]}` adds two.
        assert!(filter.matches(&nested(1)));
        assert!(filter.matches(&nested(MAX_DEPTH / 2)));
        assert!(!filter.matches(&nested(MAX_DEPTH / 2 + 1)));
        assert!(!filter.matches(&nested(1000)));
    }

    /// Filters match the `session` or `task` field, not the id anywhere in the body.
    #[test]
    fn filters_match_by_key_not_by_substring() {
        use pitcrew_protocol::model::{LinkBasis, Receipt};
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        let (x, y, t) = (SessionId::new(), SessionId::new(), TaskId::new());
        let discovered = |id: SessionId, parent: Option<SessionId>| {
            let mut session = demo.sessions[0].clone();
            session.id = id;
            session.parent = parent;
            session.task = None;
            EventBody::SessionDiscovered { session }
        };
        let mut blocked = demo.tasks[0].clone();
        blocked.id = TaskId::new();
        blocked.blocked_by = vec![t];
        let linked = |session: SessionId| EventBody::SessionLinked {
            session,
            workstream: None,
            task: Some(t),
            basis: LinkBasis::Manual,
        };
        // (body, matches session=x, matches task=t)
        let cases = [
            (EventBody::SessionEnded { session: x }, true, false),
            (discovered(x, None), true, false),
            (discovered(y, Some(x)), false, false),
            (
                EventBody::CommentPosted {
                    task: None,
                    workstream: None,
                    text: x.0.to_string(),
                    mentions: vec![],
                },
                false,
                false,
            ),
            (
                EventBody::TurnEnded {
                    session: y,
                    receipt: Receipt::Transcript {
                        session: x,
                        offset: 0,
                    },
                },
                true,
                false,
            ),
            (linked(y), false, true),
            (linked(x), true, true),
            (EventBody::TaskCreated { task: blocked }, false, false),
        ];
        let source = Arc::new(MemorySource::new("log", 4));
        source.append(
            cases
                .iter()
                .map(|(body, ..)| Event::now(WorkspaceId::new(), MemberId::new(), body.clone()))
                .collect(),
        );
        let activity = Activity::new(source.clone());
        let revs = |filter: &RefFilter| -> Vec<u64> {
            let page = activity.page(u64::MAX, 100, filter).unwrap();
            assert!(page.at_start);
            let all = source.before(u64::MAX, 100).unwrap();
            page.events
                .iter()
                .map(|e| all.iter().find(|s| s.event == *e).unwrap().rev)
                .collect()
        };
        let expected = |pick: fn(&(EventBody, bool, bool)) -> bool| -> Vec<u64> {
            (1..)
                .zip(&cases)
                .filter(|(_, c)| pick(c))
                .map(|(rev, _)| rev)
                .collect()
        };
        assert_eq!(revs(&session(x)), expected(|c| c.1));
        assert_eq!(revs(&task(t)), expected(|c| c.2));
        let both = RefFilter {
            session: Some(x),
            task: Some(t),
            ..RefFilter::default()
        };
        assert_eq!(revs(&both), expected(|c| c.1 && c.2));
    }

    /// An index that answers from a script, and records its calls.
    #[derive(Debug, Default)]
    struct Scripted {
        answers: Mutex<Vec<(Vec<u64>, u64)>>,
        calls: Mutex<Vec<(RefFilter, u64, usize)>>,
    }

    impl Scripted {
        fn answering(answers: Vec<(Vec<u64>, u64)>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into_iter().rev().collect()),
                calls: Mutex::default(),
            })
        }
    }

    impl EventRefs for Scripted {
        fn revs_matching(
            &self,
            filter: &RefFilter,
            before_rev: u64,
            limit: usize,
        ) -> Result<(Vec<u64>, u64), SourceError> {
            self.calls
                .lock()
                .unwrap()
                .push((*filter, before_rev, limit));
            self.answers
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| "no more answers".into())
        }
    }

    #[test]
    fn an_index_that_breaks_its_contract_is_an_error_not_a_loop() {
        let source = log(10);
        let filter = project(ProjectId::new());
        // Asked for 2 revisions below 15, on a log of 10.
        for (answer, what) in [
            ((vec![], 15), "no progress: scanned_to at `before`"),
            ((vec![], 30), "no progress: scanned_to above `before`"),
            ((vec![5, 3], 0), "descending"),
            ((vec![5, 5], 0), "repeated"),
            ((vec![5, 15], 0), "not below `before`"),
            ((vec![1, 2, 3], 0), "more than `limit`"),
            ((vec![0], 0), "revision 0"),
            ((vec![12], 0), "beyond the log"),
        ] {
            let refs = Scripted::answering(vec![answer]);
            let activity = Activity::new(source.clone()).with_refs(refs);
            assert!(activity.page(15, 2, &filter).is_err(), "{what}");
        }
        // The same answers within the contract are pages.
        for (answer, page) in [
            ((vec![], 4), (4, 0, false)),
            ((vec![9, 10], 0), (9, 10, true)),
        ] {
            let refs = Scripted::answering(vec![answer]);
            let got = Activity::new(source.clone())
                .with_refs(refs)
                .page(15, 2, &filter)
                .unwrap();
            assert_eq!((got.from_rev, got.to_rev, got.at_start), page);
        }
    }

    #[test]
    fn the_index_answer_is_read_in_runs() {
        #[derive(Debug)]
        struct Counting(Arc<MemorySource>, Mutex<Vec<(u64, usize)>>);
        impl EventSource for Counting {
            fn log_id(&self) -> String {
                self.0.log_id()
            }
            fn latest_rev(&self) -> Result<u64, SourceError> {
                self.0.latest_rev()
            }
            fn since(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError> {
                self.1.lock().unwrap().push((rev, limit));
                self.0.since(rev, limit)
            }
            fn before(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError> {
                self.0.before(rev, limit)
            }
            fn subscribe(&self) -> crate::source::Subscription {
                self.0.subscribe()
            }
        }
        let inner = log(20);
        let counting = Arc::new(Counting(inner.clone(), Mutex::default()));
        let refs = Scripted::answering(vec![(vec![1, 2, 3, 7, 9, 10], 0)]);
        let activity = Activity::new(counting.clone()).with_refs(refs);
        let page = activity.page(21, 10, &project(ProjectId::new())).unwrap();
        let all = inner.before(21, 20).unwrap();
        let want: Vec<Event> = [1usize, 2, 3, 7, 9, 10]
            .iter()
            .map(|&rev| all[rev - 1].event.clone())
            .collect();
        assert_eq!(page.events, want);
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (1, 10, true));
        assert_eq!(*counting.1.lock().unwrap(), vec![(0, 3), (6, 1), (8, 2)]);
    }

    #[test]
    fn a_window_is_asked_again_from_where_the_index_stopped() {
        let source = log(10);
        let t = TaskId::new();
        // The task's window is 1..=10: the index stops at 7 the first time (its budget), then
        // answers the rest.
        let refs = Scripted::answering(vec![(vec![9], 7), (vec![2, 4], 0)]);
        let activity = Activity::new(source).with_refs(refs.clone());
        let page = activity.page(u64::MAX, 10, &task(t)).unwrap();
        assert_eq!(page.events.len(), 3);
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (2, 9, true));
        let calls = refs.calls.lock().unwrap();
        assert_eq!(*calls, vec![(task(t), 11, 10), (task(t), 7, 6)]);
    }

    #[test]
    fn a_window_where_the_index_makes_no_progress_is_an_error() {
        let refs = Scripted::answering(vec![(vec![], 11)]);
        let activity = Activity::new(log(10)).with_refs(refs);
        assert!(activity.page(u64::MAX, 10, &task(TaskId::new())).is_err());
    }
}
