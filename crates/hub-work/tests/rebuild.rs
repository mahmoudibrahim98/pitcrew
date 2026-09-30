//! Acceptance: dropping and rebuilding every projection from the log gives identical tables, and
//! so does meeting the projections only after the log was written, or applying one event at a
//! time.

mod common;

use common::{RUNNER, SAM, WRITER, agent, demo, member, person, seeded};
use pitcrew_hub_work::projection::{NAMES, TABLES};
use pitcrew_hub_work::{
    AnswerAsk, BriefEdit, NewAsk, NewComment, NewTask, TaskRef, WorkService, WorkstreamPatch,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{DispatchId, EventId, MemberId, SubtaskId, TaskId};
use pitcrew_protocol::model::{
    AskKind, BriefTarget, Dispatch, DispatchOutcome, Health, LinkBasis, Liveness, Priority,
    SessionState, Subtask, SubtaskSource, TaskStatus, Team,
};
use pitcrew_protocol::transcript::{PlanItem, PlanStatus};
use pitcrew_store::sql::types::Value;
use pitcrew_store::{Store, StoreOptions, StoredEvent};
use std::collections::BTreeMap;
use std::path::Path;

type Tables = BTreeMap<&'static str, Vec<String>>;

/// Every work table, rows as text, sorted (row order on disk is not part of the contract).
fn dump(store: &Store) -> Tables {
    store
        .read(|c| -> pitcrew_store::Result<Tables> {
            let mut out = Tables::new();
            for table in TABLES {
                let mut stmt = c.prepare(&format!("SELECT * FROM {table}"))?;
                let n = stmt.column_count();
                let mut rows: Vec<String> = stmt
                    .query_map([], |r| {
                        (0..n)
                            .map(|i| r.get::<_, Value>(i))
                            .collect::<Result<Vec<_>, _>>()
                            .map(|v| format!("{v:?}"))
                    })?
                    .collect::<Result<_, _>>()?;
                rows.sort();
                out.insert(table, rows);
            }
            Ok(out)
        })
        .expect("dump")
}

fn raw(work: &WorkService, author: MemberId, obo: Option<MemberId>, body: EventBody) {
    let event = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: work.workspace(),
        author,
        on_behalf_of: obo,
        body,
    };
    work.store().append(&[event]).expect("append");
}

