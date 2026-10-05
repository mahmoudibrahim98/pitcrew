//! `POST /v1/tasks/{id}/dispatch`: what is recorded, where the session runs, the refusals, and
//! what happens when the runner link cannot start the session. A test double stands in for the
//! runner link (stream D).

mod common;

use common::{
    PAPER, RUNNER, SAM, SEED_RUNS, WRITER, agent, app, call, demo, expect, member, open, person,
};
use pitcrew_hub_work::{
    DispatchError, DispatchRequest, Dispatcher, ENDED_WITHOUT_REPORT, INTERNAL_MESSAGE, MAX_BRIEF,
    NEVER_STARTED, NewDispatch, RecordedStart, TaskRef, WorkService,
};
use pitcrew_protocol::api::{Caller, ErrorCode, TokenScope};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{
    DispatchId, EventId, MachineId, MemberId, ProjectId, ProjectKey, SessionId,
};
use pitcrew_protocol::model::{
    DispatchOutcome, Engine, LinkBasis, Member, MemberKind, PermissionMode, Project, ProjectStatus,
    Session, SessionState, TaskPatch, TaskStatus,
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
            engine,
            cwd,
            brief,
            session: named,
            ..
        } => {
            assert_eq!(engine, Engine::Codex);
            assert_eq!(cwd, "/scratch/sam/diffusion-runs");
            assert_eq!(brief.as_deref(), Some(pap5.description.as_str()));
            // The runner reports the CLI under the dispatch's session, not a new one.
            assert_eq!(named, Some(session.id));
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

    // Without a configured machine, nowhere: the daemon must name the hub's machine, and the hub
    // does not guess one among the workspace's.
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
    let rev = other.store().latest_rev().expect("rev");
    let res = dispatch(&other, "BARE-1", json!({ "agent": RUNNER })).await;
    expect(&res, 503);
    assert!(
        res.1["message"]
            .as_str()
            .is_some_and(|m| m.contains("Name a machine")),
        "{}",
        res.1
    );
    assert_eq!(other.store().latest_rev().expect("rev"), rev);
    assert!(runner.calls().is_empty());
    // Naming one still works.
    expect(
        &dispatch(
            &other,
            "BARE-1",
            json!({ "agent": RUNNER, "machine": LAPTOP }),
        )
        .await,
        202,
    );
    assert_eq!(runner.calls()[0].machine, LAPTOP.parse().expect("m"));
    // Set later on the shared service (a hub set up while it runs), it is used from then on.
    other.set_hub_machine(CLUSTER.parse().expect("machine"));
    expect(
        &dispatch(&other, "BARE-1", json!({ "agent": REVIEWER })).await,
        202,
    );
    let calls = runner.calls();
    assert_eq!(
        (calls[1].machine, calls[1].cwd.as_str()),
        (CLUSTER.parse().expect("m"), "~")
    );
}

#[tokio::test]
async fn an_agent_is_dispatched_on_a_task_once_at_a_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    expect(
        &dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await,
        202,
    );
    // A second click: the same agent on the same task, while the first dispatch is active.
    let rev = work.store().latest_rev().expect("rev");
    let again = dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await;
    expect(&again, 409);
    assert_eq!(
        work.store().latest_rev().expect("rev"),
        rev,
        "nothing appended"
    );
    assert_eq!(runner.calls().len(), 1, "the runner link was asked once");
    // Another agent may work on it alongside, and the agent may work on another task.
    expect(
        &dispatch(&work, "PAP-5", json!({ "agent": REVIEWER })).await,
        202,
    );
    expect(
        &dispatch(
            &work,
            "PAP-6",
            json!({ "agent": RUNNER, "machine": LAPTOP }),
        )
        .await,
        202,
    );
    // Once the dispatch has finished, the agent may be dispatched on the task again.
    let first = runner.calls()[0].dispatch;
    work.store()
        .append(&[Event {
            id: EventId::new(),
            at: 1_790_800_000_000,
            workspace: demo().workspace.id,
            author: member(RUNNER),
            on_behalf_of: Some(member(SAM)),
            body: EventBody::DispatchFinished {
                dispatch: first,
                outcome: DispatchOutcome::Succeeded,
                summary: None,
            },
        }])
        .expect("append");
    expect(
        &dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await,
        202,
    );
}

