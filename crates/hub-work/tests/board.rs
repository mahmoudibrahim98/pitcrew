//! Board drafts through the routes (api-v1.md, "Board drafts"): the preview sends nothing, the
//! start sends what the preview showed, confined, only the drafting session's own token proposes
//! (and the session is finished then), and **nothing is created until a person accepts it**;
//! rejected items create nothing. A test double stands in for the runner link.

mod common;

use axum::Router;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use common::{
    PAPER, PARSERS, RUNNER, SAM, SEED_RUNS, SUBMISSION, TOOLING, WRITER, agent, call, demo, expect,
    open, person,
};
use pitcrew_hub_work::{
    CONFINED_BRIEF, DispatchError, DispatchRequest, Dispatcher, SessionRequest, TaskFilter,
    WorkService, agent_routes, board_device_routes, board_session_routes, device_routes,
};
use pitcrew_protocol::api::{Caller, NewTask, TokenScope};
use pitcrew_protocol::board::{DRAFTED_LABEL, MAX_PROPOSAL_BYTES};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{SessionId, WorkstreamId};
use pitcrew_protocol::import::{ImportFilter, ImportMode};
use pitcrew_protocol::model::{Engine, LinkBasis, PermissionMode, Receipt, Session, SessionState};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

const LAPTOP: &str = "01JB000000000000000MCH0001";
/// Linked to Submission and PAP-1 in the demo.
const SES1: &str = "01JB000000000000000SES0001";
/// Linked to Seed runs in the demo.
const SES2: &str = "01JB000000000000000SES0002";
/// A synthetic token, of a shape the redaction knows; never a real one.
const TOKEN: &str = "ghp_16C7e42F292c6912E7710c838347Ae178B4a";

/// Records every start and every finish, and answers as told.
#[derive(Debug)]
struct Recorder {
    starts: Mutex<Vec<SessionRequest>>,
    finished: Mutex<Vec<SessionId>>,
    fail: Option<DispatchError>,
}

impl Recorder {
    fn new(fail: Option<DispatchError>) -> Arc<Self> {
        Arc::new(Self {
            starts: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            fail,
        })
    }

    fn starts(&self) -> Vec<SessionRequest> {
        self.starts.lock().expect("starts").clone()
    }

    fn finished(&self) -> Vec<SessionId> {
        self.finished.lock().expect("finished").clone()
    }
}

/// Where the test double says a confined session runs: a synthetic private folder.
fn folder_of(session: &SessionId) -> String {
    format!("/home/sam/.cache/pitcrew/scratch/{}", session.0)
}

impl Dispatcher for Recorder {
    fn start(&self, _: &DispatchRequest) -> Result<(), DispatchError> {
        panic!("a board draft never dispatches a task")
    }

    fn start_session(&self, request: &SessionRequest) -> Result<(), DispatchError> {
        self.starts.lock().expect("starts").push(request.clone());
        self.fail.clone().map_or(Ok(()), Err)
    }

    fn confined_folder(&self, session: &SessionId) -> Option<String> {
        Some(folder_of(session))
    }

    fn finish_session(&self, session: &SessionId) -> Result<(), DispatchError> {
        self.finished.lock().expect("finished").push(*session);
        Ok(())
    }
}

/// The session token the hub's runner link would give a draft's CLI: bound to its session, acting
/// as its agent for the agent's owner.
fn drafter(draft: &Value) -> Caller {
    let session: SessionId = draft["session"]
        .as_str()
        .expect("session")
        .parse()
        .expect("session id");
    Caller {
        member: draft["agent"]
            .as_str()
            .expect("agent")
            .parse()
            .expect("agent id"),
        scope: TokenScope::Session(session),
        on_behalf_of: Some(SAM.parse().expect("sam")),
    }
}

fn service(dir: &Path, runner: Option<Arc<Recorder>>) -> Arc<WorkService> {
    let demo = demo();
    let mut work = WorkService::new(open(&dir.join("hub.db")), demo.workspace.clone())
        .with_hub_machine(LAPTOP.parse().expect("machine"));
    if let Some(runner) = runner {
        work = work.with_dispatcher(runner);
    }
    let work = Arc::new(work);
    work.seed(&demo).expect("seed");
    work
}

async fn require_device(request: axum::extract::Request, next: Next) -> Response {
    match request.extensions().get::<pitcrew_protocol::api::Caller>() {
        Some(c) if c.is_person() => next.run(request).await,
        _ => (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(json!({"code": "forbidden", "message": "device only"})),
        )
            .into_response(),
    }
}

