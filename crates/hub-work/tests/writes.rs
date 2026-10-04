//! Outward writes, the hub's side (`crate::writes`): the commands are the gate.
//!
//! - a write and its approval ask are appended together; one cause proposes once;
//! - a write starts only after its ask is answered "Send" by a person, never twice at once, and
//!   never again once sent;
//! - a denial can only be recorded as "not sent";
//! - an approval ask an agent raises itself has no write, so nothing can ever start from it;
//! - a retry is a failed write's only way back, and only for whoever may answer its ask;
//! - an issue created from a task gives the task its source.

mod common;

use common::{SAM, SEED_RUNS, WRITER, agent, member, person, seeded};
use pitcrew_hub_work::{
    AnswerAsk, NewAsk, NewTask, SyncCommands, SyncOutcome, TaskRef, WorkService, WriteFilter,
};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{AskId, EventId, IntegrationId, TaskId};
use pitcrew_protocol::model::{AskKind, AskState, ExternalRef, ExternalSystem, Receipt};
use pitcrew_protocol::writes::{
    APPROVAL_OPTIONS, IssueState, WriteFields, WriteOperation, WriteProposal, WriteResult,
    WriteState,
};

fn task(work: &WorkService) -> TaskId {
    work.create_task(
        &person(SAM),
        NewTask {
            project: "01JB000000000000000PRJ0001".parse().expect("project"),
            workstream: Some(SEED_RUNS.parse().expect("workstream")),
            title: "Write the release notes".into(),
            description: None,
            status: None,
            priority: None,
            assignee: None,
            labels: None,
            due: None,
        },
    )
    .expect("task")
    .id
}

fn close(task: TaskId, cause: Option<EventId>) -> WriteProposal {
    WriteProposal {
        ask: AskId::new(),
        integration: IntegrationId::new(),
        system: ExternalSystem::Github,
        scope: "example-org/demo-repo".into(),
        target: Some(ExternalRef {
            system: ExternalSystem::Github,
            key: "example-org/demo-repo#1".into(),
            url: None,
        }),
        task: Some(task),
        operation: WriteOperation::Close,
        before: WriteFields {
            state: Some(IssueState::Open),
            ..WriteFields::default()
        },
        after: WriteFields {
            state: Some(IssueState::Closed),
            ..WriteFields::default()
        },
        requested_by: member(SAM),
        cause,
    }
}

fn sync(work: &WorkService) -> SyncCommands<'_> {
    let member = work.ensure_sync_member(member(SAM)).expect("sync member");
    work.sync_commands(member.id).expect("commands")
}

fn answer(work: &WorkService, ask: &AskId, option: usize) {
    work.answer_ask(
        &person(SAM),
        ask,
        AnswerAsk {
            option: Some(option),
            text: None,
        },
    )
    .expect("answer");
}

fn refused<T: std::fmt::Debug>(outcome: SyncOutcome<T>) -> String {
    match outcome {
        SyncOutcome::Refused(reason) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn changed<T>(outcome: SyncOutcome<T>) -> T {
    match outcome {
        SyncOutcome::Changed(value) => value,
        _ => panic!("expected a change"),
    }
}

#[test]
fn a_write_and_its_approval_ask_come_together_and_once_per_cause() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let id = task(&work);
    let commands = sync(&work);
    let before = work.store().latest_rev().unwrap();
    let cause = EventId::new();
    let write = commands
        .propose_write(
            member(SAM),
            close(id, Some(cause)),
            "GitHub: close #1",
            "state: open → closed",
        )
        .expect("propose")
        .expect("proposed");
    assert_eq!(write.state, WriteState::Pending);
    assert_eq!(write.attempts, 0);
    let appended = work.store().since(before, 10).unwrap();
    assert_eq!(appended.len(), 2, "the ask and the write, in one append");
    assert!(
        matches!(&appended[0].event.body, EventBody::AskRaised { ask } if ask.id == write.proposal.ask)
    );
    assert!(
        matches!(&appended[1].event.body, EventBody::WriteProposed { write: w } if w.ask == write.proposal.ask)
    );
    for stored in &appended {
        assert_eq!(stored.event.author, commands.member());
        assert_eq!(stored.event.on_behalf_of, Some(member(SAM)));
    }
    let ask = work.ask(&write.proposal.ask).unwrap();
    assert_eq!(ask.kind, AskKind::Approval);
    assert_eq!(ask.to, member(SAM));
    assert_eq!(ask.task, Some(id));
    assert_eq!(ask.options, APPROVAL_OPTIONS.map(str::to_owned).to_vec());
    assert_eq!(ask.receipts, vec![Receipt::Event { id: cause }]);

    // The same cause proposes nothing more.
    assert!(
        commands
            .propose_write(member(SAM), close(id, Some(cause)), "again", "again")
            .unwrap()
            .is_none()
    );
    assert_eq!(work.store().latest_rev().unwrap(), before + 2);

    // Only a person approves; a write sends something.
    let err = commands
        .propose_write(member(WRITER), close(id, None), "t", "b")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Invalid);
    let mut empty = close(id, None);
    empty.after = WriteFields::default();
    assert_eq!(
        commands
            .propose_write(member(SAM), empty, "t", "b")
            .unwrap_err()
            .code(),
        ErrorCode::Invalid
    );
    let listed = work
        .writes(&WriteFilter {
            task: Some(id),
            states: vec![WriteState::Pending],
        })
        .unwrap();
    assert_eq!(listed, vec![write]);
}

