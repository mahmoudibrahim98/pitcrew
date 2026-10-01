//! Acceptance: the back office acting on the hub through `pitcrew_office::Commands`.
//!
//! - a finished dispatch moves its task to review;
//! - an action the hub refuses is refused, and logged in the run log as refused;
//! - nothing bypasses `can_move`: the hub re-checks every move, whatever the action says;
//! - answers, asks, comments and brief proposals are re-validated like any caller's.

mod common;

use common::{PAPER, SAM, SEED_RUNS, SUBMISSION, WRITER, agent, demo, member, person};
use pitcrew_hub_work::{
    BackOffice, NewAsk, NewTask, OfficeRun, TaskPatch, TaskRef, WorkError, WorkService,
    projection::NAMES, projections, projections_with_office,
};
use pitcrew_office::{
    Action, ApplyError, AskDraft, Commands, Config, Context, Entry, Outcome, RUN_LOG, Refusal,
    Rule, read_runs,
};
use pitcrew_protocol::api::{Caller, ErrorCode};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{AskId, DispatchId, EventId, MemberId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    Answer, AskKind, AskState, BriefSource, BriefTarget, DispatchOutcome, Member, MemberKind,
    Mover, Receipt, TaskStatus,
};
use pitcrew_recap::{BriefProposal, Disposition, propose_paused};
use pitcrew_store::{RevRange, Store, StoreOptions};
use std::path::Path;
use std::sync::Arc;

/// @office, the back office's agent, owned by @sam.
const OFFICE: &str = "01JB000000000000000MEM0006";
const PAP1: &str = "01JB000000000000000TSK0001";
const PAP3: &str = "01JB000000000000000TSK0003";
const PAP5: &str = "01JB000000000000000TSK0005";
/// PAP-1's active dispatch (@writer, session 1).
const DSP1: &str = "01JB000000000000000DSP0001";
/// PAP-3's finished dispatch.
const DSP4: &str = "01JB000000000000000DSP0004";
/// An open question from @writer to @sam.
const ASK1: &str = "01JB000000000000000ASK0001";
/// An open decision for @sam.
const ASK2: &str = "01JB000000000000000ASK0002";

const STATUSES: [TaskStatus; 6] = [
    TaskStatus::Backlog,
    TaskStatus::Todo,
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Done,
    TaskStatus::Canceled,
];

fn office() -> BackOffice {
    BackOffice::new(member(OFFICE))
}

/// A seeded hub whose store keeps `office`'s run log, with the office already run over the seed.
fn hub_with(dir: &Path, office: &BackOffice) -> Arc<WorkService> {
    let store = Arc::new(
        Store::open_with(
            dir.join("hub.db"),
            StoreOptions::default(),
            projections_with_office(office),
        )
        .expect("open"),
    );
    let demo = demo();
    let work = Arc::new(WorkService::new(store, demo.workspace.clone()));
    let seeded = work.seed(&demo).expect("seed");
    work.run_office(office, seeded)
        .expect("office over the seed");
    work
}

/// Appends one event as the runner link or a racing writer would, and returns its revisions.
fn append(
    work: &WorkService,
    author: &str,
    on_behalf_of: Option<&str>,
    body: EventBody,
) -> RevRange {
    let event = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: work.workspace(),
        author: member(author),
        on_behalf_of: on_behalf_of.map(member),
        body,
    };
    work.store().append(&[event]).expect("append")
}

fn status(work: &WorkService, task: &str) -> TaskStatus {
    work.task(&TaskRef::parse(task).expect("ref"))
        .expect("task")
        .status
}

fn events_since(work: &WorkService, rev: u64) -> Vec<Event> {
    work.store()
        .since(rev, usize::MAX)
        .expect("log")
        .into_iter()
        .map(|e| e.event)
        .collect()
}

fn runs(work: &WorkService, after: u64) -> Vec<Entry> {
    work.read(|c| read_runs(c, after, usize::MAX).map_err(|e| WorkError::internal(e.to_string())))
        .expect("run log")
}

fn evidence() -> Vec<Receipt> {
    vec![Receipt::Event { id: EventId::new() }]
}

fn code<T: std::fmt::Debug>(result: Result<T, WorkError>) -> ErrorCode {
    result.expect_err("refused").code()
}

#[test]
fn the_run_log_is_registered_with_the_offices_settings() {
    let office = office();
    assert_eq!(office.member(), member(OFFICE));
    assert_eq!(office.config().office, Some(member(OFFICE)));
    let names: Vec<String> = projections_with_office(&office)
        .iter()
        .map(|p| p.name().to_owned())
        .collect();
    assert_eq!(names.len(), NAMES.len() + 1);
    assert!(names.iter().any(|n| n == RUN_LOG));
    // Other settings keep their values; the member is always the office's.
    let config = Config {
        per_rule_per_hour: 3,
        office: Some(member(SAM)),
        ..Config::default()
    };
    let office = BackOffice::with_config(member(OFFICE), config);
    assert_eq!(office.config().per_rule_per_hour, 3);
    assert_eq!(office.config().office, Some(member(OFFICE)));
}

