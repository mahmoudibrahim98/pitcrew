//! The work routes: creating and assigning tasks, subtasks, comments, briefs, workstreams, and the
//! error contract.

mod common;

use common::{
    PAPER, PARSERS, RUNNER, SAM, SEED_RUNS, SUBMISSION, TOOLING, WRITER, agent, app, call, expect,
    get, member, person, seeded,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::{BriefTarget, Receipt};
use serde_json::{Value, json};

fn plan(id: &str, text: &str, agent: &str) -> Value {
    json!({ "id": id, "text": text, "done": false, "source": { "kind": "agent_plan", "agent": agent } })
}

fn human(id: &str, text: &str) -> Value {
    json!({ "id": id, "text": text, "done": true, "source": { "kind": "human" } })
}

fn texts(task: &Value) -> Vec<String> {
    task["subtasks"]
        .as_array()
        .expect("subtasks")
        .iter()
        .map(|s| s["text"].as_str().expect("text").to_owned())
        .collect()
}

const S1: &str = "01JB000000000000000SBT1001";
const S2: &str = "01JB000000000000000SBT1002";
const S3: &str = "01JB000000000000000SBT1003";
const S4: &str = "01JB000000000000000SBT1004";
const S5: &str = "01JB000000000000000SBT1005";

#[tokio::test]
async fn creating_tasks_allocates_keys_per_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = Some(person(SAM));
    let create = |body: Value| call(&app, sam, "POST", "/v1/tasks", Some(body));

    let first = create(json!({ "project": PAPER, "title": "Write the abstract" })).await;
    expect(&first, 201);
    assert_eq!(first.1["key"], "PAP-8");
    assert_eq!(first.1["status"], "todo");
    assert_eq!(first.1["priority"], "none");
    assert_eq!(first.1["description"], "");
    assert_eq!(first.1["id"].as_str().map(str::len), Some(26));
    let second = create(json!({ "project": PAPER, "title": "Check references" })).await;
    assert_eq!(second.1["key"], "PAP-9");
    let tooling = create(json!({
        "project": TOOLING, "workstream": PARSERS, "title": "Benchmark OpenCode parsing",
        "status": "backlog", "priority": "low", "labels": ["performance", "tests"],
        "due": "2026-10-31", "assignee": RUNNER, "description": "Measure it."
    }))
    .await;
    expect(&tooling, 201);
    assert_eq!(tooling.1["key"], "TL-4");
    assert_eq!(tooling.1["labels"], json!(["performance", "tests"]));
    let fetched = get(&app, person(SAM), "/v1/tasks/TL-4").await;
    assert_eq!(fetched.1, tooling.1);
    let event = work
        .store()
        .since(work.store().latest_rev().expect("rev") - 1, 1)
        .expect("log");
    assert!(matches!(event[0].event.body, EventBody::TaskCreated { .. }));
    assert_eq!(event[0].event.author, member(SAM));

    for bad in [
        json!({ "title": "No project" }),
        json!({ "project": PAPER }),
        json!({ "project": PAPER, "title": "  " }),
        json!({ "project": PAPER, "title": "x", "status": "finished" }),
        json!({ "project": PAPER, "title": "x", "due": "2026-13-01" }),
        json!({ "project": PAPER, "workstream": PARSERS, "title": "Wrong project" }),
        json!({ "project": "01JB000000000000000PRJ0099", "title": "Unknown project" }),
        json!({ "project": PAPER, "title": "x", "assignee": "01JB000000000000000MEM0099" }),
        json!({ "project": "PAP", "title": "Key, not id" }),
        json!(["not", "an", "object"]),
    ] {
        expect(&create(bad.clone()).await, 400);
    }
    // Nothing was created by the refused requests.
    let tasks = get(&app, person(SAM), &format!("/v1/tasks?project={PAPER}")).await;
    assert_eq!(tasks.1.as_array().map(Vec::len), Some(9));
}

