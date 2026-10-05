//! Acceptance: every rule and error code of `PATCH /v1/tasks/{id-or-key}`, `POST /v1/projects` and
//! `POST /v1/workstreams`, through the routes (the same cases as apps/mock-hub/test/edits.test.ts).

mod common;

use common::{
    PAPER, PARSERS, SAM, SEED_RUNS, SUBMISSION, TOOLING, WRITER, agent, app, call, expect, get,
    member, person, seeded,
};
use pitcrew_hub_work::{TaskRef, WorkService};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, ProjectId, ProjectKey};
use pitcrew_protocol::model::{Project, ProjectStatus, TaskPatch};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt as _;

const UNKNOWN_PROJECT: &str = "01JB000000000000000PRJ0099";
const UNKNOWN_WORKSTREAM: &str = "01JB000000000000000WST0099";
const UNKNOWN_MEMBER: &str = "01JB000000000000000MEM0099";
const UNKNOWN_MACHINE: &str = "01JB000000000000000MCH0099";
const UNKNOWN_TASK: &str = "01JB000000000000000TSK0099";
const LAPTOP: &str = "01JB000000000000000MCH0001";
const RUNNER: &str = "01JB000000000000000MEM0003";
const PAP1: &str = "01JB000000000000000TSK0001";
const PAP2: &str = "01JB000000000000000TSK0002";
const PAP4: &str = "01JB000000000000000TSK0004";
const PAP8: &str = "01JB000000000000000TSK0008";

fn rev(work: &WorkService) -> u64 {
    work.store().latest_rev().expect("rev")
}

fn last_event(work: &WorkService) -> Event {
    work.store()
        .since(rev(work) - 1, 1)
        .expect("since")
        .pop()
        .expect("an event")
        .event
}

/// The last event's body as JSON (`{"type": …, "data": …}`).
fn last_body(work: &WorkService) -> Value {
    serde_json::to_value(last_event(work).body).expect("json")
}

struct Hub {
    _dir: tempfile::TempDir,
    work: Arc<WorkService>,
    app: axum::Router,
}

fn hub() -> Hub {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    Hub {
        _dir: dir,
        work,
        app,
    }
}

impl Hub {
    async fn patch(&self, task: &str, body: Value) -> (u16, Value) {
        call(
            &self.app,
            Some(person(SAM)),
            "PATCH",
            &format!("/v1/tasks/{task}"),
            Some(body),
        )
        .await
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        call(&self.app, Some(person(SAM)), "POST", path, Some(body)).await
    }
}

// ─── POST /v1/projects ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn archive_and_restore_keep_the_task_and_emit_only_changes() {
    let hub = hub();
    let before = get(&hub.app, person(SAM), "/v1/tasks/PAP-1").await;
    let revision = rev(&hub.work);
    let archived = hub.patch("PAP-1", json!({ "archived": true })).await;
    expect(&archived, 200);
    assert_eq!(archived.1["archived"], true);
    assert_eq!(archived.1["subtasks"], before.1["subtasks"]);
    assert_eq!(
        last_body(&hub.work),
        json!({ "type": "task_updated", "data": {
        "task": PAP1, "patch": { "archived": true }
    } })
    );
    assert_eq!(rev(&hub.work), revision + 1);
    expect(&hub.patch("PAP-1", json!({ "archived": true })).await, 200);
    assert_eq!(rev(&hub.work), revision + 1, "archiving twice is a no-op");
    let refused = call(
        &hub.app,
        Some(agent(WRITER)),
        "PATCH",
        "/v1/tasks/PAP-1",
        Some(json!({ "archived": false })),
    )
    .await;
    expect(&refused, 403);
    let invalid = hub.patch("PAP-1", json!({ "archived": "yes" })).await;
    expect(&invalid, 400);
    assert_eq!(rev(&hub.work), revision + 1, "refusals append nothing");
    let restored = hub.patch("PAP-1", json!({ "archived": false })).await;
    expect(&restored, 200);
    assert_eq!(restored.1, before.1);
    assert_eq!(rev(&hub.work), revision + 2);
}

