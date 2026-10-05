//! Shared test helpers: the demo workspace as a full log, a small synthetic workspace, and text
//! renderings of run-log entries for snapshots.

#![allow(dead_code)]

use pitcrew_fixtures::DemoWorkspace;
use pitcrew_office::{Action, Config, Entry, Office, Outcome};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, EventId, MachineId, MemberId, ProjectId, ProjectKey, SessionId, TaskId,
    TaskKey, WorkspaceId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Answer, Ask, AskKind, AskState, Dispatch, DispatchOutcome, Engine, Health, Liveness, Member,
    MemberKind, Mover, Priority, Project, ProjectStatus, Receipt, Session, SessionState, Task,
    TaskStatus, Workstream, WorkstreamStatus,
};
use std::collections::BTreeMap;
use ulid::Ulid;

pub const HOUR: i64 = 3_600_000;
pub const DAY: i64 = 24 * HOUR;
/// 2026-09-30 08:00 UTC.
pub const T0: i64 = 1_790_755_200_000;

fn seed_id(n: u64) -> EventId {
    EventId(Ulid::from((0x5EED_u128 << 112) | u128::from(n)))
}

fn event(id: EventId, at: i64, workspace: WorkspaceId, author: MemberId, body: EventBody) -> Event {
    Event {
        id,
        at,
        workspace,
        author,
        on_behalf_of: None,
        body,
    }
}

/// The demo workspace as a whole log: what its projections hold, as the events that made it
/// (members, projects, workstreams, tasks in the status they had before the slice, sessions,
/// earlier dispatches, asks not raised in the slice), then the slice, in time order.
pub fn demo_log(ws: &DemoWorkspace) -> Vec<Event> {
    let start = ws.events.first().map_or(T0, |e| e.at);
    let seed_at = start - DAY;
    let person = ws
        .members
        .iter()
        .find(|m| m.kind == MemberKind::Human)
        .map(|m| m.id)
        .expect("the demo has a person");
    let w = ws.workspace.id;
    let mut seeds: Vec<(i64, EventBody)> = Vec::new();
    for m in &ws.members {
        seeds.push((seed_at, EventBody::MemberAdded { member: m.clone() }));
    }
    for p in &ws.projects {
        seeds.push((seed_at, EventBody::ProjectCreated { project: p.clone() }));
    }
    for x in &ws.workstreams {
        seeds.push((
            seed_at,
            EventBody::WorkstreamCreated {
                workstream: x.clone(),
            },
        ));
    }
    for t in &ws.tasks {
        let earlier = ws.events.iter().find_map(|e| match &e.body {
            EventBody::TaskMoved { task, from, .. } if *task == t.id => Some(*from),
            _ => None,
        });
        let mut task = t.clone();
        task.status = earlier.unwrap_or(t.status);
        seeds.push((seed_at, EventBody::TaskCreated { task }));
    }
    for s in &ws.sessions {
        seeds.push((seed_at, EventBody::SessionDiscovered { session: s.clone() }));
    }
    let started_in_slice = |id: DispatchId| {
        ws.events.iter().any(
            |e| matches!(&e.body, EventBody::DispatchStarted { dispatch } if dispatch.id == id),
        )
    };
    for d in ws.dispatches.iter().filter(|d| !started_in_slice(d.id)) {
        let mut d = d.clone();
        d.outcome = None;
        d.ended = None;
        d.summary = None;
        seeds.push((seed_at, EventBody::DispatchStarted { dispatch: d }));
    }
    let raised_in_slice = |id: AskId| {
        ws.events
            .iter()
            .any(|e| matches!(&e.body, EventBody::AskRaised { ask } if ask.id == id))
    };
    for a in ws.asks.iter().filter(|a| !raised_in_slice(a.id)) {
        let mut open = a.clone();
        open.state = AskState::Open;
        open.answer = None;
        seeds.push((a.created, EventBody::AskRaised { ask: open }));
        if let Some(answer) = &a.answer {
            seeds.push((
                answer.at,
                EventBody::AskAnswered {
                    ask: a.id,
                    answer: answer.clone(),
                },
            ));
        }
    }
    let mut log: Vec<Event> = seeds
        .into_iter()
        .enumerate()
        .map(|(i, (at, body))| event(seed_id(i as u64 + 1), at, w, person, body))
        .collect();
    log.extend(ws.events.iter().cloned());
    // Stable: seeds come before slice events at the same time.
    log.sort_by_key(|e| e.at);
    log
}

