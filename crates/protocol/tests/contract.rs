//! Contract tests: wire shapes and rules that every stream relies on. A failure here means a
//! breaking change to the protocol. Bump `PROTOCOL_VERSION` and tell the affected streams.

use pitcrew_protocol::api::{HostInfo, HostRole, NewProject, NewTask, NewWorkstream, StreamFrame};
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{
    CommandId, EventId, MachineId, MemberId, ProjectId, ProjectKey, SessionId, TaskId, TaskKey,
    WorkspaceId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Brief, BriefProposal, BriefSource, Date, Engine, MachineInfo, Member, MemberKind, Mover,
    PermissionMode, Priority, Receipt, Scheduler, Task, TaskPatch, TaskStatus,
};
use pitcrew_protocol::runner::{
    Capability, CommandOutcome, HubToRunner, RunnerCommand, RunnerToHub, decode_line, encode_line,
};
use pitcrew_protocol::{PROTOCOL_VERSION, is_compatible};
use serde_json::json;

#[test]
fn ids_accept_prefixed_and_bare_forms() {
    let id = TaskId::new();
    let shown = id.to_string();
    assert!(shown.starts_with("tsk_"));
    assert_eq!(shown.parse::<TaskId>().unwrap(), id);
    assert_eq!(id.0.to_string().parse::<TaskId>().unwrap(), id);
    assert!("tsk_not-a-ulid".parse::<TaskId>().is_err());
    // On the wire an id is the bare ULID.
    let wire = serde_json::to_value(id).unwrap();
    assert_eq!(wire, json!(id.0.to_string()));
}

#[test]
fn project_and_task_keys() {
    assert!(ProjectKey::new("CMP").is_ok());
    assert!(ProjectKey::new("TL2").is_ok());
    for bad in ["", "c", "cmp", "1AB", "TOOLONGKEY1", "A-B"] {
        assert!(ProjectKey::new(bad).is_err(), "{bad:?} should be rejected");
    }
    let key: TaskKey = "CMP-104".parse().unwrap();
    assert_eq!(key.project.as_str(), "CMP");
    assert_eq!(key.number, 104);
    assert_eq!(serde_json::to_value(&key).unwrap(), json!("CMP-104"));
    assert!("CMP-0".parse::<TaskKey>().is_err());
    assert!("cmp-1".parse::<TaskKey>().is_err());
    assert!("CMP".parse::<TaskKey>().is_err());
}

#[test]
fn dates() {
    assert!(Date("2026-10-24".into()).is_well_formed());
    for bad in [
        "2026-13-01",
        "2026-1-01",
        "26-10-24",
        "2026-+1-01",
        "2026-10-2é",
    ] {
        assert!(
            !Date(bad.into()).is_well_formed(),
            "{bad:?} should be rejected"
        );
    }
}

#[test]
fn agents_must_have_an_owner_people_must_not() {
    let person = MemberId::new();
    let mut agent = Member {
        id: MemberId::new(),
        kind: MemberKind::Agent,
        handle: "@writer".into(),
        name: "Writer".into(),
        owner: Some(person),
        persona: None,
    };
    assert!(agent.is_valid());
    agent.owner = None;
    assert!(!agent.is_valid());
    agent.owner = Some(agent.id);
    assert!(!agent.is_valid(), "an agent cannot own itself");
    let human = Member {
        id: person,
        kind: MemberKind::Human,
        handle: "@me".into(),
        name: "Me".into(),
        owner: None,
        persona: None,
    };
    assert!(human.is_valid());
}