#[test]
fn a_finished_dispatch_moves_its_task_to_review() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = office();
    let work = hub_with(dir.path(), &office);
    assert_eq!(status(&work, "PAP-1"), TaskStatus::InProgress);
    let finished = append(
        &work,
        WRITER,
        Some(SAM),
        EventBody::DispatchFinished {
            dispatch: DSP1.parse().expect("dispatch"),
            outcome: DispatchOutcome::Succeeded,
            summary: Some("§3.2 is drafted.".into()),
        },
    );
    let run = work.run_office(&office, finished).expect("run");
    let applied: Vec<&str> = run.applied().map(|a| a.entry.rule.as_str()).collect();
    assert_eq!(applied, ["dispatch_to_review"], "{run:?}");
    assert_eq!(run.refused().count(), 0);
    assert_eq!(status(&work, "PAP-1"), TaskStatus::Review);

    // The move is the back office's, authored by its member on behalf of its owner.
    let moved = events_since(&work, finished.to_rev);
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(moved[0].author, member(OFFICE));
    assert_eq!(moved[0].on_behalf_of, Some(member(SAM)));
    assert_eq!(
        moved[0].body,
        EventBody::TaskMoved {
            task: PAP1.parse().expect("task"),
            from: TaskStatus::InProgress,
            to: TaskStatus::Review,
            mover: Mover::BackOffice { accept_auto: false },
        }
    );
    // The run log says what the office did, with its evidence.
    let logged = runs(&work, finished.from_rev - 1);
    let entry = logged
        .iter()
        .find(|e| e.rev == finished.to_rev && e.rule == "dispatch_to_review")
        .expect("logged");
    assert_eq!(entry.outcome, Outcome::Emitted);

    // Running the office over its own move does nothing more, and nothing twice.
    let own = RevRange {
        from_rev: finished.to_rev + 1,
        to_rev: work.store().latest_rev().expect("rev"),
    };
    let again = work.run_office(&office, own).expect("run");
    assert!(
        again
            .actions
            .iter()
            .all(|a| a.entry.rule != "dispatch_to_review"),
        "{again:?}"
    );
    // Running the same range again (as after a restart) appends nothing: the move is replayed.
    let rev = work.store().latest_rev().expect("rev");
    let replay = work.run_office(&office, finished).expect("replay");
    let replayed: Vec<&str> = replay.replayed().map(|a| a.entry.rule.as_str()).collect();
    assert_eq!(replayed, ["dispatch_to_review"], "{replay:?}");
    assert_eq!(replay.applied().count(), 0);
    assert_eq!(replay.refused().count(), 0);
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    assert_eq!(status(&work, "PAP-1"), TaskStatus::Review);
}

/// A rule that does whatever a comment saying "reckless" asks: four things the office must never
/// do, and one it may.
struct Reckless;

impl Rule for Reckless {
    fn name(&self) -> &'static str {
        "reckless"
    }

    fn on_event(&mut self, ctx: &mut Context<'_>, event: &Event) -> Vec<Action> {
        let EventBody::CommentPosted { text, .. } = &event.body else {
            return Vec::new();
        };
        if text != "reckless" || ctx.from_office(event) {
            return Vec::new();
        }
        reckless_actions(event)
    }
}

fn reckless_actions(event: &Event) -> Vec<Action> {
    let because = vec![Receipt::Event { id: event.id }];
    vec![
        // PAP-3 is in review and does not allow automatic acceptance.
        Action::Append {
            body: EventBody::TaskMoved {
                task: PAP3.parse().expect("task"),
                from: TaskStatus::Review,
                to: TaskStatus::Done,
                mover: Mover::BackOffice { accept_auto: true },
            },
            because: because.clone(),
        },
        // PAP-5 is todo: the back office never starts work.
        Action::Append {
            body: EventBody::TaskMoved {
                task: PAP5.parse().expect("task"),
                from: TaskStatus::Todo,
                to: TaskStatus::InProgress,
                mover: Mover::BackOffice { accept_auto: false },
            },
            because: because.clone(),
        },
        // A decision addressed to @sam.
        Action::Append {
            body: EventBody::AskAnswered {
                ask: ASK2.parse().expect("ask"),
                answer: Answer {
                    by: member(OFFICE),
                    option: Some(0),
                    text: None,
                    at: event.at,
                },
            },
            because: because.clone(),
        },
        // An approval is how an outward write is requested.
        Action::RaiseAsk {
            ask: AskDraft {
                kind: AskKind::Approval,
                to: member(SAM),
                task: None,
                session: None,
                title: "Push to GitHub?".into(),
                body: String::new(),
                options: Vec::new(),
                receipts: because.clone(),
            },
        },
        // Allowed: a comment on PAP-1.
        Action::Append {
            body: EventBody::CommentPosted {
                task: Some(PAP1.parse().expect("task")),
                workstream: None,
                text: "Noted.".into(),
                mentions: Vec::new(),
            },
            because,
        },
    ]
}