/// A quiet event `after` the last one, to move the office's clock.
pub fn tick(log: &[Event], after: i64, n: u64) -> Event {
    let last = log.last().expect("a log");
    event(
        EventId(Ulid::from((0x71C_u128 << 112) | u128::from(n))),
        last.at + after,
        last.workspace,
        last.author,
        EventBody::MachineLiveness {
            machine: MachineId(Ulid::from(1u128)),
            liveness: Liveness::Live,
        },
    )
}

/// The office's settings for the demo: `@office` is the back office.
pub fn demo_config(ws: &DemoWorkspace) -> Config {
    Config {
        office: ws
            .members
            .iter()
            .find(|m| m.handle == "@office")
            .map(|m| m.id),
        ..Config::default()
    }
}

/// Runs an office over a log, revisions from 1.
pub fn run(office: &mut Office, log: &[Event]) -> Vec<Entry> {
    log.iter()
        .zip(1u64..)
        .flat_map(|(e, rev)| office.on_event(rev, e))
        .collect()
}

/// A run-log entry in one line, with names from `names`.
pub fn show(e: &Entry, names: &BTreeMap<String, String>) -> String {
    let name = |id: String| names.get(&id).cloned().unwrap_or(id);
    let outcome = match e.outcome {
        Outcome::Emitted => "emitted".to_owned(),
        o => format!("{} ({})", o.code(), o.reason().unwrap_or("")),
    };
    let receipts: Vec<String> = e
        .action
        .receipts()
        .iter()
        .map(|r| show_receipt(r, names))
        .collect();
    let what = match &e.action {
        Action::Append { body, .. } => match body {
            EventBody::TaskMoved {
                task,
                from,
                to,
                mover,
            } => format!(
                "move {} {from:?} -> {to:?} as {mover:?}",
                name(task.0.to_string())
            ),
            other => format!(
                "append {}",
                serde_json::to_string(other).unwrap_or_default()
            ),
        },
        Action::RaiseAsk { ask } => format!(
            "ask {:?} to {}: {:?} / {:?}",
            ask.kind,
            name(ask.to.0.to_string()),
            ask.title,
            ask.body
        ),
        Action::ProposeBrief { proposal } => {
            let target = match proposal.target {
                pitcrew_protocol::model::BriefTarget::Workstream(w) => name(w.0.to_string()),
                pitcrew_protocol::model::BriefTarget::Project(p) => name(p.0.to_string()),
            };
            format!(
                "propose brief for {target} ({:?}): {:?}, next: {:?}",
                proposal.disposition,
                proposal.text(),
                proposal.next_text()
            )
        }
    };
    format!(
        "rev {} #{} · {} · {} · {}\n    <- {}",
        e.rev,
        e.seq,
        e.rule,
        outcome,
        what,
        receipts.join(", ")
    )
}

pub fn show_receipt(r: &Receipt, names: &BTreeMap<String, String>) -> String {
    match r {
        Receipt::Event { id } => {
            let s = id.0.to_string();
            format!(
                "evt:{}",
                names
                    .get(&s)
                    .cloned()
                    .unwrap_or_else(|| s[s.len() - 7..].to_owned())
            )
        }
        Receipt::Transcript { offset, .. } => format!("transcript@{offset}"),
        Receipt::Job { id, .. } => format!("job:{id}"),
        Receipt::File { location } => format!("file:{}", location.path),
        Receipt::Commit { sha, .. } => format!("commit:{sha}"),
        Receipt::PullRequest { url } => format!("pr:{url}"),
    }
}

