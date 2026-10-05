//! The Orchestrator through its routes (api-v1.md, "Orchestrator"): a question starts a session
//! that only reads, in the person's scratch folder; its answer streams from the session's
//! transcript, with what it cites checked and what it suggests kept as suggestions; follow-ups
//! type into the live session; bounds, cancel, clear, who may, and what the file keeps. A test
//! double stands in for the runner link, with transcripts in memory.

mod common;

use axum::Router;
use common::{OFFICE, SAM, WRITER, agent, call, demo, expect, open, person, reader};
use pitcrew_hub_work::{
    CONFINED_BRIEF, DispatchError, DispatchRequest, Dispatcher, RunToken, SessionRequest,
    WorkService, orchestrator_routes,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{MachineId, SessionId};
use pitcrew_protocol::model::{Engine, PermissionMode};
use pitcrew_protocol::runner::RunnerCommand;
use pitcrew_protocol::transcript::{TranscriptItem, TranscriptPage};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

const LAPTOP: &str = "01JB000000000000000MCH0001";
/// Linked to Submission and PAP-1 in the demo.
const SES1: &str = "01JB000000000000000SES0001";
const SUBMISSION: &str = "01JB000000000000000WST0001";
const PAPER: &str = "01JB000000000000000PRJ0001";

/// An engine the Orchestrator offers besides Claude Code: OpenCode, but on Windows none.
const OTHER: &str = if cfg!(windows) { "claude" } else { "opencode" };

/// The runner link, with transcripts in memory.
#[derive(Debug, Default)]
struct Runner {
    starts: Mutex<Vec<SessionRequest>>,
    commands: Mutex<Vec<RunnerCommand>>,
    transcripts: Mutex<HashMap<SessionId, Vec<TranscriptItem>>>,
    missing: Mutex<Vec<Engine>>,
    fail: Mutex<Option<DispatchError>>,
    /// The confined sessions finished (their token stopped, their CLI ended).
    finished: Mutex<Vec<SessionId>>,
}

impl Runner {
    fn starts(&self) -> Vec<SessionRequest> {
        self.starts.lock().expect("starts").clone()
    }

    fn commands(&self) -> Vec<RunnerCommand> {
        self.commands.lock().expect("commands").clone()
    }

    fn finished(&self) -> Vec<SessionId> {
        self.finished.lock().expect("finished").clone()
    }

    /// Appends `items` to `session`'s transcript, at offsets after what it holds.
    fn write(&self, session: SessionId, items: Vec<Item>) {
        let mut all = self.transcripts.lock().expect("transcripts");
        let list = all.entry(session).or_default();
        let mut offset = list.last().map_or(0, |i| i.offset() + 100);
        for item in items {
            list.push(item.at(offset));
            offset += 100;
        }
    }
}

/// A transcript item, before it has an offset.
enum Item {
    Prompt(String),
    Said(String),
    Tool,
    End,
}

impl Item {
    fn at(self, offset: u64) -> TranscriptItem {
        match self {
            Self::Prompt(text) => TranscriptItem::UserPrompt {
                at: 1_000,
                text,
                offset,
            },
            Self::Said(text) => TranscriptItem::AssistantText {
                at: 1_000,
                text,
                offset,
            },
            Self::Tool => TranscriptItem::ToolUse {
                at: 1_000,
                call_id: format!("call-{offset}"),
                tool: "Bash".into(),
                target: "pitcrew session list --json".into(),
                input: None,
                offset,
            },
            Self::End => TranscriptItem::TurnEnded {
                at: 1_790_900_012_000,
                offset,
            },
        }
    }
}

impl Dispatcher for Runner {
    fn start(&self, _: &DispatchRequest) -> Result<(), DispatchError> {
        panic!("the Orchestrator never dispatches a task")
    }

    fn start_session(&self, request: &SessionRequest) -> Result<(), DispatchError> {
        self.starts.lock().expect("starts").push(request.clone());
        self.fail.lock().expect("fail").clone().map_or(Ok(()), Err)
    }

    fn confined_folder(&self, session: &SessionId) -> Option<String> {
        Some(format!("/cache/scratch/{}", session.0))
    }

    fn finish_session(&self, session: &SessionId) -> Result<(), DispatchError> {
        self.finished.lock().expect("finished").push(*session);
        Ok(())
    }

    fn command(&self, _: &MachineId, command: &RunnerCommand) -> Result<(), DispatchError> {
        self.commands
            .lock()
            .expect("commands")
            .push(command.clone());
        Ok(())
    }

    fn transcript(
        &self,
        _: &MachineId,
        session: &SessionId,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, DispatchError> {
        let all = self.transcripts.lock().expect("transcripts");
        let items: Vec<TranscriptItem> = all
            .get(session)
            .map(|list| {
                list.iter()
                    .filter(|i| before.is_none_or(|b| i.offset() < b))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let start = items.len().saturating_sub(limit);
        let page: Vec<TranscriptItem> = items[start..].to_vec();
        Ok(TranscriptPage {
            from: page.first().map_or(0, TranscriptItem::offset),
            to: page.last().map_or(0, |i| i.offset() + 1),
            at_start: start == 0,
            items: page,
        })
    }

    fn installed(&self, _: &MachineId, engine: Engine) -> Option<bool> {
        Some(!self.missing.lock().expect("missing").contains(&engine))
    }
}

/// A service over the demo, with `runner` as its runner link, a clock the test moves, and its
/// conversations in `dir`'s `orchestrator.json`.
fn service(dir: &Path, runner: &Arc<Runner>, clock: &Arc<AtomicI64>) -> Arc<WorkService> {
    let demo = demo();
    let now = Arc::clone(clock);
    let work = Arc::new(
        WorkService::new(open(&dir.join("hub.db")), demo.workspace.clone())
            .with_hub_machine(LAPTOP.parse().expect("machine"))
            .with_dispatcher(Arc::clone(runner) as Arc<dyn Dispatcher>)
            .with_clock(Arc::new(move || now.load(Ordering::SeqCst)))
            .with_orchestrator_file(dir.join("orchestrator.json"))
            .expect("the file"),
    );
    work.seed(&demo).expect("seed");
    work
}

/// The Orchestrator's routes with the work routes, as the daemon mounts them.
fn app(work: &Arc<WorkService>) -> Router {
    common::app(work).merge(orchestrator_routes().layer(axum::Extension(Arc::clone(work))))
}

async fn ask(work: &Arc<WorkService>, body: Value) -> (u16, Value) {
    call(
        &app(work),
        Some(person(SAM)),
        "POST",
        "/v1/orchestrator/questions",
        Some(body),
    )
    .await
}

async fn state(work: &Arc<WorkService>) -> Value {
    let res = call(
        &app(work),
        Some(person(SAM)),
        "GET",
        "/v1/orchestrator",
        None,
    )
    .await;
    expect(&res, 200);
    res.1
}

fn session_of(conversation: &Value) -> SessionId {
    conversation["turns"]
        .as_array()
        .and_then(|t| t.last())
        .and_then(|t| t["session"].as_str())
        .expect("a turn's session")
        .parse()
        .expect("session id")
}

/// The session ends, as the runner reports it.
fn end_session(work: &WorkService, session: SessionId) {
    work.store()
        .append(&[Event::now(
            work.workspace(),
            SAM.parse().expect("sam"),
            EventBody::SessionEnded { session },
        )])
        .expect("append");
}

#[tokio::test]
async fn a_question_starts_a_reading_session_and_its_answer_streams_from_its_transcript() {
    let tmp = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Runner::default());
    let clock = Arc::new(AtomicI64::new(1_790_900_000_000));
    let work = service(tmp.path(), &runner, &clock);

    let empty = state(&work).await;
    // Codex is not offered (its sandbox keeps `pitcrew` from the hub), nor OpenCode on Windows.
    let engines = if cfg!(windows) {
        json!([{"engine": "claude", "installed": true}])
    } else {
        json!([
            {"engine": "claude", "installed": true},
            {"engine": "opencode", "installed": true},
        ])
    };
    assert_eq!(empty["engines"], engines);
    assert!(empty.get("engine").is_none());
    assert_eq!(empty["conversations"], json!([]));
    assert_eq!(empty["limits"]["answer_bytes"], 16384);

    let rev = work.store().latest_rev().expect("rev");
    let res = ask(
        &work,
        json!({"text": "  What did my agents do today?\u{202E}  "}),
    )
    .await;
    expect(&res, 202);
    let conversation = res.1;
    assert_eq!(conversation["engine"], "claude");
    assert_eq!(conversation["agent"], OFFICE);
    let turn = &conversation["turns"][0];
    assert_eq!(turn["question"], "What did my agents do today?");
    assert_eq!(turn["state"], "answering");
    assert_eq!(turn["answer"], "");
    assert!(turn.get("usage").is_none());
    let session = session_of(&conversation);
    assert_eq!(conversation["session"], session.0.to_string());

    // Stored first, as the back office's, titled Orchestrator, in its own confined folder;
    // started confined, with a reader token, the prompt in its folder's file.
    let stored = work.session(&session).expect("stored");
    assert_eq!(stored.title.as_deref(), Some("Orchestrator"));
    assert_eq!(stored.agent, Some(OFFICE.parse().expect("office")));
    assert_eq!(stored.workstream, None);
    assert_eq!(stored.cwd, format!("/cache/scratch/{}", session.0));
    let appended: Vec<String> = work
        .store()
        .since(rev, usize::MAX)
        .expect("log")
        .into_iter()
        .map(|e| pitcrew_store::event_type(&e.event.body).expect("type"))
        .collect();
    assert_eq!(appended, vec!["session_discovered"]);
    let starts = runner.starts();
    assert_eq!(starts.len(), 1);
    let start = &starts[0];
    assert_eq!(start.session, session);
    assert_eq!(start.permission_mode, PermissionMode::Default);
    assert_eq!(start.name, "Orchestrator");
    assert_eq!(start.cwd, stored.cwd);
    assert!(
        start
            .brief
            .ends_with("The question:\n\nWhat did my agents do today?\n")
    );
    assert!(start.brief.contains("pitcrew session list"));
    assert!(start.brief.contains("Sam Rivera (@sam)"));
    let confinement = start.confinement.clone().expect("a confined run");
    assert_eq!(confinement, pitcrew_hub_work::orchestrator_confinement());
    assert_eq!(
        confinement.token,
        RunToken::Reader,
        "its CLI gets a reader token"
    );
    assert!(confinement.writes.is_empty(), "it writes no file");
    assert!(confinement.commands.contains(&"session list".to_owned()));
    assert!(!confinement.commands.iter().any(|c| c.starts_with("board")));
    // Its command line carries one plain line, never the prompt (a Windows `.cmd` shim refuses
    // the prompt's quotes and brackets); the prompt goes in its folder's prompt.md.
    match start.start_command() {
        RunnerCommand::StartSession {
            brief, confined, ..
        } => {
            assert_eq!(brief.as_deref(), Some(CONFINED_BRIEF));
            assert!(confined);
        }
        other => panic!("{other:?}"),
    }
    assert!(!CONFINED_BRIEF.chars().any(|c| "\"%!^&|<>()".contains(c)));
    assert_eq!(
        work.orchestrator_asker(&session),
        Some(SAM.parse().expect("sam"))
    );
    assert!(!work.is_orchestrator_session(&SES1.parse().expect("session")));

    // Nothing in the transcript yet: still answering.
    assert_eq!(work.follow_orchestrator().expect("follow"), 1);
    runner.write(
        session,
        vec![
            Item::Prompt(start.brief.clone()),
            Item::Said("Looking at today's sessions.".into()),
            Item::Tool,
        ],
    );
    assert_eq!(work.follow_orchestrator().expect("follow"), 1);
    let partial = state(&work).await;
    let turn = &partial["conversations"][0]["turns"][0];
    assert_eq!(turn["state"], "answering");
    assert_eq!(turn["answer"], "Looking at today's sessions.");
    assert!(turn.get("usage").is_none(), "shown once it has ended");

    let unknown = SessionId::new();
    runner.write(
        session,
        vec![
            Item::Said(format!(
                "- @writer worked on **PAP-1** in ses_{SES1}; see recap:wst_{SUBMISSION}@2026-09-30.\n\
                 - ses_{} is not known, nor is ZZZ-99.\n\n\
                 Suggestion: move PAP-1 to review\n\
                 Suggestion: open wst_{SUBMISSION}\n\
                 Suggestion: move ZZZ-99 to done",
                unknown.0
            )),
            Item::End,
        ],
    );
    clock.fetch_add(12_000, Ordering::SeqCst);
    assert_eq!(work.follow_orchestrator().expect("follow"), 0);
    let answered = state(&work).await;
    let conversation = &answered["conversations"][0];
    let turn = &conversation["turns"][0];
    assert_eq!(turn["state"], "answered");
    let answer = turn["answer"].as_str().expect("answer");
    assert!(answer.starts_with("Looking at today's sessions.\n\n- @writer worked on **PAP-1**"));
    assert!(
        !answer.contains("Suggestion: move PAP-1"),
        "a suggestion is not text"
    );
    assert!(
        answer.ends_with("Suggestion: move ZZZ-99 to done"),
        "one naming nothing known stays text"
    );
    assert_eq!(
        turn["references"],
        json!([
            {"text": "PAP-1", "target": {"kind": "task", "id": "01JB000000000000000TSK0001", "key": "PAP-1"},
             "label": "PAP-1 Draft the method section"},
            {"text": format!("ses_{SES1}"), "target": {"kind": "session", "id": SES1},
             "label": "Draft method section"},
            {"text": format!("recap:wst_{SUBMISSION}@2026-09-30"),
             "target": {"kind": "recap", "project": PAPER, "workstream": SUBMISSION, "date": "2026-09-30"},
             "label": "Recap of Submission, 2026-09-30"},
        ])
    );
    assert_eq!(
        turn["suggestions"],
        json!([
            {"kind": "move_task", "task": "01JB000000000000000TSK0001", "key": "PAP-1",
             "to": "review", "label": "Move PAP-1 to review"},
            {"kind": "open", "target": {"kind": "workstream", "id": SUBMISSION, "project": PAPER},
             "label": "Open Submission"},
        ])
    );
    assert_eq!(turn["usage"]["tool_runs"], 1);
    assert_eq!(turn["usage"]["duration_ms"], 12_000);
    assert_eq!(turn["usage"]["answer_bytes"], answer.len());
    assert_eq!(turn["ended"], 1_790_900_012_000_i64);
    // Suggestions are data: the task did not move, and the log has nothing new but the session.
    assert_eq!(
        work.task(&pitcrew_hub_work::TaskRef::parse("PAP-1").expect("key"))
            .expect("task")
            .status,
        pitcrew_protocol::model::TaskStatus::InProgress
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev + 1);
}

#[tokio::test]
async fn follow_ups_type_into_the_live_session_and_a_new_conversation_ends_the_old_one() {
    let tmp = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Runner::default());
    let clock = Arc::new(AtomicI64::new(1_790_900_000_000));
    let work = service(tmp.path(), &runner, &clock);
    let first = ask(&work, json!({"text": "What is blocked?", "engine": OTHER})).await;
    expect(&first, 202);
    let id = first.1["id"].as_str().expect("id").to_owned();
    let session = session_of(&first.1);
    assert_eq!(
        serde_json::to_value(runner.starts()[0].engine).expect("engine"),
        OTHER
    );
    runner.write(
        session,
        vec![
            Item::Prompt("the prompt".into()),
            Item::Said("PAP-4 waits for a review.".into()),
            Item::End,
        ],
    );
    work.follow_orchestrator().expect("follow");

    // A follow-up: one line, typed into the live session; its answer is after its own prompt.
    let res = ask(
        &work,
        json!({"text": "And\nyesterday?", "conversation": id, "engine": "claude"}),
    )
    .await;
    expect(&res, 202);
    assert_eq!(res.1["engine"], OTHER, "a follow-up keeps its engine");
    assert_eq!(runner.starts().len(), 1, "no new session");
    assert_eq!(
        runner.commands(),
        vec![RunnerCommand::SendText {
            session,
            text: "And yesterday?".into()
        }]
    );
    assert_eq!(res.1["turns"][1]["question"], "And yesterday?");
    runner.write(
        session,
        vec![
            Item::Prompt("And yesterday?".into()),
            Item::Said("Nothing.".into()),
            Item::End,
        ],
    );
    work.follow_orchestrator().expect("follow");
    let now = state(&work).await;
    assert_eq!(now["engine"], OTHER, "remembered");
    let turns = &now["conversations"][0]["turns"];
    assert_eq!(turns[0]["answer"], "PAP-4 waits for a review.");
    assert_eq!(turns[1]["state"], "answered");
    assert_eq!(turns[1]["answer"], "Nothing.");

    // A follow-up the CLI would read as a command is typed as a question.
    let res = ask(&work, json!({"text": "/clear", "conversation": id})).await;
    expect(&res, 202);
    assert_eq!(
        runner.commands().last(),
        Some(&RunnerCommand::SendText {
            session,
            text: "Q: /clear".into()
        })
    );
    runner.write(
        session,
        vec![
            Item::Prompt("Q: /clear".into()),
            Item::Said("No.".into()),
            Item::End,
        ],
    );
    work.follow_orchestrator().expect("follow");

    // A new conversation finishes the old one's session (its token stops, its CLI ends), and
    // remembers the engine asked for.
    let second = ask(
        &work,
        json!({"text": "Which session touched method.tex?", "engine": "claude"}),
    )
    .await;
    expect(&second, 202);
    assert_eq!(runner.finished(), vec![session]);
    assert_eq!(runner.starts().len(), 2);
    let now = state(&work).await;
    assert_eq!(now["engine"], "claude");
    assert_eq!(
        now["conversations"][0]["id"], second.1["id"],
        "newest first"
    );
    end_session(&work, session);
    let now = state(&work).await;
    assert!(
        now["conversations"][1].get("session").is_none(),
        "its session ended"
    );

    // A follow-up in a conversation whose session ended starts a new one, with the conversation
    // so far as data.
    let second_session = session_of(&second.1);
    runner.write(
        second_session,
        vec![
            Item::Prompt("p".into()),
            Item::Said("ses_x".into()),
            Item::End,
        ],
    );
    work.follow_orchestrator().expect("follow");
    let res = ask(
        &work,
        json!({"text": "And before that?", "conversation": id}),
    )
    .await;
    expect(&res, 202);
    let starts = runner.starts();
    assert_eq!(starts.len(), 3);
    let brief = &starts[2].brief;
    assert_eq!(
        serde_json::to_value(starts[2].engine).expect("engine"),
        OTHER
    );
    assert!(brief.contains("<earlier>\nQ: What is blocked?\nA: PAP-4 waits for a review.\n"));
    assert!(brief.ends_with("And before that?\n"));
    assert_ne!(session_of(&res.1), session);
    // The newer conversation's session finished, as only one runs at a time.
    assert_eq!(runner.finished().last(), Some(&second_session));
}

#[tokio::test]
async fn bounds_one_answer_at_a_time_size_time_and_an_ended_session() {
    let tmp = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Runner::default());
    let clock = Arc::new(AtomicI64::new(1_790_900_000_000));
    let work = service(tmp.path(), &runner, &clock);
    for bad in [
        json!({"text": " \u{7} "}),
        json!({"text": "x".repeat(4001)}),
        json!([1]),
        json!({}),
    ] {
        expect(&ask(&work, bad).await, 400);
    }
    expect(&ask(&work, json!({"text": "x".repeat(4000)})).await, 202);
    let busy = ask(&work, json!({"text": "Another?"})).await;
    expect(&busy, 409);

    // Past its size: cut, too_long, and the CLI gets Esc.
    let session = session_of(&state(&work).await["conversations"][0]);
    runner.write(
        session,
        vec![Item::Prompt("p".into()), Item::Said("é".repeat(9000))],
    );
    assert_eq!(work.follow_orchestrator().expect("follow"), 0);
    let turn = state(&work).await["conversations"][0]["turns"][0].clone();
    assert_eq!(turn["state"], "too_long");
    assert!(turn["answer"].as_str().expect("a").len() <= 16 * 1024);
    assert!(turn["note"].as_str().expect("note").contains("16 KiB"));
    assert_eq!(
        runner.commands().last(),
        Some(&RunnerCommand::Interrupt { session })
    );

    // Past its time: timed_out, Esc.
    let res = ask(&work, json!({"text": "Slow?"})).await;
    expect(&res, 202);
    let slow = session_of(&res.1);
    clock.fetch_add(301_000, Ordering::SeqCst);
    assert_eq!(work.follow_orchestrator().expect("follow"), 0);
    let turn = state(&work).await["conversations"][0]["turns"][0].clone();
    assert_eq!(turn["state"], "timed_out");
    assert_eq!(turn["usage"]["duration_ms"], 301_000);
    assert!(turn["note"].as_str().expect("note").contains("300 seconds"));
    assert_eq!(
        runner.commands().last(),
        Some(&RunnerCommand::Interrupt { session: slow })
    );

    // Its session ends first: failed.
    let res = ask(&work, json!({"text": "Gone?"})).await;
    expect(&res, 202);
    end_session(&work, session_of(&res.1));
    work.follow_orchestrator().expect("follow");
    let turn = state(&work).await["conversations"][0]["turns"][0].clone();
    assert_eq!(turn["state"], "failed");
    assert_eq!(turn["note"], "Its session ended before it answered.");

    // At most twenty questions in a conversation.
    let res = ask(&work, json!({"text": "Q0"})).await;
    let id = res.1["id"].as_str().expect("id").to_owned();
    let session = session_of(&res.1);
    for i in 0..20 {
        runner.write(
            session,
            vec![
                Item::Prompt(format!("Q{i}")),
                Item::Said("A".into()),
                Item::End,
            ],
        );
        work.follow_orchestrator().expect("follow");
        let next = ask(
            &work,
            json!({"text": format!("Q{}", i + 1), "conversation": id}),
        )
        .await;
        if i < 19 {
            expect(&next, 202);
        } else {
            expect(&next, 409);
            assert!(next.1["message"].as_str().expect("m").contains("new one"));
        }
    }
}

#[tokio::test]
async fn cancel_stops_the_answer_and_clear_forgets_and_ends() {
    let tmp = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Runner::default());
    let clock = Arc::new(AtomicI64::new(1_790_900_000_000));
    let work = service(tmp.path(), &runner, &clock);
    let res = ask(&work, json!({"text": "What is blocked?", "engine": OTHER})).await;
    expect(&res, 202);
    let id = res.1["id"].as_str().expect("id").to_owned();
    let session = session_of(&res.1);
    let cancel = format!("/v1/orchestrator/conversations/{id}/cancel");
    let app = app(&work);
    let canceled = call(&app, Some(person(SAM)), "POST", &cancel, None).await;
    expect(&canceled, 200);
    assert_eq!(canceled.1["turns"][0]["state"], "canceled");
    assert_eq!(
        runner.commands(),
        vec![RunnerCommand::Interrupt { session }]
    );
    expect(
        &call(&app, Some(person(SAM)), "POST", &cancel, None).await,
        409,
    );
    let missing = format!(
        "/v1/orchestrator/conversations/cnv_{}/cancel",
        SessionId::new().0
    );
    expect(
        &call(&app, Some(person(SAM)), "POST", &missing, None).await,
        404,
    );
    // Late text of the canceled turn changes nothing.
    runner.write(
        session,
        vec![
            Item::Prompt("p".into()),
            Item::Said("late".into()),
            Item::End,
        ],
    );
    work.follow_orchestrator().expect("follow");
    assert_eq!(
        state(&work).await["conversations"][0]["turns"][0]["answer"],
        ""
    );

    let cleared = call(
        &app,
        Some(person(SAM)),
        "DELETE",
        "/v1/orchestrator/conversations",
        None,
    )
    .await;
    assert_eq!(cleared.0, 204);
    assert_eq!(
        runner.finished(),
        vec![session],
        "its token stops, its CLI ends"
    );
    let now = state(&work).await;
    assert_eq!(now["conversations"], json!([]));
    assert_eq!(now["engine"], OTHER, "the engine stays remembered");
    // Still known as theirs: its transcript stays theirs alone, and it gets no agent's token.
    assert_eq!(
        work.orchestrator_asker(&session),
        Some(SAM.parse().expect("sam"))
    );
    let file = std::fs::read_to_string(tmp.path().join("orchestrator.json")).expect("file");
    assert!(
        !file.contains("What is blocked?"),
        "a clear forgets the questions"
    );
    // Clearing again finishes nothing more: the session is not a conversation's any more.
    expect(
        &call(
            &app,
            Some(person(SAM)),
            "DELETE",
            "/v1/orchestrator/conversations",
            None,
        )
        .await,
        204,
    );
    assert_eq!(runner.finished(), vec![session]);
}