#[tokio::test]
async fn assigning_needs_the_key_and_a_known_member() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = Some(person(SAM));
    let assign = |body: Value| call(&app, sam, "POST", "/v1/tasks/PAP-5/assign", Some(body));
    let res = assign(json!({ "assignee": RUNNER })).await;
    expect(&res, 200);
    assert_eq!(res.1["assignee"], RUNNER);
    let rev = work.store().latest_rev().expect("rev");
    // The same assignee again changes nothing and appends nothing.
    expect(&assign(json!({ "assignee": RUNNER })).await, 200);
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    let res = assign(json!({ "assignee": null })).await;
    expect(&res, 200);
    assert!(res.1.get("assignee").is_none());
    expect(&assign(json!({})).await, 400);
    expect(
        &assign(json!({ "assignee": "01JB000000000000000MEM0099" })).await,
        400,
    );
    expect(&assign(json!({ "assignee": 7 })).await, 400);
    expect(
        &call(
            &app,
            sam,
            "POST",
            "/v1/tasks/PAP-99/assign",
            Some(json!({ "assignee": null })),
        )
        .await,
        404,
    );
}

#[tokio::test]
async fn an_agents_subtasks_replace_only_its_own_plan_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    // A person sets up a mixed list on @writer's PAP-1.
    let mixed = json!([
        human(S1, "Agree the outline"),
        plan(S2, "Draft §3.1", WRITER),
        plan(S3, "Check the numbers", RUNNER),
        plan(S4, "Draft §3.2", WRITER),
    ]);
    let res = call(
        &app,
        Some(person(SAM)),
        "PUT",
        "/v1/tasks/PAP-1/subtasks",
        Some(mixed),
    )
    .await;
    expect(&res, 200);
    assert_eq!(texts(&res.1).len(), 4);

    // @writer's new plan replaces its two lines, where they stood; the rest stay.
    let new_plan = json!([plan(S5, "Draft all of §3", WRITER)]);
    let res = call(
        &app,
        Some(agent(WRITER)),
        "PUT",
        "/v1/tasks/PAP-1/subtasks",
        Some(new_plan),
    )
    .await;
    expect(&res, 200);
    assert_eq!(
        texts(&res.1),
        ["Agree the outline", "Draft all of §3", "Check the numbers"]
    );
    assert_eq!(res.1["subtasks"][0]["source"], json!({ "kind": "human" }));
    // The event carries the whole resulting list, authored by the agent for its owner.
    let rev = work.store().latest_rev().expect("rev");
    let last = work.store().since(rev - 1, 1).expect("log").remove(0).event;
    assert_eq!(last.author, member(WRITER));
    assert_eq!(last.on_behalf_of, Some(member(SAM)));
    match last.body {
        EventBody::SubtasksReplaced { subtasks, .. } => assert_eq!(subtasks.len(), 3),
        other => panic!("{other:?}"),
    }

    // An agent may not write a person's line, or another agent's plan.
    let writer = Some(agent(WRITER));
    for body in [
        json!([human(S5, "Sneaky")]),
        json!([plan(S5, "Sneaky", RUNNER)]),
    ] {
        expect(
            &call(&app, writer, "PUT", "/v1/tasks/PAP-1/subtasks", Some(body)).await,
            403,
        );
    }
    // Malformed lists.
    let sam = Some(person(SAM));
    for body in [
        json!([human(S1, "One"), human(S1, "Same id")]),
        json!([human(S1, " ")]),
        json!([plan(S1, "Plan of a person", SAM)]),
        json!([plan(S1, "Plan of nobody", "01JB000000000000000MEM0099")]),
        json!({ "not": "a list" }),
    ] {
        expect(
            &call(&app, sam, "PUT", "/v1/tasks/PAP-1/subtasks", Some(body)).await,
            400,
        );
    }
    // An agent re-using a person's line id in its plan is refused too.
    expect(
        &call(
            &app,
            writer,
            "PUT",
            "/v1/tasks/PAP-1/subtasks",
            Some(json!([plan(S1, "Clash", WRITER)])),
        )
        .await,
        400,
    );
    // A person replaces the whole list.
    let res = call(
        &app,
        sam,
        "PUT",
        "/v1/tasks/PAP-1/subtasks",
        Some(json!([])),
    )
    .await;
    expect(&res, 200);
    assert!(texts(&res.1).is_empty());
}