#[tokio::test]
async fn dispatch_reads_resolve_tasks_and_allow_both_token_scopes() {
    let hub = hub();
    let keyed = get(&hub.app, person(SAM), "/v1/tasks/PAP-1/dispatches").await;
    expect(&keyed, 200);
    let by_id = get(
        &hub.app,
        agent(WRITER),
        &format!("/v1/tasks/{PAP1}/dispatches"),
    )
    .await;
    expect(&by_id, 200);
    assert_eq!(by_id.1, keyed.1);
    assert!(
        keyed
            .1
            .as_array()
            .expect("dispatch list")
            .iter()
            .all(|run| run["task"] == PAP1)
    );
    let missing = get(&hub.app, person(SAM), "/v1/tasks/PAP-99999/dispatches").await;
    expect(&missing, 404);
}

#[tokio::test]
async fn a_project_is_created_with_the_defaults() {
    let hub = hub();
    let res = hub
        .post("/v1/projects", json!({ "key": "THS", "name": "Thesis" }))
        .await;
    expect(&res, 201);
    let id = res.1["id"].as_str().expect("id").to_owned();
    assert_eq!(id.len(), 26);
    assert_eq!(
        res.1,
        json!({
            "id": id, "key": "THS", "name": "Thesis", "status": "in_progress", "lead": SAM,
            "members": [SAM], "external": [],
        })
    );
    let fetched = get(&hub.app, person(SAM), &format!("/v1/projects/{id}")).await;
    assert_eq!(fetched.1, res.1);
    let event = last_event(&hub.work);
    assert_eq!(event.author, member(SAM));
    assert_eq!(event.on_behalf_of, None);
    assert_eq!(
        serde_json::to_value(event.body).expect("json"),
        json!({ "type": "project_created", "data": { "project": res.1 } })
    );
    // Its first task gets key THS-1.
    let task = hub
        .post(
            "/v1/tasks",
            json!({ "project": id, "title": "Outline chapter 1" }),
        )
        .await;
    expect(&task, 201);
    assert_eq!(task.1["key"], "THS-1");
}

#[tokio::test]
async fn a_project_keeps_every_field_given_and_its_lead_is_a_member() {
    let hub = hub();
    let res = hub
        .post(
            "/v1/projects",
            json!({
                "key": "AB12", "name": "Ablations", "lead": WRITER,
                "members": [RUNNER, RUNNER, SAM], "status": "planning",
                "start": "2026-10-01", "due": "2026-10-01",
                "root": { "machine": LAPTOP, "path": "/work/ablations", "branch": "main" },
            }),
        )
        .await;
    expect(&res, 201);
    assert_eq!(res.1["lead"], WRITER);
    assert_eq!(res.1["members"], json!([WRITER, RUNNER, SAM]));
    assert_eq!(res.1["status"], "planning");
    assert_eq!(res.1["start"], "2026-10-01");
    assert_eq!(res.1["due"], "2026-10-01");
    assert_eq!(
        res.1["root"],
        json!({ "machine": LAPTOP, "path": "/work/ablations", "branch": "main" })
    );
    // A lead already in the list keeps its place.
    let res = hub
        .post(
            "/v1/projects",
            json!({ "key": "AB13", "name": "More", "members": [RUNNER, SAM] }),
        )
        .await;
    expect(&res, 201);
    assert_eq!(res.1["members"], json!([RUNNER, SAM]));
}

#[tokio::test]
async fn a_project_key_in_use_is_a_conflict() {
    let hub = hub();
    let before = rev(&hub.work);
    let res = hub
        .post(
            "/v1/projects",
            json!({ "key": "PAP", "name": "Another paper" }),
        )
        .await;
    expect(&res, 409);
    assert_eq!(rev(&hub.work), before);
    let projects = get(&hub.app, person(SAM), "/v1/projects").await;
    assert_eq!(projects.1.as_array().map(Vec::len), Some(2));
    // A body that is also malformed is a 400 first.
    expect(
        &hub.post("/v1/projects", json!({ "key": "PAP", "name": " " }))
            .await,
        400,
    );
}

