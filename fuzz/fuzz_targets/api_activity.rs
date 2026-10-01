//! The activity route (`GET /v1/events?before=&limit=&project=&workstream=&task=&session=`,
//! `pitcrew_api::Activity`) on arbitrary query parameters, over in-memory logs of 0 to 1,200
//! events (the demo workspace's, repeated), with and without a fake activity index.
//!
//! Input: a flags byte (index or not, which log, raw or structured), then
//! - raw: the query string itself, or
//! - structured: `before`, `limit` and the four filters, each chosen by a byte from the demo
//!   workspace's ids, an unknown id, or text from the rest of the input.
//!
//! Checks, besides "no panic":
//! - the answer is 200 with an `EventsPage`, or 400 `invalid`; never a 500 (the fake index keeps
//!   its contract);
//! - a page holds at most `limit` events (at most 500), oldest first, and `from_rev`/`to_rev` are
//!   its first and last revisions (`to_rev` 0 when empty);
//! - structured: 400 exactly for a bad id, `limit=0`, or `project`/`workstream` without an index;
//!   otherwise, paging back from `before` with each page's `from_rev` ends with `at_start`, every
//!   page moves back, and the pages together are exactly the events below `before` that match
//!   every filter (the index's answer, or for `task`/`session` also a matching key in the body),
//!   newest page first.
#![no_main]

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use libfuzzer_sys::fuzz_target;
use pitcrew_api::activity::{DEFAULT_LIMIT, MAX_DEPTH, MAX_LIMIT};
use pitcrew_api::source::SourceError;
use pitcrew_api::{Activity, EventRefs, EventSource, MemorySource, RefFilter};
use pitcrew_protocol::api::EventsPage;
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::{EventId, ProjectId, SessionId, TaskId, WorkstreamId};
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use tower::ServiceExt as _;

/// Log sizes: none, one, the demo slice, and the slice repeated past the route's scan page (500).
const SIZES: [usize; 5] = [0, 1, 15, 600, 1200];
/// Index entries the fake index examines per call, so it also stops early.
const INDEX_BUDGET: u64 = 64;

/// The fake index's answer: whether event `rev` is about the filter's id. Deterministic, and
/// unrelated to the body, so it adds events a body match would miss.
fn about(kind: u8, id: &str, rev: u64) -> bool {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ u64::from(kind);
    for b in id.bytes().chain(rev.to_le_bytes()) {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h % 5 == 0
}

fn about_all(filter: &RefFilter, rev: u64) -> bool {
    filter
        .project
        .is_none_or(|id| about(0, &id.to_string(), rev))
        && filter
            .workstream
            .is_none_or(|id| about(1, &id.to_string(), rev))
        && filter.task.is_none_or(|id| about(2, &id.to_string(), rev))
        && filter
            .session
            .is_none_or(|id| about(3, &id.to_string(), rev))
}

/// An index over revisions `1..=len` that keeps `EventRefs`'s contract.
#[derive(Debug)]
struct FakeIndex {
    len: u64,
}

impl EventRefs for FakeIndex {
    fn revs_matching(
        &self,
        filter: &RefFilter,
        before_rev: u64,
        limit: usize,
    ) -> Result<(Vec<u64>, u64), SourceError> {
        let mut found = Vec::new();
        let mut rev = before_rev.min(self.len + 1);
        let mut examined = 0;
        loop {
            if rev <= 1 {
                found.reverse();
                return Ok((found, 0));
            }
            if examined == INDEX_BUDGET {
                found.reverse();
                return Ok((found, rev));
            }
            rev -= 1;
            examined += 1;
            if about_all(filter, rev) {
                found.push(rev);
                if found.len() == limit {
                    found.reverse();
                    return Ok((found, rev));
                }
            }
        }
    }
}

struct Log {
    events: Vec<Event>,
    /// Each event's body as JSON, for the model's key matching.
    bodies: Vec<serde_json::Value>,
    rev_of: HashMap<EventId, u64>,
    plain: Router,
    indexed: Router,
}

struct World {
    runtime: tokio::runtime::Runtime,
    logs: Vec<Log>,
    projects: Vec<ProjectId>,
    workstreams: Vec<WorkstreamId>,
    tasks: Vec<TaskId>,
    sessions: Vec<SessionId>,
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let demo = pitcrew_fixtures::demo_workspace().expect("the demo workspace");
        let logs = SIZES
            .iter()
            .map(|&size| {
                let events: Vec<Event> = (0..size)
                    .map(|i| {
                        let mut e = demo.events[i % demo.events.len()].clone();
                        e.id = format!("{:026}", i + 1).parse().expect("an event id");
                        e
                    })
                    .collect();
                let source = Arc::new(MemorySource::new("fuzz", 16));
                source.append(events.clone());
                let source: Arc<dyn EventSource> = source;
                let index = Arc::new(FakeIndex { len: size as u64 });
                Log {
                    rev_of: events.iter().zip(1..).map(|(e, rev)| (e.id, rev)).collect(),
                    bodies: events
                        .iter()
                        .map(|e| serde_json::to_value(&e.body).expect("a body serializes"))
                        .collect(),
                    events,
                    plain: Activity::new(Arc::clone(&source)).routes(),
                    indexed: Activity::new(source).with_refs(index).routes(),
                }
            })
            .collect();
        let unknown = |n: u128| ulid_text(n);
        World {
            runtime: tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a runtime"),
            logs,
            projects: ids(demo.projects.iter().map(|p| p.id), unknown(1)),
            workstreams: ids(demo.workstreams.iter().map(|w| w.id), unknown(2)),
            tasks: ids(demo.tasks.iter().map(|t| t.id), unknown(3)),
            sessions: ids(demo.sessions.iter().map(|s| s.id), unknown(4)),
        }
    })
}