#[test]
fn concurrent_dispatches_of_one_agent_start_one_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let task = TaskRef::parse("PAP-5").expect("key");
    let results: Vec<Result<_, _>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let work = Arc::clone(&work);
                let task = task.clone();
                s.spawn(move || {
                    work.dispatch_task(
                        &person(SAM),
                        &task,
                        NewDispatch {
                            agent: member(RUNNER),
                            brief: None,
                            machine: None,
                        },
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect()
    });
    let started = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(started, 1, "{results:?}");
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.code() == ErrorCode::Conflict),
        "{results:?}"
    );
    assert_eq!(runner.calls().len(), 1);
    let pap5 = work.task(&task).expect("task").id;
    let active = work
        .dispatches()
        .expect("dispatches")
        .into_iter()
        .filter(|d| d.task == pap5 && d.agent == member(RUNNER) && d.ended.is_none())
        .count();
    assert_eq!(active, 1);
}

#[tokio::test]
async fn refused_dispatches_record_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let rev = work.store().latest_rev().expect("rev");
    for (key, body, status) in [
        // Unknown task, whatever the body.
        ("PAP-99", json!({ "agent": RUNNER }), 404),
        ("garbage", json!({ "agent": RUNNER }), 404),
        ("PAP-99", json!("not an object"), 404),
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
        // The failed dispatch is over, so the agent can be dispatched on the task again.
        let again = dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await;
        assert_eq!(again.0, status, "{answer:?}: {}", again.1);
        assert_eq!(runner.calls().len(), 2);
    }
}

/// A runner link that cannot start anything now (no runner attached yet, say), or refuses.
#[derive(Debug)]
struct NotReady(DispatchError);

impl Dispatcher for NotReady {
    fn can_start(&self, _: &MachineId) -> Result<(), DispatchError> {
        Err(self.0.clone())
    }

    fn start(&self, request: &DispatchRequest) -> Result<(), DispatchError> {
        panic!("asked to start {request:?} after saying it cannot");
    }
}

/// A runner link that cannot start a session answers before anything is recorded, after the
/// plan's own refusals (api-v1's order: 404, 400, then 409, then 503).
#[tokio::test]
async fn a_runner_link_that_cannot_start_refuses_before_recording_anything() {
    for (error, why, status) in [
        (
            DispatchError::Unavailable("no runner is attached yet".into()),
            "no runner is attached yet",
            503,
        ),
        (
            DispatchError::Rejected("not on this machine".into()),
            "not on this machine",
            409,
        ),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let demo = demo();
        let work = Arc::new(
            WorkService::new(open(&dir.path().join("hub.db")), demo.workspace.clone())
                .with_dispatcher(Arc::new(NotReady(error.clone()))),
        );
        work.seed(&demo).expect("seed");
        let rev = work.store().latest_rev().expect("rev");
        let res = dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await;
        assert_eq!(res.0, status, "{error:?}: {}", res.1);
        assert!(
            res.1["message"].as_str().is_some_and(|m| m.contains(why)),
            "{}",
            res.1
        );
        // The plan's own answers come first.
        expect(
            &dispatch(&work, "PAP-7", json!({ "agent": RUNNER })).await,
            409,
        );
        expect(
            &dispatch(&work, "PAP-5", json!({ "agent": SAM })).await,
            400,
        );
        expect(
            &dispatch(&work, "PAP-99", json!({ "agent": RUNNER })).await,
            404,
        );
        assert_eq!(
            work.store().latest_rev().expect("rev"),
            rev,
            "nothing recorded"
        );
    }

    // Without a runner link at all, too: a done task is a conflict, not unavailable.
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), None);
    expect(
        &dispatch(&work, "PAP-7", json!({ "agent": RUNNER })).await,
        409,
    );
}

