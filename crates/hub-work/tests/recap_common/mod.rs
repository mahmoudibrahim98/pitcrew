//! Shared by the recap index's tests: a synthetic workspace with the events that create it, a
//! generator of activity, the oracle (the recap engine over the whole log at once), and dumps of
//! every page a recap index serves.

#![allow(dead_code)]

use pitcrew_hub_work::{BlockFilter, DaysScope, RecapIndex, Recaps, WorkService};
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, EventId, MachineId, MemberId, ProjectId, ProjectKey, SessionId, SubtaskId,
    TaskId, TaskKey, WorkspaceId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Answer, Ask, AskKind, AskState, Dispatch, DispatchOutcome, Engine, Health, LinkBasis, Liveness,
    Member, MemberKind, Mover, Priority, Project, ProjectStatus, Receipt, Scheduler, Session,
    SessionState, Subtask, SubtaskSource, Task, TaskPatch, TaskStatus, Workspace, Workstream,
    WorkstreamStatus,
};
use pitcrew_protocol::recap::{Block, DayRecap, RecapBlock, Summary};
use pitcrew_recap::{Config, Directory, RuleSummarizer, block_line, day_recaps};
use pitcrew_store::{Store, StoreOptions};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use ulid::Ulid;

/// 2026-09-30 08:00 UTC.
pub const T0: i64 = 1_790_755_200_000;

fn id(kind: u128, n: u128) -> Ulid {
    Ulid::from((kind << 96) | n)
}

/// Event ids in the order they are made.
#[derive(Debug, Default)]
pub struct Ids(u64);

impl Ids {
    pub fn next(&mut self) -> EventId {
        self.0 += 1;
        EventId(id(1, u128::from(self.0)))
    }
}

pub fn open_store(dir: &Path) -> Arc<Store> {
    Arc::new(
        Store::open_with(
            dir.join("hub.db"),
            StoreOptions::default(),
            pitcrew_hub_work::projections(),
        )
        .expect("open store"),
    )
}

/// A synthetic workspace: a person, four agents, projects with workstreams, tasks (every fifth
/// outside any workstream) and sessions (every third unlinked).
#[derive(Debug, Clone)]
pub struct World {
    pub workspace: Workspace,
    pub person: MemberId,
    pub agents: Vec<MemberId>,
    pub projects: Vec<Project>,
    pub workstreams: Vec<Workstream>,
    pub tasks: Vec<Task>,
    pub sessions: Vec<Session>,
    pub asks: Vec<AskId>,
    pub dispatches: Vec<DispatchId>,
}

const KEYS: [&str; 3] = ["PAP", "GEN", "OPS"];

impl World {
    pub fn new(projects: usize, workstreams: usize, tasks: usize, sessions: usize) -> Self {
        let workspace = Workspace {
            id: WorkspaceId(id(2, 1)),
            name: "Generated".into(),
        };
        let person = MemberId(id(4, 0));
        let agents: Vec<MemberId> = (1..=4u128).map(|n| MemberId(id(4, n))).collect();
        let projects: Vec<Project> = (0..projects.clamp(1, KEYS.len()))
            .map(|i| Project {
                id: ProjectId(id(3, i as u128)),
                key: ProjectKey::new(KEYS[i]).expect("key"),
                name: format!("Project {i}"),
                status: ProjectStatus::InProgress,
                lead: person,
                members: vec![person],
                start: None,
                due: None,
                root: None,
                external: vec![],
            })
            .collect();
        let workstreams: Vec<Workstream> = (0..workstreams.max(1))
            .map(|i| Workstream {
                id: WorkstreamId(id(5, i as u128)),
                project: projects[i % projects.len()].id,
                name: format!("Stream {i}"),
                status: WorkstreamStatus::Active,
                health: Health::OnTrack,
                locations: vec![],
                external: vec![],
            })
            .collect();
        let tasks: Vec<Task> = (0..tasks.max(1))
            .map(|i| {
                let project = &projects[i % projects.len()];
                let theirs: Vec<&Workstream> = workstreams
                    .iter()
                    .filter(|w| w.project == project.id)
                    .collect();
                // Every fifth task lives outside any workstream; the others take turns.
                let workstream = (i % 5 != 4 && !theirs.is_empty())
                    .then(|| theirs[(i / projects.len()) % theirs.len()].id);
                task(
                    TaskId(id(6, i as u128)),
                    TaskKey::new(project.key.clone(), i as u32 + 1).expect("task key"),
                    project.id,
                    workstream,
                )
            })
            .collect();
        let sessions: Vec<Session> = (0..sessions.max(1))
            .map(|i| {
                // Every third session is unlinked.
                let task = (i % 3 != 2).then(|| tasks[i % tasks.len()].id);
                session(SessionId(id(7, i as u128)), agents[i % agents.len()], task)
            })
            .collect();
        Self {
            workspace,
            person,
            agents,
            projects,
            workstreams,
            tasks,
            sessions,
            asks: (0..8u128).map(|n| AskId(id(8, n))).collect(),
            dispatches: (0..8u128).map(|n| DispatchId(id(9, n))).collect(),
        }
    }