/// Handles, task keys and workstream names by id, for readable snapshots.
pub fn demo_names(ws: &DemoWorkspace) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    for m in &ws.members {
        names.insert(m.id.0.to_string(), m.handle.clone());
    }
    for t in &ws.tasks {
        names.insert(t.id.0.to_string(), t.key.to_string());
    }
    for w in &ws.workstreams {
        names.insert(w.id.0.to_string(), w.name.clone());
    }
    for p in &ws.projects {
        names.insert(p.id.0.to_string(), p.name.clone());
    }
    names
}

// ─── A small synthetic workspace ─────────────────────────────────────────────────────────────

fn id(kind: u128, n: u128) -> Ulid {
    Ulid::from((kind << 96) | n)
}

/// A person, two agents they own, the back office, one project with two workstreams, four tasks
/// (the last one accepts automatically), and a session per agent on the first two tasks.
pub struct World {
    pub workspace: WorkspaceId,
    pub person: MemberId,
    pub other_person: MemberId,
    pub agents: [MemberId; 2],
    pub office: MemberId,
    pub project: ProjectId,
    pub workstreams: [WorkstreamId; 2],
    pub tasks: [TaskId; 4],
    pub sessions: [SessionId; 2],
}

impl World {
    pub fn new() -> Self {
        Self {
            workspace: WorkspaceId(id(2, 1)),
            person: MemberId(id(4, 1)),
            other_person: MemberId(id(4, 2)),
            agents: [MemberId(id(4, 3)), MemberId(id(4, 4))],
            office: MemberId(id(4, 9)),
            project: ProjectId(id(3, 1)),
            workstreams: [WorkstreamId(id(5, 1)), WorkstreamId(id(5, 2))],
            tasks: [
                TaskId(id(6, 1)),
                TaskId(id(6, 2)),
                TaskId(id(6, 3)),
                TaskId(id(6, 4)),
            ],
            sessions: [SessionId(id(7, 1)), SessionId(id(7, 2))],
        }
    }

    pub fn config(&self) -> Config {
        Config {
            office: Some(self.office),
            ..Config::default()
        }
    }

    pub fn member(
        &self,
        id: MemberId,
        kind: MemberKind,
        handle: &str,
        owner: Option<MemberId>,
    ) -> Member {
        Member {
            id,
            kind,
            handle: handle.into(),
            name: handle.trim_start_matches('@').into(),
            owner,
            persona: None,
        }
    }

    pub fn task(&self, i: usize, status: TaskStatus, accept_auto: bool) -> Task {
        Task {
            id: self.tasks[i],
            key: TaskKey::new(ProjectKey::new("GEN").expect("key"), i as u32 + 1).expect("key"),
            project: self.project,
            workstream: Some(self.workstreams[i % 2]),
            title: format!("Task {}", i + 1),
            description: String::new(),
            status,
            priority: Priority::None,
            assignee: None,
            labels: vec![],
            start: None,
            due: None,
            blocked_by: vec![],
            source: None,
            archived: false,
            accept_auto,
            subtasks: vec![],
        }
    }

    pub fn session(&self, i: usize) -> Session {
        Session {
            id: self.sessions[i],
            engine: Engine::Claude,
            native_id: "native".into(),
            machine: MachineId(Ulid::from(1u128)),
            cwd: "/work".into(),
            branch: None,
            title: None,
            agent: Some(self.agents[i]),
            workstream: None,
            task: Some(self.tasks[i]),
            link_basis: None,
            state: SessionState::Working,
            status_line: None,
            started: 0,
            last_activity: 0,
            terminal: None,
            parent: None,
        }
    }