/// What the runner reports about a session, appended as its sink appends it (authored by the
/// workspace's person), then followed as the runner link follows it.
fn runner_reports(work: &WorkService, bodies: Vec<EventBody>) {
    let events: Vec<Event> = bodies
        .into_iter()
        .map(|body| Event {
            id: EventId::new(),
            at: 1_790_900_000_000,
            workspace: work.workspace(),
            author: member(SAM),
            on_behalf_of: None,
            body,
        })
        .collect();
    work.store().append(&events).expect("append");
    work.follow_sessions(&events).expect("follow");
}

/// The runner's statement of a dispatched session it found: under the dispatch's id, with the
/// CLI's id, in `state`, naming no agent and no link.
fn restated(work: &WorkService, id: SessionId, state: SessionState) -> EventBody {
    let stored = work.session(&id).expect("session");
    EventBody::SessionDiscovered {
        session: Session {
            native_id: "019a0000-0000-7000-8000-000000000001".into(),
            agent: None,
            workstream: None,
            task: None,
            link_basis: None,
            title: None,
            state,
            ..stored
        },
    }
}

fn state_changed(session: SessionId, from: SessionState, to: SessionState) -> EventBody {
    EventBody::SessionStateChanged {
        session,
        from,
        to,
        status_line: None,
    }
}

/// Dispatches PAP-5 to @runner; its dispatch and session.
async fn dispatched(work: &Arc<WorkService>) -> (DispatchId, SessionId) {
    let res = dispatch(work, "PAP-5", json!({ "agent": RUNNER })).await;
    expect(&res, 202);
    (
        res.1["id"].as_str().expect("id").parse().expect("dispatch"),
        res.1["session"]
            .as_str()
            .expect("session")
            .parse()
            .expect("session"),
    )
}

fn pap5(work: &WorkService) -> pitcrew_protocol::model::Task {
    work.task(&TaskRef::parse("PAP-5").expect("key"))
        .expect("task")
}

#[tokio::test]
async fn a_first_read_with_a_finished_turn_moves_the_task() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(runner));
    let (dispatch_id, session) = dispatched(&work).await;
    let ended = || EventBody::TurnEnded {
        session,
        receipt: pitcrew_protocol::model::Receipt::Transcript {
            session,
            offset: 123,
        },
    };
    runner_reports(
        &work,
        vec![restated(&work, session, SessionState::Idle), ended()],
    );
    assert_eq!(
        pap5(&work).status,
        TaskStatus::InProgress,
        "a finished first turn is evidence that the dispatched session worked"
    );
    assert_eq!(
        work.session(&session).expect("session").state,
        SessionState::Idle
    );
    work.move_task(&person(SAM), &TaskRef::Id(pap5(&work).id), TaskStatus::Todo)
        .expect("back");
    runner_reports(&work, vec![ended()]);
    assert_eq!(
        pap5(&work).status,
        TaskStatus::Todo,
        "later turns respect a person's move"
    );
    work.move_task(
        &person(SAM),
        &TaskRef::Id(pap5(&work).id),
        TaskStatus::InProgress,
    )
    .expect("forward");
    work.move_task(
        &agent(RUNNER),
        &TaskRef::Id(pap5(&work).id),
        TaskStatus::Review,
    )
    .expect("report accepted");
    assert_eq!(
        work.dispatch(&dispatch_id).expect("dispatch").outcome,
        Some(DispatchOutcome::Succeeded)
    );
}

