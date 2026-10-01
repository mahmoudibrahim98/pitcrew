//! Tasks move themselves: a dispatched session starting work moves its task to in progress, and
//! an agent's live plan is mirrored as its task's `agent_plan` subtasks, never touching people's
//! lines.

mod common;

use common::{PAPER, RUNNER, SAM, WRITER, member, person, seeded};
use pitcrew_hub_work::{NewTask, TaskRef, WorkService};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{DispatchId, EventId, SessionId, SubtaskId};
use pitcrew_protocol::model::{
    Dispatch, DispatchOutcome, Mover, Subtask, SubtaskSource, Task, TaskStatus,
};
use pitcrew_protocol::transcript::{PlanItem, PlanStatus};

fn append(work: &WorkService, body: EventBody) {
    let event = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: work.workspace(),
        author: member(SAM),
        on_behalf_of: None,
        body,
    };
    work.store().append(&[event]).expect("append");
}

fn last_event(work: &WorkService) -> Event {
    let rev = work.store().latest_rev().expect("rev");
    work.store().since(rev - 1, 1).expect("log").remove(0).event
}

fn item(text: &str, status: PlanStatus) -> PlanItem {
    PlanItem {
        text: text.into(),
        status,
    }
}

fn new_task(work: &WorkService, status: TaskStatus) -> Task {
    work.create_task(
        &person(SAM),
        NewTask {
            project: PAPER.parse().expect("project"),
            workstream: None,
            title: "Dispatched".into(),
            description: None,
            status: Some(status),
            priority: None,
            assignee: None,
            labels: None,
            due: None,
        },
    )
    .expect("create")
}

fn start_dispatch(work: &WorkService, task: &Task) -> DispatchId {
    let dispatch = Dispatch {
        id: DispatchId::new(),
        task: task.id,
        agent: member(RUNNER),
        session: None,
        brief: "Go.".into(),
        started: 1_790_800_000_000,
        ended: None,
        outcome: None,
        summary: None,
    };
    let id = dispatch.id;
    append(work, EventBody::DispatchStarted { dispatch });
    id
}

#[test]
fn a_working_dispatch_moves_its_task_to_in_progress() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let task = new_task(&work, TaskStatus::Todo);
    let dispatch = start_dispatch(&work, &task);

    let moved = work.dispatch_working(&dispatch).expect("working");
    assert_eq!(moved.status, TaskStatus::InProgress);
    let event = last_event(&work);
    assert_eq!(event.author, member(RUNNER));
    assert_eq!(event.on_behalf_of, Some(member(SAM)));
    assert_eq!(
        event.body,
        EventBody::TaskMoved {
            task: task.id,
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
            mover: Mover::Agent { on_own_task: true },
        }
    );

    // Already in progress: nothing more.
    let rev = work.store().latest_rev().expect("rev");
    assert_eq!(
        work.dispatch_working(&dispatch).expect("again").status,
        TaskStatus::InProgress
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);

    // A task in review stays there: an agent may not move it back.
    let review = new_task(&work, TaskStatus::Review);
    let on_review = start_dispatch(&work, &review);
    assert_eq!(
        work.dispatch_working(&on_review).expect("review").status,
        TaskStatus::Review
    );

    // An ended dispatch starts nothing.
    append(
        &work,
        EventBody::DispatchFinished {
            dispatch,
            outcome: DispatchOutcome::Canceled,
            summary: None,
        },
    );
    let err = work.dispatch_working(&dispatch).expect_err("ended");
    assert_eq!(err.code(), ErrorCode::Conflict);
    let err = work
        .dispatch_working(&DispatchId::new())
        .expect_err("unknown");
    assert_eq!(err.code(), ErrorCode::NotFound);
}

#[test]
fn an_agents_plan_is_mirrored_on_its_own_task_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let pap1 = TaskRef::parse("PAP-1").expect("key");
    // A person adds their own line to @writer's PAP-1, first in the list.
    let mut lines = work.task(&pap1).expect("task").subtasks;
    lines.insert(
        0,
        Subtask {
            id: SubtaskId::new(),
            text: "Agree the outline".into(),
            done: true,
            source: SubtaskSource::Human,
        },
    );
    work.replace_subtasks(&person(SAM), &pap1, lines.clone())
        .expect("person's lines");

    // SES0001 is @writer's session on PAP-1. The plan moves on: §3.2 done, a new §3.4.
    let session: SessionId = "01JB000000000000000SES0001".parse().expect("session");
    let plan = [
        item("Read notes/method-outline.md", PlanStatus::Completed),
        item("Write §3.1 Model", PlanStatus::Completed),
        item("Write §3.2 Noise schedule", PlanStatus::Completed),
        item("Write §3.3 Training objective", PlanStatus::InProgress),
        item("Write §3.4 Sampling", PlanStatus::Pending),
    ];
    let task = work
        .mirror_plan(&session, &plan)
        .expect("mirror")
        .expect("the writer's own task");
    assert_eq!(task.subtasks[0], lines[0], "the person's line is untouched");
    let texts: Vec<_> = task.subtasks.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts[1..],
        plan.iter().map(|i| i.text.as_str()).collect::<Vec<_>>()[..]
    );
    // Lines with the same text keep their ids; new ones get new ids.
    for (old, new) in lines[1..].iter().zip(&task.subtasks[1..5]) {
        assert_eq!(old.id, new.id);
    }
    assert!(task.subtasks[3].done, "§3.2 is done now");
    assert!(!lines[1..].iter().any(|s| s.id == task.subtasks[5].id));
    let event = last_event(&work);
    assert_eq!(event.author, member(WRITER));
    assert_eq!(event.on_behalf_of, Some(member(SAM)));

    // The same plan again appends nothing.
    let rev = work.store().latest_rev().expect("rev");
    work.mirror_plan(&session, &plan).expect("again");
    assert_eq!(work.store().latest_rev().expect("rev"), rev);

    // A session without an agent (SES0005), or on a task that is not the agent's, mirrors
    // nothing.
    let unnamed: SessionId = "01JB000000000000000SES0005".parse().expect("session");
    assert_eq!(work.mirror_plan(&unnamed, &plan).expect("unnamed"), None);
    // SES0006 is @writer's ended session on PAP-3; reassign PAP-3 and the dispatch has ended,
    // so it is no longer the writer's.
    work.assign_task(&person(SAM), &TaskRef::parse("PAP-3").expect("key"), None)
        .expect("unassign");
    let ended: SessionId = "01JB000000000000000SES0006".parse().expect("session");
    assert_eq!(work.mirror_plan(&ended, &plan).expect("not own"), None);
    assert_eq!(work.store().latest_rev().expect("rev"), rev + 1);
    assert!(
        work.task(&TaskRef::parse("PAP-3").expect("key"))
            .expect("task")
            .subtasks
            .is_empty()
    );
    let err = work
        .mirror_plan(&SessionId::new(), &plan)
        .expect_err("unknown");
    assert_eq!(err.code(), ErrorCode::NotFound);
}