#[tokio::test]
async fn malformed_projects_are_refused_and_change_nothing() {
    let hub = hub();
    let before = rev(&hub.work);
    for body in [
        json!({ "name": "No key" }),
        json!({ "key": "pap", "name": "Lower case" }),
        json!({ "key": "P", "name": "Too short" }),
        json!({ "key": "1AB", "name": "Starts with a digit" }),
        json!({ "key": "TOOLONGKEY1", "name": "Eleven characters" }),
        json!({ "key": "A-B", "name": "Punctuation" }),
        json!({ "key": "NEW" }),
        json!({ "key": "NEW", "name": "   " }),
        json!({ "key": "NEW", "name": "x", "lead": UNKNOWN_MEMBER }),
        json!({ "key": "NEW", "name": "x", "members": [SAM, UNKNOWN_MEMBER] }),
        json!({ "key": "NEW", "name": "x", "status": "someday" }),
        json!({ "key": "NEW", "name": "x", "start": "2026-13-01" }),
        json!({ "key": "NEW", "name": "x", "start": "2026-11-02", "due": "2026-11-01" }),
        json!({ "key": "NEW", "name": "x", "root": { "machine": UNKNOWN_MACHINE, "path": "/w" } }),
        json!({ "key": "NEW", "name": "x", "root": { "machine": LAPTOP, "path": "" } }),
        json!(["NEW", "Positional"]),
    ] {
        let res = hub.post("/v1/projects", body.clone()).await;
        assert_eq!(res.0, 400, "{body}: {}", res.1);
        expect(&res, 400);
    }
    assert_eq!(rev(&hub.work), before);
    let agent_tries = call(
        &hub.app,
        Some(agent(WRITER)),
        "POST",
        "/v1/projects",
        Some(json!({ "key": "NEW", "name": "x" })),
    )
    .await;
    expect(&agent_tries, 403);
}

/// A racing writer's `project_created` with a key already held is not applied: the first project
/// keeps the key, through every rebuild, and the command that lost answers 409.
#[tokio::test]
async fn a_racing_writers_project_with_a_taken_key_is_not_applied() {
    let hub = hub();
    let paper = hub
        .work
        .project(&PAPER.parse().expect("id"))
        .expect("paper");
    let mut clash: Project = paper.clone();
    clash.id = ProjectId::new();
    clash.name = "Same key".into();
    let event = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: common::demo().workspace.id,
        author: member(SAM),
        on_behalf_of: None,
        body: EventBody::ProjectCreated {
            project: clash.clone(),
        },
    };
    hub.work.store().append(&[event]).expect("append");
    assert!(hub.work.project(&clash.id).is_err(), "not applied");
    assert_eq!(
        hub.work
            .read(|c| pitcrew_hub_work::query::project_with_key(
                c,
                &ProjectKey::new("PAP").expect("key")
            ))
            .expect("read")
            .map(|p| p.id),
        Some(paper.id)
    );
    // A re-stated project may not take another's key either.
    let mut tooling = hub
        .work
        .project(&TOOLING.parse().expect("id"))
        .expect("tooling");
    tooling.key = ProjectKey::new("PAP").expect("key");
    let event = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: common::demo().workspace.id,
        author: member(SAM),
        on_behalf_of: None,
        body: EventBody::ProjectCreated { project: tooling },
    };
    hub.work.store().append(&[event]).expect("append");
    let tooling = hub
        .work
        .project(&TOOLING.parse().expect("id"))
        .expect("tooling");
    assert_eq!(tooling.key.as_str(), "TL");
    for name in pitcrew_hub_work::projection::NAMES {
        hub.work.store().rebuild(name).expect("rebuild");
    }
    assert!(
        hub.work.project(&clash.id).is_err(),
        "not applied on rebuild"
    );
    assert_eq!(
        hub.work
            .project(&TOOLING.parse().expect("id"))
            .expect("tooling")
            .key
            .as_str(),
        "TL"
    );
}