#[tokio::test]
async fn a_cli_exit_releases_the_dispatch_and_preserves_a_report_that_won() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), Some(Recorder::new(Answer::Start)));
    let (first, session) = dispatched(&work).await;
    runner_reports(&work, vec![restated(&work, session, SessionState::Idle)]);
    let machine = work.session(&session).expect("session").machine;
    assert!(
        work.reconciling_sessions(&machine)
            .expect("reconciling")
            .iter()
            .any(|s| s.id == session)
    );
    assert!(
        !work
            .reconciling_sessions(&MachineId::new())
            .expect("other machine")
            .iter()
            .any(|s| s.id == session)
    );
    work.dispatched_cli_exited(&session).expect("exit");
    assert_eq!(
        work.dispatch(&first).expect("dispatch").outcome,
        Some(DispatchOutcome::Canceled)
    );
    assert_eq!(
        work.session(&session).expect("session").state,
        SessionState::Ended
    );
    let rev = work.store().latest_rev().expect("rev");
    work.dispatched_cli_exited(&session).expect("repeat");
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    assert!(
        !work
            .reconciling_sessions(&machine)
            .expect("ended")
            .iter()
            .any(|s| s.id == session)
    );
    let (second, next) = dispatched(&work).await;
    runner_reports(&work, vec![restated(&work, next, SessionState::Working)]);
    work.move_task(
        &agent(RUNNER),
        &TaskRef::Id(pap5(&work).id),
        TaskStatus::Review,
    )
    .expect("report");
    let rev = work.store().latest_rev().expect("rev");
    work.dispatched_cli_exited(&next)
        .expect("exit after report");
    assert_eq!(
        work.dispatch(&second).expect("dispatch").outcome,
        Some(DispatchOutcome::Succeeded)
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    assert!(
        !work
            .reconciling_sessions(&machine)
            .expect("reported")
            .iter()
            .any(|s| s.id == next)
    );
}

/// The task moves with its dispatched session: to in progress the first time it works (once: a
/// person who moves it back is not overruled by the next turn), and the agent's report (its move
/// to review) finishes the dispatch as succeeded in the same transaction.
#[tokio::test]
async fn the_task_moves_with_its_dispatched_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let (dispatch_id, session) = dispatched(&work).await;
    assert_eq!(pap5(&work).status, TaskStatus::Todo);

    // Found idle: nothing moves. Its link and agent stay the dispatch's.
    runner_reports(&work, vec![restated(&work, session, SessionState::Idle)]);
    assert_eq!(pap5(&work).status, TaskStatus::Todo);
    let stored = work.session(&session).expect("session");
    assert_eq!(stored.agent, Some(member(RUNNER)));
    assert_eq!(stored.link_basis, Some(LinkBasis::Dispatch));
    assert_eq!(stored.task, Some(pap5(&work).id));

    // Working: in progress, moved by the agent for its owner.
    runner_reports(
        &work,
        vec![state_changed(
            session,
            SessionState::Idle,
            SessionState::Working,
        )],
    );
    let task = pap5(&work);
    assert_eq!(task.status, TaskStatus::InProgress);
    let moved = events_after(&work, work.store().latest_rev().expect("rev") - 1);
    assert_eq!(moved[0].author, member(RUNNER));
    assert_eq!(moved[0].on_behalf_of, Some(member(SAM)));

    // A person moves it back; the agent's next turn does not move it again.
    work.move_task(&person(SAM), &TaskRef::Id(task.id), TaskStatus::Todo)
        .expect("back");
    runner_reports(
        &work,
        vec![
            state_changed(session, SessionState::Working, SessionState::Idle),
            state_changed(session, SessionState::Idle, SessionState::Working),
        ],
    );
    assert_eq!(pap5(&work).status, TaskStatus::Todo);

    // The agent reports it done (`pitcrew report PAP-5 --review`): the move and the dispatch's
    // end, in one append, by the agent.
    work.move_task(&person(SAM), &TaskRef::Id(task.id), TaskStatus::InProgress)
        .expect("forward");
    let rev = work.store().latest_rev().expect("rev");
    let app = app(&work);
    expect(
        &call(
            &app,
            Some(agent(RUNNER)),
            "POST",
            "/v1/tasks/PAP-5/move",
            Some(json!({ "to": "review" })),
        )
        .await,
        200,
    );
    let events = events_after(&work, rev);
    assert_eq!(types(&events), ["task_moved", "dispatch_finished"]);
    assert!(events.iter().all(|e| e.author == member(RUNNER)));
    let finished = work.dispatch(&dispatch_id).expect("dispatch");
    assert_eq!(finished.outcome, Some(DispatchOutcome::Succeeded));
    assert!(finished.ended.is_some());
    assert_eq!(pap5(&work).status, TaskStatus::Review);

    // The session ending afterwards changes nothing more.
    let rev = work.store().latest_rev().expect("rev");
    runner_reports(&work, vec![EventBody::SessionEnded { session }]);
    assert_eq!(
        work.store().latest_rev().expect("rev"),
        rev + 1,
        "only the end itself"
    );

    // A person moving a task to review reports no dispatch's work.
    let (other, _) = dispatched_on(&work, "PAP-6").await;
    work.move_task(
        &person(SAM),
        &TaskRef::parse("PAP-6").expect("key"),
        TaskStatus::InProgress,
    )
    .expect("start");
    work.move_task(
        &person(SAM),
        &TaskRef::parse("PAP-6").expect("key"),
        TaskStatus::Review,
    )
    .expect("review");
    assert_eq!(work.dispatch(&other).expect("dispatch").ended, None);
}