#[tokio::test]
async fn who_may_ask_and_what_refuses_a_question() {
    let tmp = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Runner::default());
    let clock = Arc::new(AtomicI64::new(1_790_900_000_000));
    let work = service(tmp.path(), &runner, &clock);
    let app = app(&work);
    // People only: agents and readers are refused on every route.
    let id = format!("cnv_{}", SessionId::new().0);
    for caller in [agent(WRITER), reader(OFFICE)] {
        for (method, path, body) in [
            ("GET", "/v1/orchestrator".to_owned(), None),
            (
                "POST",
                "/v1/orchestrator/questions".to_owned(),
                Some(json!({"text": "Hi"})),
            ),
            (
                "POST",
                format!("/v1/orchestrator/conversations/{id}/cancel"),
                None,
            ),
            ("DELETE", "/v1/orchestrator/conversations".to_owned(), None),
        ] {
            expect(&call(&app, Some(caller), method, &path, body).await, 403);
        }
    }
    // Another person's conversation is not found; a person not owning the agent is refused.
    let mine = ask(&work, json!({"text": "Mine?"})).await;
    expect(&mine, 202);
    let lee = "01JB000000000000000MEM0007";
    let follow = call(
        &app,
        Some(person(lee)),
        "POST",
        "/v1/orchestrator/questions",
        Some(json!({"text": "Theirs?", "conversation": mine.1["id"]})),
    )
    .await;
    expect(&follow, 404);
    let theirs = call(
        &app,
        Some(person(lee)),
        "POST",
        "/v1/orchestrator/questions",
        Some(json!({"text": "Theirs?", "agent": WRITER})),
    )
    .await;
    expect(&theirs, 403);
    let none = call(
        &app,
        Some(person(lee)),
        "POST",
        "/v1/orchestrator/questions",
        Some(json!({"text": "No office?"})),
    )
    .await;
    expect(&none, 400);
    assert!(
        none.1["message"]
            .as_str()
            .expect("m")
            .contains("back office")
    );
    expect(
        &ask(&work, json!({"text": "A person?", "agent": SAM})).await,
        400,
    );
    let lee_view = call(&app, Some(person(lee)), "GET", "/v1/orchestrator", None).await;
    assert_eq!(lee_view.1["conversations"], json!([]));

    // An engine not offered (Codex: its read-only sandbox keeps `pitcrew` from the hub); one that
    // is not installed; a start the runner refuses.
    let tmp = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Runner::default());
    runner.missing.lock().expect("missing").push(Engine::Claude);
    let work = service(tmp.path(), &runner, &clock);
    let res = ask(&work, json!({"text": "Hi", "engine": "codex"})).await;
    expect(&res, 400);
    assert!(
        res.1["message"]
            .as_str()
            .expect("m")
            .contains("Codex cannot answer")
    );
    let res = ask(&work, json!({"text": "Hi", "engine": "claude"})).await;
    expect(&res, 409);
    assert!(
        res.1["message"]
            .as_str()
            .expect("m")
            .contains("Claude Code is not installed")
    );
    assert!(runner.starts().is_empty());
    runner.missing.lock().expect("missing").clear();
    *runner.fail.lock().expect("fail") = Some(DispatchError::Rejected("synthetic refusal".into()));
    let res = ask(&work, json!({"text": "Hi"})).await;
    expect(&res, 409);
    let failed = state(&work).await;
    let turn = &failed["conversations"][0]["turns"][0];
    assert_eq!(turn["state"], "failed");
    assert!(
        turn["note"]
            .as_str()
            .expect("note")
            .contains("synthetic refusal")
    );
    let session: SessionId = turn["session"].as_str().expect("s").parse().expect("id");
    assert_eq!(
        work.session(&session).expect("session").state,
        pitcrew_protocol::model::SessionState::Ended,
        "a session that did not start is ended"
    );

    // No runner link: nothing can start.
    let tmp = tempfile::tempdir().expect("tmp");
    let demo = demo();
    let bare = Arc::new(
        WorkService::new(open(&tmp.path().join("hub.db")), demo.workspace.clone())
            .with_hub_machine(LAPTOP.parse().expect("machine")),
    );
    bare.seed(&demo).expect("seed");
    expect(&ask(&bare, json!({"text": "Hi"})).await, 503);
    let view = state(&bare).await;
    assert!(
        view["engines"]
            .as_array()
            .expect("e")
            .iter()
            .all(|e| e["installed"] == false)
    );
}

