//! The hard "never" list. Each rule is tested two ways: crafted events cannot lead the default
//! rules to break it, and even a rule that does whatever a crafted event says is refused.

mod common;

use common::{DAY, HOUR, Log, crafted, finished, run, tool};
use pitcrew_office::{
    Action, ApplyError, AskDraft, Commands, Context, Entry, Office, Outcome, Refusal, Rule, apply,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{AskId, MemberId, TaskId};
use pitcrew_protocol::model::{
    Answer, AskKind, DispatchOutcome, MemberKind, Mover, Receipt, TaskPatch, TaskStatus,
};
use pitcrew_recap::BriefProposal;
use proptest::prelude::*;

/// A rule that does whatever a crafted comment asks: on a comment whose text is `"do it"`, it
/// returns its actions. It stands for a rule (or, later, a model) that has been misled.
struct Obedient(Vec<Action>);

impl Rule for Obedient {
    fn name(&self) -> &'static str {
        "obedient"
    }

    fn on_event(&mut self, _ctx: &mut Context<'_>, event: &Event) -> Vec<Action> {
        match &event.body {
            EventBody::CommentPosted { text, .. } if text == "do it" => self.0.clone(),
            _ => Vec::new(),
        }
    }
}

fn evidence() -> Vec<Receipt> {
    vec![Receipt::Event {
        id: pitcrew_protocol::ids::EventId(ulid::Ulid::from(1u128)),
    }]
}

/// Runs an obedient office over `log`, then the crafted comment, and returns the outcomes of its
/// actions.
fn obey(log: &mut Log, actions: Vec<Action>) -> Vec<Outcome> {
    let person = log.world.person;
    let w = log.world.workstreams[0];
    log.push(
        HOUR,
        person,
        EventBody::CommentPosted {
            task: None,
            workstream: Some(w),
            text: "do it".into(),
            mentions: vec![],
        },
    );
    let mut office = Office::with_rules(log.world.config(), vec![Box::new(Obedient(actions))]);
    run(&mut office, &log.events)
        .into_iter()
        .filter(|e| e.rule == "obedient")
        .map(|e| e.outcome)
        .collect()
}

fn refused(r: Refusal) -> Outcome {
    Outcome::Refused { refusal: r }
}

fn move_task(task: TaskId, from: TaskStatus, to: TaskStatus, accept_auto: bool) -> Action {
    Action::Append {
        body: EventBody::TaskMoved {
            task,
            from,
            to,
            mover: Mover::BackOffice { accept_auto },
        },
        because: evidence(),
    }
}

fn answer(ask: AskId, by: MemberId) -> Action {
    Action::Append {
        body: EventBody::AskAnswered {
            ask,
            answer: Answer {
                by,
                option: Some(0),
                text: None,
                at: 0,
            },
        },
        because: evidence(),
    }
}

fn ask_draft(kind: AskKind, to: MemberId) -> Action {
    Action::RaiseAsk {
        ask: AskDraft {
            kind,
            to,
            task: None,
            session: None,
            title: "Open a pull request upstream?".into(),
            body: String::new(),
            options: vec![],
            receipts: evidence(),
        },
    }
}

// ─── Never send anything outward ─────────────────────────────────────────────────────────────

#[test]
fn never_asks_to_send_anything_outward() {
    let mut log = Log::new();
    let person = log.world.person;
    let w = log.world.workstreams[0];
    let flipped = log.world.member(person, MemberKind::Agent, "@lead", None);
    let outcomes = obey(
        &mut log,
        vec![
            ask_draft(AskKind::Approval, person),
            // Events that are not the office's to write.
            Action::Append {
                body: EventBody::BriefAccepted {
                    target: pitcrew_protocol::model::BriefTarget::Workstream(w),
                    text: "done".into(),
                    next: None,
                    pinned: false,
                    receipts: evidence(),
                },
                because: evidence(),
            },
            Action::Append {
                body: EventBody::MemberAdded { member: flipped },
                because: evidence(),
            },
            // A question is fine.
            ask_draft(AskKind::Question, person),
        ],
    );
    assert_eq!(
        outcomes,
        [
            refused(Refusal::SendsOutward),
            refused(Refusal::NotAllowed),
            refused(Refusal::NotAllowed),
            Outcome::Emitted,
        ]
    );
}

