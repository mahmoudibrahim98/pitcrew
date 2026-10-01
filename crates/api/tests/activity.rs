//! `GET /v1/events` through the whole router, over the demo workspace's events, without and with
//! an activity index (a fake `EventRefs`).

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_api::activity::{self, NEEDS_INDEX};
use pitcrew_api::source::SourceError;
use pitcrew_api::{Activity, EventRefs, EventSource, MemorySource, RefFilter, RouterParts};
use pitcrew_auth::TokenStore;
use pitcrew_fixtures::DemoWorkspace;
use pitcrew_protocol::api::EventsPage;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::{MemberId, WorkspaceId};
use std::str::FromStr;
use std::sync::Arc;

fn router(f: &Fixture, activity: Activity) -> axum::Router {
    let tokens: Arc<dyn TokenStore> = f.tokens.clone();
    pitcrew_api::router(
        pitcrew_api::local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().device(activity.routes()),
    )
}

/// The route without an index, as `activity::routes` builds it.
fn app(f: &Fixture, source: Arc<dyn EventSource>) -> axum::Router {
    let tokens: Arc<dyn TokenStore> = f.tokens.clone();
    pitcrew_api::router(
        pitcrew_api::local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().device(activity::routes(source)),
    )
}

/// The route with `refs` as its index.
fn indexed(f: &Fixture, source: Arc<dyn EventSource>, refs: Arc<dyn EventRefs>) -> axum::Router {
    router(f, Activity::new(source).with_refs(refs))
}

/// What an event is about, as an activity index keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct About {
    project: Option<ProjectId>,
    workstream: Option<WorkstreamId>,
    task: Option<TaskId>,
    session: Option<SessionId>,
}

impl About {
    fn matches(&self, filter: &RefFilter) -> bool {
        fn field<T: PartialEq>(want: Option<T>, got: Option<T>) -> bool {
            want.is_none() || want == got
        }
        field(filter.project, self.project)
            && field(filter.workstream, self.workstream)
            && field(filter.task, self.task)
            && field(filter.session, self.session)
    }
}

/// A fake activity index: rows by revision, searched as `EventRefs` says. Each call examines at
/// most `budget` rows; unlike the work model's index, that bounds one-field filters too, so the
/// route must cope with an index that stops early on any filter.
#[derive(Debug)]
struct FakeRefs {
    rows: Vec<(u64, About)>,
    budget: usize,
}

impl FakeRefs {
    fn new(rows: Vec<(u64, About)>, budget: usize) -> Arc<Self> {
        assert!(rows.windows(2).all(|pair| pair[0].0 < pair[1].0));
        Arc::new(Self { rows, budget })
    }
}

impl EventRefs for FakeRefs {
    fn revs_matching(
        &self,
        filter: &RefFilter,
        before_rev: u64,
        limit: usize,
    ) -> Result<(Vec<u64>, u64), SourceError> {
        assert_ne!(*filter, RefFilter::default(), "never asked with no filter");
        assert!(limit > 0, "never asked for 0");
        // Walks back like the work model's index: at most `budget` rows, and `scanned_to` is the
        // last one examined when the budget ran out (even if nothing older exists), the oldest
        // returned with more than `limit` matches, else 0.
        let mut found = Vec::new(); // newest first
        let mut examined = 0;
        let mut last = 0;
        let below = self.rows.iter().rev().filter(|(rev, _)| *rev < before_rev);
        for (rev, about) in below.take(self.budget) {
            examined += 1;
            last = *rev;
            if about.matches(filter) {
                found.push(*rev);
                if found.len() > limit {
                    found.truncate(limit);
                    let oldest = *found.last().unwrap();
                    found.reverse();
                    return Ok((found, oldest));
                }
            }
        }
        let scanned_to = if examined >= self.budget { last } else { 0 };
        found.reverse();
        Ok((found, scanned_to))
    }
}

/// An index that cannot be read.
#[derive(Debug)]
struct BrokenRefs;