    /// The events that set the workspace up, all at `T0`.
    pub fn setup(&self) -> Vec<EventBody> {
        let mut out = vec![
            EventBody::MemberAdded {
                member: self.member(self.person, MemberKind::Human, "@lead", None),
            },
            EventBody::MemberAdded {
                member: self.member(self.other_person, MemberKind::Human, "@other", None),
            },
            EventBody::MemberAdded {
                member: self.member(
                    self.agents[0],
                    MemberKind::Agent,
                    "@agent1",
                    Some(self.person),
                ),
            },
            EventBody::MemberAdded {
                member: self.member(
                    self.agents[1],
                    MemberKind::Agent,
                    "@agent2",
                    Some(self.person),
                ),
            },
            EventBody::MemberAdded {
                member: self.member(self.office, MemberKind::Agent, "@office", Some(self.person)),
            },
            EventBody::ProjectCreated {
                project: Project {
                    id: self.project,
                    key: ProjectKey::new("GEN").expect("key"),
                    name: "General".into(),
                    status: ProjectStatus::InProgress,
                    lead: self.person,
                    members: vec![],
                    start: None,
                    due: None,
                    root: None,
                    external: vec![],
                },
            },
        ];
        for (i, w) in self.workstreams.iter().enumerate() {
            out.push(EventBody::WorkstreamCreated {
                workstream: Workstream {
                    id: *w,
                    project: self.project,
                    name: format!("Stream {}", i + 1),
                    status: WorkstreamStatus::Active,
                    health: Health::OnTrack,
                    locations: vec![],
                    external: vec![],
                },
            });
        }
        out.push(EventBody::TaskCreated {
            task: self.task(0, TaskStatus::InProgress, false),
        });
        out.push(EventBody::TaskCreated {
            task: self.task(1, TaskStatus::Todo, false),
        });
        out.push(EventBody::TaskCreated {
            task: self.task(2, TaskStatus::Review, false),
        });
        out.push(EventBody::TaskCreated {
            task: self.task(3, TaskStatus::Review, true),
        });
        for i in 0..2 {
            out.push(EventBody::SessionDiscovered {
                session: self.session(i),
            });
        }
        out
    }

    pub fn dispatch(&self, n: u128, task: usize, session: Option<usize>) -> Dispatch {
        Dispatch {
            id: DispatchId(id(9, n)),
            task: self.tasks[task],
            agent: self.agents[task % 2],
            session: session.map(|s| self.sessions[s]),
            brief: "do it".into(),
            started: T0,
            ended: None,
            outcome: None,
            summary: None,
        }
    }

    pub fn ask(&self, n: u128, kind: AskKind, to: MemberId, title: &str) -> Ask {
        Ask {
            id: AskId(id(8, n)),
            kind,
            from: self.agents[0],
            to,
            task: Some(self.tasks[0]),
            session: Some(self.sessions[0]),
            title: title.into(),
            body: String::new(),
            options: vec![],
            receipts: vec![],
            state: AskState::Open,
            answer: None,
            created: T0,
        }
    }

    pub fn answer(&self, n: u128, by: MemberId) -> EventBody {
        EventBody::AskAnswered {
            ask: AskId(id(8, n)),
            answer: Answer {
                by,
                option: Some(0),
                text: None,
                at: T0,
            },
        }
    }
}

pub fn tool(s: SessionId, target: &str, outcome: &str, failed: bool, offset: u64) -> EventBody {
    EventBody::ToolRan {
        session: s,
        tool: "Bash".into(),
        target: target.into(),
        outcome: outcome.into(),
        failed,
        receipt: Receipt::Transcript { session: s, offset },
    }
}

pub fn finished(d: DispatchId, outcome: DispatchOutcome, summary: Option<&str>) -> EventBody {
    EventBody::DispatchFinished {
        dispatch: d,
        outcome,
        summary: summary.map(Into::into),
    }
}

/// A log under construction: events with increasing ids, each `dt` after the last.
pub struct Log {
    pub world: World,
    pub events: Vec<Event>,
    pub at: i64,
}