// ─── POST /v1/workstreams ────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_workstream_is_active_and_on_track_by_default() {
    let hub = hub();
    let res = hub
        .post(
            "/v1/workstreams",
            json!({ "project": PAPER, "name": "Figures" }),
        )
        .await;
    expect(&res, 201);
    let id = res.1["id"].as_str().expect("id").to_owned();
    assert_eq!(
        res.1,
        json!({
            "id": id, "project": PAPER, "name": "Figures", "status": "active",
            "health": "on_track", "locations": [], "external": [],
        })
    );
    let list = get(
        &hub.app,
        person(SAM),
        &format!("/v1/workstreams?project={PAPER}"),
    )
    .await;
    assert!(
        list.1
            .as_array()
            .expect("list")
            .iter()
            .any(|w| w["id"] == json!(id))
    );
    assert_eq!(
        last_body(&hub.work),
        json!({ "type": "workstream_created", "data": { "workstream": res.1 } })
    );
    assert_eq!(last_event(&hub.work).author, member(SAM));
    // Tasks can be moved into it.
    let moved = hub.patch("PAP-1", json!({ "workstream": id })).await;
    expect(&moved, 200);
    assert_eq!(moved.1["workstream"], json!(id));
}

#[tokio::test]
async fn a_workstream_keeps_a_given_status_and_locations() {
    let hub = hub();
    let locations = json!([{ "machine": LAPTOP, "path": "/work/paper/figures" }]);
    let res = hub
        .post(
            "/v1/workstreams",
            json!({ "project": TOOLING, "name": "Packaging", "status": "idea",
                    "locations": locations }),
        )
        .await;
    expect(&res, 201);
    assert_eq!(res.1["project"], TOOLING);
    assert_eq!(res.1["status"], "idea");
    assert_eq!(res.1["health"], "on_track");
    assert_eq!(res.1["locations"], locations);
}

#[tokio::test]
async fn a_workstream_of_an_unknown_project_is_not_found_and_malformed_ones_are_refused() {
    let hub = hub();
    let before = rev(&hub.work);
    expect(
        &hub.post(
            "/v1/workstreams",
            json!({ "project": UNKNOWN_PROJECT, "name": "Lost" }),
        )
        .await,
        404,
    );
    for body in [
        json!({ "name": "No project" }),
        json!({ "project": PAPER }),
        json!({ "project": PAPER, "name": "" }),
        json!({ "project": PAPER, "name": "x", "status": "finished" }),
        json!({ "project": PAPER, "name": "x", "health": "on_track", "status": "on_track" }),
        json!({ "project": PAPER, "name": "x",
                "locations": [{ "machine": UNKNOWN_MACHINE, "path": "/work" }] }),
        json!({ "project": PAPER, "name": "x", "locations": [{ "machine": LAPTOP, "path": " " }] }),
        json!({ "project": PAPER, "name": "x", "locations": { "machine": LAPTOP, "path": "/w" } }),
        // A malformed body is a 400 before the unknown project's 404.
        json!({ "project": UNKNOWN_PROJECT, "name": " " }),
        json!([PAPER, "Positional"]),
    ] {
        let res = hub.post("/v1/workstreams", body.clone()).await;
        assert_eq!(res.0, 400, "{body}: {}", res.1);
        expect(&res, 400);
    }
    assert_eq!(rev(&hub.work), before);
    let agent_tries = call(
        &hub.app,
        Some(agent(WRITER)),
        "POST",
        "/v1/workstreams",
        Some(json!({ "project": PAPER, "name": "x" })),
    )
    .await;
    expect(&agent_tries, 403);
}

// ─── PATCH /v1/tasks/{id-or-key} ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_patch_emits_only_the_fields_that_changed() {
    let hub = hub();
    let res = hub
        .patch(
            "PAP-2",
            json!({
                "title": "  Make figure 3  ",
                "description": "Mean and spread over the four good seeds.",
                "priority": "medium",
                "labels": [" figures ", "paper", "figures"],
                "start": "2026-10-05",
                "accept_auto": false,
                "unknown_field": "ignored",
                "status": "done",
                "assignee": SAM,
            }),
        )
        .await;
    expect(&res, 200);
    assert_eq!(res.1["title"], "Make figure 3");
    assert_eq!(
        res.1["description"],
        "Mean and spread over the four good seeds."
    );
    assert_eq!(res.1["labels"], json!(["figures", "paper"]));
    assert_eq!(res.1["start"], "2026-10-05");
    assert_eq!(res.1["status"], "todo", "status has its own route");
    assert_eq!(res.1["assignee"], WRITER, "so has the assignee");
    let event = last_event(&hub.work);
    assert_eq!(event.author, member(SAM));
    // Priority and accept_auto were already medium and false.
    assert_eq!(
        serde_json::to_value(event.body).expect("json"),
        json!({
            "type": "task_updated",
            "data": {
                "task": PAP2,
                "patch": {
                    "title": "Make figure 3",
                    "description": "Mean and spread over the four good seeds.",
                    "labels": ["figures", "paper"],
                    "start": "2026-10-05",
                },
            },
        })
    );
    let fetched = get(&hub.app, person(SAM), &format!("/v1/tasks/{PAP2}")).await;
    assert_eq!(fetched.1, res.1);
    // The list sends the same stored document.
    let list = get(&hub.app, person(SAM), "/v1/tasks?status=todo").await;
    assert!(list.1.as_array().expect("list").contains(&res.1));
}