fn ulid_text(n: u128) -> String {
    format!("{n:026}")
}

fn ids<T: FromStr>(known: impl Iterator<Item = T>, unknown: String) -> Vec<T>
where
    T::Err: std::fmt::Debug,
{
    let mut out: Vec<T> = known.collect();
    out.push(unknown.parse().expect("an id"));
    out
}

fuzz_target!(|input: &[u8]| {
    let Some((&flags, rest)) = input.split_first() else {
        return;
    };
    let world = world();
    let log = &world.logs[usize::from(flags >> 1) % SIZES.len()];
    let with_index = flags & 1 == 1;
    let router = if with_index { &log.indexed } else { &log.plain };
    if flags & 0x80 != 0 {
        let query = String::from_utf8_lossy(rest);
        if let Some((status, page)) = get(world, router, &query) {
            if let Some(page) = page {
                check_page(log, &page, MAX_LIMIT, u64::MAX);
            }
            assert!(status == StatusCode::OK || status == StatusCode::BAD_REQUEST);
        }
        return;
    }
    structured(world, log, router, with_index, rest);
});

/// A request with the given query: its status, and the page on a 200.
fn get(world: &World, router: &Router, query: &str) -> Option<(StatusCode, Option<EventsPage>)> {
    let request = Request::get(format!("/v1/events?{query}"))
        .body(Body::empty())
        .ok()?;
    world.runtime.block_on(async {
        let response = router.clone().oneshot(request).await.expect("infallible");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 24)
            .await
            .expect("a body");
        assert!(
            status == StatusCode::OK || status == StatusCode::BAD_REQUEST,
            "status {status}: {}",
            String::from_utf8_lossy(&body)
        );
        if status == StatusCode::OK {
            let page: EventsPage = serde_json::from_slice(&body).expect("a 200 is an EventsPage");
            Some((status, Some(page)))
        } else {
            let error: serde_json::Value = serde_json::from_slice(&body).expect("a JSON error");
            assert_eq!(error["code"], "invalid", "a 400 that is not `invalid`");
            Some((status, None))
        }
    })
}

/// A page's own consistency: at most `limit` events, oldest first, all below `before`, with
/// `from_rev`/`to_rev` its first and last revisions. Returns the revisions.
fn check_page(log: &Log, page: &EventsPage, limit: usize, before: u64) -> Vec<u64> {
    assert!(page.events.len() <= limit.min(MAX_LIMIT), "over the limit");
    let revs: Vec<u64> = page
        .events
        .iter()
        .map(|e| *log.rev_of.get(&e.id).expect("an event from the log"))
        .collect();
    assert!(revs.windows(2).all(|w| w[0] < w[1]), "not oldest first");
    match (revs.first(), revs.last()) {
        (Some(&first), Some(&last)) => {
            assert_eq!((page.from_rev, page.to_rev), (first, last));
            assert!(last < before, "an event at or after `before`");
        }
        _ => assert_eq!(page.to_rev, 0, "an empty page with a `to_rev`"),
    }
    revs
}

/// One query parameter: absent, a known id, an unknown id, or text from the input.
fn pick<T: Display + Copy>(choice: u8, known: &[T], text: &str) -> Option<String> {
    match choice % 8 {
        0..=3 => None,
        4 | 5 => Some(known[usize::from(choice >> 3) % known.len()].to_string()),
        6 => Some(known[known.len() - 1].to_string()),
        _ => Some(text.to_owned()),
    }
}