#[test]
fn a_write_starts_only_once_a_person_answers_send_and_never_twice() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let id = task(&work);
    let commands = sync(&work);
    let write = commands
        .propose_write(member(SAM), close(id, None), "GitHub: close #1", "body")
        .unwrap()
        .unwrap();
    let ask = write.proposal.ask;

    // Not before the answer, and nothing to finish yet.
    assert!(refused(commands.start_write(&ask).unwrap()).contains("pending"));
    assert!(
        refused(
            commands
                .finish_write(
                    &ask,
                    WriteResult::Sent {
                        created: None,
                        url: None
                    }
                )
                .unwrap()
        )
        .contains("pending")
    );
    // An agent cannot answer an approval.
    let err = work
        .answer_ask(
            &agent(WRITER),
            &ask,
            AnswerAsk {
                option: Some(0),
                text: None,
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);

    answer(&work, &ask, 0);
    assert_eq!(work.write(&ask).unwrap().state, WriteState::Approved);
    let started = changed(commands.start_write(&ask).unwrap());
    assert_eq!((started.state, started.attempts), (WriteState::Sending, 1));
    // A second start while it is being sent is refused, and appends nothing.
    let rev = work.store().latest_rev().unwrap();
    assert!(refused(commands.start_write(&ask).unwrap()).contains("sending"));
    assert_eq!(work.store().latest_rev().unwrap(), rev);
    // Not sent while being sent: only sent or failed.
    assert!(
        refused(
            commands
                .finish_write(&ask, WriteResult::NotSent { reason: "x".into() })
                .unwrap()
        )
        .contains("sending")
    );

    // A failure can be retried by the person; a retry starts it once more.
    changed(
        commands
            .finish_write(
                &ask,
                WriteResult::Failed {
                    message: "Bad gateway".into(),
                    status: Some(502),
                },
            )
            .unwrap(),
    );
    assert_eq!(
        work.check_retry(&person(SAM), &ask).unwrap().state,
        WriteState::Failed
    );
    assert_eq!(
        work.check_retry(&agent(WRITER), &ask).unwrap_err().code(),
        ErrorCode::Forbidden
    );
    let again = changed(commands.start_write(&ask).unwrap());
    assert_eq!(again.attempts, 2);
    let sent = changed(
        commands
            .finish_write(
                &ask,
                WriteResult::Sent {
                    created: None,
                    url: None,
                },
            )
            .unwrap(),
    );
    assert_eq!(sent.state, WriteState::Sent);
    assert!(sent.finished_at.is_some());
    assert_eq!(sent.answered_by, Some(member(SAM)));

    // Sent is final: no retry, no start, no other result.
    assert_eq!(
        work.check_retry(&person(SAM), &ask).unwrap_err().code(),
        ErrorCode::Conflict
    );
    refused(commands.start_write(&ask).unwrap());
    refused(
        commands
            .finish_write(&ask, WriteResult::NotSent { reason: "x".into() })
            .unwrap(),
    );
    assert_eq!(
        work.check_retry(&person(SAM), &AskId::new())
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
}

#[test]
fn a_denial_is_only_ever_recorded_as_not_sent() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let id = task(&work);
    let commands = sync(&work);
    let ask = commands
        .propose_write(member(SAM), close(id, None), "t", "b")
        .unwrap()
        .unwrap()
        .proposal
        .ask;
    answer(&work, &ask, 1);
    assert_eq!(work.write(&ask).unwrap().state, WriteState::Denied);
    assert!(refused(commands.start_write(&ask).unwrap()).contains("denied"));
    let done = changed(
        commands
            .finish_write(
                &ask,
                WriteResult::NotSent {
                    reason: "Not sent: Sam chose not to.".into(),
                },
            )
            .unwrap(),
    );
    assert_eq!(done.state, WriteState::NotSent);
    refused(commands.start_write(&ask).unwrap());
    assert_eq!(
        work.check_retry(&person(SAM), &ask).unwrap_err().code(),
        ErrorCode::Conflict
    );
    // A text-only answer is no approval either.
    let other = commands
        .propose_write(member(SAM), close(id, None), "t", "b")
        .unwrap()
        .unwrap()
        .proposal
        .ask;
    work.answer_ask(
        &person(SAM),
        &other,
        AnswerAsk {
            option: None,
            text: Some("Send it".into()),
        },
    )
    .unwrap();
    assert_eq!(work.write(&other).unwrap().state, WriteState::Denied);
    refused(commands.start_write(&other).unwrap());
}

