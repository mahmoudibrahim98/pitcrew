//! `POST /v1/tasks/{id}/dispatch`: what is recorded, where the session runs, the refusals, and
//! what happens when the runner link cannot start the session. A test double stands in for the
//! runner link (stream D).

mod common;

use common::{
    PAPER, RUNNER, SAM, SEED_RUNS, WRITER, agent, app, call, demo, expect, member, open, person,
};
use pitcrew_hub_work::{
    DispatchError, DispatchRequest, Dispatcher, INTERNAL_MESSAGE, TaskRef, WorkService,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, ProjectId, ProjectKey};
use pitcrew_protocol::model::{
    DispatchOutcome, Engine, LinkBasis, PermissionMode, Project, ProjectStatus, SessionState,
    TaskStatus,
};
use pitcrew_protocol::runner::RunnerCommand;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

const LAPTOP: &str = "01JB000000000000000MCH0001";
const CLUSTER: &str = "01JB000000000000000MCH0002";
const GPU_BOX: &str = "01JB000000000000000MCH0003";
const REVIEWER: &str = "01JB000000000000000MEM0004";

/// What the runner link does when asked to start a session.
#[derive(Debug, Clone)]
enum Answer {
    Start,
    Fail(DispatchError),
    Panic,
}

/// Records every request, and answers as told.
#[derive(Debug)]
struct Recorder {
    calls: Mutex<Vec<DispatchRequest>>,
    answer: Answer,
}

impl Recorder {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            answer,
        })
    }

    fn calls(&self) -> Vec<DispatchRequest> {
        self.calls.lock().expect("calls").clone()
    }
}

impl Dispatcher for Recorder {
    fn start(&self, request: &DispatchRequest) -> Result<(), DispatchError> {
        self.calls.lock().expect("calls").push(request.clone());
        match &self.answer {
            Answer::Start => Ok(()),
            Answer::Fail(e) => Err(e.clone()),
            Answer::Panic => panic!("the runner link fell over"),
        }
    }
}

fn service(dir: &Path, dispatcher: Option<Arc<Recorder>>) -> Arc<WorkService> {
    let demo = demo();
    let mut work = WorkService::new(open(&dir.join("hub.db")), demo.workspace.clone());
    if let Some(d) = dispatcher {
        work = work.with_dispatcher(d);
    }
    let work = Arc::new(work);
    work.seed(&demo).expect("seed");
    work
}

fn events_after(work: &WorkService, rev: u64) -> Vec<Event> {
    work.store()
        .since(rev, usize::MAX)
        .expect("log")
        .into_iter()
        .map(|e| e.event)
        .collect()
}

fn types(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| pitcrew_store::event_type(&e.body).expect("type"))
        .collect()
}