#[test]
fn task_move_rules() {
    use TaskStatus::{Backlog, Canceled, Done, InProgress, Review, Todo};
    let own = Mover::Agent { on_own_task: true };
    let other = Mover::Agent { on_own_task: false };

    // People may do anything except a no-op.
    assert!(Review.can_move(Done, Mover::Person));
    assert!(Done.can_move(Todo, Mover::Person));
    assert!(!Todo.can_move(Todo, Mover::Person));

    // Agents: only their own task, only forward, never to done or canceled.
    assert!(Todo.can_move(InProgress, own));
    assert!(Backlog.can_move(InProgress, own));
    assert!(InProgress.can_move(Review, own));
    assert!(!Review.can_move(Done, own));
    assert!(!InProgress.can_move(Done, own));
    assert!(!InProgress.can_move(Canceled, own));
    assert!(!Todo.can_move(InProgress, other));

    // Back office: on evidence, and review → done only if the task allows automatic acceptance.
    assert!(InProgress.can_move(Review, Mover::BackOffice { accept_auto: false }));
    assert!(!Review.can_move(Done, Mover::BackOffice { accept_auto: false }));
    assert!(Review.can_move(Done, Mover::BackOffice { accept_auto: true }));

    // Sync mirrors upstream close and reopen, and never touches a task in progress.
    assert!(Todo.can_move(Done, Mover::Sync));
    assert!(Done.can_move(Todo, Mover::Sync));
    assert!(!InProgress.can_move(Done, Mover::Sync));
}

#[test]
fn event_wire_shape_is_stable() {
    let ws = WorkspaceId::new();
    let me = MemberId::new();
    let session = SessionId::new();
    let event = Event::now(
        ws,
        me,
        EventBody::ToolRan {
            session,
            tool: "Bash".into(),
            target: "cargo test".into(),
            outcome: "212 of 240 passed".into(),
            failed: true,
            receipt: Receipt::Transcript {
                session,
                offset: 4096,
            },
        },
    );
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["body"]["type"], "tool_ran");
    assert_eq!(value["body"]["data"]["receipt"]["kind"], "transcript");
    assert!(
        value.get("on_behalf_of").is_none(),
        "absent optionals are omitted"
    );
    let back: Event = serde_json::from_value(value).unwrap();
    assert_eq!(back, event);

    let brief = EventBody::BriefProposed {
        target: BriefTarget::Workstream(WorkstreamId::new()),
        text: "Seeds 1, 3 and 4 converged.".into(),
        next: None,
        receipts: vec![Receipt::Job {
            scheduler: Scheduler::Slurm,
            id: "131002".into(),
        }],
    };
    let v = serde_json::to_value(&brief).unwrap();
    assert_eq!(v["data"]["target"]["kind"], "workstream");
    assert_eq!(v["data"]["receipts"][0]["scheduler"], "slurm");
}

#[test]
fn runner_frames_round_trip_as_single_lines() {
    let hello = RunnerToHub::Hello {
        runner_version: "0.0.0".into(),
        protocol: PROTOCOL_VERSION,
        machine: MachineInfo {
            hostname: "login01".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            has_tmux: true,
            scheduler: Some(Scheduler::Slurm),
            home_on_network_fs: true,
        },
        capabilities: vec![Capability::Tmux, Capability::Slurm, Capability::Watch],
    };
    let line = encode_line(&hello).unwrap();
    assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
    assert_eq!(decode_line::<RunnerToHub>(&line).unwrap(), hello);

    let cmd = HubToRunner::Command {
        id: CommandId::new(),
        command: RunnerCommand::StartSession {
            engine: Engine::OpenCode,
            cwd: "/home/u/project".into(),
            name: "writer".into(),
            brief: Some("Line one\nLine two".into()),
            persona: None,
            model: None,
            account: None,
            permission_mode: PermissionMode::Default,
        },
    };
    let line = encode_line(&cmd).unwrap();
    assert_eq!(
        line.matches('\n').count(),
        1,
        "newlines inside strings are escaped"
    );
    assert_eq!(decode_line::<HubToRunner>(&line).unwrap(), cmd);
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["command"]["engine"], "opencode");
    assert_eq!(v["command"]["permission_mode"], "default");

    let ok = RunnerToHub::CommandResult {
        command: CommandId::new(),
        outcome: CommandOutcome::Ok { detail: None },
    };
    let v = serde_json::to_value(&ok).unwrap();
    assert_eq!(v["outcome"]["status"], "ok");
}

