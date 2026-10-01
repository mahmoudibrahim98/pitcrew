//! Acceptance: the move rules end to end through the routes, the agent scope, and who may answer
//! an ask.

mod common;

use common::{
    PAPER, RUNNER, SAM, WRITER, agent, app, call, demo, expect, get, member, person, seeded,
};
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{DispatchId, EventId, MemberId};
use pitcrew_protocol::model::{Dispatch, DispatchOutcome, Member, MemberKind, Mover, TaskStatus};
use serde_json::{Value, json};
use std::sync::Arc;

const STATUSES: [TaskStatus; 6] = [
    TaskStatus::Backlog,
    TaskStatus::Todo,
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Done,
    TaskStatus::Canceled,
];

fn name(s: TaskStatus) -> String {
    serde_json::to_value(s)
        .expect("status")
        .as_str()
        .expect("string")
        .to_owned()
}

fn last_event(work: &WorkService) -> Event {
    let rev = work.store().latest_rev().expect("rev");
    work.store()
        .since(rev - 1, 1)
        .expect("since")
        .pop()
        .expect("an event")
        .event
}

/// Appends an event as the hub would, for setting up situations no route creates.
fn append(work: &WorkService, author: MemberId, body: EventBody) {
    let event = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: demo().workspace.id,
        author,
        on_behalf_of: None,
        body,
    };
    work.store().append(&[event]).expect("append");
}

#[derive(Clone, Copy, Debug)]
enum Who {
    Person,
    OwnAgent,
    OtherAgent,
}