fn structured(world: &World, log: &Log, router: &Router, with_index: bool, rest: &[u8]) {
    let Some((&[b, l, p, w, t, s], text)) = rest.split_first_chunk::<6>() else {
        return;
    };
    let text = String::from_utf8_lossy(text);
    let len = log.events.len() as u64;
    let before = match b % 4 {
        0 => None,
        1 => Some(u64::from(b >> 2)),
        2 => Some(len * u64::from(b >> 2) / 63 + 1),
        _ => Some(u64::MAX - u64::from(b >> 2)),
    };
    // Small limits on the large logs would mean a thousand pages per input; at least 1/32 of the
    // log per page keeps paging to a few dozen requests.
    let limit = match l % 4 {
        0 => None,
        1 => Some(0),
        2 => Some(usize::from(l >> 2).max(log.events.len() / 32 + 1)),
        _ => Some(usize::from(l) * 4),
    };
    let project = pick(p, &world.projects, &text);
    let workstream = pick(w, &world.workstreams, &text);
    let task = pick(t, &world.tasks, &text);
    let session = pick(s, &world.sessions, &text);

    fn parsed<T: FromStr>(value: &Option<String>) -> Result<Option<T>, ()> {
        value
            .as_deref()
            .map(|v| v.parse().map_err(|_| ()))
            .transpose()
    }
    let filter = (|| {
        Ok::<_, ()>(RefFilter {
            project: parsed(&project)?,
            workstream: parsed(&workstream)?,
            task: parsed(&task)?,
            session: parsed(&session)?,
        })
    })();
    let refused = filter.is_err()
        || limit == Some(0)
        || (!with_index && (project.is_some() || workstream.is_some()));

    let query = |before: Option<u64>| {
        let mut q = Vec::new();
        if let Some(before) = before {
            q.push(format!("before={before}"));
        }
        if let Some(limit) = limit {
            q.push(format!("limit={limit}"));
        }
        for (name, value) in [
            ("project", &project),
            ("workstream", &workstream),
            ("task", &task),
            ("session", &session),
        ] {
            if let Some(value) = value {
                q.push(format!("{name}={}", encode(value)));
            }
        }
        q.join("&")
    };

    let Some((status, page)) = get(world, router, &query(before)) else {
        return;
    };
    if refused {
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "accepted: {}",
            query(before)
        );
        return;
    }
    assert_eq!(status, StatusCode::OK, "refused: {}", query(before));
    let Ok(filter) = filter else { return };
    let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let start = before.unwrap_or(len + 1);

    let expected: Vec<u64> = (1..=len)
        .rev()
        .filter(|&rev| {
            rev < start && matches(&filter, with_index, rev, &log.bodies[rev as usize - 1])
        })
        .collect();
    let mut got: Vec<u64> = Vec::new();
    let mut page = page.expect("a page");
    let mut at = start;
    for _ in 0..(2 * len + 16) {
        let revs = check_page(log, &page, limit, at);
        got.extend(revs.iter().rev());
        if page.at_start {
            assert_eq!(got, expected, "the pages are not the matching events");
            return;
        }
        assert!(page.from_rev < at, "a page that does not move back");
        at = page.from_rev;
        let (status, next) = get(world, router, &query(Some(at))).expect("a request");
        assert_eq!(status, StatusCode::OK);
        page = next.expect("a page");
    }
    panic!("paging did not reach the start");
}

/// Whether the event at `rev` matches every filter: through the index, or for `task` and
/// `session` also through its body.
fn matches(filter: &RefFilter, with_index: bool, rev: u64, body: &serde_json::Value) -> bool {
    let index = |kind: u8, id: Option<String>| -> bool {
        with_index && id.is_some_and(|id| about(kind, &id, rev))
    };
    if filter.project.is_some() && !index(0, filter.project.map(|i| i.to_string())) {
        return false;
    }
    if filter.workstream.is_some() && !index(1, filter.workstream.map(|i| i.to_string())) {
        return false;
    }
    if let Some(task) = filter.task
        && !index(2, Some(task.to_string()))
        && !names(body, "task", &task.0.to_string(), MAX_DEPTH)
    {
        return false;
    }
    if let Some(session) = filter.session
        && !index(3, Some(session.to_string()))
        && !names(body, "session", &session.0.to_string(), MAX_DEPTH)
    {
        return false;
    }
    true
}

/// Whether `value` has a `key` field, within `depth` levels, holding `id` or an object whose
/// `id` is `id`.
fn names(value: &serde_json::Value, key: &str, id: &str, depth: usize) -> bool {
    let Some(deeper) = depth.checked_sub(1) else {
        return false;
    };
    let holds = |field: &serde_json::Value| {
        let field = match field {
            serde_json::Value::Object(map) => map.get("id"),
            other => Some(other),
        };
        field.and_then(serde_json::Value::as_str) == Some(id)
    };
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(k, field)| (k == key && holds(field)) || names(field, key, id, deeper)),
        serde_json::Value::Array(items) => items.iter().any(|item| names(item, key, id, deeper)),
        _ => false,
    }
}

/// Percent-encodes everything but unreserved characters.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