#[tokio::test]
async fn conversations_survive_a_restart_in_a_private_file() {
    let tmp = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Runner::default());
    let clock = Arc::new(AtomicI64::new(1_790_900_000_000));
    let session = {
        let work = service(tmp.path(), &runner, &clock);
        let res = ask(&work, json!({"text": "Kept?"})).await;
        expect(&res, 202);
        session_of(&res.1)
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(tmp.path().join("orchestrator.json"))
            .expect("file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "private to its user");
    }
    // The same store and file, opened again: the answer is still followed.
    let demo = demo();
    let now = Arc::clone(&clock);
    let work = Arc::new(
        WorkService::new(open(&tmp.path().join("hub.db")), demo.workspace.clone())
            .with_hub_machine(LAPTOP.parse().expect("machine"))
            .with_dispatcher(Arc::clone(&runner) as Arc<dyn Dispatcher>)
            .with_clock(Arc::new(move || now.load(Ordering::SeqCst)))
            .with_orchestrator_file(tmp.path().join("orchestrator.json"))
            .expect("the file"),
    );
    assert!(work.is_orchestrator_session(&session));
    runner.write(
        session,
        vec![
            Item::Prompt("p".into()),
            Item::Said("Yes.".into()),
            Item::End,
        ],
    );
    assert_eq!(work.follow_orchestrator().expect("follow"), 0);
    let view = state(&work).await;
    assert_eq!(view["conversations"][0]["turns"][0]["answer"], "Yes.");
    // When the hub restarts, the sessions still running end (their tokens were in memory), and
    // their runs are finished; the turn one was answering fails. (This stand-in runner reports no
    // end of its own, so the first session, finished when the second started, runs on too.)
    let ask_again = ask(&work, json!({"text": "And now?"})).await;
    expect(&ask_again, 202);
    let running = session_of(&ask_again.1);
    assert_eq!(
        work.end_orchestrator_sessions("synthetic restart")
            .expect("ended"),
        2
    );
    assert_eq!(
        work.session(&session).expect("session").state,
        pitcrew_protocol::model::SessionState::Ended
    );
    assert_eq!(
        work.session(&running).expect("session").state,
        pitcrew_protocol::model::SessionState::Ended
    );
    assert!(runner.finished().contains(&running));
    work.follow_orchestrator().expect("follow");
    assert_eq!(
        state(&work).await["conversations"][0]["turns"][0]["state"],
        "failed"
    );
    assert_eq!(work.end_orchestrator_sessions("again").expect("none"), 0);
}