#[tokio::test]
async fn a_task_is_patched_by_key_id_or_prefixed_id() {
    let hub = hub();
    for (i, task) in [
        "PAP-6",
        "01JB000000000000000TSK0006",
        "tsk_01JB000000000000000TSK0006",
    ]
    .iter()
    .enumerate()
    {
        let res = hub
            .patch(
                task,
                json!({ "title": format!("Aggregate the results, take {i}") }),
            )
            .await;
        expect(&res, 200);
        assert_eq!(res.1["key"], "PAP-6");
    }
}

#[tokio::test]
async fn a_patch_that_changes_nothing_appends_nothing() {
    let hub = hub();
    let before = rev(&hub.work);
    let empty = hub.patch("PAP-1", json!({})).await;
    expect(&empty, 200);
    assert_eq!(empty.1["title"], "Draft the method section");
    let same = hub
        .patch(
            "PAP-1",
            json!({
                "title": "Draft the method section ",
                "priority": "high",
                "labels": ["writing"],
                "due": "2026-10-10",
                "workstream": SUBMISSION,
                "blocked_by": [],
                "accept_auto": false,
                "title_typo": "x",
            }),
        )
        .await;
    expect(&same, 200);
    assert_eq!(same.1, empty.1);
    // On the plain fields, null is the same as leaving the field out.
    expect(
        &hub.patch(
            "PAP-1",
            json!({ "title": null, "labels": null, "priority": null }),
        )
        .await,
        200,
    );
    assert_eq!(rev(&hub.work), before);
}

#[tokio::test]
async fn null_clears_workstream_start_and_due() {
    let hub = hub();
    expect(
        &hub.patch("PAP-1", json!({ "start": "2026-10-01" })).await,
        200,
    );
    let res = hub
        .patch(
            "PAP-1",
            json!({ "workstream": null, "start": null, "due": null }),
        )
        .await;
    expect(&res, 200);
    for field in ["workstream", "start", "due"] {
        assert!(res.1.get(field).is_none(), "{field}");
    }
    assert_eq!(
        last_body(&hub.work),
        json!({
            "type": "task_updated",
            "data": { "task": PAP1, "patch": { "workstream": null, "start": null, "due": null } },
        })
    );
    // The filter columns follow: PAP-1 has left the workstream's list.
    let in_submission = get(
        &hub.app,
        person(SAM),
        &format!("/v1/tasks?workstream={SUBMISSION}"),
    )
    .await;
    assert!(
        !in_submission
            .1
            .as_array()
            .expect("list")
            .iter()
            .any(|t| t["key"] == "PAP-1")
    );
    // Clearing what is already clear changes nothing.
    let before = rev(&hub.work);
    expect(
        &hub.patch("PAP-1", json!({ "workstream": null, "due": null }))
            .await,
        200,
    );
    assert_eq!(rev(&hub.work), before);
}