/// The work routes and the board routes, mounted as the daemon mounts them.
fn app(work: &Arc<WorkService>) -> Router {
    agent_routes()
        .merge(board_session_routes())
        .merge(
            device_routes()
                .merge(board_device_routes())
                .layer(middleware::from_fn(require_device)),
        )
        .layer(axum::Extension(Arc::clone(work)))
}

fn latest(work: &WorkService) -> u64 {
    work.store().latest_rev().expect("rev")
}

fn types_after(work: &WorkService, rev: u64) -> Vec<String> {
    work.store()
        .since(rev, usize::MAX)
        .expect("log")
        .into_iter()
        .map(|e| pitcrew_store::event_type(&e.event.body).expect("type"))
        .collect()
}

fn task_count(work: &WorkService) -> usize {
    work.tasks(&TaskFilter::default()).expect("tasks").len()
}

/// Appends events as the runner would, for a session of Submission with no task, whose title
/// and activity hold a synthetic secret.
fn noisy_session(work: &WorkService) -> SessionId {
    let demo = demo();
    let id = SessionId::new();
    let now = 1_790_900_000_000;
    let session = Session {
        id,
        engine: Engine::Claude,
        native_id: "6f1c2a3e-0000-4000-8000-000000000001".into(),
        machine: LAPTOP.parse().expect("machine"),
        cwd: "/home/sam/work/diffusion-paper/paper".into(),
        branch: Some("fix/sam@example.com".into()),
        title: Some(format!("Push the figures with {TOKEN}")),
        agent: None,
        workstream: Some(SUBMISSION.parse().expect("workstream")),
        task: None,
        link_basis: Some(LinkBasis::Folder),
        state: SessionState::Idle,
        status_line: None,
        started: now,
        last_activity: now + 60_000,
        terminal: None,
        parent: None,
    };
    let by = |body| {
        let mut e = Event::now(demo.workspace.id, SAM.parse().expect("sam"), body);
        e.at = now + 1_000;
        e
    };
    work.store()
        .append(&[
            by(EventBody::SessionDiscovered { session }),
            by(EventBody::ToolRan {
                session: id,
                tool: "Bash".into(),
                target: format!("curl -H 'Authorization: Bearer {TOKEN}' https://api.example.com"),
                outcome: "200 OK".into(),
                failed: false,
                receipt: Receipt::Transcript {
                    session: id,
                    offset: 128,
                },
            }),
            by(EventBody::FileEdited {
                session: id,
                path: format!("/home/sam/work/{TOKEN}/figure.py"),
                added: 3,
                removed: 1,
                receipt: None,
            }),
            by(EventBody::TurnEnded {
                session: id,
                receipt: Receipt::Transcript {
                    session: id,
                    offset: 256,
                },
            }),
        ])
        .expect("append");
    id
}

async fn preview(work: &Arc<WorkService>, workstream: &str) -> Value {
    let res = call(
        &app(work),
        Some(person(SAM)),
        "GET",
        &format!("/v1/workstreams/{workstream}/board-draft"),
        None,
    )
    .await;
    expect(&res, 200);
    res.1
}

async fn start(work: &Arc<WorkService>, workstream: &str, body: Value) -> (u16, Value) {
    call(
        &app(work),
        Some(person(SAM)),
        "POST",
        &format!("/v1/workstreams/{workstream}/board-drafts"),
        Some(body),
    )
    .await
}

/// Previews and starts a draft of `workstream` as @writer: the draft.
async fn started(work: &Arc<WorkService>, workstream: &str) -> Value {
    let shown = preview(work, workstream).await;
    let res = start(
        work,
        workstream,
        json!({ "agent": WRITER, "digest": shown["digest"] }),
    )
    .await;
    expect(&res, 202);
    res.1
}

async fn propose(
    work: &Arc<WorkService>,
    draft: &str,
    who: pitcrew_protocol::api::Caller,
    body: Value,
) -> (u16, Value) {
    call(
        &app(work),
        Some(who),
        "POST",
        &format!("/v1/board-drafts/{draft}/proposal"),
        Some(body),
    )
    .await
}

async fn review(work: &Arc<WorkService>, draft: &str, accept: Value) -> (u16, Value) {
    call(
        &app(work),
        Some(person(SAM)),
        "POST",
        &format!("/v1/board-drafts/{draft}/review"),
        Some(json!({ "accept": accept })),
    )
    .await
}

fn three_tasks() -> Value {
    json!({
        "tasks": [
            {"title": "Finish the method section", "status": "in_progress", "evidence": [SES1]},
            {"title": "Ask for a second review", "status": "todo"},
            {"title": "Submit the camera-ready", "status": "done",
             "description": "The checklist says it went out.", "evidence": [SES1]}
        ],
        "note": "Two sessions say the same."
    })
}