/// A bit of everything, through the commands where there is one and as raw events otherwise.
fn workload(work: &WorkService) {
    let sam = person(SAM);
    let writer = agent(WRITER);
    let pap = "01JB000000000000000PRJ0001".parse().expect("project");
    let task = work
        .create_task(
            &sam,
            NewTask {
                project: pap,
                workstream: Some("01JB000000000000000WST0002".parse().expect("ws")),
                title: "Rerun seed 3".into(),
                description: Some("With lr 1e-4.".into()),
                status: None,
                priority: Some(Priority::Urgent),
                assignee: None,
                labels: Some(vec!["compute".into(), "rerun".into()]),
                due: None,
            },
        )
        .expect("create");
    let key = TaskRef::Id(task.id);
    work.assign_task(&sam, &key, Some(member(RUNNER)))
        .expect("assign");
    work.move_task(&agent(RUNNER), &key, TaskStatus::InProgress)
        .expect("agent move");
    work.move_task(
        &sam,
        &TaskRef::parse("PAP-7").expect("key"),
        TaskStatus::Todo,
    )
    .expect("person move");
    work.replace_subtasks(
        &sam,
        &TaskRef::parse("PAP-1").expect("key"),
        vec![Subtask {
            id: SubtaskId::new(),
            text: "Agree the outline".into(),
            done: true,
            source: SubtaskSource::Human,
        }],
    )
    .expect("subtasks");
    work.mirror_plan(
        &"01JB000000000000000SES0001".parse().expect("session"),
        &[
            PlanItem {
                text: "Write §3.2".into(),
                status: PlanStatus::Completed,
            },
            PlanItem {
                text: "Write §3.3".into(),
                status: PlanStatus::InProgress,
            },
        ],
    )
    .expect("mirror");
    work.post_comment(
        &writer,
        &TaskRef::parse("PAP-1").expect("key"),
        NewComment {
            text: "@sam §3.2 is drafted.".into(),
            mentions: vec![member(SAM)],
        },
    )
    .expect("comment");
    let ask = work
        .raise_ask(
            &writer,
            NewAsk {
                kind: AskKind::Decision,
                to: member(SAM),
                title: "Two pages or three?".into(),
                body: None,
                options: Some(vec!["Two".into(), "Three".into()]),
                task: Some("01JB000000000000000TSK0001".parse().expect("task")),
                session: None,
                receipts: None,
            },
        )
        .expect("ask");
    work.answer_ask(
        &sam,
        &ask.id,
        AnswerAsk {
            option: Some(0),
            text: None,
        },
    )
    .expect("answer");
    work.put_brief(
        &sam,
        BriefTarget::Workstream("01JB000000000000000WST0004".parse().expect("ws")),
        BriefEdit {
            text: "Not started.".into(),
            next: None,
            pinned: true,
        },
    )
    .expect("brief");
    work.patch_workstream(
        &sam,
        &"01JB000000000000000WST0004".parse().expect("ws"),
        WorkstreamPatch {
            status: Some(pitcrew_protocol::model::WorkstreamStatus::Active),
            health: Some(Health::AtRisk),
        },
    )
    .expect("patch");

    // Things without commands yet: runners' session events, dispatches, membership changes.
    let dispatch = Dispatch {
        id: DispatchId::new(),
        task: task.id,
        agent: member(RUNNER),
        session: Some("01JB000000000000000SES0002".parse().expect("session")),
        brief: "Rerun it.".into(),
        started: 1_790_800_000_000,
        ended: None,
        outcome: None,
        summary: None,
    };
    raw(
        work,
        member(SAM),
        None,
        EventBody::DispatchStarted {
            dispatch: dispatch.clone(),
        },
    );
    raw(
        work,
        member(RUNNER),
        Some(member(SAM)),
        EventBody::DispatchFinished {
            dispatch: dispatch.id,
            outcome: DispatchOutcome::Failed,
            summary: Some("Diverged again.".into()),
        },
    );
    let ses2 = "01JB000000000000000SES0002".parse().expect("session");
    raw(
        work,
        member(RUNNER),
        Some(member(SAM)),
        EventBody::SessionStateChanged {
            session: ses2,
            from: SessionState::Working,
            to: SessionState::Idle,
            status_line: None,
        },
    );
    raw(
        work,
        member(RUNNER),
        Some(member(SAM)),
        EventBody::SessionLinked {
            session: ses2,
            workstream: None,
            task: Some(task.id),
            basis: LinkBasis::Manual,
        },
    );
    raw(
        work,
        member(RUNNER),
        Some(member(SAM)),
        EventBody::SessionUpdated {
            session: ses2,
            title: Some("Seed 3 rerun".into()),
            branch: None,
        },
    );
    raw(
        work,
        member(RUNNER),
        None,
        EventBody::SessionEnded { session: ses2 },
    );
    raw(
        work,
        member(SAM),
        None,
        EventBody::MachineLiveness {
            machine: "01JB000000000000000MCH0003".parse().expect("machine"),
            liveness: Liveness::Live,
        },
    );
    raw(
        work,
        member(SAM),
        None,
        EventBody::TeamSaved {
            team: Team {
                id: "01JB000000000000000TEA0001".parse().expect("team"),
                name: "Paper team".into(),
                lead: member(SAM),
                members: vec![member(SAM), member(WRITER)],
            },
        },
    );
    // Events about things nobody knows change nothing, and do not fail.
    raw(
        work,
        member(SAM),
        None,
        EventBody::TaskMoved {
            task: TaskId::new(),
            from: TaskStatus::Todo,
            to: TaskStatus::Done,
            mover: pitcrew_protocol::model::Mover::Person,
        },
    );
    raw(
        work,
        member(SAM),
        None,
        EventBody::SubtasksReplaced {
            task: TaskId::new(),
            subtasks: Vec::new(),
        },
    );
}