#[tokio::test]
async fn a_task_moves_only_to_a_workstream_of_its_project() {
    let hub = hub();
    let moved = hub.patch("PAP-1", json!({ "workstream": SEED_RUNS })).await;
    expect(&moved, 200);
    assert_eq!(moved.1["workstream"], SEED_RUNS);
    let in_seeds = get(
        &hub.app,
        person(SAM),
        &format!("/v1/tasks?workstream={SEED_RUNS}"),
    )
    .await;
    assert!(
        in_seeds
            .1
            .as_array()
            .expect("list")
            .iter()
            .any(|t| t["key"] == "PAP-1")
    );
    expect(
        &hub.patch("PAP-1", json!({ "workstream": PARSERS })).await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "workstream": UNKNOWN_WORKSTREAM }))
            .await,
        400,
    );
}

#[tokio::test]
async fn titles_are_1_to_500_characters_after_trimming() {
    let hub = hub();
    expect(&hub.patch("PAP-1", json!({ "title": "   " })).await, 400);
    expect(
        &hub.patch("PAP-1", json!({ "title": "x".repeat(501) }))
            .await,
        400,
    );
    expect(&hub.patch("PAP-1", json!({ "title": 42 })).await, 400);
    let res = hub
        .patch(
            "PAP-1",
            json!({ "title": format!(" {} ", "x".repeat(500)) }),
        )
        .await;
    expect(&res, 200);
    assert_eq!(res.1["title"], json!("x".repeat(500)));
    // Characters are code points: 500 test-tube emoji are 2,000 bytes.
    expect(
        &hub.patch("PAP-1", json!({ "title": "\u{1F9EA}".repeat(500) }))
            .await,
        200,
    );
    expect(
        &hub.patch("PAP-1", json!({ "title": "\u{1F9EA}".repeat(501) }))
            .await,
        400,
    );
}

#[tokio::test]
async fn labels_are_trimmed_deduplicated_and_bounded() {
    let hub = hub();
    let many = |n: usize| -> Vec<String> { (0..n).map(|i| format!("label-{i}")).collect() };
    let mut labels = many(32);
    labels.push(" label-0 ".into());
    labels.push("label-31".into());
    let ok = hub.patch("PAP-1", json!({ "labels": labels })).await;
    expect(&ok, 200);
    assert_eq!(ok.1["labels"], json!(many(32)));
    expect(
        &hub.patch("PAP-1", json!({ "labels": many(33) })).await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "labels": ["ok", "  "] })).await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "labels": ["y".repeat(65)] }))
            .await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "labels": "writing" })).await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "labels": ["z".repeat(64)] }))
            .await,
        200,
    );
    let cleared = hub.patch("PAP-1", json!({ "labels": [] })).await;
    expect(&cleared, 200);
    assert_eq!(cleared.1["labels"], json!([]));
}

#[tokio::test]
async fn blockers_exist_are_not_the_task_and_close_no_cycle() {
    let hub = hub();
    expect(
        &hub.patch("PAP-1", json!({ "blocked_by": [UNKNOWN_TASK] }))
            .await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "blocked_by": ["not-an-id"] }))
            .await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "blocked_by": [PAP1] })).await,
        400,
    );
    // PAP-2 already waits on PAP-4.
    expect(
        &hub.patch("PAP-4", json!({ "blocked_by": [PAP2] })).await,
        409,
    );
    let ok = hub
        .patch("PAP-4", json!({ "blocked_by": [PAP1, PAP1] }))
        .await;
    expect(&ok, 200);
    assert_eq!(ok.1["blocked_by"], json!([PAP1]), "duplicates dropped");
    // Now PAP-2 → PAP-4 → PAP-1, so PAP-1 may not wait on PAP-2.
    expect(
        &hub.patch("PAP-1", json!({ "blocked_by": [PAP2] })).await,
        409,
    );
    // A malformed field in the same patch is a 400 first.
    expect(
        &hub.patch(
            "PAP-1",
            json!({ "blocked_by": [PAP2], "priority": "critical" }),
        )
        .await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "blocked_by": [PAP8] })).await,
        200,
    );
    assert_eq!(
        last_body(&hub.work),
        json!({ "type": "task_updated", "data": { "task": PAP1, "patch": { "blocked_by": [PAP8] } } })
    );
    // The dependency rows follow the document.
    let blockers: Vec<String> = hub
        .work
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT blocked_by FROM work_task_deps WHERE task = ?1 ORDER BY position",
            )?;
            let rows = stmt.query_map([PAP1], |r| r.get(0))?;
            Ok(rows.collect::<Result<Vec<String>, _>>()?)
        })
        .expect("deps");
    assert_eq!(blockers, [PAP8]);
}

