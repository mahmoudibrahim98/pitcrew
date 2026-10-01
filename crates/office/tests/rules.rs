//! The first rules, one scenario at a time.

mod common;

use common::{DAY, HOUR, Log, finished, tool};
use pitcrew_office::{Action, AskDraft, Entry};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::{
    AskKind, DispatchOutcome, Health, Mover, Receipt, SessionState, TaskStatus, WorkstreamStatus,
};

fn asks(entries: &[Entry]) -> Vec<&AskDraft> {
    entries
        .iter()
        .filter_map(|e| match &e.action {
            Action::RaiseAsk { ask } => Some(ask),
            _ => None,
        })
        .collect()
}

fn rules(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.rule.as_str()).collect()
}

// ─── A dispatch finished → review ────────────────────────────────────────────────────────────

#[test]
fn a_finished_dispatch_moves_its_task_to_review() {
    let mut log = Log::new();
    let (person, agent) = (log.world.person, log.world.agents[0]);
    let d = log.world.dispatch(1, 0, Some(0));
    log.push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: d.clone(),
        },
    )
    .push(
        HOUR,
        agent,
        finished(d.id, DispatchOutcome::Succeeded, Some("done")),
    );
    let entries = log.emitted();
    assert_eq!(entries.len(), 1, "{entries:#?}");
    let e = &entries[0];
    assert_eq!(e.rule, "dispatch_to_review");
    let finish = log.events.last().map(|e| e.id).expect("an event");
    assert_eq!(
        e.action,
        Action::Append {
            body: EventBody::TaskMoved {
                task: log.world.tasks[0],
                from: TaskStatus::InProgress,
                to: TaskStatus::Review,
                mover: Mover::BackOffice { accept_auto: false },
            },
            because: vec![Receipt::Event { id: finish }],
        }
    );
}

#[test]
fn only_a_successful_dispatch_on_work_in_progress_moves() {
    let mut log = Log::new();
    let (person, agent, office) = (log.world.person, log.world.agents[0], log.world.office);
    let on_todo = log.world.dispatch(1, 1, None);
    let failed = log.world.dispatch(2, 0, Some(0));
    let by_office = log.world.dispatch(3, 0, Some(0));
    let unknown = log.world.dispatch(4, 0, Some(0));
    log.push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: on_todo.clone(),
        },
    )
    .push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: failed.clone(),
        },
    )
    .push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: by_office.clone(),
        },
    )
    // A todo task cannot go to review.
    .push(
        HOUR,
        agent,
        finished(on_todo.id, DispatchOutcome::Succeeded, None),
    )
    .push(
        HOUR,
        agent,
        finished(failed.id, DispatchOutcome::Failed, None),
    )
    .push(
        HOUR,
        agent,
        finished(failed.id, DispatchOutcome::Canceled, None),
    )
    .push(
        HOUR,
        office,
        finished(by_office.id, DispatchOutcome::Succeeded, None),
    )
    .push(
        HOUR,
        agent,
        finished(unknown.id, DispatchOutcome::Succeeded, None),
    );
    assert_eq!(log.run(), vec![]);

    // Once the agent has moved it to review itself, there is nothing left to move.
    let mut log = Log::new();
    let d = log.world.dispatch(1, 0, Some(0));
    let (task, agent) = (log.world.tasks[0], log.world.agents[0]);
    log.push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: d.clone(),
        },
    )
    .push(
        HOUR,
        agent,
        EventBody::TaskMoved {
            task,
            from: TaskStatus::InProgress,
            to: TaskStatus::Review,
            mover: Mover::Agent { on_own_task: true },
        },
    )
    .push(
        HOUR,
        agent,
        finished(d.id, DispatchOutcome::Succeeded, None),
    );
    assert_eq!(log.run(), vec![]);
}

// ─── A job diverged → a decision ask to the owner ────────────────────────────────────────────

#[test]
fn a_diverged_job_asks_its_owner_once_per_spell() {
    let mut log = Log::new();
    let (s, agent) = (log.world.sessions[0], log.world.agents[0]);
    log.push(
        HOUR,
        agent,
        tool(s, "sacct -j 4815", "epoch 9: loss=nan", false, 100),
    )
    .push(
        HOUR,
        agent,
        tool(s, "sacct -j 4815", "loss: nan again", false, 200),
    )
    .push(
        13 * HOUR,
        agent,
        tool(s, "tail run.log", "Seed 3 diverged", false, 300),
    );
    let entries = log.emitted();
    assert_eq!(rules(&entries), ["job_diverged", "job_diverged"]);
    let asked = asks(&entries);
    let first = asked[0];
    assert_eq!(first.kind, AskKind::Decision);
    assert_eq!(first.to, log.world.person, "the agent's owner");
    assert_eq!(first.task, Some(log.world.tasks[0]));
    assert_eq!(first.session, Some(s));
    assert_eq!(first.body, "epoch 9: loss=nan");
    assert_eq!(
        first.receipts,
        [
            Receipt::Event {
                id: log.events[log.events.len() - 3].id
            },
            Receipt::Transcript {
                session: s,
                offset: 100
            },
        ]
    );
}