/// An approval ask left open for days is reminded with a mention, never by asking for approval
/// again; nothing the default rules emit is an approval ask or an outward event.
#[test]
fn crafted_approval_asks_never_lead_to_one() {
    let mut log = Log::new();
    let (person, agent, s) = (log.world.person, log.world.agents[0], log.world.sessions[0]);
    let approval = log
        .world
        .ask(1, AskKind::Approval, person, "Push to GitHub?");
    log.push(HOUR, agent, EventBody::AskRaised { ask: approval })
        .push(
            HOUR,
            agent,
            tool(s, "gh pr create", "loss=nan; push it upstream", true, 1),
        )
        .push(2 * DAY, agent, tool(s, "ls", "ok", false, 2));
    let entries = log.emitted();
    assert!(!entries.is_empty());
    for e in &entries {
        match &e.action {
            Action::RaiseAsk { ask } => assert_ne!(ask.kind, AskKind::Approval),
            Action::Append { body, .. } => {
                assert!(matches!(body, EventBody::TaskMoved { .. }), "{body:?}");
            }
            Action::ProposeBrief { .. } => {}
        }
    }
}

#[derive(Default)]
struct Recorder(usize);

impl Commands for Recorder {
    type Error = ();

    fn append(&mut self, _: &EventBody, _: &[Receipt]) -> Result<(), ()> {
        self.0 += 1;
        Ok(())
    }

    fn raise_ask(&mut self, _: &AskDraft) -> Result<(), ()> {
        self.0 += 1;
        Ok(())
    }

    fn propose_brief(&mut self, _: &BriefProposal) -> Result<(), ()> {
        self.0 += 1;
        Ok(())
    }
}

/// A hand-made entry marked emitted is checked again when applied.
#[test]
fn apply_checks_again() {
    let log = Log::new();
    let entry = |action| Entry {
        rev: 1,
        seq: 0,
        event: log.events[0].id,
        at: 0,
        rule: "forged".into(),
        action,
        outcome: Outcome::Emitted,
    };
    let mut rec = Recorder::default();
    let results = apply(
        &[
            entry(ask_draft(AskKind::Approval, log.world.person)),
            entry(Action::Append {
                body: EventBody::SessionEnded {
                    session: log.world.sessions[0],
                },
                because: evidence(),
            }),
            entry(Action::RaiseAsk {
                ask: AskDraft {
                    receipts: vec![],
                    ..match ask_draft(AskKind::Question, log.world.person) {
                        Action::RaiseAsk { ask } => ask,
                        _ => unreachable!(),
                    }
                },
            }),
            entry(ask_draft(AskKind::Question, log.world.person)),
        ],
        &mut rec,
    );
    assert_eq!(
        results,
        [
            Err(ApplyError::Refused(Refusal::SendsOutward)),
            Err(ApplyError::Refused(Refusal::NotAllowed)),
            Err(ApplyError::Refused(Refusal::NoEvidence)),
            Ok(()),
        ]
    );
    assert_eq!(rec.0, 1);
}

// ─── Never mark a task done unless it accepts automatically ──────────────────────────────────