fn reckless_office() -> BackOffice {
    BackOffice::with_rules(member(OFFICE), Config::default(), || {
        vec![Box::new(Reckless) as Box<dyn Rule>]
    })
}

fn comment(work: &WorkService, text: &str) -> RevRange {
    append(
        work,
        SAM,
        None,
        EventBody::CommentPosted {
            task: Some(PAP1.parse().expect("task")),
            workstream: None,
            text: text.into(),
            mentions: Vec::new(),
        },
    )
}

#[test]
fn actions_the_hub_refuses_are_refused_and_logged_as_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = reckless_office();
    let work = hub_with(dir.path(), &office);
    let trigger = comment(&work, "reckless");
    let run = work.run_office(&office, trigger).expect("run");

    // The run log refused the four, by the "never" list and the move rules...
    let logged = runs(&work, trigger.from_rev - 1);
    let outcomes: Vec<Outcome> = logged.iter().map(|e| e.outcome).collect();
    let refused = |refusal| Outcome::Refused { refusal };
    assert_eq!(
        outcomes,
        [
            refused(Refusal::MarksDone),
            refused(Refusal::MoveNotAllowed),
            refused(Refusal::AnswersPerson),
            refused(Refusal::SendsOutward),
            Outcome::Emitted,
        ]
    );
    // ...so the hub applied only the comment.
    assert_eq!(run.actions.len(), 1);
    assert!(run.actions[0].result.is_ok());
    let appended = events_since(&work, trigger.to_rev);
    assert_eq!(appended.len(), 1);
    assert_eq!(appended[0].author, member(OFFICE));
    assert!(matches!(appended[0].body, EventBody::CommentPosted { .. }));
    assert_eq!(status(&work, "PAP-3"), TaskStatus::Review);
    assert_eq!(status(&work, "PAP-5"), TaskStatus::Todo);

    // The hub refuses each of the four itself, too: handed straight to its Commands, past the
    // office's guard (what a faulty office, or a crafted run log, would do).
    let since_trigger = events_since(&work, trigger.from_rev - 1);
    let event = &since_trigger[0];
    let actions = reckless_actions(event);
    let mut commands = work.office_commands(&office).expect("commands");
    let rev = work.store().latest_rev().expect("rev");
    let mut codes = Vec::new();
    for action in &actions[..4] {
        let result = match action {
            Action::Append { body, because } => commands.append(body, because),
            Action::RaiseAsk { ask } => commands.raise_ask(ask),
            Action::ProposeBrief { proposal } => commands.propose_brief(proposal),
        };
        codes.push(code(result));
    }
    assert_eq!(
        codes,
        [
            ErrorCode::Conflict,
            ErrorCode::Conflict,
            ErrorCode::Forbidden,
            ErrorCode::Forbidden
        ]
    );
    assert_eq!(
        work.store().latest_rev().expect("rev"),
        rev,
        "nothing appended"
    );
    // And `apply` alone, which checks only shape, still refuses the approval.
    let crafted: Vec<Entry> = actions
        .into_iter()
        .take(4)
        .enumerate()
        .map(|(i, action)| Entry {
            rev: trigger.to_rev,
            seq: u32::try_from(i).expect("seq"),
            event: event.id,
            at: event.at,
            rule: "crafted".into(),
            action,
            outcome: Outcome::Emitted,
        })
        .collect();
    let results = pitcrew_office::apply(&crafted, &mut commands);
    assert!(matches!(
        results[3],
        Err(ApplyError::Refused(Refusal::SendsOutward))
    ));
    assert!(results.iter().all(Result::is_err));
    assert_eq!(
        work.store().latest_rev().expect("rev"),
        rev,
        "nothing appended"
    );
}

#[test]
fn an_action_emitted_on_an_out_of_date_view_is_refused_by_the_hub() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = office();
    let work = hub_with(dir.path(), &office);
    // A racing writer's stale move: the hub ignores it (PAP-3 is in review, not todo), but the
    // office's own view of PAP-3 follows it to in progress.
    let stale = append(
        &work,
        SAM,
        None,
        EventBody::TaskMoved {
            task: PAP3.parse().expect("task"),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
            mover: Mover::Person,
        },
    );
    work.run_office(&office, stale).expect("run");
    assert_eq!(status(&work, "PAP-3"), TaskStatus::Review);
    // So when PAP-3's dispatch reports success, the office asks for in progress → review...
    let finished = append(
        &work,
        WRITER,
        Some(SAM),
        EventBody::DispatchFinished {
            dispatch: DSP4.parse().expect("dispatch"),
            outcome: DispatchOutcome::Succeeded,
            summary: None,
        },
    );
    let run: OfficeRun = work.run_office(&office, finished).expect("run");
    let entry = runs(&work, finished.from_rev - 1)
        .into_iter()
        .find(|e| e.rule == "dispatch_to_review")
        .expect("logged");
    assert_eq!(
        entry.outcome,
        Outcome::Emitted,
        "the office's view allowed it"
    );
    // ...and the hub, where PAP-3 is in review, refuses it and appends nothing.
    let refused: Vec<_> = run.refused().collect();
    assert_eq!(refused.len(), 1, "{run:?}");
    match &refused[0].result {
        Err(ApplyError::Failed(error)) => assert_eq!(error.code(), ErrorCode::Conflict),
        other => panic!("expected the hub's refusal, got {other:?}"),
    }
    assert_eq!(status(&work, "PAP-3"), TaskStatus::Review);
    assert!(events_since(&work, finished.to_rev).is_empty());
}