/// Every (from, to) pair for every kind of mover, through `POST /v1/tasks/{id}/move`, against
/// `TaskStatus::can_move`.
#[tokio::test]
async fn the_move_rules_table_holds_through_the_routes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = person(SAM);
    let mut checked = 0;
    for who in [Who::Person, Who::OwnAgent, Who::OtherAgent] {
        for from in STATUSES {
            for to in STATUSES {
                // A task assigned to @writer, created straight in `from`.
                let created = call(
                    &app,
                    Some(sam),
                    "POST",
                    "/v1/tasks",
                    Some(json!({
                        "project": PAPER, "title": "Rules", "status": name(from),
                        "assignee": WRITER,
                    })),
                )
                .await;
                expect(&created, 201);
                let key = created.1["key"].as_str().expect("key").to_owned();
                let (caller, mover) = match who {
                    Who::Person => (sam, Mover::Person),
                    Who::OwnAgent => (agent(WRITER), Mover::Agent { on_own_task: true }),
                    Who::OtherAgent => (agent(RUNNER), Mover::Agent { on_own_task: false }),
                };
                let rev = work.store().latest_rev().expect("rev");
                let moved = call(
                    &app,
                    Some(caller),
                    "POST",
                    &format!("/v1/tasks/{key}/move"),
                    Some(json!({ "to": name(to) })),
                )
                .await;
                let allowed = from.can_move(to, mover);
                let status = match who {
                    Who::OtherAgent => 403,
                    _ if allowed => 200,
                    _ => 409,
                };
                expect(&moved, status);
                let now = get(&app, sam, &format!("/v1/tasks/{key}")).await;
                let expected = if status == 200 { to } else { from };
                assert_eq!(
                    now.1["status"],
                    json!(name(expected)),
                    "{who:?} {from:?}→{to:?}"
                );
                if status == 200 {
                    assert_eq!(moved.1["status"], json!(name(to)));
                    let event = last_event(&work);
                    assert_eq!(event.author, caller.member);
                    assert_eq!(event.on_behalf_of, caller.on_behalf_of);
                    assert_eq!(
                        event.body,
                        EventBody::TaskMoved {
                            task: event_task(&event),
                            from,
                            to,
                            mover
                        }
                    );
                } else {
                    assert_eq!(
                        work.store().latest_rev().expect("rev"),
                        rev,
                        "nothing appended"
                    );
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 108);
}

fn event_task(event: &Event) -> pitcrew_protocol::ids::TaskId {
    match &event.body {
        EventBody::TaskMoved { task, .. } => *task,
        other => panic!("not a move: {other:?}"),
    }
}

#[tokio::test]
async fn an_active_dispatch_makes_a_task_the_agents_own() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    // PAP-5 has no assignee. @runner may not touch it...
    let move_to = |to: &str| json!({ "to": to });
    let refused = call(
        &app,
        Some(agent(RUNNER)),
        "POST",
        "/v1/tasks/PAP-5/move",
        Some(move_to("in_progress")),
    )
    .await;
    expect(&refused, 403);
    // ...until it holds a dispatch on it.
    let dispatch = Dispatch {
        id: DispatchId::new(),
        task: "01JB000000000000000TSK0005".parse().expect("task"),
        agent: member(RUNNER),
        session: None,
        brief: "Rerun seed 3.".into(),
        started: 1_790_800_000_000,
        ended: None,
        outcome: None,
        summary: None,
    };
    append(
        &work,
        member(SAM),
        EventBody::DispatchStarted {
            dispatch: dispatch.clone(),
        },
    );
    let moved = call(
        &app,
        Some(agent(RUNNER)),
        "POST",
        "/v1/tasks/PAP-5/move",
        Some(move_to("in_progress")),
    )
    .await;
    expect(&moved, 200);
    // A finished dispatch no longer counts.
    append(
        &work,
        member(RUNNER),
        EventBody::DispatchFinished {
            dispatch: dispatch.id,
            outcome: DispatchOutcome::Succeeded,
            summary: None,
        },
    );
    let refused = call(
        &app,
        Some(agent(RUNNER)),
        "POST",
        "/v1/tasks/PAP-5/move",
        Some(move_to("review")),
    )
    .await;
    expect(&refused, 403);
}

#[tokio::test]
async fn agents_write_only_to_their_own_tasks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let writer = Some(agent(WRITER));
    // PAP-4 belongs to @runner: every write by @writer is a 403, a legal-looking move included.
    for (method, path, body) in [
        ("POST", "/v1/tasks/PAP-4/move", json!({ "to": "review" })),
        (
            "POST",
            "/v1/tasks/PAP-4/comments",
            json!({ "text": "Looks good", "mentions": [] }),
        ),
        ("PUT", "/v1/tasks/PAP-4/subtasks", json!([])),
    ] {
        expect(&call(&app, writer, method, path, Some(body)).await, 403);
    }
    // Reads are workspace-wide.
    let other = get(&app, agent(WRITER), "/v1/tasks/PAP-4").await;
    expect(&other, 200);
    assert_eq!(other.1["status"], "in_progress");
    // Device-only routes refuse agents.
    for (method, path, body) in [
        (
            "POST",
            "/v1/tasks",
            Some(json!({ "project": PAPER, "title": "Sneaky" })),
        ),
        (
            "POST",
            "/v1/tasks/PAP-1/assign",
            Some(json!({ "assignee": WRITER })),
        ),
        ("GET", "/v1/machines", None),
        ("GET", "/v1/briefs", None),
        (
            "PATCH",
            "/v1/workstreams/01JB000000000000000WST0001",
            Some(json!({ "health": "blocked" })),
        ),
    ] {
        expect(&call(&app, writer, method, path, body).await, 403);
    }
}

/// Every device route, with a request that a person could make.
fn device_requests() -> Vec<(&'static str, String, Option<Value>)> {
    let ws = "01JB000000000000000WST0001";
    let session = "01JB000000000000000SES0001";
    vec![
        ("GET", "/v1/workspace".into(), None),
        ("GET", "/v1/machines".into(), None),
        ("GET", "/v1/personas".into(), None),
        ("GET", "/v1/teams".into(), None),
        ("GET", "/v1/projects".into(), None),
        ("GET", format!("/v1/projects/{PAPER}"), None),
        ("GET", "/v1/workstreams".into(), None),
        ("GET", format!("/v1/workstreams/{ws}"), None),
        (
            "PATCH",
            format!("/v1/workstreams/{ws}"),
            Some(json!({ "health": "blocked" })),
        ),
        (
            "POST",
            "/v1/tasks".into(),
            Some(json!({ "project": PAPER, "title": "Sneaky" })),
        ),
        (
            "POST",
            "/v1/tasks/PAP-1/assign".into(),
            Some(json!({ "assignee": WRITER })),
        ),
        (
            "POST",
            "/v1/tasks/PAP-5/dispatch".into(),
            Some(json!({ "agent": WRITER })),
        ),
        ("GET", "/v1/sessions".into(), None),
        ("GET", format!("/v1/sessions/{session}"), None),
        ("GET", "/v1/briefs".into(), None),
        (
            "PUT",
            format!("/v1/briefs/workstream/{ws}"),
            Some(json!({ "text": "Sneaky", "pinned": false })),
        ),
    ]
}

