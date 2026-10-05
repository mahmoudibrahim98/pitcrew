//! A tracker sync's commands (`SyncCommands`) and a workstream's links upstream.
//!
//! - the sync acts as its own member (`@sync`, an agent of the person), and every event says so;
//! - tasks keep their upstream source, and a second create for the same source is the same task;
//! - moves follow `can_move(.., Sync)`: in-progress work is never touched, and a move to where the
//!   task already is appends nothing;
//! - a workstream is shipped only while none of its tasks is in progress;
//! - conflicts and notes are never raised or posted twice;
//! - `PATCH /v1/workstreams/{id}` with `external` appends `workstream_linked`.

mod common;

use common::{PAPER, SAM, SEED_RUNS, SUBMISSION, WRITER, agent, member, person, seeded};
use pitcrew_hub_work::{
    SYNC_FALLBACK_HANDLE, SYNC_HANDLE, SyncOutcome, WorkError, WorkService, WorkstreamPatch,
    links::{LinkScope, scope_of},
};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    AskKind, ExternalRef, ExternalSystem, Member, MemberKind, Mover, TaskStatus, WorkstreamStatus,
};

fn last_events(work: &WorkService, n: usize) -> Vec<Event> {
    let latest = work.store().latest_rev().expect("rev");
    work.store()
        .since(latest.saturating_sub(n as u64), n)
        .expect("since")
        .into_iter()
        .map(|s| s.event)
        .collect()
}

fn issue(n: u64) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("example-org/demo-repo#{n}"),
        url: Some(format!(
            "https://github.com/example-org/demo-repo/issues/{n}"
        )),
    }
}

fn code(e: &WorkError) -> ErrorCode {
    e.code()
}

#[test]
fn the_sync_acts_as_its_own_member_of_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let sync = work.ensure_sync_member(member(SAM)).expect("sync member");
    assert_eq!(sync.handle, SYNC_HANDLE);
    assert_eq!(sync.kind, MemberKind::Agent);
    assert_eq!(sync.owner, Some(member(SAM)));
    let again = work.ensure_sync_member(member(SAM)).expect("again");
    assert_eq!(again.id, sync.id);

    // Only a person owns it.
    let err = work.ensure_sync_member(member(WRITER)).unwrap_err();
    assert_eq!(code(&err), ErrorCode::Invalid);
    // An agent of nobody cannot act as the sync.
    assert!(work.sync_commands(member(SAM)).is_err());
}

#[test]
fn a_taken_handle_falls_back_to_the_other_one() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let squatter = Member {
        id: pitcrew_protocol::ids::MemberId::new(),
        kind: MemberKind::Human,
        handle: SYNC_HANDLE.into(),
        name: "Someone".into(),
        owner: None,
        persona: None,
        avatar: None,
    };
    work.store()
        .append(&[Event::now(
            work.workspace(),
            member(SAM),
            EventBody::MemberAdded { member: squatter },
        )])
        .unwrap();
    let sync = work.ensure_sync_member(member(SAM)).expect("sync member");
    assert_eq!(sync.handle, SYNC_FALLBACK_HANDLE);
}