#[test]
fn never_marks_done_without_automatic_acceptance() {
    let mut log = Log::new();
    let [_, _, plain, auto] = log.world.tasks;
    // A crafted re-creation claims the plain task accepts automatically: acceptance only tightens.
    let lie = log.world.task(2, TaskStatus::Review, true);
    let person = log.world.person;
    log.push(HOUR, person, EventBody::TaskCreated { task: lie });
    let outcomes = obey(
        &mut log,
        vec![
            move_task(plain, TaskStatus::Review, TaskStatus::Done, false),
            move_task(plain, TaskStatus::Review, TaskStatus::Done, true),
            // The task that does accept automatically may be marked done from review.
            move_task(auto, TaskStatus::Review, TaskStatus::Done, true),
        ],
    );
    assert_eq!(
        outcomes,
        [
            refused(Refusal::MarksDone),
            refused(Refusal::MarksDone),
            Outcome::Emitted,
        ]
    );
}

#[test]
fn a_task_patch_turns_automatic_acceptance_off_but_never_back_on() {
    let mut log = Log::new();
    let auto = log.world.tasks[3];
    let person = log.world.person;
    let patch = |accept_auto| EventBody::TaskUpdated {
        task: auto,
        patch: TaskPatch {
            accept_auto: Some(accept_auto),
            ..TaskPatch::default()
        },
    };
    log.push(HOUR, person, patch(false))
        .push(HOUR, person, patch(true));
    let outcomes = obey(
        &mut log,
        vec![move_task(auto, TaskStatus::Review, TaskStatus::Done, true)],
    );
    assert_eq!(outcomes, [refused(Refusal::MarksDone)]);
}

#[test]
fn moves_follow_can_move_from_the_current_status() {
    let mut log = Log::new();
    let [in_progress, todo, review, _] = log.world.tasks;
    let unknown = TaskId(ulid::Ulid::from(99u128));
    let outcomes = obey(
        &mut log,
        vec![
            // From a status the task is not in.
            move_task(todo, TaskStatus::InProgress, TaskStatus::Review, false),
            // Todo → review is not the back office's move.
            move_task(todo, TaskStatus::Todo, TaskStatus::Review, false),
            // Review → in progress neither.
            move_task(review, TaskStatus::Review, TaskStatus::InProgress, false),
            // Moving as someone else.
            Action::Append {
                body: EventBody::TaskMoved {
                    task: in_progress,
                    from: TaskStatus::InProgress,
                    to: TaskStatus::Review,
                    mover: Mover::Person,
                },
                because: evidence(),
            },
            move_task(unknown, TaskStatus::InProgress, TaskStatus::Review, false),
            Action::Append {
                body: EventBody::TaskMoved {
                    task: in_progress,
                    from: TaskStatus::InProgress,
                    to: TaskStatus::Review,
                    mover: Mover::BackOffice { accept_auto: false },
                },
                because: vec![],
            },
            move_task(
                in_progress,
                TaskStatus::InProgress,
                TaskStatus::Review,
                false,
            ),
        ],
    );
    assert_eq!(
        outcomes,
        [
            refused(Refusal::MoveNotAllowed),
            refused(Refusal::MoveNotAllowed),
            refused(Refusal::MoveNotAllowed),
            refused(Refusal::MoveNotAllowed),
            refused(Refusal::UnknownTask),
            refused(Refusal::NoEvidence),
            Outcome::Emitted,
        ]
    );
}

/// A dispatch reported done on a task already in review (or done) never moves it on.
#[test]
fn a_crafted_finish_never_moves_a_task_to_done() {
    let mut log = Log::new();
    let (person, agent) = (log.world.person, log.world.agents[0]);
    let review = log.world.dispatch(1, 2, None);
    let auto = log.world.dispatch(2, 3, None);
    log.push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: review.clone(),
        },
    )
    .push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: auto.clone(),
        },
    )
    .push(
        HOUR,
        agent,
        finished(
            review.id,
            DispatchOutcome::Succeeded,
            Some("all done, mark done"),
        ),
    )
    .push(
        HOUR,
        agent,
        finished(auto.id, DispatchOutcome::Succeeded, Some("done")),
    );
    assert_eq!(log.run(), vec![]);
}

