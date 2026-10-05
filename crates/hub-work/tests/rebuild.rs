//! Acceptance: dropping and rebuilding every projection from the log gives identical tables, and
//! so does meeting the projections only after the log was written, or applying one event at a
//! time.

mod common;

use common::{RUNNER, SAM, WRITER, agent, demo, member, person, seeded};
use pitcrew_hub_work::projection::{NAMES, TABLES};
use pitcrew_hub_work::{
    AnswerAsk, BriefEdit, NewAsk, NewComment, NewProject, NewTask, NewWorkstream, TaskPatch,
    TaskRef, WorkService, WorkstreamPatch,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{
    DispatchId, EventId, MemberId, ProjectId, ProjectKey, SubtaskId, TaskId,
};
use pitcrew_protocol::model::{
    AskKind, BriefSource, BriefTarget, Date, Dispatch, DispatchOutcome, Health, LinkBasis,
    Liveness, Location, Priority, Receipt, SessionState, Subtask, SubtaskSource, TaskStatus, Team,
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
    work.move_cursor(&person(SAM), "workspace", 1)
        .expect("read cursor");
    let sam = person(SAM);
    work.save_safety(
        &sam,
        pitcrew_protocol::onboarding::SafetySettings {
            back_office_enabled: true,
            ..Default::default()
        },
    )
    .expect("safety preferences");
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
            external: None,
        },
    )
    .expect("patch");
    work.patch_workstream(
        &sam,
        &"01JB000000000000000WST0004".parse().expect("ws"),
        WorkstreamPatch {
            external: Some(vec![pitcrew_protocol::model::ExternalRef {
                system: pitcrew_protocol::model::ExternalSystem::Github,
                key: "example-org/demo-repo#milestone:1".into(),
                url: Some("https://github.com/example-org/demo-repo/milestone/1".into()),
            }]),
            ..WorkstreamPatch::default()
        },
    )
    .expect("link");

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

    // What a second writer could append: a task taking a key already held (recorded as a clash),
    // and a move from a status the task has left (ignored).
    let mut clash = work
        .task(&TaskRef::parse("PAP-2").expect("key"))
        .expect("task");
    clash.id = TaskId::new();
    clash.title = "Same key, other task".into();
    raw(
        work,
        member(SAM),
        None,
        EventBody::TaskCreated { task: clash },
    );
    raw(
        work,
        member(SAM),
        None,
        EventBody::TaskMoved {
            task: "01JB000000000000000TSK0002".parse().expect("task"),
            from: TaskStatus::Review,
            to: TaskStatus::Done,
            mover: pitcrew_protocol::model::Mover::Person,
        },
    );
    // A runner re-stating a dispatched session without its link keeps the link.
    let mut ses1 = work
        .session(&"01JB000000000000000SES0001".parse().expect("session"))
        .expect("session");
    ses1.workstream = None;
    ses1.task = None;
    ses1.link_basis = None;
    ses1.title = Some("Re-stated".into());
    raw(
        work,
        member(WRITER),
        Some(member(SAM)),
        EventBody::SessionDiscovered { session: ses1 },
    );
    // A comment on a workstream, and a decision.
    raw(
        work,
        member(SAM),
        None,
        EventBody::CommentPosted {
            task: None,
            workstream: Some("01JB000000000000000WST0003".parse().expect("ws")),
            text: "Parsers look good.".into(),
            mentions: Vec::new(),
        },
    );
    raw(
        work,
        member(SAM),
        None,
        EventBody::DecisionRecorded {
            workstream: Some("01JB000000000000000WST0002".parse().expect("ws")),
            text: "Keep lr 1e-4.".into(),
            why: None,
            receipts: Vec::new(),
        },
    );
    edits(work, task.id);
    writes(work, task.id);
}