#[test]
fn tasks_keep_their_source_and_are_created_once() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let sync_member = work.ensure_sync_member(member(SAM)).unwrap();
    let sync = work.sync_commands(sync_member.id).unwrap();
    let ws: WorkstreamId = SEED_RUNS.parse().unwrap();
    let labels = vec!["bug".to_string(), " bug".to_string(), String::new()];
    let task = sync
        .create_task(
            &ws,
            issue(1),
            "  Fix flaky login\u{200b} test ",
            "Steps.",
            &labels,
        )
        .expect("create");
    assert_eq!(task.title, "Fix flaky login test");
    assert_eq!(task.labels, vec!["bug".to_string()]);
    assert_eq!(task.status, TaskStatus::Todo);
    assert_eq!(task.workstream, Some(ws));
    assert_eq!(task.project, PAPER.parse().unwrap());
    assert_eq!(task.source, Some(issue(1)));
    let created = last_events(&work, 1).pop().unwrap();
    assert_eq!(created.author, sync_member.id);
    assert_eq!(created.on_behalf_of, Some(member(SAM)));

    let rev = work.store().latest_rev().unwrap();
    let again = sync
        .create_task(&ws, issue(1), "Other", "", &[])
        .expect("again");
    assert_eq!(again.id, task.id);
    assert_eq!(work.store().latest_rev().unwrap(), rev);
    assert_eq!(sync.task_by_source(&issue(1)).unwrap().unwrap().id, task.id);
    assert!(sync.task_by_source(&issue(2)).unwrap().is_none());

    // Updates change only what differs.
    let updated = sync
        .update_task(
            &task.id,
            Some("Fix login"),
            None,
            Some(&["bug".into()]),
            None,
        )
        .unwrap();
    assert!(matches!(updated, SyncOutcome::Changed(ref t) if t.title == "Fix login"));
    let last = last_events(&work, 1).pop().unwrap();
    match last.body {
        EventBody::TaskUpdated { patch, .. } => {
            assert_eq!(patch.title.as_deref(), Some("Fix login"));
            assert!(patch.labels.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }
    let rev = work.store().latest_rev().unwrap();
    assert_eq!(
        sync.update_task(&task.id, Some("Fix login"), Some("Steps."), None, Some(ws))
            .unwrap(),
        SyncOutcome::Unchanged
    );
    assert_eq!(work.store().latest_rev().unwrap(), rev);
    // A workstream of another project is refused.
    let other: WorkstreamId = common::PARSERS.parse().unwrap();
    let err = sync
        .update_task(&task.id, None, None, None, Some(other))
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::Invalid);
}

#[test]
fn moves_follow_the_sync_rules() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let sync_member = work.ensure_sync_member(member(SAM)).unwrap();
    let sync = work.sync_commands(sync_member.id).unwrap();
    // PAP-5 is todo, PAP-1 in progress, PAP-7 done.
    let todo: TaskId = "01JB000000000000000TSK0005".parse().unwrap();
    let in_progress: TaskId = "01JB000000000000000TSK0001".parse().unwrap();
    let done: TaskId = "01JB000000000000000TSK0007".parse().unwrap();

    assert!(matches!(
        sync.move_task(&todo, TaskStatus::Done).unwrap(),
        SyncOutcome::Changed(ref t) if t.status == TaskStatus::Done
    ));
    let moved = last_events(&work, 1).pop().unwrap();
    assert!(matches!(
        moved.body,
        EventBody::TaskMoved {
            mover: Mover::Sync,
            to: TaskStatus::Done,
            ..
        }
    ));
    let rev = work.store().latest_rev().unwrap();
    assert_eq!(
        sync.move_task(&done, TaskStatus::Done).unwrap(),
        SyncOutcome::Unchanged
    );
    assert!(matches!(
        sync.move_task(&in_progress, TaskStatus::Done).unwrap(),
        SyncOutcome::Refused(_)
    ));
    assert_eq!(work.store().latest_rev().unwrap(), rev);
    assert!(matches!(
        sync.move_task(&done, TaskStatus::Todo).unwrap(),
        SyncOutcome::Changed(_)
    ));
}

#[test]
fn a_workstream_ships_only_without_work_in_progress() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let sync_member = work.ensure_sync_member(member(SAM)).unwrap();
    let sync = work.sync_commands(sync_member.id).unwrap();
    // Seed runs has PAP-4 in progress; the noise schedule idea has no tasks.
    let busy: WorkstreamId = SEED_RUNS.parse().unwrap();
    let idle: WorkstreamId = "01JB000000000000000WST0004".parse().unwrap();
    assert!(sync.work_in_progress(&busy).unwrap());
    assert!(matches!(
        sync.set_workstream_status(&busy, WorkstreamStatus::Shipped)
            .unwrap(),
        SyncOutcome::Refused(_)
    ));
    assert!(matches!(
        sync.set_workstream_status(&idle, WorkstreamStatus::Shipped).unwrap(),
        SyncOutcome::Changed(ref w) if w.status == WorkstreamStatus::Shipped
    ));
    assert_eq!(
        sync.set_workstream_status(&idle, WorkstreamStatus::Shipped)
            .unwrap(),
        SyncOutcome::Unchanged
    );
}