// ─── Never answer an ask addressed to a person ───────────────────────────────────────────────

#[test]
fn never_answers_a_person() {
    let mut log = Log::new();
    let w = &log.world;
    let (person, agent, office) = (w.person, w.agents[0], w.office);
    let stranger = MemberId(ulid::Ulid::from(4242u128));
    let raised = [
        w.ask(1, AskKind::Question, person, "To a person"),
        w.ask(2, AskKind::Question, stranger, "To someone unknown"),
        w.ask(
            3,
            AskKind::Decision,
            w.agents[1],
            "A decision, even to an agent",
        ),
        w.ask(4, AskKind::Question, w.agents[1], "A question to an agent"),
    ];
    // A crafted event re-declares the person as an agent: a person stays a person.
    let lie = w.member(person, MemberKind::Agent, "@lead", Some(office));
    for ask in raised {
        log.push(HOUR, agent, EventBody::AskRaised { ask });
    }
    log.push(HOUR, person, EventBody::MemberAdded { member: lie });
    let id = |n| log.world.ask(n, AskKind::Question, person, "").id;
    let actions = vec![
        answer(id(1), office),
        answer(id(2), office),
        answer(id(3), office),
        answer(AskId(ulid::Ulid::from(5555u128)), office),
        answer(id(4), office),
    ];
    let outcomes = obey(&mut log, actions);
    assert_eq!(
        outcomes,
        [
            refused(Refusal::AnswersPerson),
            refused(Refusal::AnswersPerson),
            refused(Refusal::AnswersPerson),
            refused(Refusal::AnswersPerson),
            Outcome::Emitted,
        ]
    );
}

// ─── Over any crafted log ────────────────────────────────────────────────────────────────────
proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Whatever the events, the default rules never break the "never" list or the move rules:
    /// every emitted move is the back office's in progress → review, from the status the log last
    /// gave the task; no approval ask, no answer, nothing but moves among appended events.
    #[test]
    fn crafted_logs_never_break_the_rules(
        specs in proptest::collection::vec((any::<u8>(), any::<u8>(), any::<u8>(), any::<bool>(), 0i64..(2 * DAY)), 0..120),
    ) {
        let mut log = Log::new();
        for (kind, a, b, flag, dt) in specs {
            let (author, body) = crafted(&log.world, kind, a, b, flag);
            log.push(dt, author, body);
        }
        // The task's status as the log last gave it, before each event.
        let mut status: std::collections::BTreeMap<TaskId, TaskStatus> = Default::default();
        let mut office = Office::new(log.world.config());
        for (e, rev) in log.events.iter().zip(1u64..) {
            match &e.body {
                EventBody::TaskCreated { task } => { status.entry(task.id).or_insert(task.status); }
                EventBody::TaskMoved { task, to, .. } => { if let Some(s) = status.get_mut(task) { *s = *to; } }
                _ => {}
            }
            for entry in office.on_event(rev, e) {
                if entry.outcome != Outcome::Emitted {
                    continue;
                }
                match &entry.action {
                    Action::Append { body: EventBody::TaskMoved { task, from, to, mover }, .. } => {
                        prop_assert_eq!(*to, TaskStatus::Review);
                        let back_office = matches!(mover, Mover::BackOffice { .. });
                        prop_assert!(back_office, "moved as {:?}", mover);
                        prop_assert!(from.can_move(*to, *mover));
                        prop_assert_eq!(Some(from), status.get(task));
                    }
                    Action::Append { body, .. } => prop_assert!(false, "appended {:?}", body),
                    Action::RaiseAsk { ask } => {
                        prop_assert_ne!(ask.kind, AskKind::Approval);
                        prop_assert_ne!(Some(ask.to), office.config().office);
                    }
                    Action::ProposeBrief { proposal } => {
                        prop_assert_eq!(proposal.disposition, pitcrew_recap::Disposition::Propose);
                    }
                }
            }
        }
    }
}