/// Every move the back office could ask for, on tasks in every status with and without automatic
/// acceptance, handed straight to the hub's Commands: only `can_move` for `Mover::BackOffice`
/// with the task's own policy gets through, whatever mover the action claims.
#[test]
fn nothing_bypasses_can_move() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = office();
    let work = hub_with(dir.path(), &office);
    let sam = person(SAM);
    let movers = [
        Mover::BackOffice { accept_auto: true },
        Mover::BackOffice { accept_auto: false },
        Mover::Person,
        Mover::Agent { on_own_task: true },
        Mover::Sync,
    ];
    let mut commands = work.office_commands(&office).expect("commands");
    let mut moved = 0;
    let mut checked = 0;
    for accept_auto in [false, true] {
        for from in STATUSES {
            for to in STATUSES {
                for mover in movers {
                    let task = work
                        .create_task(
                            &sam,
                            NewTask {
                                project: PAPER.parse().expect("project"),
                                workstream: None,
                                title: "Office rules".into(),
                                description: None,
                                status: Some(from),
                                priority: None,
                                assignee: None,
                                labels: None,
                                due: None,
                            },
                        )
                        .expect("create");
                    let task_ref = TaskRef::Id(task.id);
                    work.patch_task(
                        &sam,
                        &task_ref,
                        TaskPatch {
                            accept_auto: Some(accept_auto),
                            ..TaskPatch::default()
                        },
                    )
                    .expect("accept_auto");
                    let rev = work.store().latest_rev().expect("rev");
                    let body = EventBody::TaskMoved {
                        task: task.id,
                        from,
                        to,
                        mover,
                    };
                    let result = commands.append(&body, &evidence());
                    let ours = Mover::BackOffice { accept_auto };
                    let allowed =
                        matches!(mover, Mover::BackOffice { .. }) && from.can_move(to, ours);
                    let now = work.task(&task_ref).expect("task");
                    if allowed {
                        result.expect("allowed");
                        assert_eq!(now.status, to);
                        let event = events_since(&work, rev).pop().expect("event");
                        assert_eq!(
                            event.body,
                            EventBody::TaskMoved {
                                task: task.id,
                                from,
                                to,
                                mover: ours
                            },
                            "the mover carries the task's own policy"
                        );
                        assert_eq!(event.author, member(OFFICE));
                        moved += 1;
                    } else {
                        let error = result.expect_err("refused");
                        let expected = if matches!(mover, Mover::BackOffice { .. }) {
                            ErrorCode::Conflict
                        } else {
                            ErrorCode::Forbidden
                        };
                        assert_eq!(error.code(), expected, "{from:?}→{to:?} by {mover:?}");
                        assert_eq!(now.status, from);
                        assert_eq!(work.store().latest_rev().expect("rev"), rev);
                    }
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 2 * 6 * 6 * movers.len());
    // in_progress → review (both policies, both claims), review → done (automatic acceptance
    // only, both claims).
    assert_eq!(moved, 2 * 2 + 2);
    // A move from a status the task has left is refused too.
    let error = commands
        .append(
            &EventBody::TaskMoved {
                task: PAP3.parse().expect("task"),
                from: TaskStatus::InProgress,
                to: TaskStatus::Review,
                mover: Mover::BackOffice { accept_auto: false },
            },
            &evidence(),
        )
        .expect_err("stale");
    assert_eq!(error.code(), ErrorCode::Conflict);
    // And an action without evidence, or an unknown task.
    assert_eq!(
        code(commands.append(
            &EventBody::TaskMoved {
                task: PAP1.parse().expect("task"),
                from: TaskStatus::InProgress,
                to: TaskStatus::Review,
                mover: Mover::BackOffice { accept_auto: false },
            },
            &[],
        )),
        ErrorCode::Invalid
    );
    assert_eq!(
        code(commands.append(
            &EventBody::TaskMoved {
                task: TaskId::new(),
                from: TaskStatus::InProgress,
                to: TaskStatus::Review,
                mover: Mover::BackOffice { accept_auto: false },
            },
            &evidence(),
        )),
        ErrorCode::NotFound
    );
}

/// An ask raised by `by` to `to`.
fn ask_to(work: &WorkService, by: Caller, kind: AskKind, to: MemberId) -> String {
    work.raise_ask(
        &by,
        NewAsk {
            kind,
            to,
            title: "Which seed first?".into(),
            body: None,
            options: Some(vec!["Seed 3".into(), "Seed 5".into()]),
            task: None,
            session: None,
            receipts: None,
        },
    )
    .expect("ask")
    .id
    .to_string()
}

#[test]
fn the_office_answers_only_its_own_questions_and_mentions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = office();
    let work = hub_with(dir.path(), &office);
    let mut commands = work.office_commands(&office).expect("commands");
    let answer = |ask: &str, option: Option<usize>, text: Option<&str>| EventBody::AskAnswered {
        ask: ask.parse().expect("ask"),
        answer: Answer {
            by: member(SAM),
            option,
            text: text.map(str::to_owned),
            at: 0,
        },
    };
    // Never a person's ask: a question to @sam, a decision to @sam.
    for ask in [ASK1, ASK2] {
        assert_eq!(
            code(commands.append(&answer(ask, Some(0), None), &evidence())),
            ErrorCode::Forbidden
        );
    }
    // Nor another agent's, even one of @sam's like the office.
    let to_runner = ask_to(
        &work,
        agent(WRITER),
        AskKind::Question,
        member("01JB000000000000000MEM0003"),
    );
    assert_eq!(
        code(commands.append(&answer(&to_runner, Some(0), None), &evidence())),
        ErrorCode::Forbidden
    );
    // A question from @writer to the office itself.
    let id = ask_to(&work, agent(WRITER), AskKind::Question, member(OFFICE));
    let question: AskId = id.parse().expect("ask");
    assert_eq!(
        code(commands.append(&answer(&id, Some(2), None), &evidence())),
        ErrorCode::Invalid,
        "option out of range"
    );
    assert_eq!(
        code(commands.append(&answer(&id, None, Some("  ")), &evidence())),
        ErrorCode::Invalid,
        "no answer"
    );
    commands
        .append(
            &answer(&id, Some(0), Some("Seed 3, it diverged.")),
            &evidence(),
        )
        .expect("answered");
    let answered = work.ask(&question).expect("ask");
    assert_eq!(answered.state, AskState::Answered);
    let given = answered.answer.expect("answer");
    assert_eq!(given.by, member(OFFICE), "the office answers as itself");
    assert_eq!(given.option, Some(0));
    assert_eq!(
        code(commands.append(&answer(&id, Some(1), None), &evidence())),
        ErrorCode::Conflict,
        "already answered"
    );
    // A mention of the office, answered with text.
    let mention = ask_to(&work, person(SAM), AskKind::Mention, member(OFFICE));
    commands
        .append(&answer(&mention, None, Some("Seen.")), &evidence())
        .expect("answered");
    // Never a decision or a review, even one addressed to the office.
    for kind in [AskKind::Decision, AskKind::Review] {
        let ask = ask_to(&work, person(SAM), kind, member(OFFICE));
        assert_eq!(
            code(commands.append(&answer(&ask, Some(0), None), &evidence())),
            ErrorCode::Forbidden,
            "{kind:?}"
        );
    }
    // Nor the agent of another person.
    let alex = MemberId::new();
    let helper = MemberId::new();
    for m in [
        Member {
            id: alex,
            kind: MemberKind::Human,
            handle: "@alex".into(),
            name: "Alex".into(),
            owner: None,
            persona: None,
        },
        Member {
            id: helper,
            kind: MemberKind::Agent,
            handle: "@helper".into(),
            name: "Helper".into(),
            owner: Some(alex),
            persona: None,
        },
    ] {
        append(&work, SAM, None, EventBody::MemberAdded { member: m });
    }
    let other = work
        .raise_ask(
            &agent(WRITER),
            NewAsk {
                kind: AskKind::Question,
                to: helper,
                title: "Ping".into(),
                body: None,
                options: None,
                task: None,
                session: None,
                receipts: None,
            },
        )
        .expect("question");
    assert_eq!(
        code(commands.append(&answer(&other.id.to_string(), None, Some("x")), &evidence())),
        ErrorCode::Forbidden
    );
    assert_eq!(
        code(commands.append(
            &answer(&AskId::new().to_string(), None, Some("x")),
            &evidence()
        )),
        ErrorCode::NotFound
    );
}

#[test]
fn asks_comments_and_other_events_are_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = office();
    let work = hub_with(dir.path(), &office);
    let mut commands = work.office_commands(&office).expect("commands");
    let draft = |kind, to: MemberId, receipts: Vec<Receipt>| AskDraft {
        kind,
        to,
        task: Some(PAP1.parse().expect("task")),
        session: Some("01JB000000000000000SES0001".parse().expect("session")),
        title: "Tests failed 3 times: keep going?".into(),
        body: "The latest run: pytest".into(),
        options: vec!["Keep going".into(), "I'll step in".into()],
        receipts,
    };
    let rev = work.store().latest_rev().expect("rev");
    commands
        .raise_ask(&draft(AskKind::Decision, member(SAM), evidence()))
        .expect("raised");
    let raised = events_since(&work, rev).pop().expect("event");
    assert_eq!(raised.author, member(OFFICE));
    assert_eq!(raised.on_behalf_of, Some(member(SAM)));
    let EventBody::AskRaised { ask } = raised.body else {
        panic!("not an ask: {:?}", raised.body);
    };
    assert_eq!(ask.from, member(OFFICE));
    assert_eq!(ask.state, AskState::Open);
    assert_eq!(work.ask(&ask.id).expect("stored").title, ask.title);

    let rev = work.store().latest_rev().expect("rev");
    for (draft, expected) in [
        (
            draft(AskKind::Approval, member(SAM), evidence()),
            ErrorCode::Forbidden,
        ),
        (
            draft(AskKind::Decision, member(SAM), Vec::new()),
            ErrorCode::Invalid,
        ),
        (
            draft(AskKind::Decision, MemberId::new(), evidence()),
            ErrorCode::Invalid,
        ),
        (
            AskDraft {
                task: Some(TaskId::new()),
                ..draft(AskKind::Decision, member(SAM), evidence())
            },
            ErrorCode::Invalid,
        ),
        (
            AskDraft {
                title: " ".into(),
                ..draft(AskKind::Decision, member(SAM), evidence())
            },
            ErrorCode::Invalid,
        ),
    ] {
        assert_eq!(code(commands.raise_ask(&draft)), expected);
    }

    let comment = |task: Option<TaskId>,
                   workstream: Option<WorkstreamId>,
                   text: &str,
                   mentions: Vec<MemberId>| {
        EventBody::CommentPosted {
            task,
            workstream,
            text: text.into(),
            mentions,
        }
    };
    let pap1: TaskId = PAP1.parse().expect("task");
    for (body, expected) in [
        (
            comment(None, None, "Nowhere", Vec::new()),
            ErrorCode::Invalid,
        ),
        (
            comment(Some(pap1), None, " ", Vec::new()),
            ErrorCode::Invalid,
        ),
        (
            comment(Some(TaskId::new()), None, "Lost", Vec::new()),
            ErrorCode::NotFound,
        ),
        (
            comment(Some(pap1), None, "Who?", vec![MemberId::new()]),
            ErrorCode::Invalid,
        ),
        // PAP-1 is in the submission workstream, not seed runs.
        (
            comment(
                Some(pap1),
                Some(SEED_RUNS.parse().expect("ws")),
                "Elsewhere",
                Vec::new(),
            ),
            ErrorCode::Invalid,
        ),
        // Only moves, answers and comments; nothing else, however harmless it looks.
        (
            EventBody::TaskAssigned {
                task: pap1,
                assignee: None,
            },
            ErrorCode::Forbidden,
        ),
        (
            EventBody::BriefAccepted {
                target: BriefTarget::Project(PAPER.parse().expect("project")),
                text: "Done.".into(),
                next: None,
                pinned: false,
                receipts: evidence(),
            },
            ErrorCode::Forbidden,
        ),
    ] {
        assert_eq!(
            code(commands.append(&body, &evidence())),
            expected,
            "{body:?}"
        );
    }
    assert_eq!(
        work.store().latest_rev().expect("rev"),
        rev,
        "nothing appended"
    );
    commands
        .append(
            &comment(
                None,
                Some(SUBMISSION.parse().expect("ws")),
                "Quiet for a while.",
                vec![member(SAM)],
            ),
            &evidence(),
        )
        .expect("comment on a workstream");
    commands
        .append(
            &comment(
                Some(pap1),
                Some(SUBMISSION.parse().expect("ws")),
                "On PAP-1, in its workstream.",
                Vec::new(),
            ),
            &evidence(),
        )
        .expect("comment on a task and its workstream");
}