#[test]
fn an_approval_ask_raised_through_the_asks_route_never_starts_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let crafted = work
        .raise_ask(
            &agent(WRITER),
            NewAsk {
                kind: AskKind::Approval,
                to: member(SAM),
                title: "Push to GitHub?".into(),
                body: None,
                options: Some(APPROVAL_OPTIONS.map(str::to_owned).to_vec()),
                task: None,
                session: None,
                receipts: None,
            },
        )
        .expect("an agent may raise an approval ask");
    answer(&work, &crafted.id, 0);
    assert_eq!(work.ask(&crafted.id).unwrap().state, AskState::Answered);
    let commands = sync(&work);
    assert_eq!(
        commands.start_write(&crafted.id).unwrap_err().code(),
        ErrorCode::NotFound
    );
    assert!(work.writes(&WriteFilter::default()).unwrap().is_empty());
}

#[test]
fn an_issue_created_from_a_task_becomes_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let id = task(&work);
    let commands = sync(&work);
    let mut create = close(id, None);
    create.operation = WriteOperation::CreateIssue;
    create.target = None;
    create.before = WriteFields::default();
    create.after = WriteFields {
        title: Some("Write the release notes".into()),
        ..WriteFields::default()
    };
    let ask = commands
        .propose_write(member(SAM), create, "t", "b")
        .unwrap()
        .unwrap()
        .proposal
        .ask;
    answer(&work, &ask, 0);
    changed(commands.start_write(&ask).unwrap());
    let issue = ExternalRef {
        system: ExternalSystem::Github,
        key: "example-org/demo-repo#8".into(),
        url: Some("https://github.com/example-org/demo-repo/issues/8".into()),
    };
    changed(
        commands
            .finish_write(
                &ask,
                WriteResult::Sent {
                    created: Some(issue.clone()),
                    url: issue.url.clone(),
                },
            )
            .unwrap(),
    );
    let task = work.task(&TaskRef::Id(id)).unwrap();
    assert_eq!(task.source, Some(issue.clone()));
    assert_eq!(
        commands.task_by_source(&issue).unwrap().map(|t| t.id),
        Some(id)
    );
    // The write's events are the task's activity.
    let (revs, _) = work
        .read(|c| {
            pitcrew_hub_work::query::revs_matching(
                c,
                &pitcrew_hub_work::RefFilter {
                    task: Some(id),
                    ..pitcrew_hub_work::RefFilter::default()
                },
                u64::MAX,
                100,
                pitcrew_hub_work::REF_SCAN_BUDGET,
            )
        })
        .unwrap();
    let events = work.store().since(0, 100_000).unwrap();
    let kinds: Vec<&str> = events
        .iter()
        .filter(|e| revs.contains(&e.rev))
        .filter_map(|e| match &e.event.body {
            EventBody::WriteProposed { .. } => Some("write_proposed"),
            EventBody::WriteStarted { .. } => Some("write_started"),
            EventBody::WriteFinished { .. } => Some("write_finished"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["write_proposed", "write_started", "write_finished"]
    );
}