impl EventRefs for BrokenRefs {
    fn revs_matching(
        &self,
        _: &RefFilter,
        _: u64,
        _: usize,
    ) -> Result<(Vec<u64>, u64), SourceError> {
        Err("the index file at /very/secret/place is corrupt".into())
    }
}

fn parse<T: FromStr>(value: &serde_json::Value) -> Option<T>
where
    T::Err: std::fmt::Debug,
{
    value
        .as_str()
        .or_else(|| value.get("id").and_then(serde_json::Value::as_str))
        .map(|id| id.parse().unwrap())
}

/// What a demo event is about, worked out independently of the route, as the mock hub does: what
/// the event names (a dispatch or an ask by its task and session), then the session's task, the
/// task's (else the session's) workstream, and the workstream's (else the task's) project, from
/// the demo's lists.
fn about(demo: &DemoWorkspace, event: &Event) -> About {
    let body = serde_json::to_value(&event.body).unwrap();
    let data = &body["data"];
    let mut about = About::default();
    match body["type"].as_str().unwrap() {
        "dispatch_started" => {
            about.task = parse(&data["dispatch"]["task"]);
            about.session = parse(&data["dispatch"]["session"]);
        }
        "dispatch_finished" => {
            let id = data["dispatch"].as_str().unwrap();
            let dispatch = demo
                .dispatches
                .iter()
                .find(|d| d.id.0.to_string() == id)
                .unwrap();
            about.task = Some(dispatch.task);
            about.session = dispatch.session;
        }
        "ask_raised" => {
            about.task = parse(&data["ask"]["task"]);
            about.session = parse(&data["ask"]["session"]);
        }
        "brief_proposed" | "brief_accepted" => match data["target"]["kind"].as_str() {
            Some("project") => about.project = parse(&data["target"]["id"]),
            _ => about.workstream = parse(&data["target"]["id"]),
        },
        _ => {
            about.session = parse(&data["session"]);
            about.task = parse(&data["task"]);
            about.workstream = parse(&data["workstream"]);
        }
    }
    let session = about
        .session
        .and_then(|id| demo.sessions.iter().find(|s| s.id == id));
    about.task = about.task.or(session.and_then(|s| s.task));
    let task = about
        .task
        .and_then(|id| demo.tasks.iter().find(|t| t.id == id));
    about.workstream = about
        .workstream
        .or(task.and_then(|t| t.workstream))
        .or(session.and_then(|s| s.workstream));
    let workstream = about
        .workstream
        .and_then(|id| demo.workstreams.iter().find(|w| w.id == id));
    about.project = about
        .project
        .or(workstream.map(|w| w.project))
        .or(task.map(|t| t.project));
    about
}

/// The demo's index rows: what each event is about, for the events about anything.
fn demo_rows(demo: &DemoWorkspace) -> Vec<(u64, About)> {
    (1..)
        .zip(&demo.events)
        .map(|(rev, event)| (rev, about(demo, event)))
        .filter(|(_, about)| *about != About::default())
        .collect()
}

fn demo() -> (Vec<Event>, Arc<MemorySource>) {
    let events = pitcrew_fixtures::demo_workspace().unwrap().events;
    let source = Arc::new(MemorySource::new("demo", 16));
    source.append(events.clone());
    (events, source)
}

/// Whether any object in `value` has `key` naming `id`, as a string or as `{"id": …}`. An
/// independent reading of "the event is about this session/task", to check the route against.
fn refers(value: &serde_json::Value, key: &str, id: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(k, v)| {
            (k == key && (v == id || v.get("id").is_some_and(|inner| inner == id)))
                || refers(v, key, id)
        }),
        serde_json::Value::Array(items) => items.iter().any(|v| refers(v, key, id)),
        _ => false,
    }
}

/// Every page of a query, joined oldest first.
async fn all_pages(app: &axum::Router, token: &str, filter: &str, limit: usize) -> Vec<Event> {
    let mut events = Vec::new();
    let mut before = String::new();
    loop {
        let path = format!("/v1/events?limit={limit}{filter}{before}");
        let (status, body) = call(app.clone(), get_request(&path, Some(token))).await;
        assert_eq!(status, 200, "{path}: {body}");
        let page: EventsPage = serde_json::from_value(body).unwrap();
        events.splice(0..0, page.events);
        if page.at_start {
            return events;
        }
        before = format!("&before={}", page.from_rev);
    }
}