async fn dispatched_on(work: &Arc<WorkService>, key: &str) -> (DispatchId, SessionId) {
    let res = dispatch(work, key, json!({ "agent": RUNNER })).await;
    expect(&res, 202);
    (
        res.1["id"].as_str().expect("id").parse().expect("dispatch"),
        res.1["session"]
            .as_str()
            .expect("session")
            .parse()
            .expect("session"),
    )
}

/// A dispatched session that ends without the agent's report finishes its dispatch as canceled
/// ("stopped work"); one the runner never reported, as failed.
#[tokio::test]
async fn a_dispatch_whose_session_ends_without_a_report_is_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));

    let (first, session) = dispatched(&work).await;
    runner_reports(
        &work,
        vec![
            restated(&work, session, SessionState::Working),
            EventBody::SessionEnded { session },
        ],
    );
    assert_eq!(pap5(&work).status, TaskStatus::InProgress, "it did work");
    let over = work.dispatch(&first).expect("dispatch");
    assert_eq!(over.outcome, Some(DispatchOutcome::Canceled));
    assert_eq!(over.summary.as_deref(), Some(ENDED_WITHOUT_REPORT));
    let last = events_after(&work, work.store().latest_rev().expect("rev") - 1);
    assert_eq!(last[0].author, member(RUNNER));
    assert_eq!(last[0].on_behalf_of, Some(member(SAM)));

    // Over, so the agent can be dispatched again; this one's CLI never reports its session.
    let (second, session) = dispatched(&work).await;
    runner_reports(&work, vec![EventBody::SessionEnded { session }]);
    let over = work.dispatch(&second).expect("dispatch");
    assert_eq!(over.outcome, Some(DispatchOutcome::Failed));
    assert_eq!(over.summary.as_deref(), Some(NEVER_STARTED));

    // Reports about sessions without a dispatch change nothing.
    let rev = work.store().latest_rev().expect("rev");
    work.follow_sessions(&events_after(&work, 0))
        .expect("replay");
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
}

/// A session the hub stored whose CLI never started (a crash between the dispatch and its
/// start, or a terminal gone before its transcript appeared) is abandoned: its dispatch fails and
/// it ends. One the runner reported meanwhile, or one already ended, is left as it is.
#[tokio::test]
async fn a_session_whose_cli_never_started_is_abandoned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let cluster: MachineId = CLUSTER.parse().expect("machine");
    let before = work.unreported_sessions(&cluster).expect("unreported");

    let (dispatch_id, session) = dispatched(&work).await;
    let unreported = work.unreported_sessions(&cluster).expect("unreported");
    assert_eq!(unreported.len(), before.len() + 1);
    assert!(unreported.iter().any(|s| s.id == session));
    let rev = work.store().latest_rev().expect("rev");
    work.abandon_session(&session, "its terminal is gone")
        .expect("abandon");
    let events = events_after(&work, rev);
    assert_eq!(types(&events), ["dispatch_finished", "session_ended"]);
    assert!(events.iter().all(|e| e.author == member(RUNNER)));
    let over = work.dispatch(&dispatch_id).expect("dispatch");
    assert_eq!(over.outcome, Some(DispatchOutcome::Failed));
    assert!(
        over.summary
            .as_deref()
            .is_some_and(|s| s.contains("its terminal is gone")),
        "{over:?}"
    );
    assert_eq!(
        work.session(&session).expect("session").state,
        SessionState::Ended
    );
    assert_eq!(
        work.unreported_sessions(&cluster).expect("unreported"),
        before
    );

    // Ended: nothing more. Reported meanwhile: left to run.
    let rev = work.store().latest_rev().expect("rev");
    work.abandon_session(&session, "again").expect("ended");
    let (alive, session) = dispatched(&work).await;
    runner_reports(
        &work,
        vec![restated(&work, session, SessionState::Starting)],
    );
    let rev2 = work.store().latest_rev().expect("rev");
    work.abandon_session(&session, "late").expect("reported");
    assert_eq!(work.store().latest_rev().expect("rev"), rev2);
    assert!(rev2 > rev);
    assert_eq!(work.dispatch(&alive).expect("dispatch").ended, None);
    let err = work
        .abandon_session(&SessionId::new(), "unknown")
        .expect_err("unknown");
    assert_eq!(err.code(), ErrorCode::NotFound);
}

