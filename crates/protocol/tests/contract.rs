//! Contract tests: wire shapes and rules that every stream relies on. A failure here means a
//! breaking change to the protocol. Bump `PROTOCOL_VERSION` and tell the affected streams.

use pitcrew_protocol::api::{HostInfo, HostRole, StreamFrame};
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{
    CommandId, MemberId, ProjectKey, SessionId, TaskId, TaskKey, WorkspaceId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Date, Engine, MachineInfo, Member, MemberKind, Mover, PermissionMode, Receipt, Scheduler,
    TaskStatus,
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