#[test]
fn host_info_and_stream_frames() {
    assert!(is_compatible(PROTOCOL_VERSION));
    assert!(!is_compatible(PROTOCOL_VERSION + 1));
    let info = HostInfo {
        name: "pitcrewd".into(),
        version: "0.0.0".into(),
        protocol: PROTOCOL_VERSION,
        protocol_min: 1,
        roles: vec![HostRole::Hub, HostRole::Runner],
        machine: MachineInfo {
            hostname: "laptop".into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            has_tmux: false,
            scheduler: None,
            home_on_network_fs: false,
        },
        capabilities: vec![Capability::Pty],
    };
    let back: HostInfo = serde_json::from_value(serde_json::to_value(&info).unwrap()).unwrap();
    assert_eq!(back, info);
    let frame = StreamFrame::Hello {
        rev: 7,
        log: "01JB000000000000000LOG0001".into(),
    };
    assert_eq!(
        serde_json::to_value(&frame).unwrap(),
        json!({"type": "hello", "rev": 7, "log": "01JB000000000000000LOG0001"})
    );
}

#[test]
fn transcript_items_are_tagged_by_kind() {
    use pitcrew_protocol::transcript::{PlanItem, PlanStatus, TranscriptItem, TranscriptPage};
    let plan = TranscriptItem::PlanUpdated {
        at: 1,
        items: vec![PlanItem {
            text: "Write §3.1".into(),
            status: PlanStatus::InProgress,
        }],
        offset: 42,
    };
    assert_eq!(plan.offset(), 42);
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        json!({"kind": "plan_updated", "at": 1, "offset": 42,
               "items": [{"text": "Write §3.1", "status": "in_progress"}]})
    );
    let page = TranscriptPage {
        items: vec![plan],
        from: 42,
        to: 120,
        at_start: false,
    };
    let back: TranscriptPage =
        serde_json::from_value(serde_json::to_value(&page).unwrap()).unwrap();
    assert_eq!(back, page);
}

/// Decodes `value` as `T`, and checks that it encodes back to exactly `value`.
fn round_trip<T>(value: &serde_json::Value) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let decoded: T = serde_json::from_value(value.clone()).expect("decodes");
    assert_eq!(&serde_json::to_value(&decoded).expect("encodes"), value);
    decoded
}

#[test]
fn task_patch_tells_left_out_null_and_value_apart() {
    // Left out: unchanged. An empty patch is `{}`.
    let empty: TaskPatch = round_trip(&json!({}));
    assert!(empty.is_empty());
    assert_eq!(empty, TaskPatch::default());

    // `null` clears the nullable fields.
    let cleared: TaskPatch = round_trip(&json!({"workstream": null, "start": null, "due": null}));
    assert_eq!(cleared.workstream, Some(None));
    assert_eq!(cleared.start, Some(None));
    assert_eq!(cleared.due, Some(None));
    assert!(!cleared.is_empty());

    // A value sets them.
    let ws = WorkstreamId::new();
    let set: TaskPatch =
        round_trip(&json!({"workstream": ws, "start": "2026-10-01", "due": "2026-10-24"}));
    assert_eq!(set.workstream, Some(Some(ws)));
    assert_eq!(set.start, Some(Some(Date("2026-10-01".into()))));
    assert_eq!(set.due, Some(Some(Date("2026-10-24".into()))));

    // One field present leaves the others out.
    let due_only: TaskPatch = round_trip(&json!({"due": null}));
    assert_eq!(
        due_only,
        TaskPatch {
            due: Some(None),
            ..TaskPatch::default()
        }
    );

    // On the plain fields, `null` is the same as leaving the field out.
    let plain: TaskPatch =
        serde_json::from_value(json!({"title": null, "labels": null, "accept_auto": null}))
            .unwrap();
    assert!(plain.is_empty());

    // Every field at once.
    let every: TaskPatch = round_trip(&json!({
        "workstream": ws, "title": "Rerun seed 3", "description": "Lower the learning rate.",
        "priority": "urgent", "labels": ["gpu", "seeds"], "start": null, "due": "2026-10-24",
        "blocked_by": [TaskId::new()], "accept_auto": true
    }));
    assert_eq!(every.priority, Some(Priority::Urgent));
    assert_eq!(every.start, Some(None));
}

fn sample_task() -> Task {
    Task {
        id: TaskId::new(),
        key: "PAP-4".parse().expect("key"),
        project: ProjectId::new(),
        workstream: Some(WorkstreamId::new()),
        title: "Rerun seed 3".into(),
        description: "Lower the learning rate.".into(),
        status: TaskStatus::Todo,
        priority: Priority::High,
        assignee: None,
        labels: vec!["gpu".into()],
        start: Some(Date("2026-10-01".into())),
        due: Some(Date("2026-10-24".into())),
        blocked_by: vec![],
        source: None,
        accept_auto: false,
        subtasks: vec![],
    }
}