async fn dispatch(work: &Arc<WorkService>, key: &str, body: Value) -> (u16, Value) {
    call(
        &app(work),
        Some(person(SAM)),
        "POST",
        &format!("/v1/tasks/{key}/dispatch"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn a_dispatch_records_the_assignment_the_dispatch_and_its_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let rev = work.store().latest_rev().expect("rev");

    // PAP-5 (Seed runs, no assignee) to @runner, whose persona runs Codex.
    let res = dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await;
    expect(&res, 202);
    let pap5 = work
        .task(&TaskRef::parse("PAP-5").expect("key"))
        .expect("task");
    assert_eq!(res.1["task"], json!(pap5.id.0.to_string()));
    assert_eq!(res.1["agent"], RUNNER);
    assert_eq!(
        res.1["brief"],
        json!(pap5.description),
        "the description by default"
    );
    assert!(res.1.get("ended").is_none());
    let session_id = res.1["session"].as_str().expect("session").to_owned();

    // task_assigned (it had no assignee), dispatch_started, session_discovered: one append, by
    // the person who dispatched.
    let events = events_after(&work, rev);
    assert_eq!(
        types(&events),
        ["task_assigned", "dispatch_started", "session_discovered"]
    );
    assert!(
        events
            .iter()
            .all(|e| e.author == member(SAM) && e.on_behalf_of.is_none())
    );
    assert_eq!(pap5.assignee, Some(member(RUNNER)));

    // The session is recorded as starting, linked by the dispatch.
    let session = work
        .session(&session_id.parse().expect("id"))
        .expect("session");
    assert_eq!(session.state, SessionState::Starting);
    assert_eq!(session.link_basis, Some(LinkBasis::Dispatch));
    assert_eq!(session.task, Some(pap5.id));
    assert_eq!(session.workstream, Some(SEED_RUNS.parse().expect("ws")));
    assert_eq!(session.agent, Some(member(RUNNER)));
    assert_eq!(session.engine, Engine::Codex);
    assert_eq!(session.machine, CLUSTER.parse().expect("machine"));
    assert_eq!(session.cwd, "/scratch/sam/diffusion-runs");

    // The runner link was asked to start exactly that.
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    let request = &calls[0];
    assert_eq!(request.session, session.id);
    assert_eq!(
        request.dispatch.0.to_string(),
        res.1["id"].as_str().expect("id")
    );
    assert_eq!(request.task, pap5.id);
    assert_eq!(request.key.to_string(), "PAP-5");
    assert_eq!(request.agent, member(RUNNER));
    assert_eq!(request.owner, Some(member(SAM)));
    assert_eq!(request.machine, CLUSTER.parse().expect("machine"));
    assert_eq!(request.cwd, "/scratch/sam/diffusion-runs");
    assert_eq!(request.engine, Engine::Codex);
    assert_eq!(request.permission_mode, PermissionMode::Default);
    assert_eq!(request.name, format!("PAP-5 {}", pap5.title));
    match request.start_command() {
        RunnerCommand::StartSession {
            engine, cwd, brief, ..
        } => {
            assert_eq!(engine, Engine::Codex);
            assert_eq!(cwd, "/scratch/sam/diffusion-runs");
            assert_eq!(brief.as_deref(), Some(pap5.description.as_str()));
        }
        other => panic!("{other:?}"),
    }

    // The dispatch makes the task the agent's own, and when the session starts working the task
    // moves to in progress.
    let dispatch_id = request.dispatch;
    let moved = work.dispatch_working(&dispatch_id).expect("working");
    assert_eq!(moved.status, TaskStatus::InProgress);
}

#[tokio::test]
async fn an_assigned_task_keeps_its_assignee_and_the_brief_can_be_given() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let rev = work.store().latest_rev().expect("rev");
    // PAP-2 (Submission, @writer's) to @reviewer, on the laptop where Submission's folder is.
    let res = dispatch(
        &work,
        "PAP-2",
        json!({ "agent": REVIEWER, "brief": "Check the related work." }),
    )
    .await;
    expect(&res, 202);
    assert_eq!(res.1["brief"], "Check the related work.");
    assert_eq!(
        types(&events_after(&work, rev)),
        ["dispatch_started", "session_discovered"]
    );
    let pap2 = work
        .task(&TaskRef::parse("PAP-2").expect("key"))
        .expect("task");
    assert_eq!(pap2.assignee, Some(member(WRITER)));
    let calls = runner.calls();
    let request = &calls[0];
    assert_eq!(request.machine, LAPTOP.parse().expect("machine"));
    assert_eq!(request.cwd, "/home/sam/work/diffusion-paper/paper");
    assert_eq!(request.engine, Engine::Claude);
    assert_eq!(request.permission_mode, PermissionMode::Plan);
}

#[tokio::test]
async fn where_a_dispatch_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let place = |i: usize| {
        let calls = runner.calls();
        let r = &calls[i];
        (r.machine.0.to_string(), r.cwd.clone(), r.branch.clone())
    };
    // A named machine with a folder of the task's there: that folder (and its branch).
    expect(
        &dispatch(&work, "TL-2", json!({ "agent": RUNNER, "machine": LAPTOP })).await,
        202,
    );
    assert_eq!(
        place(0),
        (
            LAPTOP.into(),
            "/home/sam/work/lab-tools".into(),
            Some("parsers".into())
        )
    );
    // A named machine with the project's root but not the workstream's folder: the root.
    expect(
        &dispatch(
            &work,
            "PAP-6",
            json!({ "agent": RUNNER, "machine": LAPTOP }),
        )
        .await,
        202,
    );
    assert_eq!(
        place(1),
        (LAPTOP.into(), "/home/sam/work/diffusion-paper".into(), None)
    );
    // A named machine with neither: its home.
    expect(
        &dispatch(
            &work,
            "PAP-2",
            json!({ "agent": RUNNER, "machine": CLUSTER }),
        )
        .await,
        202,
    );
    assert_eq!(place(2), (CLUSTER.into(), "~".into(), None));
    // No workstream folder and no machine named: the project's root.
    let ablation = work
        .create_task(
            &person(SAM),
            pitcrew_hub_work::NewTask {
                project: PAPER.parse().expect("project"),
                workstream: Some("01JB000000000000000WST0004".parse().expect("ws")),
                title: "Try a cosine schedule".into(),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                labels: None,
                due: None,
            },
        )
        .expect("create");
    let res = dispatch(&work, &ablation.key.to_string(), json!({ "agent": WRITER })).await;
    expect(&res, 202);
    assert_eq!(
        res.1["brief"], "Try a cosine schedule",
        "the title without a description"
    );
    assert_eq!(
        place(3),
        (LAPTOP.into(), "/home/sam/work/diffusion-paper".into(), None)
    );
}