#[tokio::test]
async fn dates_are_well_formed_and_start_is_not_after_due() {
    let hub = hub();
    expect(
        &hub.patch("PAP-1", json!({ "start": "2026-10-32" })).await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "due": "10/10/2026" })).await,
        400,
    );
    expect(
        &hub.patch(
            "PAP-1",
            json!({ "start": "2026-10-02", "due": "2026-10-01" }),
        )
        .await,
        400,
    );
    // PAP-1 is due 2026-10-10.
    expect(
        &hub.patch("PAP-1", json!({ "start": "2026-10-11" })).await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "start": "2026-10-10" })).await,
        200,
    );
    expect(
        &hub.patch("PAP-1", json!({ "due": "2026-10-09" })).await,
        400,
    );
    expect(
        &hub.patch("PAP-1", json!({ "due": null, "start": "2026-12-01" }))
            .await,
        200,
    );
}

#[tokio::test]
async fn malformed_patches_change_nothing() {
    let hub = hub();
    let before = rev(&hub.work);
    for body in [
        json!({ "priority": "critical" }),
        json!({ "accept_auto": "yes" }),
        json!({ "description": 7 }),
        json!({ "workstream": 12 }),
        json!({ "blocked_by": "PAP-2" }),
        // A valid title does not land when another field is wrong.
        json!({ "title": "A new title", "labels": [""] }),
        json!([]),
        json!("title"),
    ] {
        let res = hub.patch("PAP-1", body.clone()).await;
        assert_eq!(res.0, 400, "{body}: {}", res.1);
        expect(&res, 400);
    }
    assert_eq!(rev(&hub.work), before);
    let task = get(&hub.app, person(SAM), "/v1/tasks/PAP-1").await;
    assert_eq!(task.1["title"], "Draft the method section");
}

#[tokio::test]
async fn patching_an_unknown_task_is_not_found_and_agents_are_forbidden() {
    let hub = hub();
    expect(&hub.patch("PAP-99", json!({ "title": "x" })).await, 404);
    expect(&hub.patch(UNKNOWN_TASK, json!({ "title": "x" })).await, 404);
    // 404 for the path before 400 for the body.
    expect(&hub.patch("PAP-99", json!({ "title": 42 })).await, 404);
    // PAP-1 is @writer's own task; editing it still needs a person.
    let res = call(
        &hub.app,
        Some(agent(WRITER)),
        "PATCH",
        "/v1/tasks/PAP-1",
        Some(json!({ "title": "x" })),
    )
    .await;
    expect(&res, 403);
    // The command refuses agents too, whatever mounts it.
    let err = hub
        .work
        .patch_task(
            &agent(WRITER),
            &TaskRef::parse("PAP-1").expect("key"),
            TaskPatch {
                title: Some("x".into()),
                ..TaskPatch::default()
            },
        )
        .expect_err("agent");
    assert_eq!(err.code(), pitcrew_protocol::api::ErrorCode::Forbidden);
}

#[tokio::test]
async fn activity_finds_task_updated_by_task_and_workstream() {
    let hub = hub();
    expect(&hub.patch(PAP4, json!({ "priority": "urgent" })).await, 200);
    let rev = rev(&hub.work);
    let task: pitcrew_protocol::ids::TaskId = PAP4.parse().expect("task");
    let refs = pitcrew_hub_work::RefFilter {
        task: Some(task),
        ..Default::default()
    };
    let (revs, _) =
        pitcrew_hub_work::EventRefs::revs_matching(&*hub.work, &refs, rev + 1, 1).expect("refs");
    assert_eq!(revs, [rev]);
    // Moved to another workstream, later events about the task count there.
    expect(
        &hub.patch(PAP4, json!({ "workstream": SUBMISSION })).await,
        200,
    );
    let moved_at = rev + 1;
    let refs = pitcrew_hub_work::RefFilter {
        workstream: Some(SUBMISSION.parse().expect("ws")),
        ..Default::default()
    };
    let (revs, _) = pitcrew_hub_work::EventRefs::revs_matching(&*hub.work, &refs, moved_at + 1, 1)
        .expect("refs");
    assert_eq!(revs, [moved_at]);
}