#[test]
fn task_patch_applies_only_its_fields() {
    let before = sample_task();
    let mut task = before.clone();
    TaskPatch::default().apply(&mut task);
    assert_eq!(task, before, "an empty patch changes nothing");

    let blocker = TaskId::new();
    TaskPatch {
        workstream: Some(None),
        title: Some("Rerun seed 3 at a lower rate".into()),
        priority: Some(Priority::Urgent),
        labels: Some(vec![]),
        due: Some(None),
        blocked_by: Some(vec![blocker]),
        accept_auto: Some(true),
        ..TaskPatch::default()
    }
    .apply(&mut task);
    let mut expected = before.clone();
    expected.workstream = None;
    expected.title = "Rerun seed 3 at a lower rate".into();
    expected.priority = Priority::Urgent;
    expected.labels = vec![];
    expected.due = None;
    expected.blocked_by = vec![blocker];
    expected.accept_auto = true;
    assert_eq!(
        task, expected,
        "description, start and the rest are untouched"
    );

    let ws = WorkstreamId::new();
    TaskPatch {
        workstream: Some(Some(ws)),
        description: Some(String::new()),
        start: Some(Some(Date("2026-10-02".into()))),
        due: Some(Some(Date("2026-10-30".into()))),
        ..TaskPatch::default()
    }
    .apply(&mut task);
    assert_eq!(task.workstream, Some(ws));
    assert_eq!(task.description, "");
    assert_eq!(task.start, Some(Date("2026-10-02".into())));
    assert_eq!(task.due, Some(Date("2026-10-30".into())));
}

#[test]
fn task_updated_carries_the_changed_fields() {
    let task = TaskId::new();
    let body: EventBody = round_trip(&json!({"type": "task_updated", "data": {
        "task": task, "patch": {"title": "Rerun seed 3", "due": null}}}));
    assert_eq!(
        body,
        EventBody::TaskUpdated {
            task,
            patch: TaskPatch {
                title: Some("Rerun seed 3".into()),
                due: Some(None),
                ..TaskPatch::default()
            },
        }
    );
}

#[test]
fn old_brief_events_still_decode() {
    let target = json!({"kind": "project", "id": ProjectId::new()});
    let job = json!({"kind": "job", "scheduler": "slurm", "id": "4815162"});

    // Written before `next` and `receipts` existed on these events.
    let proposed: EventBody = round_trip(&json!({"type": "brief_proposed", "data": {
        "target": target, "text": "Seeds 1, 2, 4 and 5 are training.", "receipts": [job]}}));
    let EventBody::BriefProposed { next, receipts, .. } = &proposed else {
        panic!("not a proposal: {proposed:?}");
    };
    assert_eq!(*next, None);
    assert_eq!(receipts.len(), 1);

    let accepted: EventBody = round_trip(&json!({"type": "brief_accepted", "data": {
        "target": target, "text": "Seeds 1, 2, 4 and 5 are training.", "pinned": true}}));
    let EventBody::BriefAccepted {
        next,
        receipts,
        pinned,
        ..
    } = &accepted
    else {
        panic!("not an acceptance: {accepted:?}");
    };
    assert_eq!(*next, None);
    assert!(receipts.is_empty());
    assert!(*pinned);

    // The new fields.
    let proposed: EventBody = round_trip(&json!({"type": "brief_proposed", "data": {
        "target": target, "text": "Seed 3 diverged.", "next": "Rerun seed 3.",
        "receipts": [job]}}));
    assert!(
        matches!(proposed, EventBody::BriefProposed { next: Some(n), .. } if n == "Rerun seed 3.")
    );
    let accepted: EventBody = round_trip(&json!({"type": "brief_accepted", "data": {
        "target": target, "text": "Seed 3 diverged.", "next": "Rerun seed 3.", "pinned": false,
        "receipts": [job, {"kind": "event", "id": EventId::new()}]}}));
    assert!(matches!(accepted, EventBody::BriefAccepted { receipts, .. } if receipts.len() == 2));
}