    fn event(&self, ids: &mut Ids, at: i64, author: MemberId, body: EventBody) -> Event {
        Event {
            id: ids.next(),
            at,
            workspace: self.workspace.id,
            author,
            on_behalf_of: None,
            body,
        }
    }

    /// The events that create the world, all at `at`.
    pub fn setup(&self, ids: &mut Ids, at: i64) -> Vec<Event> {
        let mut out = Vec::new();
        let mut members = vec![Member {
            id: self.person,
            kind: MemberKind::Human,
            handle: "@lead".into(),
            name: "Lead".into(),
            owner: None,
            persona: None,
        }];
        for (i, a) in self.agents.iter().enumerate() {
            members.push(agent(*a, self.person, &format!("@agent{}", i + 1)));
        }
        for member in members {
            out.push(self.event(ids, at, self.person, EventBody::MemberAdded { member }));
        }
        for project in &self.projects {
            let project = project.clone();
            out.push(self.event(ids, at, self.person, EventBody::ProjectCreated { project }));
        }
        for workstream in &self.workstreams {
            let workstream = workstream.clone();
            let body = EventBody::WorkstreamCreated { workstream };
            out.push(self.event(ids, at, self.person, body));
        }
        for task in &self.tasks {
            let task = task.clone();
            out.push(self.event(ids, at, self.person, EventBody::TaskCreated { task }));
        }
        for session in &self.sessions {
            let author = session.agent.unwrap_or(self.person);
            let session = session.clone();
            out.push(self.event(ids, at, author, EventBody::SessionDiscovered { session }));
        }
        out
    }
}

pub fn agent(id: MemberId, owner: MemberId, handle: &str) -> Member {
    Member {
        id,
        kind: MemberKind::Agent,
        handle: handle.into(),
        name: handle.trim_start_matches('@').into(),
        owner: Some(owner),
        persona: None,
    }
}

pub fn task(
    id: TaskId,
    key: TaskKey,
    project: ProjectId,
    workstream: Option<WorkstreamId>,
) -> Task {
    Task {
        id,
        title: format!("Task {key}"),
        key,
        project,
        workstream,
        description: String::new(),
        status: TaskStatus::Todo,
        priority: Priority::None,
        assignee: None,
        labels: vec![],
        start: None,
        due: None,
        blocked_by: vec![],
        source: None,
        accept_auto: false,
        subtasks: vec![],
    }
}

pub fn session(id: SessionId, agent: MemberId, task: Option<TaskId>) -> Session {
    Session {
        id,
        engine: Engine::Claude,
        native_id: "native".into(),
        machine: MachineId(Ulid::from(1u128)),
        cwd: "/work".into(),
        branch: None,
        title: Some("Generated session".into()),
        agent: Some(agent),
        workstream: None,
        task,
        link_basis: task.map(|_| LinkBasis::Manual),
        state: SessionState::Working,
        status_line: None,
        started: 0,
        last_activity: 0,
        terminal: None,
        parent: None,
    }
}

/// One generated event: `(kind, a, b, flag, dt)`. `a` and `b` pick sessions, tasks, files and so
/// on; `dt` is the time since the previous event (it may be negative, like a skewed clock).
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