#[test]
fn conflicts_and_notes_are_not_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let sync_member = work.ensure_sync_member(member(SAM)).unwrap();
    let sync = work.sync_commands(sync_member.id).unwrap();
    let task: TaskId = "01JB000000000000000TSK0001".parse().unwrap();
    let ask = sync
        .raise_conflict(
            Some(task),
            "GitHub closed example-org/demo-repo#1",
            "PAP-1 is in progress.",
        )
        .unwrap()
        .expect("raised");
    assert_eq!(ask.kind, AskKind::Decision);
    assert_eq!(ask.from, sync_member.id);
    assert_eq!(ask.to, member(SAM));
    assert!(
        sync.raise_conflict(
            Some(task),
            "GitHub closed example-org/demo-repo#1",
            "Again."
        )
        .unwrap()
        .is_none()
    );
    let text = "Merged upstream: https://github.com/example-org/demo-repo/pull/7";
    assert!(sync.note(&task, text).unwrap());
    assert!(!sync.note(&task, text).unwrap());
}

#[test]
fn a_workstream_is_linked_and_unlinked_by_a_person() {
    let dir = tempfile::tempdir().unwrap();
    let work = seeded(dir.path());
    let ws: WorkstreamId = SUBMISSION.parse().unwrap();
    let links = vec![
        ExternalRef {
            system: ExternalSystem::Github,
            key: "example-org/demo-repo#milestone:1".into(),
            url: Some("https://github.com/example-org/demo-repo/milestone/1".into()),
        },
        ExternalRef {
            system: ExternalSystem::Jira,
            key: "DEMO-5".into(),
            url: None,
        },
    ];
    let patch = WorkstreamPatch {
        external: Some(links.clone()),
        ..WorkstreamPatch::default()
    };
    let linked = work
        .patch_workstream(&person(SAM), &ws, patch.clone())
        .expect("link");
    assert_eq!(linked.external, links);
    let event = last_events(&work, 1).pop().unwrap();
    assert!(matches!(
        event.body,
        EventBody::WorkstreamLinked { workstream, ref external } if workstream == ws && *external == links
    ));
    assert_eq!(
        scope_of(&linked.external[0]),
        Some(LinkScope::GithubMilestone {
            repo: "example-org/demo-repo".into(),
            number: 1
        })
    );
    // The same list again appends nothing.
    let rev = work.store().latest_rev().unwrap();
    work.patch_workstream(&person(SAM), &ws, patch).unwrap();
    assert_eq!(work.store().latest_rev().unwrap(), rev);
    // An agent may not; a bad link is refused.
    let err = work
        .patch_workstream(
            &agent(WRITER),
            &ws,
            WorkstreamPatch {
                external: Some(vec![]),
                ..WorkstreamPatch::default()
            },
        )
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::Forbidden);
    let err = work
        .patch_workstream(
            &person(SAM),
            &ws,
            WorkstreamPatch {
                external: Some(vec![ExternalRef {
                    system: ExternalSystem::Github,
                    key: "example-org/demo-repo".into(),
                    url: Some("http://github.com/example-org/demo-repo".into()),
                }]),
                ..WorkstreamPatch::default()
            },
        )
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::Invalid);
    // Status and links together: both events, in one append.
    let both = work
        .patch_workstream(
            &person(SAM),
            &ws,
            WorkstreamPatch {
                status: Some(WorkstreamStatus::Paused),
                external: Some(vec![]),
                ..WorkstreamPatch::default()
            },
        )
        .unwrap();
    assert_eq!(both.status, WorkstreamStatus::Paused);
    assert!(both.external.is_empty());
    let events = last_events(&work, 2);
    assert!(matches!(
        events[0].body,
        EventBody::WorkstreamChanged { .. }
    ));
    assert!(matches!(events[1].body, EventBody::WorkstreamLinked { .. }));
}