fn paused(workstream: &str) -> BriefProposal {
    propose_paused(
        workstream.parse().expect("ws"),
        EventId::new(),
        1_790_000_000_000,
        1_790_800_000_000,
        0,
    )
}

#[test]
fn brief_proposals_are_proposed_and_auto_accepted_only_when_unpinned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = office();
    let work = hub_with(dir.path(), &office);
    let mut commands = work.office_commands(&office).expect("commands");
    let brief = |id: &str| {
        work.briefs()
            .expect("briefs")
            .into_iter()
            .find(|b| match b.target {
                BriefTarget::Project(p) => p.to_string().ends_with(id),
                BriefTarget::Workstream(w) => w.to_string().ends_with(id),
            })
            .expect("brief")
    };

    // A proposal: pending, the brief in force unchanged.
    let before = brief(SUBMISSION);
    let proposal = paused(SUBMISSION);
    assert_eq!(proposal.disposition, Disposition::Propose);
    commands.propose_brief(&proposal).expect("proposed");
    let now = brief(SUBMISSION);
    assert_eq!(now.text, before.text);
    let pending = now.proposal.expect("pending");
    assert_eq!(pending.text, proposal.text());
    assert_eq!(pending.next.as_deref(), proposal.next_text());
    assert_eq!(pending.receipts, proposal.receipts);

    // Accepted automatically: the brief in force is the back office's, nothing pending.
    let mut auto = paused(SUBMISSION);
    auto.disposition = Disposition::AutoAccept;
    let rev = work.store().latest_rev().expect("rev");
    commands.propose_brief(&auto).expect("applied");
    let appended = events_since(&work, rev);
    assert_eq!(appended.len(), 2, "proposed, then accepted, in one append");
    assert!(appended.iter().all(|e| e.author == member(OFFICE)));
    let now = brief(SUBMISSION);
    assert_eq!(now.text, auto.text());
    assert_eq!(now.next.as_deref(), auto.next_text());
    assert_eq!(now.source, BriefSource::BackOffice);
    assert_eq!(now.receipts, auto.receipts);
    assert!(!now.pinned);
    assert!(now.proposal.is_none());

    // A pinned brief only ever gets a proposal, whatever the proposal says.
    let pinned = brief(SEED_RUNS);
    assert!(pinned.pinned);
    let mut auto = paused(SEED_RUNS);
    auto.disposition = Disposition::AutoAccept;
    commands.propose_brief(&auto).expect("proposed");
    let now = brief(SEED_RUNS);
    assert_eq!(now.text, pinned.text);
    assert!(now.pinned);
    assert_eq!(now.source, BriefSource::Person);
    assert_eq!(now.proposal.expect("pending").text, auto.text());

    // Unknown targets and proposals without evidence are refused.
    let rev = work.store().latest_rev().expect("rev");
    assert_eq!(
        code(commands.propose_brief(&paused("01JB000000000000000WST0099"))),
        ErrorCode::NotFound
    );
    let mut bare = paused(SUBMISSION);
    bare.receipts.clear();
    assert_eq!(code(commands.propose_brief(&bare)), ErrorCode::Invalid);
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
}