#[test]
fn project_statuses_default_in_the_command_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let project = work
        .create_project(
            &person(SAM),
            pitcrew_hub_work::NewProject {
                key: ProjectKey::new("CMD").expect("key"),
                name: "Through the command".into(),
                lead: None,
                members: None,
                status: None,
                start: None,
                due: None,
                root: None,
            },
        )
        .expect("project");
    assert_eq!(project.status, ProjectStatus::InProgress);
    assert_eq!(project.members, [member(SAM)]);
    let err = work
        .create_project(
            &agent(WRITER),
            pitcrew_hub_work::NewProject {
                key: ProjectKey::new("AGT").expect("key"),
                name: "By an agent".into(),
                lead: None,
                members: None,
                status: None,
                start: None,
                due: None,
                root: None,
            },
        )
        .expect_err("agent");
    assert_eq!(err.code(), pitcrew_protocol::api::ErrorCode::Forbidden);
}

/// Sends a raw body (not a `serde_json::Value`, which cannot hold a repeated key).
async fn raw(app: &axum::Router, method: &str, path: &str, body: String) -> (u16, Value) {
    let mut request = axum::extract::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .expect("request");
    request.extensions_mut().insert(person(SAM));
    let response = app.clone().oneshot(request).await.expect("infallible");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("JSON"))
}

/// A key given twice counts once, with its last value, as `JSON.parse` does in the mock hub.
#[tokio::test]
async fn a_repeated_key_takes_its_last_value() {
    let hub = hub();
    let res = raw(
        &hub.app,
        "PATCH",
        "/v1/tasks/PAP-1",
        r#"{"title":"First","title":"Second","labels":["a"],"labels":["b","b"]}"#.to_owned(),
    )
    .await;
    expect(&res, 200);
    assert_eq!(res.1["title"], "Second");
    assert_eq!(res.1["labels"], json!(["b"]));
}

/// Lists as long as a 1 MiB body allows are checked in time linear in their length: every label
/// repeated, every label distinct (refused at the 33rd), every blocker the same task, unknown
/// blockers, and as many project members.
#[tokio::test]
async fn long_lists_are_checked_without_quadratic_work() {
    let hub = hub();
    let budget = Duration::from_secs(5);
    let timed = |started: Instant, what: &str| {
        let took = started.elapsed();
        assert!(took < budget, "{what} took {took:?}");
    };

    let started = Instant::now();
    let same = vec!["writing"; 90_000];
    let res = hub.patch("PAP-2", json!({ "labels": same })).await;
    expect(&res, 200);
    assert_eq!(res.1["labels"], json!(["writing"]));
    timed(started, "90,000 repeated labels");

    let started = Instant::now();
    let distinct: Vec<String> = (0..80_000).map(|i| format!("l{i}")).collect();
    expect(
        &hub.patch("PAP-2", json!({ "labels": distinct })).await,
        400,
    );
    timed(started, "80,000 distinct labels");

    let started = Instant::now();
    let blockers = vec![PAP8; 30_000];
    let res = hub.patch("PAP-2", json!({ "blocked_by": blockers })).await;
    expect(&res, 200);
    assert_eq!(res.1["blocked_by"], json!([PAP8]));
    timed(started, "30,000 repeated blockers");

    // Every distinct id is looked up in one query: 20,000 unknown ones are a 400.
    let started = Instant::now();
    let unknown: Vec<String> = (0..20_000_u32)
        .map(|i| format!("01JB000000000000000T{i:06}"))
        .collect();
    expect(
        &hub.patch("PAP-2", json!({ "blocked_by": unknown })).await,
        400,
    );
    timed(started, "20,000 unknown blockers");

    let started = Instant::now();
    let members = vec![WRITER; 30_000];
    let res = hub
        .post(
            "/v1/projects",
            json!({ "key": "BIG", "name": "Many members", "members": members }),
        )
        .await;
    expect(&res, 201);
    assert_eq!(res.1["members"], json!([SAM, WRITER]));
    timed(started, "30,000 repeated members");
}