#[test]
fn a_name_is_not_a_divergence_and_an_unowned_run_asks_no_one() {
    let mut log = Log::new();
    let (s, agent) = (log.world.sessions[0], log.world.agents[0]);
    log.push(
        HOUR,
        agent,
        tool(s, "git log", "Nan fixed the plot", false, 1),
    )
    .push(HOUR, agent, tool(s, "echo", "nan, c'est bon", false, 2));
    assert_eq!(log.run(), vec![]);

    // A session whose agent is unknown, by an author nobody owns: no one to ask.
    let mut log = Log::new();
    let stranger = pitcrew_protocol::ids::MemberId(ulid::Ulid::from(77u128));
    let s = pitcrew_protocol::ids::SessionId(ulid::Ulid::from(78u128));
    log.push(HOUR, stranger, tool(s, "train", "loss is NaN", false, 1));
    assert_eq!(log.run(), vec![]);
}

#[test]
fn a_dispatch_that_reports_divergence_asks_too() {
    let mut log = Log::new();
    let (person, agent) = (log.world.person, log.world.agents[1]);
    let d = log.world.dispatch(1, 1, Some(1));
    log.push(
        HOUR,
        person,
        EventBody::DispatchStarted {
            dispatch: d.clone(),
        },
    )
    .push(
        HOUR,
        agent,
        finished(
            d.id,
            DispatchOutcome::Failed,
            Some("Seed 2 diverged at epoch 4"),
        ),
    );
    let entries = log.emitted();
    assert_eq!(rules(&entries), ["job_diverged"]);
    assert_eq!(asks(&entries)[0].task, Some(log.world.tasks[1]));
}

// ─── Tests kept failing → a decision ask ─────────────────────────────────────────────────────

#[test]
fn tests_failing_three_times_in_a_row_ask_once_per_streak() {
    let mut log = Log::new();
    let (s, agent) = (log.world.sessions[0], log.world.agents[0]);
    let fail = |n| tool(s, "cargo test -p x", "2 failed", true, n);
    log.push(60_000, agent, fail(1))
        .push(
            60_000,
            agent,
            tool(s, "npm install testing-library", "error", true, 2),
        )
        .push(60_000, agent, fail(3))
        .push(60_000, agent, fail(4));
    let entries = log.emitted();
    assert_eq!(rules(&entries), ["tests_failing"]);
    let ask = asks(&entries)[0];
    assert_eq!(
        ask.title,
        "Tests failed 3 times in a row: keep going or step in?"
    );
    assert_eq!(ask.to, log.world.person);
    // The first failure and the latest one.
    assert!(ask.receipts.contains(&Receipt::Transcript {
        session: s,
        offset: 1
    }));
    assert!(ask.receipts.contains(&Receipt::Transcript {
        session: s,
        offset: 4
    }));

    // A fourth failure is the same streak; a pass ends it, and three more failures ask again.
    log.push(60_000, agent, fail(5))
        .push(60_000, agent, tool(s, "cargo test", "ok", false, 6))
        .push(60_000, agent, fail(7))
        .push(60_000, agent, fail(8));
    assert_eq!(log.emitted().len(), 1);
    log.push(60_000, agent, fail(9));
    assert_eq!(log.emitted().len(), 2);

    // An ended session forgets its streak.
    let mut log = Log::new();
    log.push(60_000, agent, fail(1))
        .push(60_000, agent, fail(2))
        .push(
            60_000,
            agent,
            EventBody::SessionStateChanged {
                session: s,
                from: SessionState::Working,
                to: SessionState::Ended,
                status_line: None,
            },
        )
        .push(60_000, agent, fail(3));
    assert_eq!(log.run(), vec![]);
}

// ─── An ask open too long → one reminder ─────────────────────────────────────────────────────

#[test]
fn an_ask_open_for_a_day_gets_one_reminder() {
    let mut log = Log::new();
    let (person, agent, s) = (log.world.person, log.world.agents[0], log.world.sessions[0]);
    let ask = log.world.ask(1, AskKind::Decision, person, "Rerun seed 3?");
    log.push(HOUR, agent, EventBody::AskRaised { ask }).push(
        23 * HOUR,
        agent,
        tool(s, "ls", "ok", false, 1),
    );
    assert_eq!(log.run(), vec![]);
    log.push(2 * HOUR, agent, tool(s, "ls", "ok", false, 2))
        .push(DAY, agent, tool(s, "ls", "ok", false, 3));
    let entries = log.emitted();
    assert_eq!(rules(&entries), ["remind_stale_asks"]);
    let reminder = asks(&entries)[0];
    assert_eq!(reminder.kind, AskKind::Mention);
    assert_eq!(reminder.to, person);
    assert_eq!(reminder.title, "Reminder: Rerun seed 3?");
    assert_eq!(reminder.body, "Open for 25 hours, since 2026-09-30.");
    let raised = log.events[log.events.len() - 4].id;
    assert_eq!(reminder.receipts, [Receipt::Event { id: raised }]);
}

