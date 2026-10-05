//! Shared test helpers: a synthetic world and event generator, the set of receipts an input
//! allows, and text renderings for snapshots.

#![allow(dead_code)]

use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, EventId, MachineId, MemberId, ProjectId, ProjectKey, SessionId, SubtaskId,
    TaskId, TaskKey, WorkspaceId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Answer, Ask, AskKind, AskState, Dispatch, DispatchOutcome, Engine, Health, LinkBasis, Liveness,
    Member, MemberKind, Priority, Receipt, Scheduler, Session, SessionState, Subtask,
    SubtaskSource, Task, TaskStatus, Workstream, WorkstreamStatus,
};
use pitcrew_recap::{Block, Directory, Summary};
use std::collections::HashSet;
use ulid::Ulid;

/// A time in the fixture's week: 2026-09-30 08:00 UTC.
pub const T0: i64 = 1_790_755_200_000;

/// What the hub's projections would know before the demo workspace's event slice.
pub fn demo_directory(ws: &pitcrew_fixtures::DemoWorkspace) -> Directory {
    let mut dir = Directory::new();
    ws.members.iter().for_each(|m| dir.add_member(m));
    ws.workstreams.iter().for_each(|w| dir.add_workstream(w));
    ws.tasks.iter().for_each(|t| dir.add_task(t));
    ws.sessions.iter().for_each(|s| dir.add_session(s));
    ws.dispatches.iter().for_each(|d| dir.add_dispatch(d));
    ws.asks.iter().for_each(|a| dir.add_ask(a));
    dir
}

fn id(kind: u128, n: u128) -> Ulid {
    Ulid::from((kind << 96) | n)
}

pub fn event_id(n: u64) -> EventId {
    EventId(id(1, u128::from(n)))
}

/// Sessions, tasks and workstreams that generated events refer to, and a directory that knows
/// about them.
pub struct World {
    pub dir: Directory,
    pub workspace: WorkspaceId,
    pub person: MemberId,
    pub agents: Vec<MemberId>,
    pub sessions: Vec<SessionId>,
    pub tasks: Vec<TaskId>,
    /// The tasks as the directory was seeded with them.
    pub task_docs: Vec<Task>,
    pub workstreams: Vec<WorkstreamId>,
    pub asks: Vec<AskId>,
    pub dispatches: Vec<DispatchId>,
}

impl World {
    pub fn new(sessions: usize, tasks: usize, workstreams: usize) -> Self {
        Self::with_limit(sessions, tasks, workstreams, None)
    }

    /// A world whose directory keeps at most `limit` entries of each kind (`None`: the default).
    pub fn with_limit(
        sessions: usize,
        tasks: usize,
        workstreams: usize,
        limit: Option<usize>,
    ) -> Self {
        let mut dir = limit.map_or_else(Directory::new, Directory::with_limit);
        let workspace = WorkspaceId(id(2, 1));
        let project = ProjectId(id(3, 1));
        let person = MemberId(id(4, 0));
        dir.add_member(&Member {
            id: person,
            kind: MemberKind::Human,
            handle: "@lead".into(),
            name: "Lead".into(),
            owner: None,
            persona: None,
        });
        let agents: Vec<MemberId> = (1..=4u128).map(|n| MemberId(id(4, n))).collect();
        for (i, a) in agents.iter().enumerate() {
            dir.add_member(&Member {
                id: *a,
                kind: MemberKind::Agent,
                handle: format!("@agent{}", i + 1),
                name: format!("Agent {}", i + 1),
                owner: Some(person),
                persona: None,
            });
        }
        let workstreams: Vec<WorkstreamId> = (0..workstreams.max(1))
            .map(|n| WorkstreamId(id(5, n as u128)))
            .collect();
        for (i, w) in workstreams.iter().enumerate() {
            dir.add_workstream(&Workstream {
                id: *w,
                project,
                name: format!("Stream {}", i + 1),
                status: WorkstreamStatus::Active,
                health: Health::OnTrack,
                locations: vec![],
                external: vec![],
            });
        }
        let key = ProjectKey::new("GEN").expect("valid key");
        let tasks: Vec<TaskId> = (0..tasks.max(1))
            .map(|n| TaskId(id(6, n as u128)))
            .collect();
        let mut task_docs = Vec::with_capacity(tasks.len());
        for (i, t) in tasks.iter().enumerate() {
            // Every fifth task has no workstream and lives directly under the project.
            let workstream = (i % 5 != 4).then(|| workstreams[i % workstreams.len()]);
            let task = Task {
                id: *t,
                key: TaskKey::new(key.clone(), i as u32 + 1).expect("valid key"),
                project,
                workstream,
                title: format!("Task {}", i + 1),
                description: String::new(),
                status: TaskStatus::Todo,
                priority: Priority::None,
                assignee: None,
                labels: vec![],
                start: None,
                due: None,
                blocked_by: vec![],
                source: None,
                archived: false,
                accept_auto: false,
                subtasks: vec![],
            };
            dir.add_task(&task);
            task_docs.push(task);
        }
        let sessions: Vec<SessionId> = (0..sessions.max(1))
            .map(|n| SessionId(id(7, n as u128)))
            .collect();
        for (i, s) in sessions.iter().enumerate() {
            // Every third session is unlinked.
            let task = (i % 3 != 2).then(|| tasks[i % tasks.len()]);
            dir.add_session(&session(*s, agents[i % agents.len()], task, None));
        }
        let asks = (0..8u128).map(|n| AskId(id(8, n))).collect();
        let dispatches = (0..8u128).map(|n| DispatchId(id(9, n))).collect();
        Self {
            dir,
            workspace,
            person,
            agents,
            sessions,
            tasks,
            task_docs,
            workstreams,
            asks,
            dispatches,
        }
    }

