//! `GET /v1/events` through the whole router, over the demo workspace's events.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_api::activity::{self, NEEDS_INDEX};
use pitcrew_api::{EventSource, MemorySource, RouterParts, local_host_info};
use pitcrew_auth::TokenStore;
use pitcrew_protocol::api::EventsPage;
use pitcrew_protocol::events::Event;
use std::sync::Arc;

fn app(f: &Fixture, source: Arc<dyn EventSource>) -> axum::Router {
    let tokens: Arc<dyn TokenStore> = f.tokens.clone();
    pitcrew_api::router(
        local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().device(activity::routes(source)),
    )
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