#[tokio::test]
async fn a_preview_shows_what_would_be_sent_and_sends_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(None);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let noisy = noisy_session(&work);
    let rev = latest(&work);

    let shown = preview(&work, SUBMISSION).await;
    assert_eq!(shown["workstream"], SUBMISSION);
    assert_eq!(shown["prompt"], "draft-board/v1");
    let summary = shown["summary"].as_str().expect("summary");
    // Submission's sessions (SES0001, SES0006 and the noisy one), most recently active first, and
    // its tasks; never another workstream's.
    for want in [
        SES1,
        "01JB000000000000000SES0006",
        &noisy.0.to_string(),
        "PAP-1",
        "PAP-7",
    ] {
        assert!(summary.contains(want), "{want} is missing from the summary");
    }
    assert!(
        !summary.contains(SES2),
        "another workstream's session is in it"
    );
    assert!(
        !summary.contains("PAP-4"),
        "another workstream's task is in it"
    );
    // Secrets in its title, its branch, its activity and its files never reach the summary. The
    // messages are labels only: a failure must not print the synthetic token.
    assert!(!summary.contains(TOKEN), "the token reached the summary");
    assert!(
        !summary.contains("16C7e42F"),
        "a piece of the token reached the summary"
    );
    assert!(
        !summary.contains("sam@example.com"),
        "an address reached the summary"
    );
    assert!(
        !summary.contains("/home/sam"),
        "a home folder reached the summary"
    );
    assert!(summary.contains("[redacted]"), "nothing was redacted");
    let cost = &shown["cost"];
    assert_eq!(cost["sessions"], 3);
    assert_eq!(cost["sessions_left_out"], 0);
    assert_eq!(cost["tasks"], 4);
    assert_eq!(cost["summary_bytes"], summary.len());
    assert!(cost["prompt_bytes"].as_u64().expect("bytes") > summary.len() as u64);
    assert!(cost["redacted"].as_u64().expect("redacted") >= 3);
    assert!(cost["estimate"]["input_tokens"].as_u64().expect("in") > 15_000);
    assert_eq!(cost["estimate"]["output_tokens"], MAX_PROPOSAL_BYTES / 4);
    let digest = shown["digest"].as_str().expect("digest");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
    // Shown twice, the same.
    assert_eq!(preview(&work, SUBMISSION).await, shown);

    // Nothing was stored or started.
    assert_eq!(latest(&work), rev);
    assert!(runner.starts().is_empty());

    // People only; an unknown workstream is 404.
    let app = app(&work);
    let path = format!("/v1/workstreams/{SUBMISSION}/board-draft");
    expect(
        &call(&app, Some(agent(WRITER)), "GET", &path, None).await,
        403,
    );
    let missing = WorkstreamId::new();
    expect(
        &call(
            &app,
            Some(person(SAM)),
            "GET",
            &format!("/v1/workstreams/{missing}/board-draft"),
            None,
        )
        .await,
        404,
    );
}