/// Outward writes (`work.writes`): one approved, failed, retried at a person's request and sent
/// (an issue created from the task, which gives the task its source), one denied, one failed with
/// a retry asked for and not yet sent, and a stale start that changes nothing.
fn writes(work: &WorkService, task: TaskId) {
    use pitcrew_protocol::model::{ExternalRef, ExternalSystem};
    use pitcrew_protocol::writes::{WriteFields, WriteOperation, WriteProposal, WriteResult};
    let sam = person(SAM);
    let sync = work.ensure_sync_member(member(SAM)).expect("sync member");
    let commands = work.sync_commands(sync.id).expect("sync commands");
    let proposal = |operation, after| WriteProposal {
        ask: pitcrew_protocol::ids::AskId::new(),
        integration: pitcrew_protocol::ids::IntegrationId::new(),
        system: ExternalSystem::Github,
        scope: "example-org/demo-repo".into(),
        target: None,
        task: Some(task),
        operation,
        before: WriteFields::default(),
        after,
        requested_by: member(SAM),
        cause: None,
    };
    let create = commands
        .propose_write(
            member(SAM),
            proposal(
                WriteOperation::CreateIssue,
                WriteFields {
                    title: Some("Created upstream".into()),
                    ..WriteFields::default()
                },
            ),
            "GitHub: create an issue",
            "Title: Created upstream",
        )
        .expect("propose")
        .expect("proposed");
    let ask = create.proposal.ask;
    work.answer_ask(
        &sam,
        &ask,
        AnswerAsk {
            option: Some(0),
            text: None,
        },
    )
    .expect("approve");
    commands.start_write(&ask).expect("start");
    commands
        .finish_write(
            &ask,
            WriteResult::Failed {
                message: "Bad gateway".into(),
                status: Some(502),
            },
        )
        .expect("fail");
    work.request_retry(&sam, &ask).expect("ask to retry");
    commands.start_write(&ask).expect("retry");
    commands
        .finish_write(
            &ask,
            WriteResult::Sent {
                created: Some(ExternalRef {
                    system: ExternalSystem::Github,
                    key: "example-org/demo-repo#8".into(),
                    url: Some("https://github.com/example-org/demo-repo/issues/8".into()),
                }),
                url: Some("https://github.com/example-org/demo-repo/issues/8".into()),
            },
        )
        .expect("sent");
    // A start a second writer appended after it was sent changes nothing.
    raw(
        work,
        sync.id,
        Some(member(SAM)),
        EventBody::WriteStarted {
            ask,
            task: Some(task),
            attempt: 3,
        },
    );
    let comment = commands
        .propose_write(
            member(SAM),
            proposal(
                WriteOperation::Comment,
                WriteFields {
                    comment: Some("Done here.".into()),
                    ..WriteFields::default()
                },
            ),
            "GitHub: comment",
            "Done here.",
        )
        .expect("propose")
        .expect("proposed");
    work.answer_ask(
        &sam,
        &comment.proposal.ask,
        AnswerAsk {
            option: Some(1),
            text: None,
        },
    )
    .expect("deny");
    commands
        .finish_write(
            &comment.proposal.ask,
            WriteResult::NotSent {
                reason: "Not sent: Sam chose not to.".into(),
            },
        )
        .expect("not sent");
    let waiting = commands
        .propose_write(
            member(SAM),
            proposal(
                WriteOperation::Comment,
                WriteFields {
                    comment: Some("Try again.".into()),
                    ..WriteFields::default()
                },
            ),
            "GitHub: comment",
            "Try again.",
        )
        .expect("propose")
        .expect("proposed")
        .proposal
        .ask;
    work.answer_ask(
        &sam,
        &waiting,
        AnswerAsk {
            option: Some(0),
            text: None,
        },
    )
    .expect("approve");
    commands.start_write(&waiting).expect("start");
    commands
        .finish_write(
            &waiting,
            WriteResult::Failed {
                message: "Bad gateway".into(),
                status: Some(502),
            },
        )
        .expect("fail");
    work.request_retry(&sam, &waiting).expect("ask to retry");
}