#[tokio::test]
async fn without_any_folder_a_dispatch_runs_on_the_hubs_machine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let demo = demo();
    let store = open(&dir.path().join("hub.db"));
    let work = Arc::new(
        WorkService::new(Arc::clone(&store), demo.workspace.clone())
            .with_dispatcher(Arc::clone(&runner) as Arc<dyn Dispatcher>)
            .with_hub_machine(CLUSTER.parse().expect("machine")),
    );
    work.seed(&demo).expect("seed");
    // A project with no root, and a task with no workstream.
    let bare = Project {
        id: ProjectId::new(),
        key: ProjectKey::new("BARE").expect("key"),
        name: "Bare".into(),
        status: ProjectStatus::Planning,
        lead: member(SAM),
        members: Vec::new(),
        start: None,
        due: None,
        root: None,
        external: Vec::new(),
    };
    store
        .append(&[Event {
            id: EventId::new(),
            at: 1_790_800_000_000,
            workspace: demo.workspace.id,
            author: member(SAM),
            on_behalf_of: None,
            body: EventBody::ProjectCreated {
                project: bare.clone(),
            },
        }])
        .expect("append");
    let task = work
        .create_task(
            &person(SAM),
            pitcrew_hub_work::NewTask {
                project: bare.id,
                workstream: None,
                title: "Anywhere".into(),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                labels: None,
                due: None,
            },
        )
        .expect("create");
    expect(
        &dispatch(&work, "BARE-1", json!({ "agent": RUNNER })).await,
        202,
    );
    let calls = runner.calls();
    let r = &calls[0];
    assert_eq!(r.task, task.id);
    assert_eq!(
        (r.machine, r.cwd.as_str()),
        (CLUSTER.parse().expect("m"), "~")
    );

    // Without a configured machine, the first local one.
    let second = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let other = service(second.path(), Some(Arc::clone(&runner)));
    other
        .store()
        .append(&[Event {
            id: EventId::new(),
            at: 1_790_800_000_000,
            workspace: demo.workspace.id,
            author: member(SAM),
            on_behalf_of: None,
            body: EventBody::ProjectCreated { project: bare },
        }])
        .expect("append");
    other
        .create_task(
            &person(SAM),
            pitcrew_hub_work::NewTask {
                project: task.project,
                workstream: None,
                title: "Anywhere".into(),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                labels: None,
                due: None,
            },
        )
        .expect("create");
    expect(
        &dispatch(&other, "BARE-1", json!({ "agent": RUNNER })).await,
        202,
    );
    assert_eq!(runner.calls()[0].machine, LAPTOP.parse().expect("m"));
}