/// A session a person starts for an agent or a task is stored before its CLI starts, as a
/// dispatch's is: `starting`, with the agent, linked to the task by hand. One whose start fails is
/// abandoned: it ends.
#[tokio::test]
async fn a_session_started_for_an_agent_and_a_task_is_stored_ahead_of_its_cli() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), None);
    let laptop: MachineId = LAPTOP.parse().expect("machine");
    let task = pap5(&work);
    let start = |agent: Option<&str>, task: Option<pitcrew_protocol::ids::TaskId>| {
        pitcrew_hub_work::RecordedStart {
            machine: laptop,
            engine: Engine::Claude,
            cwd: "/home/sam/work/diffusion-paper".into(),
            agent: agent.map(member),
            task,
        }
    };
    let rev = work.store().latest_rev().expect("rev");
    let session = work
        .record_start(&person(SAM), start(Some(WRITER), Some(task.id)))
        .expect("recorded");
    assert_eq!(session.state, SessionState::Starting);
    assert_eq!(session.native_id, "");
    assert_eq!(session.agent, Some(member(WRITER)));
    assert_eq!(session.task, Some(task.id));
    assert_eq!(session.workstream, task.workstream);
    assert_eq!(session.link_basis, Some(LinkBasis::Manual));
    assert_eq!(work.session(&session.id).expect("stored"), session);
    assert_eq!(types(&events_after(&work, rev)), ["session_discovered"]);
    // Without a task: no link.
    let free = work
        .record_start(&person(SAM), start(Some(WRITER), None))
        .expect("recorded");
    assert_eq!(free.link_basis, None);

    for (caller, bad, code) in [
        (person(SAM), start(Some(SAM), None), ErrorCode::Invalid),
        (
            person(SAM),
            start(Some("01JB000000000000000MEM0099"), None),
            ErrorCode::Invalid,
        ),
        (
            person(SAM),
            start(None, Some(pitcrew_protocol::ids::TaskId::new())),
            ErrorCode::Invalid,
        ),
        (
            agent(WRITER),
            start(Some(WRITER), None),
            ErrorCode::Forbidden,
        ),
    ] {
        let rev = work.store().latest_rev().expect("rev");
        let err = work.record_start(&caller, bad).expect_err("refused");
        assert_eq!(err.code(), code, "{err}");
        assert_eq!(work.store().latest_rev().expect("rev"), rev);
    }

    // Its CLI could not start: it ends, as its agent's.
    work.abandon_session(&session.id, "no terminal runtime")
        .expect("abandon");
    let last = events_after(&work, work.store().latest_rev().expect("rev") - 1);
    assert_eq!(types(&last), ["session_ended"]);
    assert_eq!(last[0].author, member(WRITER));
    assert_eq!(
        work.session(&session.id).expect("session").state,
        SessionState::Ended
    );
}