#[tokio::test]
async fn device_handlers_refuse_agents_even_when_mounted_without_the_guard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let bare = pitcrew_hub_work::routes().layer(axum::Extension(Arc::clone(&work)));
    let guarded = app(&work);
    let requests = device_requests();
    assert_eq!(
        requests.len(),
        16,
        "every device route in api-v1 that this crate serves"
    );
    let rev = work.store().latest_rev().expect("rev");
    for (method, path, body) in requests {
        for app in [&bare, &guarded] {
            expect(
                &call(app, Some(agent(WRITER)), method, &path, body.clone()).await,
                403,
            );
        }
        // A person reaches the handler (a 503 for the dispatch: there is no runner link here).
        let person_gets = call(&bare, Some(person(SAM)), method, &path, body).await;
        assert!(
            matches!(person_gets.0, 200 | 201 | 503),
            "{method} {path}: {}",
            person_gets.1
        );
    }
    expect(&call(&bare, None, "GET", "/v1/tasks", None).await, 401);
    // Nothing the agent sent was applied.
    let after: Vec<_> = work
        .store()
        .since(rev, usize::MAX)
        .expect("log")
        .into_iter()
        .filter(|e| e.event.author == member(WRITER))
        .collect();
    assert!(after.is_empty(), "{after:?}");
}

/// Authorization comes before validation: an agent that may not make a change hears `403`,
/// whatever is wrong with what it sent.
#[tokio::test]
async fn a_forbidden_agent_hears_403_before_any_400() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let writer = Some(agent(WRITER));
    let huge = "x".repeat(2 * 1024 * 1024);
    // PAP-4 is @runner's.
    for (method, path, body) in [
        ("POST", "/v1/tasks/PAP-4/move", json!({ "to": "finished" })),
        ("POST", "/v1/tasks/PAP-4/move", json!("not an object")),
        ("POST", "/v1/tasks/PAP-4/comments", json!({ "text": "" })),
        (
            "POST",
            "/v1/tasks/PAP-4/comments",
            json!({ "text": "Hi", "mentions": ["01JB000000000000000MEM0099"] }),
        ),
        (
            "POST",
            "/v1/tasks/PAP-4/comments",
            json!({ "text": huge.clone() }),
        ),
        (
            "PUT",
            "/v1/tasks/PAP-4/subtasks",
            json!([{ "id": "01JB000000000000000SBT1001", "text": " ", "done": false,
                     "source": { "kind": "human" } }]),
        ),
        (
            "PUT",
            "/v1/tasks/PAP-4/subtasks",
            json!({ "not": "a list" }),
        ),
        // An ask about another's task, with an empty title and an unknown addressee.
        (
            "POST",
            "/v1/asks",
            json!({ "kind": "question", "to": "01JB000000000000000MEM0099", "title": " ",
                    "task": "01JB000000000000000TSK0004" }),
        ),
        // The demo's decision for @sam: an option out of range, an empty answer.
        (
            "POST",
            "/v1/asks/01JB000000000000000ASK0002/answer",
            json!({ "option": 99 }),
        ),
        (
            "POST",
            "/v1/asks/01JB000000000000000ASK0002/answer",
            json!({}),
        ),
    ] {
        expect(&call(&app, writer, method, path, Some(body)).await, 403);
    }
    // On its own task, the same mistakes are 400s.
    for (method, path, body) in [
        ("POST", "/v1/tasks/PAP-1/move", json!({ "to": "finished" })),
        ("POST", "/v1/tasks/PAP-1/comments", json!({ "text": "" })),
        (
            "PUT",
            "/v1/tasks/PAP-1/subtasks",
            json!([{ "id": "01JB000000000000000SBT1001", "text": " ", "done": false,
                     "source": { "kind": "agent_plan", "agent": WRITER } }]),
        ),
    ] {
        expect(&call(&app, writer, method, path, Some(body)).await, 400);
    }
    // And an unknown task is still a 404, for agents and people.
    for caller in [agent(WRITER), person(SAM)] {
        expect(
            &call(
                &app,
                Some(caller),
                "POST",
                "/v1/tasks/PAP-99/move",
                Some(json!({ "to": "nowhere" })),
            )
            .await,
            404,
        );
    }
}