#[tokio::test]
async fn refused_dispatches_record_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let rev = work.store().latest_rev().expect("rev");
    for (key, body, status) in [
        // Unknown task.
        ("PAP-99", json!({ "agent": RUNNER }), 404),
        ("garbage", json!({ "agent": RUNNER }), 404),
        // Unknown agent, a person as the agent, an unknown machine, a malformed body.
        (
            "PAP-5",
            json!({ "agent": "01JB000000000000000MEM0099" }),
            400,
        ),
        ("PAP-5", json!({ "agent": SAM }), 400),
        (
            "PAP-5",
            json!({ "agent": RUNNER, "machine": "01JB000000000000000MCH0099" }),
            400,
        ),
        ("PAP-5", json!({ "brief": "No agent" }), 400),
        // A done task.
        ("PAP-7", json!({ "agent": RUNNER }), 409),
        // A machine whose runner cannot be reached.
        ("PAP-5", json!({ "agent": RUNNER, "machine": GPU_BOX }), 503),
    ] {
        expect(&dispatch(&work, key, body.clone()).await, status);
        assert_eq!(
            work.store().latest_rev().expect("rev"),
            rev,
            "{key} {body}: nothing appended"
        );
    }
    // A canceled task too.
    work.move_task(
        &person(SAM),
        &TaskRef::parse("PAP-6").expect("key"),
        TaskStatus::Canceled,
    )
    .expect("cancel");
    let rev = work.store().latest_rev().expect("rev");
    expect(
        &dispatch(&work, "PAP-6", json!({ "agent": RUNNER })).await,
        409,
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    assert!(runner.calls().is_empty(), "the runner link was never asked");

    // Agents may not dispatch.
    let app = app(&work);
    expect(
        &call(
            &app,
            Some(agent(WRITER)),
            "POST",
            "/v1/tasks/PAP-5/dispatch",
            Some(json!({ "agent": RUNNER })),
        )
        .await,
        403,
    );
}

#[tokio::test]
async fn without_a_runner_link_dispatching_is_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), None);
    let rev = work.store().latest_rev().expect("rev");
    expect(
        &dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await,
        503,
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
}

#[tokio::test]
async fn a_failed_start_finishes_the_dispatch_and_ends_the_session() {
    for (answer, status) in [
        (
            Answer::Fail(DispatchError::Unavailable("connection refused".into())),
            503,
        ),
        (
            Answer::Fail(DispatchError::Rejected("bypass is not allowed".into())),
            409,
        ),
        (
            Answer::Fail(DispatchError::Failed("codex: command not found".into())),
            500,
        ),
        (Answer::Panic, 500),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let runner = Recorder::new(answer.clone());
        let work = service(dir.path(), Some(Arc::clone(&runner)));
        let rev = work.store().latest_rev().expect("rev");
        let res = dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await;
        assert_eq!(res.0, status, "{answer:?}: {}", res.1);
        if status == 500 {
            assert_eq!(res.1["message"], INTERNAL_MESSAGE, "no detail for clients");
        }
        let events = events_after(&work, rev);
        assert_eq!(
            types(&events),
            [
                "task_assigned",
                "dispatch_started",
                "session_discovered",
                "dispatch_finished",
                "session_ended"
            ],
            "{answer:?}"
        );
        // Nothing is left dangling: the dispatch failed, with the reason, and the session ended.
        let dispatches = work.dispatches().expect("dispatches");
        let failed = dispatches.last().expect("the dispatch");
        assert_eq!(failed.outcome, Some(DispatchOutcome::Failed));
        assert!(failed.ended.is_some());
        let summary = failed.summary.as_deref().expect("summary");
        match &answer {
            Answer::Fail(e) => assert!(summary.contains(&e.to_string()), "{summary}"),
            _ => assert!(summary.contains("panicked"), "{summary}"),
        }
        let session = work
            .session(&failed.session.expect("session"))
            .expect("session");
        assert_eq!(session.state, SessionState::Ended);
        // The task is no longer the agent's through the dispatch, but stays assigned.
        let pap5 = work
            .task(&TaskRef::parse("PAP-5").expect("key"))
            .expect("task");
        assert_eq!(pap5.assignee, Some(member(RUNNER)));
        assert_eq!(pap5.status, TaskStatus::Todo);
    }
}