#[tokio::test]
async fn nothing_is_created_until_the_person_accepts_and_rejected_items_create_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(None);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let tasks = task_count(&work);
    let rev = latest(&work);

    let draft = started(&work, SUBMISSION).await;
    let id = draft["id"].as_str().expect("id").to_owned();
    assert_eq!(draft["state"], "running");
    assert_eq!(draft["agent"], WRITER);
    assert_eq!(draft["engine"], "claude");
    assert_eq!(draft["by"], SAM);
    assert_eq!(draft["prompt"], "draft-board/v1");
    // The start records the session and the draft, and creates no task.
    assert_eq!(
        types_after(&work, rev),
        ["session_discovered", "board_draft_started"]
    );
    assert_eq!(task_count(&work), tasks);
    // The agent's CLI starts confined in a private folder of its own, never the workstream's,
    // with the prompt the preview showed, naming its draft; and never in its persona's mode
    // (@writer's is accept-edits).
    let starts = runner.starts();
    assert_eq!(starts.len(), 1);
    let request = &starts[0];
    assert_eq!(
        request.session.0.to_string(),
        draft["session"].as_str().expect("session")
    );
    assert_eq!(request.cwd, folder_of(&request.session));
    assert_eq!(request.branch, None);
    assert_eq!(request.engine, Engine::Claude);
    assert_eq!(request.permission_mode, PermissionMode::Default);
    let confinement = request.confinement.clone().expect("confined");
    assert_eq!(confinement.commands, ["board submit"]);
    assert_eq!(confinement.writes, ["proposal.json"]);
    assert_eq!(
        confinement.max_runtime,
        std::time::Duration::from_secs(30 * 60)
    );
    match request.start_command() {
        pitcrew_protocol::runner::RunnerCommand::StartSession {
            brief,
            confined,
            permission_mode,
            cwd,
            ..
        } => {
            assert!(confined);
            assert_eq!(permission_mode, PermissionMode::Default);
            assert_eq!(brief.as_deref(), Some(CONFINED_BRIEF));
            assert_eq!(cwd, folder_of(&request.session));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        request
            .brief
            .contains(&format!("pitcrew board submit drf_{id}")),
        "{}",
        request.brief
    );
    assert_eq!(
        request.brief.len() as u64,
        draft["cost"]["prompt_bytes"].as_u64().expect("bytes")
    );
    let session = work.session(&request.session).expect("session");
    assert_eq!(session.state, SessionState::Starting);
    assert_eq!(session.cwd, folder_of(&request.session));
    assert_eq!(session.agent, Some(WRITER.parse().expect("writer")));
    assert_eq!(
        session.workstream,
        Some(SUBMISSION.parse().expect("workstream"))
    );
    assert_eq!(session.task, None);

    // The draft's session token proposes: still nothing created; and its session is finished
    // (its token stops, its CLI is ended).
    assert!(runner.finished().is_empty());
    let rev = latest(&work);
    let res = propose(&work, &id, drafter(&draft), three_tasks()).await;
    expect(&res, 201);
    assert_eq!(runner.finished(), [request.session]);
    assert_eq!(res.1["state"], "proposed");
    assert_eq!(
        res.1["proposal"]["tasks"].as_array().expect("tasks").len(),
        3
    );
    assert_eq!(res.1["proposal"]["note"], "Two sessions say the same.");
    assert_eq!(types_after(&work, rev), ["board_proposed"]);
    assert_eq!(task_count(&work), tasks);
    // Once only.
    expect(
        &propose(&work, &id, drafter(&draft), three_tasks()).await,
        409,
    );

    // The person accepts the first and the last: two tasks, drafted; the second creates nothing.
    let rev = latest(&work);
    let res = review(&work, &id, json!([2, 0])).await;
    expect(&res, 200);
    let created = res.1["tasks"].as_array().expect("tasks");
    assert_eq!(created.len(), 2);
    assert_eq!(created[0]["title"], "Finish the method section");
    assert_eq!(created[0]["status"], "in_progress");
    assert_eq!(created[1]["title"], "Submit the camera-ready");
    assert_eq!(created[1]["status"], "done");
    assert_eq!(created[1]["description"], "The checklist says it went out.");
    for task in created {
        assert_eq!(task["workstream"], SUBMISSION);
        assert_eq!(task["labels"], json!([DRAFTED_LABEL]));
        assert!(task.get("assignee").is_none());
    }
    assert_eq!(created[0]["key"], "PAP-8");
    assert_eq!(created[1]["key"], "PAP-9");
    assert_eq!(task_count(&work), tasks + 2);
    assert!(
        !work
            .tasks(&TaskFilter::default())
            .expect("tasks")
            .iter()
            .any(|t| t.title == "Ask for a second review"),
        "a rejected item became a task"
    );
    let reviewed = &res.1["draft"];
    assert_eq!(reviewed["state"], "reviewed");
    assert_eq!(reviewed["rejected"], json!([1]));
    assert_eq!(
        reviewed["accepted"],
        json!([
            {"item": 0, "task": created[0]["id"]},
            {"item": 2, "task": created[1]["id"]}
        ])
    );
    // SES0001 is PAP-1's already: it is not taken from it.
    assert_eq!(
        types_after(&work, rev),
        ["task_created", "task_created", "board_draft_reviewed"]
    );
    // Once only.
    expect(&review(&work, &id, json!([1])).await, 409);
    assert_eq!(task_count(&work), tasks + 2);

    // Listed, newest first, and by workstream.
    let app = app(&work);
    let all = call(&app, Some(person(SAM)), "GET", "/v1/board-drafts", None).await;
    expect(&all, 200);
    assert_eq!(all.1[0]["id"], id.as_str());
    let other = call(
        &app,
        Some(person(SAM)),
        "GET",
        &format!("/v1/board-drafts?workstream={SEED_RUNS}"),
        None,
    )
    .await;
    assert_eq!(other.1, json!([]));
    let one = call(
        &app,
        Some(person(SAM)),
        "GET",
        &format!("/v1/board-drafts/{id}"),
        None,
    )
    .await;
    assert_eq!(&one.1, reviewed);
}