    fn agent_of(&self, s: usize) -> MemberId {
        self.agents[s % self.sessions.len() % self.agents.len()]
    }
}

pub fn session(
    id: SessionId,
    agent: MemberId,
    task: Option<TaskId>,
    title: Option<String>,
) -> Session {
    Session {
        id,
        engine: Engine::Claude,
        native_id: "native".into(),
        machine: MachineId(Ulid::from(1u128)),
        cwd: "/work".into(),
        branch: None,
        title,
        agent: Some(agent),
        workstream: None,
        task,
        link_basis: None,
        state: SessionState::Working,
        status_line: None,
        started: 0,
        last_activity: 0,
        terminal: None,
        parent: None,
    }
}

/// One generated event: `(kind, a, b, flag, dt)`. `a` and `b` pick sessions, tasks, files and
/// so on; `dt` is the time since the previous event (it may be negative, like a skewed clock).
pub type Spec = (u8, u8, u8, bool, i64);

const COMMANDS: &[&str] = &[
    "cargo test -p x",
    "pytest -x",
    "cargo build",
    "cargo clippy",
    "ls -la",
    "squeue --me",
];
const FILES: &[&str] = &[
    "src/lib.rs",
    "src/main.rs",
    "paper/method.tex",
    "README.md",
    "tests/a.rs",
    "notes/b.md",
];
const STATUSES: &[TaskStatus] = &[
    TaskStatus::Backlog,
    TaskStatus::Todo,
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Done,
    TaskStatus::Canceled,
];