#[test]
fn the_office_runs_only_as_an_agent_with_its_run_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = office();
    let work = hub_with(dir.path(), &office);
    let range = comment(&work, "hello");
    // A person cannot be the back office.
    let sam = BackOffice::new(member(SAM));
    assert_eq!(code(work.run_office(&sam, range)), ErrorCode::Invalid);
    assert_eq!(code(work.office_commands(&sam)), ErrorCode::Invalid);
    let stranger = BackOffice::new(MemberId::new());
    assert_eq!(code(work.office_commands(&stranger)), ErrorCode::Invalid);
    // Nothing to do for an empty range.
    let empty = RevRange {
        from_rev: 5,
        to_rev: 4,
    };
    assert!(
        work.run_office(&office, empty)
            .expect("empty")
            .actions
            .is_empty()
    );

    // A store opened without the run log cannot run the office: it would silently do nothing.
    drop(work);
    let plain = Arc::new(
        Store::open_with(
            dir.path().join("hub.db"),
            StoreOptions::default(),
            projections(),
        )
        .expect("open"),
    );
    let work = WorkService::new(plain, demo().workspace);
    let range = comment(&work, "unseen");
    assert_eq!(code(work.run_office(&office, range)), ErrorCode::Internal);
}

/// A rule (named by its field) that answers each "chatty N" comment with 64 comments of its own,
/// the most one rule may take per event.
struct Chatty(&'static str);

impl Rule for Chatty {
    fn name(&self) -> &'static str {
        self.0
    }

    fn on_event(&mut self, ctx: &mut Context<'_>, event: &Event) -> Vec<Action> {
        let EventBody::CommentPosted { text, .. } = &event.body else {
            return Vec::new();
        };
        if ctx.from_office(event) || !text.starts_with("chatty") {
            return Vec::new();
        }
        (0..pitcrew_office::MAX_ACTIONS_PER_EVENT)
            .map(|i| Action::Append {
                body: EventBody::CommentPosted {
                    task: Some(PAP1.parse().expect("task")),
                    workstream: None,
                    text: format!("{text}/{i}"),
                    mentions: Vec::new(),
                },
                because: vec![Receipt::Event { id: event.id }],
            })
            .collect()
    }
}