/// What the path names is looked up before the body is read: an unknown task, workstream or
/// project is `404` whatever the body; a known one with a bad body is `400`.
#[tokio::test]
async fn an_unknown_path_is_404_before_a_bad_body_is_400() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = Some(person(SAM));
    let unknown_ws = "01JB000000000000000WST0099";
    let unknown_project = "01JB000000000000000PRJ0099";
    let bad = json!("not an object");
    for (method, path, status) in [
        ("POST", "/v1/tasks/PAP-99/dispatch".to_owned(), 404),
        ("POST", "/v1/tasks/PAP-99/assign".to_owned(), 404),
        ("PATCH", format!("/v1/workstreams/{unknown_ws}"), 404),
        ("PUT", format!("/v1/briefs/workstream/{unknown_ws}"), 404),
        ("PUT", format!("/v1/briefs/project/{unknown_project}"), 404),
        ("POST", "/v1/tasks/PAP-5/dispatch".to_owned(), 400),
        ("POST", "/v1/tasks/PAP-5/assign".to_owned(), 400),
        (
            "PATCH",
            format!("/v1/workstreams/{}", demo().workstreams[0].id.0),
            400,
        ),
        ("PUT", format!("/v1/briefs/project/{PAPER}"), 400),
    ] {
        let res = call(&app, sam, method, &path, Some(bad.clone())).await;
        assert_eq!(res.0, status, "{method} {path}: {}", res.1);
    }
}

#[tokio::test]
async fn an_answer_needs_an_option_or_some_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = person(SAM);
    let ask = "01JB000000000000000ASK0002";
    for body in [
        json!({ "text": "" }),
        json!({ "text": "   " }),
        json!({ "text": null }),
    ] {
        expect(&answer(&app, sam, ask, body).await, 400);
    }
    // With an option, a blank text is left out.
    let res = answer(&app, sam, ask, json!({ "option": 0, "text": " " })).await;
    expect(&res, 200);
    assert_eq!(res.1["answer"]["option"], 0);
    assert!(res.1["answer"].get("text").is_none(), "{}", res.1);
}

async fn raise(app: &axum::Router, kind: &str, to: &str) -> String {
    let res = call(
        app,
        Some(person(SAM)),
        "POST",
        "/v1/asks",
        Some(json!({ "kind": kind, "to": to, "title": format!("A {kind}"), "options": ["Yes", "No"] })),
    )
    .await;
    expect(&res, 201);
    res.1["id"].as_str().expect("id").to_owned()
}