/// Turns specs into events, numbered from `first_id` and timed from `start`.
pub fn gen_events(specs: &[Spec], w: &World, start: i64, first_id: u64) -> Vec<Event> {
    let mut at = start;
    let mut out = Vec::with_capacity(specs.len());
    for (i, &(kind, a, b, flag, dt)) in specs.iter().enumerate() {
        at = at.saturating_add(dt);
        let (a, b) = (usize::from(a), usize::from(b));
        let s = w.sessions[a % w.sessions.len()];
        let t = w.tasks[b % w.tasks.len()];
        let agent = w.agent_of(a);
        let n = first_id + i as u64;
        let transcript = Receipt::Transcript {
            session: s,
            offset: n * 100,
        };
        let (author, body) = match kind % 16 {
            0..=3 => (
                agent,
                EventBody::ToolRan {
                    session: s,
                    tool: if b % 7 == 6 { "Read" } else { "Bash" }.into(),
                    target: COMMANDS[b % COMMANDS.len()].into(),
                    outcome: if flag && b % 4 == 0 {
                        "loss is NaN".into()
                    } else {
                        "ok".into()
                    },
                    failed: flag,
                    receipt: transcript,
                },
            ),
            4 | 5 => (
                agent,
                EventBody::FileEdited {
                    session: s,
                    path: FILES[b % FILES.len()].into(),
                    added: u32::from(a as u8),
                    removed: u32::from(b as u8 / 3),
                    receipt: None,
                },
            ),
            6 => (
                agent,
                EventBody::TurnEnded {
                    session: s,
                    receipt: transcript,
                },
            ),
            7 => (
                if flag { w.person } else { agent },
                EventBody::TaskMoved {
                    task: t,
                    from: STATUSES[a % STATUSES.len()],
                    to: STATUSES[(a + 1 + b) % STATUSES.len()],
                    mover: pitcrew_protocol::model::Mover::Person,
                },
            ),
            8 => (
                agent,
                EventBody::SubtasksReplaced {
                    task: t,
                    subtasks: (0..b % 5)
                        .map(|k| Subtask {
                            id: SubtaskId(id(10, k as u128)),
                            text: format!("step {k}"),
                            done: k < a % 5,
                            source: SubtaskSource::AgentPlan { agent },
                        })
                        .collect(),
                },
            ),
            9 => {
                let ask = w.asks[b % w.asks.len()];
                (
                    agent,
                    EventBody::AskRaised {
                        ask: Ask {
                            id: ask,
                            kind: [AskKind::Question, AskKind::Decision, AskKind::Review][a % 3],
                            from: agent,
                            to: w.person,
                            task: flag.then_some(t),
                            session: (a % 4 != 3).then_some(s),
                            title: if b % 3 == 0 {
                                "Run 2 diverged. Rerun?".into()
                            } else {
                                "Which option?".into()
                            },
                            body: String::new(),
                            options: vec![],
                            receipts: vec![Receipt::Job {
                                scheduler: Scheduler::Slurm,
                                id: format!("{}", 1000 + b),
                            }],
                            state: AskState::Open,
                            answer: None,
                            created: at,
                        },
                    },
                )
            }
            10 => (
                w.person,
                EventBody::AskAnswered {
                    ask: w.asks[b % w.asks.len()],
                    answer: Answer {
                        by: w.person,
                        option: Some(0),
                        text: None,
                        at,
                    },
                },
            ),
            11 => (
                if flag { w.person } else { agent },
                EventBody::CommentPosted {
                    task: (b % 3 != 0).then_some(t),
                    workstream: Some(w.workstreams[b % w.workstreams.len()]),
                    text: "looks good".into(),
                    mentions: vec![w.person, agent],
                },
            ),
            12 => (
                w.person,
                EventBody::WorkstreamChanged {
                    workstream: w.workstreams[a % w.workstreams.len()],
                    status: WorkstreamStatus::Active,
                    health: [Health::OnTrack, Health::AtRisk, Health::Blocked][b % 3],
                },
            ),
            13 => (
                agent,
                EventBody::SessionStateChanged {
                    session: s,
                    from: SessionState::Working,
                    to: if flag {
                        SessionState::Waiting
                    } else {
                        SessionState::Ended
                    },
                    status_line: Some("Asks: which option?".into()),
                },
            ),
            14 => {
                let d = w.dispatches[b % w.dispatches.len()];
                if flag {
                    (
                        w.person,
                        EventBody::DispatchStarted {
                            dispatch: Dispatch {
                                id: d,
                                task: t,
                                agent,
                                session: Some(s),
                                brief: "do it".into(),
                                started: at,
                                ended: None,
                                outcome: None,
                                summary: None,
                            },
                        },
                    )
                } else {
                    (
                        agent,
                        EventBody::DispatchFinished {
                            dispatch: d,
                            outcome: [
                                DispatchOutcome::Succeeded,
                                DispatchOutcome::Failed,
                                DispatchOutcome::Canceled,
                            ][a % 3],
                            summary: Some("done".into()),
                        },
                    )
                }
            }
            _ => match b % 6 {
                // Discovered, linked by a dispatch or with no basis.
                0 => {
                    let mut found = session(s, agent, Some(t), Some("New session".into()));
                    found.link_basis = flag.then_some(LinkBasis::Dispatch);
                    (agent, EventBody::SessionDiscovered { session: found })
                }
                // Linked by a person, or inferred from the folder.
                1 => (
                    w.person,
                    EventBody::SessionLinked {
                        session: s,
                        workstream: Some(w.workstreams[a % w.workstreams.len()]),
                        task: flag.then_some(t),
                        basis: if a % 2 == 0 {
                            LinkBasis::Manual
                        } else {
                            LinkBasis::Folder
                        },
                    },
                ),
                2 => (
                    w.person,
                    EventBody::MachineLiveness {
                        machine: MachineId(Ulid::from(1u128)),
                        liveness: Liveness::Live,
                    },
                ),
                3 => (w.person, EventBody::SessionEnded { session: s }),
                // Re-stated by the runner, with no link and no agent.
                4 => {
                    let mut again = session(s, agent, None, None);
                    again.agent = None;
                    (agent, EventBody::SessionDiscovered { session: again })
                }
                // A task re-stated at some status.
                _ => {
                    let mut task = w.task_docs[b % w.task_docs.len()].clone();
                    task.status = STATUSES[a % STATUSES.len()];
                    (w.person, EventBody::TaskCreated { task })
                }
            },
        };
        out.push(Event {
            id: event_id(n),
            at,
            workspace: w.workspace,
            author,
            on_behalf_of: None,
            body,
        });
    }
    out
}