/// Members other than the demo's: added as the hub adds them, by @sam.
fn add_members(work: &WorkService, members: &[&Member]) {
    let events: Vec<Event> = members
        .iter()
        .map(|m| Event {
            id: EventId::new(),
            at: 1_790_900_000_000,
            workspace: work.workspace(),
            author: member(SAM),
            on_behalf_of: None,
            body: EventBody::MemberAdded {
                member: (*m).clone(),
            },
        })
        .collect();
    work.store().append(&events).expect("append");
}

fn someone(kind: MemberKind, handle: &str, owner: Option<MemberId>) -> Member {
    Member {
        id: MemberId::new(),
        kind,
        handle: handle.into(),
        name: handle.trim_start_matches('@').into(),
        owner,
        persona: None,
        avatar: None,
    }
}

fn device(member: MemberId) -> Caller {
    Caller {
        member,
        scope: TokenScope::Device,
        on_behalf_of: None,
    }
}

/// A person runs only their own agents. Dispatching another person's agent, or one with no
/// owner, is `403` with nothing recorded and the runner link never asked; so is storing a session
/// for one (`POST /v1/sessions` with `agent`). The agent's owner may do both.
#[tokio::test]
async fn a_person_dispatches_and_starts_only_their_own_agents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let kim = someone(MemberKind::Human, "@kim", None);
    let kimbot = someone(MemberKind::Agent, "@kimbot", Some(kim.id));
    let stray = someone(MemberKind::Agent, "@stray", None);
    add_members(&work, &[&kim, &kimbot, &stray]);
    let app = app(&work);
    let laptop: MachineId = LAPTOP.parse().expect("machine");
    let start = |agent: MemberId| RecordedStart {
        machine: laptop,
        engine: Engine::Claude,
        cwd: "/home/kim/work".into(),
        agent: Some(agent),
        task: None,
    };

    let rev = work.store().latest_rev().expect("rev");
    for (caller, agent) in [
        (person(SAM), kimbot.id),
        (person(SAM), stray.id),
        (device(kim.id), member(RUNNER)),
        (device(kim.id), stray.id),
    ] {
        let res = call(
            &app,
            Some(caller),
            "POST",
            "/v1/tasks/PAP-5/dispatch",
            Some(json!({ "agent": agent })),
        )
        .await;
        expect(&res, 403);
        assert!(
            res.1["message"]
                .as_str()
                .is_some_and(|m| m.contains("only their own agents")),
            "{}",
            res.1
        );
        let err = work
            .record_start(&caller, start(agent))
            .expect_err("not theirs");
        assert_eq!(err.code(), ErrorCode::Forbidden, "{err}");
        assert_eq!(
            work.store().latest_rev().expect("rev"),
            rev,
            "nothing recorded"
        );
    }
    assert!(runner.calls().is_empty(), "the runner link was never asked");

    // Their owner may.
    let res = call(
        &app,
        Some(device(kim.id)),
        "POST",
        "/v1/tasks/PAP-5/dispatch",
        Some(json!({ "agent": kimbot.id })),
    )
    .await;
    expect(&res, 202);
    assert_eq!(runner.calls().len(), 1);
    assert_eq!(runner.calls()[0].owner, Some(kim.id));
    let own = work
        .record_start(&device(kim.id), start(kimbot.id))
        .expect("their own");
    assert_eq!(own.agent, Some(kimbot.id));
}

/// A dispatch's brief goes on its CLI's command line: one longer than 64 KiB is `400` with
/// nothing recorded, whether given or the task's description it defaults to. 64 KiB is taken.
#[tokio::test]
async fn a_brief_longer_than_64_kib_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let rev = work.store().latest_rev().expect("rev");
    let res = dispatch(
        &work,
        "PAP-5",
        json!({ "agent": RUNNER, "brief": "a".repeat(MAX_BRIEF + 1) }),
    )
    .await;
    expect(&res, 400);
    assert!(
        res.1["message"]
            .as_str()
            .is_some_and(|m| m.contains("brief"))
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);

    work.patch_task(
        &person(SAM),
        &TaskRef::parse("PAP-5").expect("key"),
        TaskPatch {
            description: Some("d".repeat(MAX_BRIEF + 1)),
            ..TaskPatch::default()
        },
    )
    .expect("patch");
    let rev = work.store().latest_rev().expect("rev");
    let res = dispatch(&work, "PAP-5", json!({ "agent": RUNNER })).await;
    expect(&res, 400);
    assert!(
        res.1["message"]
            .as_str()
            .is_some_and(|m| m.contains("description")),
        "{}",
        res.1
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    assert!(runner.calls().is_empty());

    let res = dispatch(
        &work,
        "PAP-5",
        json!({ "agent": RUNNER, "brief": "b".repeat(MAX_BRIEF) }),
    )
    .await;
    expect(&res, 202);
    assert_eq!(runner.calls()[0].brief.len(), MAX_BRIEF);
}