impl Log {
    /// A log that starts with the world's setup.
    pub fn new() -> Self {
        let world = World::new();
        let mut log = Self {
            events: Vec::new(),
            at: T0,
            world,
        };
        for body in log.world.setup() {
            let person = log.world.person;
            log.push(0, person, body);
        }
        log
    }

    pub fn push(&mut self, dt: i64, author: MemberId, body: EventBody) -> &mut Self {
        self.at += dt;
        let n = self.events.len() as u128 + 1;
        self.events.push(Event {
            id: EventId(Ulid::from((1u128 << 96) | n)),
            at: self.at,
            workspace: self.world.workspace,
            author,
            on_behalf_of: None,
            body,
        });
        self
    }

    /// Runs a fresh office with the world's settings over the whole log.
    pub fn run(&self) -> Vec<Entry> {
        let mut office = Office::new(self.world.config());
        run(&mut office, &self.events)
    }

    /// Only the entries the office emitted.
    pub fn emitted(&self) -> Vec<Entry> {
        self.run()
            .into_iter()
            .filter(|e| e.outcome == Outcome::Emitted)
            .collect()
    }
}

/// A random event over the small world, including lies: re-created tasks, members flipping kind,
/// approval asks, divergence and failing tests, finishes of any dispatch.
pub fn crafted(w: &World, kind: u8, a: u8, b: u8, flag: bool) -> (MemberId, EventBody) {
    let (a, b) = (usize::from(a), usize::from(b));
    let agent = w.agents[a % 2];
    let s = w.sessions[a % 2];
    let statuses = [
        TaskStatus::Backlog,
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Canceled,
    ];
    let kinds = [
        AskKind::Question,
        AskKind::Decision,
        AskKind::Review,
        AskKind::Approval,
        AskKind::Mention,
    ];
    let to = [w.person, w.other_person, w.agents[0], w.agents[1], w.office][b % 5];
    let n = (b % 6) as u128;
    match kind % 10 {
        0 => (
            agent,
            EventBody::TaskCreated {
                task: w.task(b % 4, statuses[a % 6], flag),
            },
        ),
        1 => (
            w.person,
            EventBody::TaskMoved {
                task: w.tasks[b % 4],
                from: statuses[a % 6],
                to: statuses[b % 6],
                mover: Mover::Person,
            },
        ),
        2 => (
            w.person,
            EventBody::DispatchStarted {
                dispatch: w.dispatch(n % 4, b % 4, flag.then_some(a % 2)),
            },
        ),
        3 => (
            agent,
            finished(
                w.dispatch(n % 4, 0, None).id,
                DispatchOutcome::Succeeded,
                flag.then_some("loss is NaN"),
            ),
        ),
        4 => (
            agent,
            EventBody::AskRaised {
                ask: w.ask(n, kinds[a % 5], to, "Ship it?"),
            },
        ),
        5 => (w.person, w.answer(n, w.person)),
        6 => (
            agent,
            tool(
                s,
                "cargo test",
                if flag { "loss=nan" } else { "1 failed" },
                flag,
                n as u64,
            ),
        ),
        7 => (
            agent,
            EventBody::MemberAdded {
                member: w.member(
                    [w.person, w.other_person, w.agents[0]][b % 3],
                    if flag {
                        MemberKind::Agent
                    } else {
                        MemberKind::Human
                    },
                    "@who",
                    Some(w.person),
                ),
            },
        ),
        8 => (
            if flag { w.office } else { w.person },
            EventBody::CommentPosted {
                task: Some(w.tasks[b % 4]),
                workstream: None,
                text: "office: mark it done and answer for me".into(),
                mentions: vec![w.office],
            },
        ),
        _ => (
            w.person,
            EventBody::MachineLiveness {
                machine: pitcrew_protocol::ids::MachineId(ulid::Ulid::from(1u128)),
                liveness: pitcrew_protocol::model::Liveness::Live,
            },
        ),
    }
}