/// A small deterministic generator for large inputs.
pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `n` specs with gaps of up to `max_gap_ms`, and an hour's pause now and then.
    pub fn specs(&mut self, n: usize, max_gap_ms: u64) -> Vec<Spec> {
        (0..n)
            .map(|_| {
                let r = self.next();
                let dt = if r.is_multiple_of(997) {
                    3_600_000
                } else {
                    ((r >> 40) % max_gap_ms.max(1)) as i64
                };
                (
                    (r & 0xff) as u8,
                    ((r >> 8) & 0xff) as u8,
                    ((r >> 16) & 0xff) as u8,
                    (r >> 24) & 7 == 0,
                    dt,
                )
            })
            .collect()
    }
}

/// Every receipt an input can justify: its event ids, and every receipt the events carry.
pub fn allowed_receipts(events: &[Event]) -> HashSet<Receipt> {
    let mut out = HashSet::new();
    for e in events {
        out.insert(Receipt::Event { id: e.id });
        match &e.body {
            EventBody::ToolRan { receipt, .. } | EventBody::TurnEnded { receipt, .. } => {
                out.insert(receipt.clone());
            }
            EventBody::AskRaised { ask } => out.extend(ask.receipts.iter().cloned()),
            EventBody::DecisionRecorded { receipts, .. }
            | EventBody::BriefProposed { receipts, .. }
            | EventBody::BriefAccepted { receipts, .. } => out.extend(receipts.iter().cloned()),
            _ => {}
        }
    }
    out
}

/// Checks that every receipt in the blocks is justified by the input.
pub fn assert_block_receipts(blocks: &[Block], allowed: &HashSet<Receipt>) {
    for b in blocks {
        for f in &b.facts {
            assert!(!f.receipts.is_empty(), "fact without receipts: {f:?}");
        }
        for r in b.receipts() {
            assert!(allowed.contains(r), "receipt not in the input: {r:?}");
        }
    }
}

/// Checks a rule-made summary: spans have receipts from the input, and the text between spans is
/// only the punctuation that joins clauses.
pub fn assert_summary(s: &Summary, allowed: &HashSet<Receipt>) {
    let mut covered = vec![false; s.text.len()];
    for span in &s.spans {
        assert!(
            !span.receipts.is_empty(),
            "span without receipts in {:?}",
            s.text
        );
        assert!(
            s.text.get(span.range.clone()).is_some(),
            "bad range in {:?}",
            s.text
        );
        for r in &span.receipts {
            assert!(allowed.contains(r), "receipt not in the input: {r:?}");
        }
        for c in &mut covered[span.range.clone()] {
            *c = true;
        }
    }
    for (i, ch) in s.text.char_indices() {
        if !covered[i] {
            assert!(
                matches!(ch, ',' | '.' | ' '),
                "uncovered {ch:?} at {i} in {:?}",
                s.text
            );
        }
    }
}

fn short(u: Ulid) -> String {
    let s = u.to_string();
    s[s.len().saturating_sub(7)..].to_owned()
}

/// A receipt in a few characters, for snapshots.
pub fn show_receipt(r: &Receipt) -> String {
    match r {
        Receipt::Event { id } => format!("evt:{}", short(id.0)),
        Receipt::Transcript { session, offset } => {
            format!("transcript:{}@{offset}", short(session.0))
        }
        Receipt::Job { id, .. } => format!("job:{id}"),
        Receipt::File { location } => format!("file:{}", location.path),
        Receipt::Commit { sha, .. } => format!("commit:{sha}"),
        Receipt::PullRequest { url } => format!("pr:{url}"),
    }
}

/// A summary with each clause and its receipts, for snapshots.
pub fn show_summary(s: &Summary) -> String {
    let mut out = format!("{}\n", s.text);
    for span in &s.spans {
        let receipts: Vec<String> = span.receipts.iter().map(show_receipt).collect();
        out.push_str(&format!(
            "  - {:?} <- {}\n",
            s.clause(span),
            receipts.join(", ")
        ));
    }
    out
}