async fn answer(
    app: &axum::Router,
    caller: pitcrew_protocol::api::Caller,
    id: &str,
    body: Value,
) -> (u16, Value) {
    call(
        app,
        Some(caller),
        "POST",
        &format!("/v1/asks/{id}/answer"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn agents_answer_only_their_own_questions_and_mentions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let writer = agent(WRITER);
    for kind in ["question", "mention"] {
        let id = raise(&app, kind, WRITER).await;
        let res = answer(&app, writer, &id, json!({ "option": 0 })).await;
        expect(&res, 200);
        assert_eq!(res.1["state"], "answered");
        assert_eq!(res.1["answer"]["by"], WRITER);
        let event = last_event(&work);
        assert_eq!(event.author, member(WRITER));
        assert_eq!(event.on_behalf_of, Some(member(SAM)));
    }
    for kind in ["decision", "approval", "review"] {
        let id = raise(&app, kind, WRITER).await;
        expect(
            &answer(&app, writer, &id, json!({ "option": 0 })).await,
            403,
        );
        // @sam owns @writer, so a device token may answer for it.
        expect(
            &answer(&app, person(SAM), &id, json!({ "option": 1 })).await,
            200,
        );
    }
    // An agent never answers an ask addressed to someone else, even a question.
    let to_runner = raise(&app, "question", RUNNER).await;
    expect(
        &answer(&app, writer, &to_runner, json!({ "text": "yes" })).await,
        403,
    );
    // The demo's open decision for @sam.
    expect(
        &answer(
            &app,
            writer,
            "01JB000000000000000ASK0002",
            json!({ "option": 0 }),
        )
        .await,
        403,
    );
}

#[tokio::test]
async fn people_answer_asks_to_themselves_and_their_agents_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    // A second person, @alex, with an agent of their own.
    let alex = MemberId::new();
    let alex_agent = MemberId::new();
    for m in [
        Member {
            id: alex,
            kind: MemberKind::Human,
            handle: "@alex".into(),
            name: "Alex Doe".into(),
            owner: None,
            persona: None,
        },
        Member {
            id: alex_agent,
            kind: MemberKind::Agent,
            handle: "@helper".into(),
            name: "Helper".into(),
            owner: Some(alex),
            persona: None,
        },
    ] {
        append(&work, member(SAM), EventBody::MemberAdded { member: m });
    }
    let to_alex = raise(&app, "decision", &alex.0.to_string()).await;
    let to_helper = raise(&app, "question", &alex_agent.0.to_string()).await;
    expect(
        &answer(&app, person(SAM), &to_alex, json!({ "option": 0 })).await,
        403,
    );
    expect(
        &answer(&app, person(SAM), &to_helper, json!({ "option": 0 })).await,
        403,
    );
    let alex_device = pitcrew_protocol::api::Caller {
        member: alex,
        scope: pitcrew_protocol::api::TokenScope::Device,
        on_behalf_of: None,
    };
    expect(
        &answer(&app, alex_device, &to_helper, json!({ "option": 0 })).await,
        200,
    );
    expect(
        &answer(&app, alex_device, &to_alex, json!({ "text": "Go" })).await,
        200,
    );
}

#[tokio::test]
async fn answers_are_validated_and_given_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = person(SAM);
    let ask = "01JB000000000000000ASK0002";
    expect(&answer(&app, sam, ask, json!({ "option": 5 })).await, 400);
    expect(&answer(&app, sam, ask, json!({})).await, 400);
    expect(
        &answer(
            &app,
            sam,
            "01JB000000000000000ASK0099",
            json!({ "option": 0 }),
        )
        .await,
        404,
    );
    expect(
        &answer(&app, sam, "not-an-id", json!({ "option": 0 })).await,
        404,
    );
    let first = answer(&app, sam, ask, json!({ "option": 1 })).await;
    expect(&first, 200);
    assert_eq!(first.1["answer"]["option"], 1);
    expect(
        &answer(&app, sam, ask, json!({ "text": "Changed my mind" })).await,
        409,
    );
    // The demo's answered approval.
    expect(
        &answer(
            &app,
            sam,
            "01JB000000000000000ASK0004",
            json!({ "option": 1 }),
        )
        .await,
        409,
    );
    let inbox = get(&app, sam, &format!("/v1/asks?to={SAM}&state=open")).await;
    assert_eq!(inbox.1.as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn raising_asks_follows_the_agent_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let writer = Some(agent(WRITER));
    let raise_as = |body: Value| call(&app, writer, "POST", "/v1/asks", Some(body));
    // On its own task and session: fine, from itself.
    let ok = raise_as(json!({
        "kind": "decision", "to": SAM, "title": "Keep §3.2 short?",
        "task": "01JB000000000000000TSK0001", "session": "01JB000000000000000SES0001",
        "receipts": [{ "kind": "transcript", "session": "01JB000000000000000SES0001", "offset": 42 }]
    }))
    .await;
    expect(&ok, 201);
    assert_eq!(ok.1["from"], WRITER);
    assert_eq!(ok.1["state"], "open");
    assert_eq!(ok.1["receipts"][0]["offset"], 42);
    // Another agent's task or session: 403.
    expect(
        &raise_as(json!({ "kind": "question", "to": SAM, "title": "x", "task": "01JB000000000000000TSK0004" })).await,
        403,
    );
    expect(
        &raise_as(json!({ "kind": "question", "to": SAM, "title": "x", "session": "01JB000000000000000SES0003" })).await,
        403,
    );
    // Malformed: unknown addressee, empty title, unknown kind.
    expect(
        &raise_as(json!({ "kind": "question", "to": "01JB000000000000000MEM0099", "title": "x" }))
            .await,
        400,
    );
    expect(
        &raise_as(json!({ "kind": "question", "to": SAM, "title": " " })).await,
        400,
    );
    expect(
        &raise_as(json!({ "kind": "gossip", "to": SAM, "title": "x" })).await,
        400,
    );
}