/// A file this hub cannot read (a newer version, a variant it does not know, or one that does not
/// parse) never stops the hub: it is moved aside, and the hub starts with no conversations.
#[tokio::test]
async fn an_unreadable_file_is_moved_aside_and_the_hub_starts_empty() {
    let demo = demo();
    for (i, text) in [
        "{\"version\": 9, \"people\": []}".to_owned(),
        format!(
            "{{\"version\": 1, \"people\": [{{\"member\": \"{SAM}\", \"conversations\": [\
             {{\"id\": \"01JB000000000000000CNV0001\", \"engine\": \"claude\", \
             \"agent\": \"{OFFICE}\", \"started\": 1, \"turns\": [{{\"turn\": {{\
             \"question\": \"q\", \"asked\": 1, \"session\": \"{SES1}\", \
             \"state\": \"pondering\", \"answer\": \"\", \"references\": [], \
             \"suggestions\": []}}}}]}}]}}]}}"
        ),
        "{ not json".to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("orchestrator.json");
        std::fs::write(&file, &text).expect("w");
        let work = WorkService::new(open(&tmp.path().join("hub.db")), demo.workspace.clone())
            .with_orchestrator_file(file.clone())
            .unwrap_or_else(|e| panic!("case {i}: {e}"));
        assert!(!file.exists(), "case {i}: moved aside");
        let aside: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("name"))
            .filter(|n| n.starts_with("orchestrator.json.unreadable-"))
            .collect();
        assert_eq!(aside.len(), 1, "case {i}: {aside:?}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(&aside[0])).expect("r"),
            text,
            "case {i}: kept as it was"
        );
        let view = work
            .orchestrator(&person(SAM))
            .unwrap_or_else(|e| panic!("case {i}: {e}"));
        assert!(view.conversations.is_empty(), "case {i}");
    }
}