/// Turns specs into events after `start`. Besides a session's work, they rename agents and
/// workstreams, move tasks between workstreams, re-state tasks, refused task creations (a new
/// task with a key another holds), decisions and briefs.
pub fn gen_events(specs: &[Spec], w: &World, ids: &mut Ids, start: i64) -> Vec<Event> {
    let mut at = start;
    let mut out = Vec::with_capacity(specs.len());
    for (i, &(kind, a, b, flag, dt)) in specs.iter().enumerate() {
        at = at.saturating_add(dt);
        let (a, b) = (usize::from(a), usize::from(b));
        let s = w.sessions[a % w.sessions.len()].id;
        let task = &w.tasks[b % w.tasks.len()];
        let t = task.id;
        let ws = &w.workstreams[a % w.workstreams.len()];
        let agent_id = w.agents[a % w.agents.len()];
        let transcript = Receipt::Transcript {
            session: s,
            offset: (i as u64) * 100,
        };
        let (author, body) = match kind % 24 {
            0..=3 => (
                agent_id,
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
                agent_id,
                EventBody::FileEdited {
                    session: s,
                    path: FILES[b % FILES.len()].into(),
                    added: a as u32,
                    removed: (b / 3) as u32,
                    receipt: None,
                },
            ),
            6 => (
                agent_id,
                EventBody::TurnEnded {
                    session: s,
                    receipt: transcript,
                },
            ),
            7 => (
                if flag { w.person } else { agent_id },
                EventBody::TaskMoved {
                    task: t,
                    from: STATUSES[a % STATUSES.len()],
                    to: STATUSES[(a + 1 + b) % STATUSES.len()],
                    mover: Mover::Person,
                },
            ),
            8 => (
                agent_id,
                EventBody::SubtasksReplaced {
                    task: t,
                    subtasks: (0..b % 5)
                        .map(|k| Subtask {
                            id: SubtaskId(id(10, k as u128)),
                            text: format!("step {k}"),
                            done: k < a % 5,
                            source: SubtaskSource::AgentPlan { agent: agent_id },
                        })
                        .collect(),
                },
            ),
            9 => (
                agent_id,
                EventBody::AskRaised {
                    ask: Ask {
                        id: w.asks[b % w.asks.len()],
                        kind: [AskKind::Question, AskKind::Decision, AskKind::Review][a % 3],
                        from: agent_id,
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
            ),
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
                if flag { w.person } else { agent_id },
                EventBody::CommentPosted {
                    task: (b % 3 != 0).then_some(t),
                    workstream: (b % 3 == 0).then_some(ws.id),
                    text: "looks good".into(),
                    mentions: vec![w.person, agent_id],
                },
            ),
            12 => (
                w.person,
                EventBody::WorkstreamChanged {
                    workstream: ws.id,
                    status: WorkstreamStatus::Active,
                    health: [Health::OnTrack, Health::AtRisk, Health::Blocked][b % 3],
                },
            ),
            13 => (
                agent_id,
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
                                agent: agent_id,
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
                        agent_id,
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
            15 => match b % 4 {
                0 => (
                    agent_id,
                    EventBody::SessionDiscovered {
                        session: session(s, agent_id, flag.then_some(t)),
                    },
                ),
                1 => (
                    w.person,
                    EventBody::SessionLinked {
                        session: s,
                        workstream: Some(ws.id),
                        task: flag.then_some(t),
                        basis: LinkBasis::Manual,
                    },
                ),
                2 => (
                    w.person,
                    EventBody::MachineLiveness {
                        machine: MachineId(Ulid::from(1u128)),
                        liveness: Liveness::Live,
                    },
                ),
                _ => (w.person, EventBody::SessionEnded { session: s }),
            },
            // An agent's new handle: lines name it from now on, old ones included.
            16 => (
                w.person,
                EventBody::MemberAdded {
                    member: self::agent(agent_id, w.person, &format!("@agent{a}x{i}")),
                },
            ),
            17 => {
                let created = if flag {
                    // A new task with a key another task holds: the tasks projection refuses it.
                    let mut clash = task.clone();
                    clash.id = TaskId(id(11, i as u128));
                    clash
                } else {
                    // The same task, re-stated in another workstream of its project.
                    let mut again = task.clone();
                    again.workstream = w
                        .workstreams
                        .iter()
                        .filter(|x| x.project == task.project)
                        .nth(a % 2)
                        .map(|x| x.id);
                    again
                };
                (w.person, EventBody::TaskCreated { task: created })
            }
            18 => (
                w.person,
                EventBody::TaskUpdated {
                    task: t,
                    patch: TaskPatch {
                        workstream: Some(
                            w.workstreams
                                .iter()
                                .filter(|x| x.project == task.project)
                                .nth(a % 3)
                                .map(|x| x.id),
                        ),
                        ..TaskPatch::default()
                    },
                },
            ),
            19 => {
                let mut renamed = ws.clone();
                renamed.name = format!("Stream {a} v{i}");
                (
                    w.person,
                    EventBody::WorkstreamCreated {
                        workstream: renamed,
                    },
                )
            }
            20 => (
                w.person,
                EventBody::DecisionRecorded {
                    workstream: flag.then_some(ws.id),
                    text: "Use the smaller model".into(),
                    why: None,
                    receipts: vec![Receipt::Commit {
                        repo: "paper".into(),
                        sha: format!("{b:07x}"),
                    }],
                },
            ),
            21 => (
                w.person,
                EventBody::BriefAccepted {
                    target: if flag {
                        BriefTarget::Workstream(ws.id)
                    } else {
                        BriefTarget::Project(ws.project)
                    },
                    text: "On track.".into(),
                    next: None,
                    pinned: b % 2 == 0,
                    receipts: vec![],
                },
            ),
            _ => (
                agent_id,
                EventBody::BriefProposed {
                    target: BriefTarget::Workstream(ws.id),
                    text: "Paused?".into(),
                    next: None,
                    receipts: vec![],
                },
            ),
        };
        out.push(w.event(ids, at, author, body));
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

// ─── The oracle ──────────────────────────────────────────────────────────────────────────────

/// The recap engine over a whole log at once, from an empty directory: what any index fed the
/// same events must serve.
pub struct Oracle {
    pub blocks: Vec<Block>,
    pub names: Directory,
}

impl Oracle {
    /// `events` must leave out what the hub refused (see [`refused`]).
    pub fn new(events: &[Event]) -> Self {
        Self::with_seed(events, Directory::new())
    }

    pub fn with_seed(events: &[Event], seed: Directory) -> Self {
        Self::with_config(events, seed, &Config::default())
    }

    pub fn with_config(events: &[Event], seed: Directory, config: &Config) -> Self {
        let blocks = pitcrew_recap::blocks(events, &seed, config);
        let mut names = seed;
        for e in events {
            match &e.body {
                EventBody::MemberAdded { member } => names.add_member(member),
                _ => names.observe(e),
            }
        }
        Self { blocks, names }
    }

    /// Every block `filter` matches, newest first, with its line.
    pub fn blocks(&self, filter: &BlockFilter) -> Vec<RecapBlock> {
        let mut out: Vec<RecapBlock> = self
            .blocks
            .iter()
            .filter(|b| filter.matches(b))
            .map(|b| RecapBlock {
                block: b.clone(),
                line: block_line(b, &self.names),
            })
            .collect();
        out.sort_by(|a, b| b.block.id.cmp(&a.block.id));
        out
    }

    /// Every day of `scope` at `tz`, newest date first; within a date, as the engine orders them.
    pub fn days(&self, scope: DaysScope, tz: i32) -> Vec<DayRecap> {
        let mine: Vec<Block> = self
            .blocks
            .iter()
            .filter(|b| match scope {
                DaysScope::Workstream(w) => b.workstream == Some(w),
                DaysScope::Project(p) => b.project == Some(p),
            })
            .cloned()
            .collect();
        let mut days = day_recaps(&mine, &self.names, tz, &RuleSummarizer).expect("rules");
        days.sort_by(|a, b| b.date.cmp(&a.date));
        days
    }
}

/// The revisions of `task_created` events the tasks projection refused.
pub fn refused(work: &WorkService) -> HashSet<u64> {
    work.read(|c| {
        let mut stmt = c.prepare("SELECT rev FROM work_task_clashes")?;
        let revs = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        Ok(revs.into_iter().map(|r| r as u64).collect())
    })
    .expect("clashes")
}

/// Every event in the store, minus those the hub refused: what the index is fed.
pub fn log_events(work: &WorkService) -> Vec<Event> {
    let refused = refused(work);
    work.store()
        .since(0, usize::MAX)
        .expect("log")
        .into_iter()
        .filter(|e| !refused.contains(&e.rev))
        .map(|e| e.event)
        .collect()
}

// ─── Dumps ───────────────────────────────────────────────────────────────────────────────────

/// Pages through every block `filter` matches with pages of `limit`, checking the paging rules.
pub fn all_blocks(index: &dyn RecapIndex, filter: &BlockFilter, limit: usize) -> Vec<RecapBlock> {
    let mut out: Vec<RecapBlock> = Vec::new();
    let mut before = None;
    loop {
        let page = index
            .recap_blocks(filter, before, Some(limit))
            .expect("blocks");
        assert!(page.blocks.len() <= limit.min(200));
        if !page.at_start {
            assert!(!page.blocks.is_empty(), "a page not at the start is empty");
            // There are no scan budgets here: a page short of its limit is the last.
            assert_eq!(page.blocks.len(), limit.min(200));
        }
        for b in &page.blocks {
            assert!(filter.matches(&b.block));
            if let Some(before) = before {
                assert!(b.block.id < before, "before is exclusive");
            }
        }
        before = page.blocks.last().map(|b| b.block.id);
        out.extend(page.blocks);
        if page.at_start {
            return out;
        }
    }
}

/// Pages through every day of `scope` at `tz` with pages of `limit` dates, checking the paging
/// rules.
pub fn all_days(index: &dyn RecapIndex, scope: DaysScope, tz: i32, limit: usize) -> Vec<DayRecap> {
    let mut out: Vec<DayRecap> = Vec::new();
    let mut before: Option<pitcrew_protocol::model::Date> = None;
    loop {
        let page = index
            .recap_days(scope, tz, before.as_ref(), Some(limit))
            .expect("days");
        let mut dates: Vec<&pitcrew_protocol::model::Date> =
            page.days.iter().map(|d| &d.date).collect();
        dates.dedup();
        assert!(dates.len() <= limit.min(30));
        if !page.at_start {
            assert_eq!(dates.len(), limit.min(30), "a page not at the start");
        }
        assert!(dates.windows(2).all(|w| w[0] > w[1]), "newest date first");
        if let (Some(before), Some(first)) = (&before, dates.first()) {
            assert!(*first < before, "before is exclusive");
            // Whole dates: the previous page held every entry of its last date.
            assert!(out.iter().all(|d| d.date != **first));
        }
        before = page.days.last().map(|d| d.date.clone());
        out.extend(page.days);
        if page.at_start {
            return out;
        }
    }
}

/// Everything an index serves for `world`: every block, by each filter on its own, and every day
/// of every scope at a few offsets.
#[derive(Debug, PartialEq)]
pub struct Dump {
    pub blocks: Vec<RecapBlock>,
    pub filtered: Vec<Vec<RecapBlock>>,
    pub days: Vec<Vec<DayRecap>>,
}

pub const OFFSETS: [i32; 3] = [0, 120, -300];

fn filters(world: &World) -> Vec<BlockFilter> {
    let mut out = Vec::new();
    for s in world.sessions.iter().take(3) {
        out.push(BlockFilter {
            session: Some(s.id),
            ..BlockFilter::default()
        });
    }
    for t in world.tasks.iter().take(4) {
        out.push(BlockFilter {
            task: Some(t.id),
            ..BlockFilter::default()
        });
    }
    for w in &world.workstreams {
        out.push(BlockFilter {
            workstream: Some(w.id),
            ..BlockFilter::default()
        });
    }
    for p in &world.projects {
        out.push(BlockFilter {
            project: Some(p.id),
            ..BlockFilter::default()
        });
    }
    if let (Some(w), Some(t)) = (world.workstreams.first(), world.tasks.first()) {
        out.push(BlockFilter {
            workstream: Some(w.id),
            task: Some(t.id),
            project: Some(w.project),
            ..BlockFilter::default()
        });
    }
    out
}

pub fn scopes(world: &World) -> Vec<DaysScope> {
    world
        .workstreams
        .iter()
        .map(|w| DaysScope::Workstream(w.id))
        .chain(world.projects.iter().map(|p| DaysScope::Project(p.id)))
        .collect()
}

pub fn dump(index: &dyn RecapIndex, world: &World, limit: usize) -> Dump {
    Dump {
        blocks: all_blocks(index, &BlockFilter::default(), limit),
        filtered: filters(world)
            .iter()
            .map(|f| all_blocks(index, f, limit))
            .collect(),
        days: scopes(world)
            .into_iter()
            .flat_map(|s| OFFSETS.map(|tz| (s, tz)))
            .map(|(s, tz)| all_days(index, s, tz, limit.min(30)))
            .collect(),
    }
}

pub fn oracle_dump(oracle: &Oracle, world: &World) -> Dump {
    Dump {
        blocks: oracle.blocks(&BlockFilter::default()),
        filtered: filters(world).iter().map(|f| oracle.blocks(f)).collect(),
        days: scopes(world)
            .into_iter()
            .flat_map(|s| OFFSETS.map(|tz| (s, tz)))
            .map(|(s, tz)| oracle.days(s, tz))
            .collect(),
    }
}

/// [`Recaps`] as a [`RecapIndex`], for the dumps.
#[derive(Debug)]
pub struct Core(pub std::sync::Mutex<Recaps>);

impl RecapIndex for Core {
    fn recap_blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: Option<usize>,
    ) -> pitcrew_hub_work::Result<pitcrew_protocol::recap::BlocksPage> {
        self.0.lock().expect("lock").blocks(filter, before, limit)
    }

    fn recap_days(
        &self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<&pitcrew_protocol::model::Date>,
        limit: Option<usize>,
    ) -> pitcrew_hub_work::Result<pitcrew_protocol::recap::DaysPage> {
        self.0
            .lock()
            .expect("lock")
            .days(scope, tz_minutes, before, limit)
    }
}

// ─── Receipts ────────────────────────────────────────────────────────────────────────────────

/// Every receipt the log can justify: its event ids, and every receipt its events carry.
pub fn allowed_receipts(events: &[Event]) -> HashSet<Receipt> {
    let mut out = HashSet::new();
    for e in events {
        out.insert(Receipt::Event { id: e.id });
        match &e.body {
            EventBody::ToolRan { receipt, .. } | EventBody::TurnEnded { receipt, .. } => {
                out.insert(receipt.clone());
            }
            EventBody::FileEdited {
                receipt: Some(receipt),
                ..
            } => {
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

fn check_summary(s: &Summary, allowed: &HashSet<Receipt>) {
    assert!(!s.spans.is_empty() || s.text.is_empty(), "{:?}", s.text);
    for span in &s.spans {
        assert!(!span.receipts.is_empty(), "a span without receipts");
        assert!(!s.clause(span).is_empty(), "a span off the text");
        for r in &span.receipts {
            assert!(allowed.contains(r), "{r:?} is not in the log");
        }
    }
}

/// Every receipt in the dump points into `allowed`.
pub fn check_receipts(dump: &Dump, allowed: &HashSet<Receipt>) {
    let blocks = dump.blocks.iter().chain(dump.filtered.iter().flatten());
    for b in blocks {
        for r in b.block.receipts() {
            assert!(allowed.contains(r), "{r:?} is not in the log");
        }
        assert!(allowed.contains(&Receipt::Event { id: b.block.id }));
        assert!(allowed.contains(&Receipt::Event { id: b.block.last }));
        check_summary(&b.line, allowed);
    }
    for d in dump.days.iter().flatten() {
        for id in &d.blocks {
            assert!(allowed.contains(&Receipt::Event { id: *id }));
        }
        check_summary(&d.summary, allowed);
    }
}