fn uncapped() -> Config {
    Config {
        per_rule_per_hour: 10_000,
        global_per_hour: 10_000,
        ..Config::default()
    }
}

fn chatty_office(rules: &'static [&'static str]) -> BackOffice {
    BackOffice::with_rules(member(OFFICE), uncapped(), move || {
        rules
            .iter()
            .map(|&name| Box::new(Chatty(name)) as Box<dyn Rule>)
            .collect()
    })
}

fn chatty_triggers(work: &WorkService, n: i64) -> Vec<Event> {
    (0..n)
        .map(|n| Event {
            id: EventId::new(),
            at: 1_790_800_000_000 + n,
            workspace: work.workspace(),
            author: member(SAM),
            on_behalf_of: None,
            body: EventBody::CommentPosted {
                task: Some(PAP1.parse().expect("task")),
                workstream: None,
                text: format!("chatty {n}"),
                mentions: Vec::new(),
            },
        })
        .collect()
}

/// One append batch whose run log is several pages long is applied whole, once, in log order.
#[test]
fn a_long_run_log_is_applied_whole_and_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = chatty_office(&["chatty"]);
    let work = hub_with(dir.path(), &office);
    let batch = work
        .store()
        .append(&chatty_triggers(&work, 3))
        .expect("append");
    let run = work.run_office(&office, batch).expect("run");
    let per_event = pitcrew_office::MAX_ACTIONS_PER_EVENT;
    assert_eq!(run.actions.len(), 3 * per_event);
    assert_eq!(run.applied().count(), 3 * per_event);
    let texts: Vec<String> = events_since(&work, batch.to_rev)
        .into_iter()
        .map(|e| match e.body {
            EventBody::CommentPosted { text, .. } => text,
            other => panic!("not a comment: {other:?}"),
        })
        .collect();
    let expected: Vec<String> = (0..3)
        .flat_map(|n| (0..per_event).map(move |i| format!("chatty {n}/{i}")))
        .collect();
    assert_eq!(texts, expected);
    // Running a range again appends nothing: a range covering just the last trigger finds its 64
    // already applied, and nothing of the others.
    let rev = work.store().latest_rev().expect("rev");
    let last = RevRange {
        from_rev: batch.to_rev,
        to_rev: batch.to_rev,
    };
    let again = work.run_office(&office, last).expect("run");
    assert_eq!(again.actions.len(), per_event);
    assert!(again.actions.iter().all(|a| a.entry.rev == batch.to_rev));
    assert_eq!(again.replayed().count(), per_event);
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    let whole = work.run_office(&office, batch).expect("run");
    assert_eq!(whole.replayed().count(), 3 * per_event);
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
}