#[tokio::test]
async fn session_and_task_filters_match_the_fixture_events() {
    let f = Fixture::new();
    let (events, source) = demo();
    let app = app(&f, source);
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    let mut checked = 0;
    for (key, ids) in [
        (
            "session",
            demo.sessions
                .iter()
                .map(|s| s.id.0.to_string())
                .collect::<Vec<_>>(),
        ),
        (
            "task",
            demo.tasks
                .iter()
                .map(|t| t.id.0.to_string())
                .collect::<Vec<_>>(),
        ),
    ] {
        for id in ids {
            let expected: Vec<Event> = events
                .iter()
                .filter(|e| refers(&serde_json::to_value(&e.body).unwrap(), key, &id))
                .cloned()
                .collect();
            checked += expected.len();
            for limit in [1, 2, 100] {
                let got = all_pages(&app, &f.device_token, &format!("&{key}={id}"), limit).await;
                assert_eq!(got, expected, "{key}={id} limit={limit}");
            }
        }
    }
    assert!(
        checked > 0,
        "the fixtures name sessions and tasks in their events"
    );
}

#[tokio::test]
async fn unfiltered_paging_returns_the_whole_fixture_log() {
    let f = Fixture::new();
    let (events, source) = demo();
    let app = app(&f, source);
    assert_eq!(all_pages(&app, &f.device_token, "", 3).await, events);
}