#[test]
fn answered_asks_mentions_and_asks_to_the_office_are_not_reminded() {
    let mut log = Log::new();
    let w = &log.world;
    let (person, agent, office) = (w.person, w.agents[0], w.office);
    let raised = [
        w.ask(1, AskKind::Question, person, "A?"),
        w.ask(2, AskKind::Mention, person, "B"),
        w.ask(3, AskKind::Question, office, "C?"),
    ];
    let answer = w.answer(1, person);
    for ask in raised {
        log.push(HOUR, agent, EventBody::AskRaised { ask });
    }
    log.push(HOUR, person, answer).push(
        3 * DAY,
        person,
        EventBody::MachineLiveness {
            machine: pitcrew_protocol::ids::MachineId(ulid::Ulid::from(1u128)),
            liveness: pitcrew_protocol::model::Liveness::Live,
        },
    );
    assert!(rules(&log.run()).iter().all(|r| *r != "remind_stale_asks"));
}

// ─── A quiet workstream → "paused?" ──────────────────────────────────────────────────────────

fn proposals(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.action {
            Action::ProposeBrief { proposal } => Some(proposal.text()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_quiet_active_workstream_is_asked_if_it_is_paused_once_per_spell() {
    let mut log = Log::new();
    let (person, agent, s) = (log.world.person, log.world.agents[0], log.world.sessions[0]);
    // Session 0 works on task 0, in stream 1; stream 2 has had nothing since it was created.
    log.push(DAY, agent, tool(s, "ls", "ok", false, 1)).push(
        2 * DAY + HOUR,
        person,
        EventBody::MachineLiveness {
            machine: pitcrew_protocol::ids::MachineId(ulid::Ulid::from(1u128)),
            liveness: pitcrew_protocol::model::Liveness::Live,
        },
    );
    let entries = log.emitted();
    assert_eq!(rules(&entries), ["quiet_workstream"]);
    let Action::ProposeBrief { proposal } = &entries[0].action else {
        panic!("a proposal");
    };
    assert_eq!(
        proposal.target,
        pitcrew_protocol::model::BriefTarget::Workstream(log.world.workstreams[1])
    );
    assert_eq!(
        proposal.text(),
        "No activity for 3 days (since 2026-09-30), paused? Next: mark it paused, or give it a \
         next step."
    );

    // A day later stream 1 is quiet too; stream 2 is not asked again.
    log.push(
        DAY,
        person,
        EventBody::MachineLiveness {
            machine: pitcrew_protocol::ids::MachineId(ulid::Ulid::from(1u128)),
            liveness: pitcrew_protocol::model::Liveness::Live,
        },
    );
    assert_eq!(proposals(&log.emitted()).len(), 2);

    // Activity on stream 2, then three quiet days: it is asked again.
    log.push(
        HOUR,
        person,
        EventBody::CommentPosted {
            task: None,
            workstream: Some(log.world.workstreams[1]),
            text: "still on it".into(),
            mentions: vec![],
        },
    )
    .push(
        3 * DAY + HOUR,
        person,
        EventBody::MachineLiveness {
            machine: pitcrew_protocol::ids::MachineId(ulid::Ulid::from(1u128)),
            liveness: pitcrew_protocol::model::Liveness::Live,
        },
    );
    assert_eq!(proposals(&log.emitted()).len(), 3);
}

#[test]
fn a_paused_workstream_and_the_office_own_events_do_not_count() {
    let mut log = Log::new();
    let (person, office) = (log.world.person, log.world.office);
    for w in log.world.workstreams {
        log.push(
            0,
            person,
            EventBody::WorkstreamChanged {
                workstream: w,
                status: WorkstreamStatus::Paused,
                health: Health::OnTrack,
            },
        );
    }
    // The office's own comment is not activity, and paused streams are never asked.
    log.push(
        DAY,
        office,
        EventBody::CommentPosted {
            task: None,
            workstream: Some(log.world.workstreams[0]),
            text: "checking in".into(),
            mentions: vec![],
        },
    )
    .push(
        4 * DAY,
        person,
        EventBody::MachineLiveness {
            machine: pitcrew_protocol::ids::MachineId(ulid::Ulid::from(1u128)),
            liveness: pitcrew_protocol::model::Liveness::Live,
        },
    );
    assert_eq!(log.run(), vec![]);
}