/// After a crash part-way through a batch, re-running the batch applies only what was missing:
/// each action once in all.
#[test]
fn re_running_a_range_after_a_crash_applies_each_action_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let office = chatty_office(&["chatty"]);
    let work = hub_with(dir.path(), &office);
    let batch = work
        .store()
        .append(&chatty_triggers(&work, 3))
        .expect("append");
    // The daemon got through the first trigger's actions only.
    let first = RevRange {
        from_rev: batch.from_rev,
        to_rev: batch.from_rev,
    };
    let per_event = pitcrew_office::MAX_ACTIONS_PER_EVENT;
    assert_eq!(
        work.run_office(&office, first)
            .expect("run")
            .applied()
            .count(),
        per_event
    );
    // After the restart it runs the whole batch again.
    let run = work.run_office(&office, batch).expect("run");
    assert_eq!(run.replayed().count(), per_event);
    assert_eq!(run.applied().count(), 2 * per_event);
    let comments = events_since(&work, batch.to_rev);
    assert_eq!(comments.len(), 3 * per_event, "each action once");
    // The office's events carry ids derived from the run log, so they are the same every time.
    assert!(comments.iter().all(|e| e.author == member(OFFICE)));
}

/// A store whose run log has more rules than the `BackOffice` that applies it is a wiring error:
/// a revision may have more entries than this office can account for.
#[test]
fn a_run_log_with_other_rules_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registered = chatty_office(&["chatty", "echo"]);
    let work = hub_with(dir.path(), &registered);
    let batch = work
        .store()
        .append(&chatty_triggers(&work, 2))
        .expect("append");
    let fewer = chatty_office(&["chatty"]);
    assert_eq!(code(work.run_office(&fewer, batch)), ErrorCode::Internal);
    // The office it was registered with applies it.
    let run = work.run_office(&registered, batch).expect("run");
    assert_eq!(
        run.applied().count(),
        2 * 2 * pitcrew_office::MAX_ACTIONS_PER_EVENT
    );
}

/// The demo fixture has what the tests above assume.
#[test]
fn the_fixture_has_an_active_dispatch_and_an_office_agent() {
    let demo = demo();
    let dsp1: DispatchId = DSP1.parse().expect("dispatch");
    let d = demo.dispatches.iter().find(|d| d.id == dsp1).expect("DSP1");
    assert_eq!(d.task, PAP1.parse().expect("task"));
    assert!(d.ended.is_none());
    let office_member = demo
        .members
        .iter()
        .find(|m| m.id == member(OFFICE))
        .expect("@office");
    assert_eq!(office_member.kind, MemberKind::Agent);
    assert_eq!(office_member.owner, Some(member(SAM)));
}