#[tokio::test]
async fn project_and_workstream_filters_and_bad_queries_are_invalid() {
    let f = Fixture::new();
    let (_, source) = demo();
    let app = app(&f, source);
    for query in ["project=01JB0000000000000000000000", "workstream=x"] {
        let (status, body) = call(
            app.clone(),
            get_request(&format!("/v1/events?{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert_eq!(body["code"], "invalid");
        assert_eq!(body["message"], NEEDS_INDEX);
    }
    for query in [
        "limit=0",
        "limit=x",
        "before=-1",
        "session=nope",
        "task=PAP-4",
    ] {
        let (status, body) = call(
            app.clone(),
            get_request(&format!("/v1/events?{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert_eq!(body["code"], "invalid");
    }
}

#[tokio::test]
async fn activity_needs_a_device_token() {
    let f = Fixture::new();
    let (_, source) = demo();
    let (status, _) = call(app(&f, source.clone()), get_request("/v1/events", None)).await;
    assert_eq!(status, 401);
    let (status, _) = call(
        app(&f, source),
        get_request("/v1/events", Some(&f.agent_token)),
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn limits_default_to_100_and_cap_at_500() {
    let f = Fixture::new();
    let source = Arc::new(MemorySource::new("big", 16));
    let filler = pitcrew_fixtures::demo_workspace().unwrap().events;
    let mut events = Vec::new();
    while events.len() < 1200 {
        for event in &filler {
            let mut event = event.clone();
            event.id = pitcrew_protocol::EventId::new();
            events.push(event);
        }
    }
    events.truncate(1200);
    source.append(events);
    let app = app(&f, source);
    for (query, len, from, to) in [
        ("", 100, 1101, 1200),
        ("?limit=9999", 500, 701, 1200),
        ("?before=11&limit=5", 5, 6, 10),
    ] {
        let (status, body) = call(
            app.clone(),
            get_request(&format!("/v1/events{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 200);
        let page: EventsPage = serde_json::from_value(body).unwrap();
        assert_eq!(
            (page.events.len(), page.from_rev, page.to_rev, page.at_start),
            (len, from, to, false),
            "{query}"
        );
    }
    // All 1200, joined from pages of the default size.
    let mut before = String::new();
    let mut revs = Vec::new();
    loop {
        let (_, body) = call(
            app.clone(),
            get_request(&format!("/v1/events{before}"), Some(&f.device_token)),
        )
        .await;
        let page: EventsPage = serde_json::from_value(body).unwrap();
        revs.splice(0..0, page.from_rev..=page.to_rev);
        if page.at_start {
            break;
        }
        before = format!("?before={}", page.from_rev);
    }
    assert_eq!(revs, (1..=1200).collect::<Vec<u64>>());
}

// ─── With an activity index ──────────────────────────────────────────────────────────────────────

/// One page: `(events, from_rev, to_rev, at_start)`.
async fn page(app: &axum::Router, token: &str, query: &str) -> EventsPage {
    let path = format!("/v1/events?{query}");
    let (status, body) = call(app.clone(), get_request(&path, Some(token))).await;
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_value(body).unwrap()
}

fn types(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            serde_json::to_value(&e.body).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The events at these revisions of `log`.
fn at(log: &[Event], revs: &[u64]) -> Vec<Event> {
    revs.iter()
        .map(|&rev| log[usize::try_from(rev).unwrap() - 1].clone())
        .collect()
}

#[tokio::test]
async fn project_and_workstream_filters_answer_through_the_index() {
    let f = Fixture::new();
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    let (events, source) = self::demo();
    let rows = demo_rows(&demo);
    let token = &f.device_token;
    // A budget of 1 or 3 rows a call makes the index stop early: pages come back short or empty,
    // not at the start, and joining them must still give every match.
    for budget in [usize::MAX, 3, 1] {
        let app = indexed(&f, source.clone(), FakeRefs::new(rows.clone(), budget));
        let mut checked = 0;
        let filters = demo
            .projects
            .iter()
            .map(|p| ("project", p.id.0.to_string()))
            .chain(
                demo.workstreams
                    .iter()
                    .map(|w| ("workstream", w.id.0.to_string())),
            );
        for (key, id) in filters {
            let filter = RefFilter {
                project: (key == "project").then(|| id.parse().unwrap()),
                workstream: (key == "workstream").then(|| id.parse().unwrap()),
                ..RefFilter::default()
            };
            let revs: Vec<u64> = rows
                .iter()
                .filter(|(_, about)| about.matches(&filter))
                .map(|(rev, _)| *rev)
                .collect();
            checked += revs.len();
            for limit in [1, 2, 100] {
                let got = all_pages(&app, token, &format!("&{key}={id}"), limit).await;
                assert_eq!(
                    got,
                    at(&events, &revs),
                    "{key}={id} limit={limit} budget={budget}"
                );
            }
        }
        assert!(checked > 10, "the demo's events are about its projects");
    }

    // The mock hub's answer for the Tooling project (and so the contract's): 8, 13 and 14.
    let app = indexed(&f, source, FakeRefs::new(rows, usize::MAX));
    let tooling = &demo.projects[1];
    assert_eq!(tooling.name, "Tooling");
    let got = page(&app, token, &format!("project={}", tooling.id.0)).await;
    assert_eq!(got.events, at(&events, &[8, 13, 14]));
    assert_eq!((got.from_rev, got.to_rev, got.at_start), (8, 14, true));
}

#[tokio::test]
async fn task_and_session_filters_add_what_the_index_knows() {
    let f = Fixture::new();
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    let (events, source) = self::demo();
    let rows = demo_rows(&demo);
    let token = &f.device_token;
    let pap1 = demo
        .tasks
        .iter()
        .find(|t| t.key.to_string() == "PAP-1")
        .unwrap();
    let pap3 = demo
        .tasks
        .iter()
        .find(|t| t.key.to_string() == "PAP-3")
        .unwrap();

    // Every task and session: what the body names, plus what the index says.
    for budget in [usize::MAX, 3] {
        let app = indexed(&f, source.clone(), FakeRefs::new(rows.clone(), budget));
        let filters = demo
            .tasks
            .iter()
            .map(|t| ("task", t.id.0.to_string()))
            .chain(
                demo.sessions
                    .iter()
                    .map(|s| ("session", s.id.0.to_string())),
            );
        for (key, id) in filters {
            let filter = RefFilter {
                task: (key == "task").then(|| id.parse().unwrap()),
                session: (key == "session").then(|| id.parse().unwrap()),
                ..RefFilter::default()
            };
            let revs: Vec<u64> = (1..)
                .zip(&events)
                .filter(|(rev, event)| {
                    refers(&serde_json::to_value(&event.body).unwrap(), key, &id)
                        || rows
                            .iter()
                            .any(|(r, about)| r == rev && about.matches(&filter))
                })
                .map(|(rev, _)| rev)
                .collect();
            for limit in [1, 2, 100] {
                let got = all_pages(&app, token, &format!("&{key}={id}"), limit).await;
                assert_eq!(
                    got,
                    at(&events, &revs),
                    "{key}={id} limit={limit} budget={budget}"
                );
            }
        }
    }

    let app = indexed(&f, source.clone(), FakeRefs::new(rows, usize::MAX));
    let plain = self::app(&f, source);
    // PAP-1 includes its session's `file_edited`, which names only the session; as the mock hub.
    let got = page(&app, token, &format!("task={}", pap1.id.0)).await;
    assert_eq!(
        types(&got.events),
        [
            "dispatch_started",
            "task_moved",
            "subtasks_replaced",
            "file_edited"
        ]
    );
    assert_eq!((got.from_rev, got.to_rev, got.at_start), (4, 7, true));
    let got = page(&app, token, &format!("task={}&limit=2", pap1.id.0)).await;
    assert_eq!(types(&got.events), ["subtasks_replaced", "file_edited"]);
    assert_eq!((got.from_rev, got.to_rev, got.at_start), (6, 7, false));
    // Without the index, the gap: the session's events are missed.
    let got = page(&plain, token, &format!("task={}", pap1.id.0)).await;
    assert_eq!(
        types(&got.events),
        ["dispatch_started", "task_moved", "subtasks_replaced"]
    );

    // `dispatch_finished` names only the dispatch: through the index it is about PAP-3 and its
    // session; without it, about neither.
    let session = demo
        .dispatches
        .iter()
        .find(|d| d.task == pap3.id)
        .and_then(|d| d.session)
        .unwrap();
    for (query, with, without) in [
        (format!("task={}", pap3.id.0), vec![2, 3], vec![3]),
        (format!("session={}", session.0), vec![2], vec![]),
    ] {
        assert_eq!(
            page(&app, token, &query).await.events,
            at(&events, &with),
            "{query}"
        );
        assert_eq!(
            page(&plain, token, &query).await.events,
            at(&events, &without),
            "{query}"
        );
    }
}

#[tokio::test]
async fn filters_combine_with_the_index() {
    let f = Fixture::new();
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    let (events, source) = self::demo();
    let rows = demo_rows(&demo);
    let token = &f.device_token;
    let app = indexed(&f, source, FakeRefs::new(rows.clone(), 3));
    // Whether the event at `rev` names the id under `key` in its body.
    let names = |rev: u64, key: &str, id: String| {
        let event = &events[usize::try_from(rev).unwrap() - 1];
        refers(&serde_json::to_value(&event.body).unwrap(), key, &id)
    };
    // Every task with every project, and every session with every workstream: the index for the
    // project or workstream, and the body or the index for the task or session.
    let mut matched = 0;
    for task in &demo.tasks {
        for project in &demo.projects {
            let query = format!("&project={}&task={}", project.id.0, task.id.0);
            let revs: Vec<u64> = rows
                .iter()
                .filter(|(rev, about)| {
                    about.project == Some(project.id)
                        && (about.task == Some(task.id)
                            || names(*rev, "task", task.id.0.to_string()))
                })
                .map(|(rev, _)| *rev)
                .collect();
            matched += revs.len();
            assert_eq!(
                all_pages(&app, token, &query, 2).await,
                at(&events, &revs),
                "{query}"
            );
        }
    }
    assert!(matched > 0, "some tasks have events in their project");
    for session in &demo.sessions {
        for workstream in &demo.workstreams {
            let query = format!("&workstream={}&session={}", workstream.id.0, session.id.0);
            let revs: Vec<u64> = rows
                .iter()
                .filter(|(rev, about)| {
                    about.workstream == Some(workstream.id)
                        && (about.session == Some(session.id)
                            || names(*rev, "session", session.id.0.to_string()))
                })
                .map(|(rev, _)| *rev)
                .collect();
            assert_eq!(
                all_pages(&app, token, &query, 2).await,
                at(&events, &revs),
                "{query}"
            );
        }
    }
    let both = format!(
        "&project={}&workstream={}",
        demo.projects[0].id.0, demo.workstreams[0].id.0
    );
    let revs: Vec<u64> = rows
        .iter()
        .filter(|(_, a)| {
            a.project == Some(demo.projects[0].id) && a.workstream == Some(demo.workstreams[0].id)
        })
        .map(|(rev, _)| *rev)
        .collect();
    assert!(!revs.is_empty());
    assert_eq!(all_pages(&app, token, &both, 1).await, at(&events, &revs));
}

#[tokio::test]
async fn paging_across_a_gap_in_the_index() {
    let f = Fixture::new();
    let token = &f.device_token;
    let (p, q, t, s) = (
        ProjectId::new(),
        ProjectId::new(),
        TaskId::new(),
        SessionId::new(),
    );
    // 1000 events: a comment on task T at 10, a file edit in session S at 995 (linked to T), the
    // rest about nothing. The index says 10 and 995 are about project P, and 11..=990 about
    // another project Q: a gap of 980 rows between P's two events.
    let event = |body| Event::now(WorkspaceId::new(), MemberId::new(), body);
    let mut log: Vec<Event> = (0..1000).map(|_| common::event()).collect();
    log[9] = event(EventBody::CommentPosted {
        task: Some(t),
        workstream: None,
        text: "first".into(),
        mentions: vec![],
    });
    log[994] = event(EventBody::FileEdited {
        session: s,
        path: "main.tex".into(),
        added: 1,
        removed: 0,
        receipt: None,
    });
    let source = Arc::new(MemorySource::new("gap", 16));
    source.append(log.clone());
    let mut rows = vec![(
        10,
        About {
            project: Some(p),
            task: Some(t),
            ..About::default()
        },
    )];
    rows.extend((11..=990).map(|rev| {
        (
            rev,
            About {
                project: Some(q),
                ..About::default()
            },
        )
    }));
    rows.push((
        995,
        About {
            project: Some(p),
            task: Some(t),
            session: Some(s),
            ..About::default()
        },
    ));
    let app = indexed(&f, source.clone(), FakeRefs::new(rows, 100));

    // `project=P`: the index examines 100 rows a call, so the gap takes several requests.
    let mut pages = Vec::new();
    let mut before = String::new();
    loop {
        let got = page(&app, token, &format!("project={}&limit=10{before}", p.0)).await;
        let done = got.at_start;
        before = format!("&before={}", got.from_rev);
        pages.push(got);
        if done {
            break;
        }
        assert!(pages.len() < 50, "paging does not end");
    }
    let summary: Vec<(usize, u64, u64, bool)> = pages
        .iter()
        .map(|p| (p.events.len(), p.from_rev, p.to_rev, p.at_start))
        .collect();
    // 995 and 99 rows of Q; then 100 rows of Q a page; then the last 80 of Q, and 10.
    let mut expected = vec![(1, 995, 995, false)];
    expected.extend((0..9).map(|i| (0, 891 - 100 * i, 0, false)));
    expected.push((1, 10, 10, true));
    assert_eq!(summary, expected);
    let joined: Vec<Event> = pages.iter().rev().flat_map(|p| p.events.clone()).collect();
    assert_eq!(joined, at(&log, &[10, 995]));

    // `task=T` scans the log (1000 events, one request) and asks the index about each window;
    // the index stops early inside each, and is asked again from where it stopped.
    let plain = self::app(&f, source.clone());
    for (query, with, without) in [
        (format!("task={}", t.0), vec![10, 995], Some(vec![10])),
        (format!("session={}", s.0), vec![995], Some(vec![995])),
        (format!("project={}&task={}", p.0, t.0), vec![10, 995], None),
        (format!("project={}&task={}", q.0, t.0), vec![], None),
        (format!("project={}&session={}", p.0, s.0), vec![995], None),
    ] {
        let got = page(&app, token, &query).await;
        assert_eq!(got.events, at(&log, &with), "{query}");
        assert!(got.at_start, "{query}");
        if let Some(without) = without {
            assert_eq!(
                page(&plain, token, &query).await.events,
                at(&log, &without),
                "{query}"
            );
        }
    }
}

#[tokio::test]
async fn at_start_ends_index_paging_exactly() {
    let f = Fixture::new();
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    let (events, source) = self::demo();
    let app = indexed(&f, source, FakeRefs::new(demo_rows(&demo), usize::MAX));
    let token = &f.device_token;
    let tooling = demo.projects[1].id.0;
    let summary = |p: &EventsPage| (p.events.len(), p.from_rev, p.to_rev, p.at_start);
    // Exactly `limit` matches: one page, at the start.
    let got = page(&app, token, &format!("project={tooling}&limit=3")).await;
    assert_eq!(summary(&got), (3, 8, 14, true));
    // One fewer: the newest two, then the last one at the start.
    let got = page(&app, token, &format!("project={tooling}&limit=2")).await;
    assert_eq!(summary(&got), (2, 13, 14, false));
    let got = page(&app, token, &format!("project={tooling}&limit=2&before=13")).await;
    assert_eq!(summary(&got), (1, 8, 8, true));
    assert_eq!(got.events, at(&events, &[8]));
    // Nothing older, nothing at all, nothing below 1.
    for query in [
        format!("project={tooling}&before=8"),
        format!("project={}", ProjectId::new().0),
        format!("workstream={}", WorkstreamId::new().0),
        format!("project={tooling}&before=1"),
        format!("project={tooling}&before=0"),
    ] {
        assert_eq!(
            summary(&page(&app, token, &query).await),
            (0, 0, 0, true),
            "{query}"
        );
    }
}

#[tokio::test]
async fn with_an_index_bad_ids_are_invalid_and_without_one_project_is_400() {
    let f = Fixture::new();
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    let (_, source) = self::demo();
    let app = indexed(
        &f,
        source.clone(),
        FakeRefs::new(demo_rows(&demo), usize::MAX),
    );
    let plain = self::app(&f, source);
    let project = demo.projects[0].id.0;
    for query in [
        "project=x".to_owned(),
        "workstream=PAP".to_owned(),
        format!("project={project}&task=PAP-4"),
        format!("project={project}&limit=0"),
    ] {
        let (status, body) = call(
            app.clone(),
            get_request(&format!("/v1/events?{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert_eq!(body["code"], "invalid", "{query}");
        assert_ne!(body["message"], NEEDS_INDEX, "{query}");
    }
    for query in [
        format!("project={project}"),
        format!("workstream={}", demo.workstreams[0].id.0),
        format!("project={project}&task={}", demo.tasks[0].id.0),
    ] {
        let (status, body) = call(
            plain.clone(),
            get_request(&format!("/v1/events?{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert_eq!(body["message"], NEEDS_INDEX, "{query}");
    }
    // The index route is a device route too.
    let (status, _) = call(app, get_request("/v1/events", Some(&f.agent_token))).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn an_index_that_fails_is_a_500_without_its_detail() {
    let f = Fixture::new();
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    let (_, source) = self::demo();
    let app = indexed(&f, source, Arc::new(BrokenRefs));
    for query in [
        format!("project={}", demo.projects[0].id.0),
        format!("task={}", demo.tasks[0].id.0),
        format!("session={}", demo.sessions[0].id.0),
    ] {
        let (status, body) = call(
            app.clone(),
            get_request(&format!("/v1/events?{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 500, "{query}");
        assert_eq!(body["code"], "internal");
        assert_eq!(body["message"], "Could not read the event log.");
    }
    // Unfiltered activity does not need the index.
    let (status, _) = call(app, get_request("/v1/events", Some(&f.device_token))).await;
    assert_eq!(status, 200);
}