#[tokio::test]
async fn accepting_none_creates_nothing_and_another_draft_may_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), Some(Recorder::new(None)));
    let tasks = task_count(&work);
    let draft = started(&work, SUBMISSION).await;
    let id = draft["id"].as_str().expect("id").to_owned();

    // One draft at a time: a second start waits for this one.
    let shown = preview(&work, SUBMISSION).await;
    let again = start(
        &work,
        SUBMISSION,
        json!({ "agent": WRITER, "digest": shown["digest"] }),
    )
    .await;
    expect(&again, 409);

    expect(
        &propose(&work, &id, drafter(&draft), three_tasks()).await,
        201,
    );
    let shown = preview(&work, SUBMISSION).await;
    expect(
        &start(
            &work,
            SUBMISSION,
            json!({ "agent": WRITER, "digest": shown["digest"] }),
        )
        .await,
        409,
    );
    let rev = latest(&work);
    let res = review(&work, &id, json!([])).await;
    expect(&res, 200);
    assert_eq!(res.1["tasks"], json!([]));
    assert_eq!(res.1["draft"]["rejected"], json!([0, 1, 2]));
    assert_eq!(res.1["draft"]["accepted"], json!([]));
    assert_eq!(types_after(&work, rev), ["board_draft_reviewed"]);
    assert_eq!(task_count(&work), tasks);

    // Reviewed, the workstream may be drafted again; its drafting session is not in the summary.
    let shown = preview(&work, SUBMISSION).await;
    assert!(
        !shown["summary"]
            .as_str()
            .expect("summary")
            .contains(draft["session"].as_str().expect("session"))
    );
    expect(
        &start(
            &work,
            SUBMISSION,
            json!({ "agent": WRITER, "digest": shown["digest"] }),
        )
        .await,
        202,
    );
}

/// A service whose clock the test sets.
fn service_at(dir: &Path, runner: Arc<Recorder>, now: Arc<AtomicI64>) -> Arc<WorkService> {
    let demo = demo();
    let work = WorkService::new(open(&dir.join("hub.db")), demo.workspace.clone())
        .with_hub_machine(LAPTOP.parse().expect("machine"))
        .with_clock(Arc::new(move || now.load(Ordering::SeqCst)))
        .with_dispatcher(runner);
    let work = Arc::new(work);
    work.seed(&demo).expect("seed");
    work
}

/// A task for `workstream`, made by Sam: a change to what its draft would send.
fn add_task(work: &WorkService, workstream: &str, title: &str) {
    let project = if workstream == PARSERS {
        TOOLING
    } else {
        PAPER
    };
    work.create_task(
        &person(SAM),
        NewTask {
            project: project.parse().expect("project"),
            workstream: Some(workstream.parse().expect("workstream")),
            title: title.into(),
            description: None,
            status: None,
            priority: None,
            assignee: None,
            labels: None,
            due: None,
        },
    )
    .expect("task");
}

#[tokio::test]
async fn a_start_sends_the_preview_the_person_saw() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(None);
    let now = Arc::new(AtomicI64::new(1_790_900_000_000));
    let work = service_at(dir.path(), Arc::clone(&runner), Arc::clone(&now));

    // The workstream moves on after its preview (a new session's work): a start with that
    // preview's digest, while it is kept, sends exactly what the person saw.
    let shown = preview(&work, SUBMISSION).await;
    let noisy = noisy_session(&work);
    now.fetch_add(9 * 60 * 1000, Ordering::SeqCst);
    let res = start(
        &work,
        SUBMISSION,
        json!({ "agent": WRITER, "digest": shown["digest"] }),
    )
    .await;
    expect(&res, 202);
    let request = &runner.starts()[0];
    assert!(
        request
            .brief
            .contains(shown["summary"].as_str().expect("summary"))
    );
    assert!(!request.brief.contains(&noisy.0.to_string()));
    assert_eq!(
        request.brief.len() as u64,
        shown["cost"]["prompt_bytes"].as_u64().expect("bytes")
    );
    assert_eq!(res.1["cost"], shown["cost"]);

    // A preview the person has seen a newer one of starts nothing.
    let first = preview(&work, SEED_RUNS).await;
    add_task(&work, SEED_RUNS, "Synthetic new task");
    let newer = preview(&work, SEED_RUNS).await;
    assert_ne!(first["digest"], newer["digest"]);
    let rev = latest(&work);
    let res = start(
        &work,
        SEED_RUNS,
        json!({ "agent": WRITER, "digest": first["digest"] }),
    )
    .await;
    expect(&res, 409);
    assert!(
        res.1["message"]
            .as_str()
            .expect("message")
            .contains("preview")
    );
    // Nor one kept past its time, once the workstream changed.
    let old = preview(&work, SEED_RUNS).await;
    now.fetch_add(11 * 60 * 1000, Ordering::SeqCst);
    add_task(&work, SEED_RUNS, "Another synthetic task");
    let rev_after = latest(&work);
    expect(
        &start(
            &work,
            SEED_RUNS,
            json!({ "agent": WRITER, "digest": old["digest"] }),
        )
        .await,
        409,
    );
    for body in [
        json!({ "agent": WRITER }),
        json!({ "agent": WRITER, "digest": "00" }),
        json!([]),
    ] {
        let res = start(&work, SEED_RUNS, body.clone()).await;
        assert!(matches!(res.0, 400 | 409), "{body}: {res:?}");
    }
    assert_eq!(latest(&work), rev_after, "a refused start stores nothing");
    assert!(rev_after > rev);
    assert_eq!(runner.starts().len(), 1);
    // A preview kept past its time still starts while the workstream is as it was.
    let kept = preview(&work, SEED_RUNS).await;
    now.fetch_add(11 * 60 * 1000, Ordering::SeqCst);
    expect(
        &start(
            &work,
            SEED_RUNS,
            json!({ "agent": WRITER, "digest": kept["digest"] }),
        )
        .await,
        202,
    );
}