fn all_events(store: &Store) -> Vec<Event> {
    store
        .since(0, usize::MAX)
        .expect("log")
        .into_iter()
        .map(|StoredEvent { event, .. }| event)
        .collect()
}

fn fresh(path: &Path, projections: bool) -> Store {
    let list = if projections {
        pitcrew_hub_work::projections()
    } else {
        Vec::new()
    };
    Store::open_with(path, StoreOptions::default(), list).expect("open")
}

/// Every task's document matches its filter columns and its child rows.
fn task_documents_agree_with_their_columns(work: &WorkService) {
    type Row = (String, String, Option<String>, String, Vec<String>);
    let rows: Vec<Row> = work
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT t.id, t.status, t.assignee, t.doc,
                   (SELECT json_group_array(s.id || ':' || s.text || ':' || s.done || ':' ||
                      COALESCE(s.agent, '-')) FROM (SELECT * FROM work_subtasks s
                      WHERE s.task = t.id ORDER BY s.position) s)
                 FROM work_tasks t",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    let lines: String = r.get(4)?;
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        serde_json::from_str(&lines).expect("lines"),
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .expect("read");
    assert!(rows.len() > 10);
    for (id, status, assignee, doc, lines) in rows {
        let task: pitcrew_protocol::model::Task = serde_json::from_str(&doc).expect("doc");
        assert_eq!(task.id.0.to_string(), id);
        assert_eq!(
            serde_json::to_value(task.status).expect("status"),
            status.as_str()
        );
        assert_eq!(task.assignee.map(|a| a.0.to_string()), assignee);
        let expected: Vec<String> = task
            .subtasks
            .iter()
            .map(|s| {
                let agent = match s.source {
                    SubtaskSource::Human => "-".to_owned(),
                    SubtaskSource::AgentPlan { agent } => agent.0.to_string(),
                };
                format!("{}:{}:{}:{agent}", s.id.0, s.text, i32::from(s.done))
            })
            .collect();
        assert_eq!(lines, expected, "{}", task.key);
    }
}

#[test]
fn rebuilding_every_projection_gives_identical_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    workload(&work);
    let incremental = dump(work.store());
    for (table, rows) in &incremental {
        assert!(
            !rows.is_empty(),
            "{table} is empty; the workload should fill it"
        );
    }
    task_documents_agree_with_their_columns(&work);

    for name in NAMES {
        work.store().rebuild(name).expect("rebuild");
    }
    assert_eq!(dump(work.store()), incremental, "rebuilt in place");

    let events = all_events(work.store());
    // The log first, the projections later: the open builds them from scratch.
    let late = dir.path().join("late.db");
    let store = fresh(&late, false);
    store.append(&events).expect("append");
    drop(store);
    assert_eq!(dump(&fresh(&late, true)), incremental, "built on open");

    // One event per append.
    let one_by_one = fresh(&dir.path().join("one.db"), true);
    for event in &events {
        one_by_one
            .append(std::slice::from_ref(event))
            .expect("append");
    }
    assert_eq!(dump(&one_by_one), incremental, "one event at a time");
}

#[test]
fn the_seed_alone_rebuilds_identically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let seeded_tables = dump(work.store());
    for name in NAMES {
        work.store().rebuild(name).expect("rebuild");
    }
    assert_eq!(dump(work.store()), seeded_tables);
    assert_eq!(
        all_events(work.store()).len(),
        demo_event_count(),
        "one append of the snapshot and the demo's slice"
    );
}

fn demo_event_count() -> usize {
    let demo = demo();
    let back_office = demo
        .briefs
        .iter()
        .filter(|b| b.source == pitcrew_protocol::model::BriefSource::BackOffice)
        .count();
    demo.machines.len()
        + demo.personas.len()
        + demo.members.len()
        + demo.teams.len()
        + demo.projects.len()
        + demo.workstreams.len()
        + demo.tasks.len()
        + demo.sessions.len()
        + demo.dispatches.len()
        + demo.asks.len()
        + demo.briefs.len()
        + back_office
        + demo.events.len()
}