#[test]
fn a_brief_carries_its_pending_proposal() {
    let target = json!({"kind": "workstream", "id": WorkstreamId::new()});
    // A brief written before `proposal` existed, or with nothing pending.
    let plain: Brief = round_trip(&json!({
        "target": target, "text": "Seed 3 diverged.", "next": "Rerun seed 3.", "pinned": true,
        "source": "person", "updated": 1_790_761_500_000_i64, "receipts": []
    }));
    assert_eq!(plain.proposal, None);
    assert_eq!(plain.source, BriefSource::Person);

    let job = Receipt::Job {
        scheduler: Scheduler::Slurm,
        id: "4815162".into(),
    };
    let pending: Brief = round_trip(&json!({
        "target": target, "text": "Seed 3 diverged.", "pinned": true, "source": "person",
        "updated": 1_790_761_500_000_i64, "receipts": [],
        "proposal": {"text": "Seed 3 reran and converged.", "next": "Make figure 3.",
                     "receipts": [job], "at": 1_790_762_100_000_i64}
    }));
    assert_eq!(
        pending.proposal,
        Some(BriefProposal {
            text: "Seed 3 reran and converged.".into(),
            next: Some("Make figure 3.".into()),
            receipts: vec![job],
            at: 1_790_762_100_000,
        })
    );
    // A proposal without a next step leaves it out; its receipts are always written.
    let bare: BriefProposal = round_trip(&json!({"text": "Nothing new.", "receipts": [], "at": 1}));
    assert_eq!(bare.next, None);
}

#[test]
fn new_task_round_trips_with_defaults() {
    let project = ProjectId::new();
    let minimal: NewTask = round_trip(&json!({"project": project, "title": "Write the abstract"}));
    assert_eq!(
        minimal,
        NewTask {
            project,
            workstream: None,
            title: "Write the abstract".into(),
            description: None,
            status: None,
            priority: None,
            assignee: None,
            labels: None,
            due: None,
        }
    );
    let every: NewTask = round_trip(&json!({
        "project": project, "workstream": WorkstreamId::new(), "title": "Benchmark parsing",
        "description": "Compare with the old parser.", "status": "backlog", "priority": "low",
        "assignee": MemberId::new(), "labels": ["performance"], "due": "2026-10-31"
    }));
    assert_eq!(every.status, Some(TaskStatus::Backlog));
    let nulls: NewTask = serde_json::from_value(
        json!({"project": project, "title": "Write the abstract", "workstream": null, "labels": null}),
    )
    .unwrap();
    assert_eq!(nulls, minimal, "null counts as left out");
}

#[test]
fn new_project_round_trips_with_defaults() {
    let minimal: NewProject = round_trip(&json!({"key": "PAP", "name": "Paper"}));
    assert_eq!(
        minimal,
        NewProject {
            key: ProjectKey::new("PAP").unwrap(),
            name: "Paper".into(),
            lead: None,
            members: None,
            status: None,
            start: None,
            due: None,
            root: None,
        }
    );
    let lead = MemberId::new();
    let every: NewProject = round_trip(&json!({
        "key": "TL2", "name": "Tooling", "lead": lead, "members": [lead, MemberId::new()],
        "status": "planning", "start": "2026-09-01", "due": "2026-11-15",
        "root": {"machine": MachineId::new(), "path": "/work/tools", "branch": "main"}
    }));
    assert_eq!(every.lead, Some(lead));
    for bad in ["pap", "P", "1AB", "TOOLONGKEY1", "PA-P"] {
        assert!(
            serde_json::from_value::<NewProject>(json!({"key": bad, "name": "Paper"})).is_err(),
            "{bad:?} is not a project key"
        );
    }
}

#[test]
fn new_workstream_round_trips_with_defaults() {
    let project = ProjectId::new();
    let minimal: NewWorkstream = round_trip(&json!({"project": project, "name": "Seed runs"}));
    assert_eq!(
        minimal,
        NewWorkstream {
            project,
            name: "Seed runs".into(),
            status: None,
            locations: None,
        }
    );
    let every: NewWorkstream = round_trip(&json!({
        "project": project, "name": "Seed runs", "status": "idea",
        "locations": [{"machine": MachineId::new(), "path": "/scratch/seeds"}]
    }));
    assert_eq!(every.locations.map(|l| l.len()), Some(1));
}