/// The daemon ends every draft still running when it starts (their session tokens were in its
/// memory): each draft ends, and the dispatcher finishes its session.
#[tokio::test]
async fn running_drafts_end_when_the_hub_restarts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Recorder::new(None);
    let work = service(dir.path(), Some(Arc::clone(&runner)));
    let draft = started(&work, SUBMISSION).await;
    let session: SessionId = draft["session"]
        .as_str()
        .expect("session")
        .parse()
        .expect("id");
    assert_eq!(work.running_draft_sessions().expect("running"), [session]);
    assert_eq!(
        work.end_running_drafts("the hub restarted").expect("end"),
        1
    );
    assert_eq!(runner.finished(), [session]);
    assert_eq!(
        work.session(&session).expect("session").state,
        SessionState::Ended
    );
    let drafts = work.board_drafts(None).expect("drafts");
    assert_eq!(drafts[0].state, pitcrew_protocol::board::DraftState::Ended);
    assert!(work.running_draft_sessions().expect("running").is_empty());
    assert_eq!(work.end_running_drafts("again").expect("end"), 0);
    // A confined session ends once, however often it is ended.
    let rev = latest(&work);
    work.end_confined_session(&session, "again").expect("end");
    assert_eq!(latest(&work), rev);
}

/// One board-draft event that cannot be read hides no other draft: it is logged and skipped.
#[tokio::test]
async fn an_unreadable_draft_event_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), Some(Recorder::new(None)));
    let first = started(&work, SUBMISSION).await;
    let second = started(&work, SEED_RUNS).await;
    {
        // The log is append-only: a damaged event (or one a newer hub wrote) is appended.
        let conn = pitcrew_store::sql::Connection::open(dir.path().join("hub.db")).expect("db");
        conn.execute(
            "INSERT INTO events (id, at, workspace, author, on_behalf_of, type, data)
             VALUES ('synthetic-damaged-1', 0, 'w', 'a', NULL, 'board_draft_started',
                     '{\"draft\": 7}')",
            [],
        )
        .expect("damage");
    }
    let ids: Vec<String> = work
        .board_drafts(None)
        .expect("drafts")
        .iter()
        .map(|d| d.id.0.to_string())
        .collect();
    assert_eq!(
        ids,
        [
            second["id"].as_str().expect("id"),
            first["id"].as_str().expect("id")
        ]
    );
    let id: pitcrew_protocol::ids::DraftId =
        first["id"].as_str().expect("id").parse().expect("draft id");
    assert_eq!(
        work.board_draft(&id).expect("draft").state,
        pitcrew_protocol::board::DraftState::Running
    );
}