/// The token an Orchestrator session's CLI gets reads the work and changes none of it: the reads
/// marked **read** and **agent** answer it; every write refuses it (`403`), the agent writes
/// included, and the service's commands refuse it too, whatever route reaches them.
#[tokio::test]
async fn a_reader_reads_and_changes_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    let work = common::seeded(tmp.path());
    let app = common::app(&work);
    let me = reader(OFFICE);
    for path in [
        "/v1/me",
        "/v1/members",
        "/v1/workspace",
        "/v1/machines",
        "/v1/personas",
        "/v1/teams",
        "/v1/projects",
        &format!("/v1/projects/{PAPER}"),
        "/v1/workstreams",
        &format!("/v1/workstreams/{SUBMISSION}"),
        "/v1/tasks",
        "/v1/tasks/PAP-1",
        "/v1/sessions",
        &format!("/v1/sessions/{SES1}"),
        "/v1/asks",
        "/v1/briefs",
    ] {
        expect(&common::get(&app, me, path).await, 200);
    }
    let before = work.store().latest_rev().expect("rev");
    for (method, path, body) in [
        (
            "POST",
            "/v1/tasks/PAP-1/move".to_owned(),
            json!({"to": "review"}),
        ),
        ("PUT", "/v1/tasks/PAP-1/subtasks".to_owned(), json!([])),
        (
            "POST",
            "/v1/tasks/PAP-1/comments".to_owned(),
            json!({"text": "x", "mentions": []}),
        ),
        (
            "POST",
            "/v1/asks".to_owned(),
            json!({"kind": "question", "to": SAM, "title": "x"}),
        ),
        (
            "POST",
            "/v1/asks/01JB000000000000000ASK0001/answer".to_owned(),
            json!({"text": "x"}),
        ),
        (
            "POST",
            "/v1/tasks".to_owned(),
            json!({"project": PAPER, "title": "x"}),
        ),
        ("PATCH", "/v1/tasks/PAP-1".to_owned(), json!({"title": "x"})),
        (
            "POST",
            "/v1/projects".to_owned(),
            json!({"key": "XYZ", "name": "x"}),
        ),
        (
            "POST",
            format!("/v1/sessions/{SES1}/link"),
            json!({"workstream": SUBMISSION}),
        ),
        (
            "PUT",
            "/v1/me/cursors/workspace".to_owned(),
            json!({"rev": 1}),
        ),
        ("GET", "/v1/me/cursors".to_owned(), Value::Null),
        ("GET", "/v1/safety".to_owned(), Value::Null),
    ] {
        let body = (!body.is_null()).then_some(body);
        expect(&call(&app, Some(me), method, &path, body).await, 403);
    }
    assert_eq!(
        work.store().latest_rev().expect("rev"),
        before,
        "nothing appended"
    );
    // The commands themselves refuse it, whatever route reaches them.
    let task = pitcrew_hub_work::TaskRef::parse("PAP-1").expect("key");
    let refused = work
        .move_task(&me, &task, pitcrew_protocol::model::TaskStatus::Review)
        .expect_err("a reader moves nothing");
    assert_eq!(refused.code(), pitcrew_protocol::api::ErrorCode::Forbidden);
    assert!(work.check_task_write(&me, &task).is_err());
    // An outward write's retry too, before it looks the write up.
    let refused = work
        .request_retry(&me, &pitcrew_protocol::ids::AskId::new())
        .expect_err("a reader retries no write");
    assert_eq!(refused.code(), pitcrew_protocol::api::ErrorCode::Forbidden);
    // An agent may not make the reads marked **read**.
    expect(&common::get(&app, agent(WRITER), "/v1/sessions").await, 403);
}