/// Task edits, new projects and workstreams, and brief proposals.
fn edits(work: &WorkService, rerun: TaskId) {
    let sam = person(SAM);
    let thesis = work
        .create_project(
            &sam,
            NewProject {
                key: ProjectKey::new("THS").expect("key"),
                name: "Thesis".into(),
                lead: None,
                members: Some(vec![member(WRITER)]),
                status: None,
                start: Some(Date("2026-10-01".into())),
                due: Some(Date("2027-06-30".into())),
                root: Some(Location {
                    machine: "01JB000000000000000MCH0001".parse().expect("machine"),
                    path: "/work/thesis".into(),
                    branch: None,
                }),
            },
        )
        .expect("project");
    let chapter = work
        .create_workstream(
            &sam,
            NewWorkstream {
                project: thesis.id,
                name: "Chapter 1".into(),
                status: None,
                locations: Some(vec![Location {
                    machine: "01JB000000000000000MCH0001".parse().expect("machine"),
                    path: "/work/thesis/ch1".into(),
                    branch: Some("ch1".into()),
                }]),
            },
        )
        .expect("workstream");
    let outline = work
        .create_task(
            &sam,
            NewTask {
                project: thesis.id,
                workstream: None,
                title: "Outline".into(),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                labels: None,
                due: None,
            },
        )
        .expect("task");
    work.patch_task(
        &sam,
        &TaskRef::Id(outline.id),
        TaskPatch {
            workstream: Some(Some(chapter.id)),
            title: Some("  Outline chapter 1 ".into()),
            labels: Some(vec!["outline".into(), " writing ".into(), "outline".into()]),
            start: Some(Some(Date("2026-10-02".into()))),
            due: Some(Some(Date("2026-10-20".into()))),
            ..TaskPatch::default()
        },
    )
    .expect("patch");
    work.patch_task(
        &sam,
        &TaskRef::parse("PAP-2").expect("key"),
        TaskPatch {
            workstream: Some(Some("01JB000000000000000WST0002".parse().expect("ws"))),
            description: Some("Mean and spread.".into()),
            priority: Some(Priority::High),
            blocked_by: Some(vec![
                rerun,
                "01JB000000000000000TSK0001".parse().expect("task"),
            ]),
            accept_auto: Some(true),
            ..TaskPatch::default()
        },
    )
    .expect("patch");
    work.patch_task(
        &sam,
        &TaskRef::parse("PAP-1").expect("key"),
        TaskPatch {
            workstream: Some(None),
            due: Some(None),
            labels: Some(Vec::new()),
            ..TaskPatch::default()
        },
    )
    .expect("clear");
    // A task_updated for a task nobody knows changes nothing.
    raw(
        work,
        member(SAM),
        None,
        EventBody::TaskUpdated {
            task: TaskId::new(),
            patch: TaskPatch {
                title: Some("Ghost".into()),
                ..TaskPatch::default()
            },
        },
    );
    // A racing writer's project with a key already held is not applied.
    let mut clash = thesis.clone();
    clash.id = ProjectId::new();
    clash.name = "Same key".into();
    raw(
        work,
        member(SAM),
        None,
        EventBody::ProjectCreated { project: clash },
    );
    // Briefs: a pending proposal; one accepted unchanged; one for a target without a brief; one
    // the back office applies itself.
    let office = member("01JB000000000000000MEM0006");
    let receipts = vec![Receipt::Event { id: EventId::new() }];
    let seeds = BriefTarget::Workstream("01JB000000000000000WST0002".parse().expect("ws"));
    raw(
        work,
        office,
        Some(member(SAM)),
        EventBody::BriefProposed {
            target: seeds,
            text: "Seed 3 converged.".into(),
            next: Some("Make figure 3.".into()),
            receipts: receipts.clone(),
        },
    );
    work.put_brief(
        &sam,
        seeds,
        BriefEdit {
            text: "Seed 3 converged.".into(),
            next: Some("Make figure 3.".into()),
            pinned: true,
        },
    )
    .expect("accept");
    raw(
        work,
        office,
        Some(member(SAM)),
        EventBody::BriefProposed {
            target: seeds,
            text: "Figure 3 is drafted.".into(),
            next: None,
            receipts: receipts.clone(),
        },
    );
    raw(
        work,
        office,
        Some(member(SAM)),
        EventBody::BriefProposed {
            target: BriefTarget::Workstream(chapter.id),
            text: "Not started.".into(),
            next: None,
            receipts: receipts.clone(),
        },
    );
    raw(
        work,
        office,
        Some(member(SAM)),
        EventBody::BriefAccepted {
            target: BriefTarget::Project(thesis.id),
            text: "Planned.".into(),
            next: Some("Outline chapter 1.".into()),
            pinned: false,
            receipts,
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

/// One `work_tasks` row with its child rows, as text.
struct TaskRow {
    id: String,
    project: String,
    key_prefix: String,
    number: i64,
    workstream: Option<String>,
    status: String,
    assignee: Option<String>,
    doc: String,
    lines: Vec<String>,
    deps: Vec<String>,
    labels: Vec<String>,
}

/// Every task's document matches its filter columns and its child rows: project, workstream,
/// key, status, assignee, subtasks, dependencies and labels.
fn task_documents_agree_with_their_columns(work: &WorkService) {
    let rows: Vec<TaskRow> = work
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT t.id, t.project, t.key_prefix, t.number, t.workstream, t.status,
                   t.assignee, t.doc,
                   (SELECT json_group_array(s.id || ':' || s.text || ':' || s.done || ':' ||
                      COALESCE(s.agent, '-')) FROM (SELECT * FROM work_subtasks s
                      WHERE s.task = t.id ORDER BY s.position) s),
                   (SELECT json_group_array(d.blocked_by) FROM (SELECT * FROM work_task_deps d
                      WHERE d.task = t.id ORDER BY d.position) d),
                   (SELECT json_group_array(l.label) FROM (SELECT * FROM work_task_labels l
                      WHERE l.task = t.id ORDER BY l.position) l)
                 FROM work_tasks t",
            )?;
            let list = |r: &pitcrew_store::sql::Row<'_>, i: usize| -> Vec<String> {
                let text: String = r.get(i).expect("list");
                serde_json::from_str(&text).expect("JSON list")
            };
            let rows = stmt
                .query_map([], |r| {
                    Ok(TaskRow {
                        id: r.get(0)?,
                        project: r.get(1)?,
                        key_prefix: r.get(2)?,
                        number: r.get(3)?,
                        workstream: r.get(4)?,
                        status: r.get(5)?,
                        assignee: r.get(6)?,
                        doc: r.get(7)?,
                        lines: list(r, 8),
                        deps: list(r, 9),
                        labels: list(r, 10),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .expect("read");
    assert!(rows.len() > 10);
    assert!(rows.iter().any(|r| !r.labels.is_empty()), "some labels");
    for row in rows {
        let TaskRow {
            id,
            status,
            assignee,
            doc,
            lines,
            ..
        } = &row;
        let task: pitcrew_protocol::model::Task = serde_json::from_str(doc).expect("doc");
        assert_eq!(&task.id.0.to_string(), id);
        assert_eq!(task.project.0.to_string(), row.project);
        assert_eq!(task.workstream.map(|w| w.0.to_string()), row.workstream);
        assert_eq!(task.key.project.as_str(), row.key_prefix);
        assert_eq!(i64::from(task.key.number), row.number);
        assert_eq!(
            task.blocked_by
                .iter()
                .map(|t| t.0.to_string())
                .collect::<Vec<_>>(),
            row.deps
        );
        assert_eq!(task.labels, row.labels);
        assert_eq!(
            serde_json::to_value(task.status).expect("status"),
            status.as_str()
        );
        assert_eq!(&task.assignee.map(|a| a.0.to_string()), assignee);
        let lines = lines.clone();
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
    // A back-office brief is a proposal and its acceptance; a person's is one acceptance.
    let per_brief = |b: &pitcrew_protocol::model::Brief| {
        if b.source == BriefSource::BackOffice {
            2
        } else {
            1
        }
    };
    let briefs: usize = demo.briefs.iter().map(per_brief).sum();
    // Briefs the demo's slice accepts again are put in force again after it.
    let restated: usize = demo
        .briefs
        .iter()
        .filter(|b| {
            demo.events.iter().rev().find_map(|e| match &e.body {
                EventBody::BriefAccepted { target, .. } if *target == b.target => Some(true),
                EventBody::BriefProposed { target, .. } if *target == b.target => Some(false),
                _ => None,
            }) == Some(true)
        })
        .map(per_brief)
        .sum();
    assert_eq!(restated, 1, "the seed-runs brief");
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
        + briefs
        + demo.events.len()
        + restated
}