#[tokio::test]
async fn who_may_start_propose_and_review() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), Some(Recorder::new(None)));
    let shown = preview(&work, SUBMISSION).await;
    let digest = shown["digest"].clone();
    let app = app(&work);
    let path = format!("/v1/workstreams/{SUBMISSION}/board-drafts");
    // An agent starts nothing.
    expect(
        &call(
            &app,
            Some(agent(WRITER)),
            "POST",
            &path,
            Some(json!({ "digest": digest })),
        )
        .await,
        403,
    );
    // A person, or an unknown member, is no agent to run.
    expect(
        &start(&work, SUBMISSION, json!({ "agent": SAM, "digest": digest })).await,
        400,
    );
    expect(
        &start(
            &work,
            SUBMISSION,
            json!({ "agent": "01J00000000000000000000000", "digest": digest }),
        )
        .await,
        400,
    );
    // Without an agent named, the back office drafts: the demo's @office is Sam's.
    let office = start(&work, SUBMISSION, json!({ "digest": digest })).await;
    expect(&office, 202);
    assert_eq!(office.1["agent"], "01JB000000000000000MEM0006");
    let id = office.1["id"].as_str().expect("id").to_owned();

    // Only the draft's own session token proposes: not a person, not another agent, not @office's
    // own agent token, and not another session's token of @office's.
    let office_agent = Caller {
        member: "01JB000000000000000MEM0006".parse().expect("office"),
        scope: TokenScope::Agent,
        on_behalf_of: Some(SAM.parse().expect("sam")),
    };
    let elsewhere = Caller {
        scope: TokenScope::Session(SessionId::new()),
        ..office_agent
    };
    let mut as_runner = drafter(&office.1);
    as_runner.member = RUNNER.parse().expect("runner");
    for who in [
        person(SAM),
        agent(RUNNER),
        agent(WRITER),
        office_agent,
        elsewhere,
        as_runner,
    ] {
        expect(&propose(&work, &id, who, three_tasks()).await, 403);
    }
    // Before the body is read: a forbidden caller hears 403 whatever it sent.
    let res = call(
        &app,
        Some(agent(RUNNER)),
        "POST",
        &format!("/v1/board-drafts/{id}/proposal"),
        Some(json!([1])),
    )
    .await;
    expect(&res, 403);
    // An unknown draft is 404.
    expect(
        &propose(
            &work,
            "01J00000000000000000000000",
            drafter(&office.1),
            three_tasks(),
        )
        .await,
        404,
    );
    expect(
        &review(&work, "01J00000000000000000000000", json!([])).await,
        404,
    );

    // Nothing to review yet.
    expect(&review(&work, &id, json!([])).await, 409);
    expect(
        &propose(&work, &id, drafter(&office.1), three_tasks()).await,
        201,
    );
    // Agents never review, and never read drafts.
    let res = call(
        &app,
        Some(agent(WRITER)),
        "POST",
        &format!("/v1/board-drafts/{id}/review"),
        Some(json!({"accept": [0]})),
    )
    .await;
    expect(&res, 403);
    expect(
        &call(&app, Some(agent(WRITER)), "GET", "/v1/board-drafts", None).await,
        403,
    );
    // Bad reviews.
    for accept in [json!([3]), json!([0, 0]), json!([-1]), json!("all")] {
        expect(&review(&work, &id, accept).await, 400);
    }
}

#[tokio::test]
async fn proposals_are_bounded_checked_and_redacted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), Some(Recorder::new(None)));
    let noisy = noisy_session(&work);
    let draft = started(&work, SUBMISSION).await;
    let id = draft["id"].as_str().expect("id").to_owned();
    let task = |evidence: Value| json!({"tasks": [{"title": "T", "status": "todo", "evidence": evidence}]});
    // Evidence: only the workstream's own sessions; never its drafting session.
    for evidence in [
        json!([SES2]),
        json!(["01J00000000000000000000000"]),
        json!([draft["session"]]),
        json!(["not-an-id"]),
    ] {
        expect(
            &propose(&work, &id, drafter(&draft), task(evidence)).await,
            400,
        );
    }
    for body in [
        json!({"tasks": [{"title": " ", "status": "todo"}]}),
        json!({"tasks": [{"title": "T", "status": "canceled"}]}),
        json!({"tasks": [{"title": "x".repeat(201), "status": "todo"}]}),
        json!({"tasks": [{"title": "T", "status": "todo", "description": "d".repeat(2001)}]}),
        json!({"tasks": vec![json!({"title": "T", "status": "todo"}); 51]}),
        json!({"tasks": [], "note": "n".repeat(2001)}),
        json!({"note": "no tasks"}),
        json!([]),
        json!({"tasks": [{"title": "T", "status": "todo", "description": "d".repeat(MAX_PROPOSAL_BYTES)}]}),
    ] {
        let res = propose(&work, &id, drafter(&draft), body).await;
        expect(&res, 400);
    }
    // Still running: nothing was stored.
    let still = call(
        &app(&work),
        Some(person(SAM)),
        "GET",
        &format!("/v1/board-drafts/{id}"),
        None,
    )
    .await;
    assert_eq!(still.1["state"], "running");

    let res = propose(
        &work,
        &id,
        drafter(&draft),
        json!({
            "tasks": [{"title": format!("  Rotate {TOKEN}  "), "status": "todo",
                       "description": "Mail sam@example.com", "evidence": [noisy, noisy]}],
            "note": format!("password={TOKEN}")
        }),
    )
    .await;
    expect(&res, 201);
    let proposed = &res.1["proposal"];
    // Compared without printing: on a failure they would hold the synthetic token.
    assert!(
        proposed["tasks"][0]["title"] == "Rotate [redacted]",
        "the title is not trimmed and redacted"
    );
    assert_eq!(proposed["tasks"][0]["description"], "Mail [email]");
    assert_eq!(proposed["tasks"][0]["evidence"], json!([noisy]));
    assert!(
        proposed["note"] == "password=[redacted]",
        "the note is not redacted"
    );

    // Accepted, the task takes its evidence session, which had no task.
    let res = review(&work, &id, json!([0])).await;
    expect(&res, 200);
    let task = res.1["tasks"][0]["id"].as_str().expect("task").to_owned();
    let session = work.session(&noisy).expect("session");
    assert_eq!(
        session.task.map(|t| t.0.to_string()).as_deref(),
        Some(task.as_str())
    );
    assert_eq!(session.link_basis, Some(LinkBasis::Manual));
}