/// The agent's report on a task a person already moved to review still reports its work done:
/// `200` with the task as it is, and the dispatch succeeds (not `canceled` when its session
/// ends). Nothing moves.
#[tokio::test]
async fn a_report_on_a_task_already_in_review_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let (dispatch_id, session) = dispatched(&work).await;
    let task = pap5(&work);
    for to in [TaskStatus::InProgress, TaskStatus::Review] {
        work.move_task(&person(SAM), &TaskRef::Id(task.id), to)
            .expect("person moves it");
    }

    let rev = work.store().latest_rev().expect("rev");
    let res = call(
        &app(&work),
        Some(agent(RUNNER)),
        "POST",
        "/v1/tasks/PAP-5/move",
        Some(json!({ "to": "review" })),
    )
    .await;
    expect(&res, 200);
    assert_eq!(res.1["status"], "review");
    let events = events_after(&work, rev);
    assert_eq!(types(&events), ["dispatch_finished"]);
    assert_eq!(events[0].author, member(RUNNER));
    let over = work.dispatch(&dispatch_id).expect("dispatch");
    assert_eq!(over.outcome, Some(DispatchOutcome::Succeeded));

    // Its session ending afterwards changes nothing more.
    runner_reports(&work, vec![EventBody::SessionEnded { session }]);
    assert_eq!(
        work.dispatch(&dispatch_id).expect("dispatch").outcome,
        Some(DispatchOutcome::Succeeded)
    );
    // Without an active dispatch, the same move is refused as before.
    let res = call(
        &app(&work),
        Some(agent(RUNNER)),
        "POST",
        "/v1/tasks/PAP-5/move",
        Some(json!({ "to": "review" })),
    )
    .await;
    expect(&res, 409);
}

/// A session the hub ended (its CLI did not start, as far as the hub could tell) stays ended when
/// the runner re-states it after all: the re-statement does not bring it back.
#[tokio::test]
async fn an_ended_session_is_not_revived_by_a_restatement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let (_, session) = dispatched(&work).await;
    work.abandon_session(&session, "its CLI did not start")
        .expect("abandon");
    assert_eq!(
        work.session(&session).expect("session").state,
        SessionState::Ended
    );
    for state in [SessionState::Working, SessionState::Idle] {
        runner_reports(&work, vec![restated(&work, session, state)]);
        let stored = work.session(&session).expect("session");
        assert_eq!(stored.state, SessionState::Ended, "re-stated {state:?}");
        assert!(!stored.native_id.is_empty(), "the rest is taken");
    }
    // A session not ended takes the state it is re-stated in.
    let (_, other) = dispatched_on(&work, "PAP-6").await;
    runner_reports(&work, vec![restated(&work, other, SessionState::Idle)]);
    assert_eq!(
        work.session(&other).expect("session").state,
        SessionState::Idle
    );
}

#[tokio::test]
async fn dispatch_without_persona_uses_workspace_permission_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(Answer::Start);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    work.save_safety(
        &person(SAM),
        pitcrew_protocol::onboarding::SafetySettings {
            permission_mode: PermissionMode::AcceptEdits,
            ..Default::default()
        },
    )
    .expect("save safety");
    let res = dispatch(
        &work,
        "PAP-2",
        json!({"agent": WRITER, "brief": "Synthetic task."}),
    )
    .await;
    expect(&res, 202);
    assert_eq!(
        runner.calls()[0].permission_mode,
        PermissionMode::AcceptEdits
    );
}