#[tokio::test]
async fn comments_are_events_from_the_caller() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let res = call(
        &app,
        Some(agent(WRITER)),
        "POST",
        "/v1/tasks/PAP-1/comments",
        Some(json!({ "text": "§3.2 is drafted.", "mentions": [SAM] })),
    )
    .await;
    expect(&res, 201);
    assert_eq!(res.1["author"], WRITER);
    assert_eq!(res.1["on_behalf_of"], SAM);
    assert_eq!(
        res.1["body"],
        json!({
            "type": "comment_posted",
            "data": { "task": "01JB000000000000000TSK0001", "text": "§3.2 is drafted.", "mentions": [SAM] }
        })
    );
    let rev = work.store().latest_rev().expect("rev");
    let stored = work.store().since(rev - 1, 1).expect("log").remove(0).event;
    assert_eq!(serde_json::to_value(&stored).expect("json"), res.1);
    // Mentions default to none; unknown members and empty text are refused.
    let sam = Some(person(SAM));
    expect(
        &call(
            &app,
            sam,
            "POST",
            "/v1/tasks/PAP-4/comments",
            Some(json!({ "text": "Nice" })),
        )
        .await,
        201,
    );
    expect(
        &call(
            &app,
            sam,
            "POST",
            "/v1/tasks/PAP-4/comments",
            Some(json!({ "text": "Hi", "mentions": ["01JB000000000000000MEM0099"] })),
        )
        .await,
        400,
    );
    expect(
        &call(
            &app,
            sam,
            "POST",
            "/v1/tasks/PAP-4/comments",
            Some(json!({ "text": "" })),
        )
        .await,
        400,
    );
    let count: i64 = work
        .read(|c| {
            Ok(
                c.query_row("SELECT COUNT(*) FROM work_comment_mentions", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .expect("count");
    // The demo's comment mentions @reviewer; the first one here mentions @sam.
    assert_eq!(count, 2);
}

#[tokio::test]
async fn briefs_are_put_by_people() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = Some(person(SAM));
    let path = format!("/v1/briefs/workstream/{SUBMISSION}");
    let res = call(
        &app,
        sam,
        "PUT",
        &path,
        Some(json!({ "text": "§3 is drafted.", "next": "Review §3.", "pinned": true })),
    )
    .await;
    expect(&res, 200);
    assert_eq!(res.1["text"], "§3 is drafted.");
    assert_eq!(res.1["source"], "person");
    assert_eq!(res.1["pinned"], true);
    assert_eq!(res.1["receipts"], json!([]));
    assert_eq!(
        res.1["target"],
        json!({ "kind": "workstream", "id": SUBMISSION })
    );
    let briefs = get(&app, person(SAM), "/v1/briefs").await;
    assert_eq!(
        briefs.1.as_array().map(Vec::len),
        Some(4),
        "replaced, not added"
    );
    assert_eq!(briefs.1[1], res.1, "keeps its place in the list");

    // A new target is added at the end.
    let res = call(
        &app,
        sam,
        "PUT",
        "/v1/briefs/workstream/01JB000000000000000WST0004",
        Some(json!({ "text": "Not started.", "pinned": false })),
    )
    .await;
    expect(&res, 200);
    let briefs = get(&app, person(SAM), "/v1/briefs").await;
    assert_eq!(briefs.1[4], res.1);

    // Accepting the back office's proposal unchanged keeps its receipts and its source.
    let receipt = Receipt::Event { id: EventId::new() };
    let proposal = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: common::demo().workspace.id,
        author: member("01JB000000000000000MEM0006"),
        on_behalf_of: Some(member(SAM)),
        body: EventBody::BriefProposed {
            target: BriefTarget::Workstream(SEED_RUNS.parse().expect("id")),
            text: "Seeds 1, 2, 4, 5 finished.".into(),
            next: None,
            receipts: vec![receipt.clone()],
        },
    };
    work.store().append(&[proposal]).expect("append");
    let res = call(
        &app,
        sam,
        "PUT",
        &format!("/v1/briefs/workstream/{SEED_RUNS}"),
        Some(json!({ "text": "Seeds 1, 2, 4, 5 finished.", "pinned": false })),
    )
    .await;
    expect(&res, 200);
    assert_eq!(res.1["source"], "back_office");
    assert_eq!(res.1["receipts"], json!([receipt]));

    for (path, status) in [
        ("/v1/briefs/task/01JB000000000000000TSK0001", 404),
        ("/v1/briefs/project/01JB000000000000000PRJ0099", 404),
        ("/v1/briefs/workstream/nope", 404),
    ] {
        expect(
            &call(
                &app,
                sam,
                "PUT",
                path,
                Some(json!({ "text": "x", "pinned": false })),
            )
            .await,
            status,
        );
    }
    expect(
        &call(&app, sam, "PUT", &path, Some(json!({ "text": "x" }))).await,
        400,
    );
}

#[tokio::test]
async fn patching_a_workstream_emits_a_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = Some(person(SAM));
    let path = format!("/v1/workstreams/{SEED_RUNS}");
    let res = call(
        &app,
        sam,
        "PATCH",
        &path,
        Some(json!({ "health": "blocked" })),
    )
    .await;
    expect(&res, 200);
    assert_eq!(res.1["health"], "blocked");
    assert_eq!(res.1["status"], "active");
    let rev = work.store().latest_rev().expect("rev");
    let last = work.store().since(rev - 1, 1).expect("log").remove(0).event;
    assert!(matches!(last.body, EventBody::WorkstreamChanged { .. }));
    // No change, no event.
    expect(
        &call(
            &app,
            sam,
            "PATCH",
            &path,
            Some(json!({ "health": "blocked" })),
        )
        .await,
        200,
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    expect(&call(&app, sam, "PATCH", &path, Some(json!({}))).await, 400);
    expect(
        &call(&app, sam, "PATCH", &path, Some(json!({ "status": "done" }))).await,
        400,
    );
    expect(
        &call(
            &app,
            sam,
            "PATCH",
            "/v1/workstreams/01JB000000000000000WST0099",
            Some(json!({ "health": "blocked" })),
        )
        .await,
        404,
    );
}

#[tokio::test]
async fn errors_follow_the_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = Some(person(SAM));
    // Missing caller (a route mounted outside authentication).
    expect(&call(&app, None, "GET", "/v1/tasks", None).await, 401);
    // Unknown things in the path.
    for path in [
        "/v1/tasks/PAP-99",
        "/v1/tasks/garbage",
        "/v1/tasks/tsk_01JB000000000000000TSK0099",
        "/v1/projects/01JB000000000000000PRJ0099",
        "/v1/projects/garbage",
        "/v1/workstreams/garbage",
        "/v1/nowhere",
    ] {
        expect(&call(&app, sam, "GET", path, None).await, 404);
    }
    // An unknown method on a known route.
    expect(&call(&app, sam, "DELETE", "/v1/tasks", None).await, 404);
    // Malformed and oversized bodies.
    let malformed = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/tasks/PAP-2/move")
        .body(axum::body::Body::from("{\"to\":"))
        .expect("request");
    let (status, body) = send(&app, malformed).await;
    assert_eq!((status, body["code"].as_str()), (400, Some("invalid")));
    let huge = json!({ "project": PAPER, "title": "x".repeat(1024 * 1024) });
    expect(&call(&app, sam, "POST", "/v1/tasks", Some(huge)).await, 400);
    expect(
        &call(&app, sam, "POST", "/v1/tasks/PAP-2/move", None).await,
        400,
    );
    expect(
        &call(
            &app,
            sam,
            "POST",
            "/v1/tasks/PAP-2/move",
            Some(json!({ "to": "finished" })),
        )
        .await,
        400,
    );
}

async fn send(app: &axum::Router, mut request: axum::extract::Request) -> (u16, Value) {
    use tower::ServiceExt as _;
    request.extensions_mut().insert(person(SAM));
    let response = app.clone().oneshot(request).await.expect("infallible");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}