#[tokio::test]
async fn a_draft_whose_session_ends_without_a_proposal_has_ended() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), Some(Recorder::new(None)));
    let draft = started(&work, SUBMISSION).await;
    let id = draft["id"].as_str().expect("id").to_owned();
    let session: SessionId = draft["session"]
        .as_str()
        .expect("session")
        .parse()
        .expect("id");
    work.abandon_session(&session, "the test ends it")
        .expect("end");
    let one = call(
        &app(&work),
        Some(person(SAM)),
        "GET",
        &format!("/v1/board-drafts/{id}"),
        None,
    )
    .await;
    assert_eq!(one.1["state"], "ended");
    expect(
        &propose(&work, &id, drafter(&draft), three_tasks()).await,
        409,
    );
    expect(&review(&work, &id, json!([])).await, 409);
    // An ended draft does not hold the workstream.
    let shown = preview(&work, SUBMISSION).await;
    expect(
        &start(
            &work,
            SUBMISSION,
            json!({ "agent": WRITER, "digest": shown["digest"] }),
        )
        .await,
        202,
    );
}

#[tokio::test]
async fn a_start_the_runner_cannot_make_ends_the_draft() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(
        dir.path(),
        Some(Recorder::new(Some(DispatchError::Rejected(
            "no such folder".into(),
        )))),
    );
    let shown = preview(&work, SUBMISSION).await;
    let res = start(
        &work,
        SUBMISSION,
        json!({ "agent": WRITER, "digest": shown["digest"] }),
    )
    .await;
    expect(&res, 409);
    let drafts = work.board_drafts(None).expect("drafts");
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].state, pitcrew_protocol::board::DraftState::Ended);
    assert_eq!(
        work.session(&drafts[0].session).expect("session").state,
        SessionState::Ended
    );

    // Without a runner link, nothing is stored at all.
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), None);
    let shown = preview(&work, SUBMISSION).await;
    let rev = latest(&work);
    let res = start(
        &work,
        SUBMISSION,
        json!({ "agent": WRITER, "digest": shown["digest"] }),
    )
    .await;
    expect(&res, 503);
    assert_eq!(latest(&work), rev);
}

#[tokio::test]
async fn sessions_the_import_choice_hides_are_neither_sent_nor_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = service(dir.path(), Some(Recorder::new(None)));
    let before = preview(&work, SUBMISSION).await;
    assert_eq!(before["cost"]["sessions"], 2);
    // Only Codex sessions: Submission's Claude sessions are hidden.
    work.commit_import(ImportFilter {
        mode: ImportMode::Filtered,
        since: None,
        engines: vec![Engine::Codex],
        folders: Vec::new(),
    })
    .expect("commit");
    let after = preview(&work, SUBMISSION).await;
    assert_eq!(after["cost"]["sessions"], 0);
    assert!(!after["summary"].as_str().expect("summary").contains(SES1));
    let draft = started(&work, SUBMISSION).await;
    let id = draft["id"].as_str().expect("id");
    let res = propose(
        &work,
        id,
        drafter(&draft),
        json!({"tasks": [{"title": "T", "status": "todo", "evidence": [SES1]}]}),
    )
    .await;
    expect(&res, 400);
}
