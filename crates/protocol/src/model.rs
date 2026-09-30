//! The domain model.
//!
//! ```text
//! Workspace → Project → Workstream → Task → Subtask
//!                 └──── members: people and agents (every agent has an owner)
//! Sessions (CLI conversations on machines) link to a workstream and usually a task.
//! ```
//!
//! Timestamps are [`TimestampMs`] (UTC milliseconds). Calendar dates are [`Date`] (`YYYY-MM-DD`).

use crate::ids::{
    AskId, DispatchId, EventId, MachineId, MemberId, PersonaId, ProjectId, ProjectKey, SessionId,
    SubtaskId, TaskId, TaskKey, TeamId, TerminalId, WorkstreamId,
};
use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch, UTC.
pub type TimestampMs = i64;

/// A calendar date without a time zone, written `YYYY-MM-DD`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Date(pub String);

impl Date {
    /// Returns `true` if the text has the shape `YYYY-MM-DD` with a plausible month and day.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let b = self.0.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return false;
        }
        let digits = |r: std::ops::Range<usize>| {
            self.0
                .get(r)
                .filter(|s| s.bytes().all(|c| c.is_ascii_digit()))
                .and_then(|s| s.parse::<u32>().ok())
        };
        matches!(
            (digits(0..4), digits(5..7), digits(8..10)),
            (Some(_), Some(1..=12), Some(1..=31))
        )
    }
}

/// A workspace: one context, such as "PhD research", hosted by one hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Id.
    pub id: crate::ids::WorkspaceId,
    /// Name.
    pub name: String,
}

// ─── Engines and machines ────────────────────────────────────────────────────────────────────

/// The agent CLI a session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Engine {
    /// Claude Code (`claude`).
    Claude,
    /// OpenAI Codex CLI (`codex`).
    Codex,
    /// OpenCode (`opencode`).
    OpenCode,
}

/// How a machine is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineKind {
    /// The computer the desktop app runs on.
    Local,
    /// A WSL distro on the local Windows computer.
    Wsl,
    /// A remote machine over SSH, such as a server or an HPC login node.
    Ssh,
}

/// Whether a machine can be reached. **Loss of contact is not death:** an unreachable machine is
/// `Unverifiable`, and its last known state is still shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Connected, and the runner answers.
    Live,
    /// Not reachable right now; the last known state may be stale.
    Unverifiable,
    /// Deliberately stopped, or removed.
    Stopped,
}

/// A batch scheduler found on a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scheduler {
    /// SLURM (`sbatch`, `squeue`, `sacct`).
    Slurm,
}

/// Facts a runner reports about its machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfo {
    /// Host name as the machine reports it.
    pub hostname: String,
    /// Operating system, e.g. `linux`, `macos`, `windows`.
    pub os: String,
    /// CPU architecture, e.g. `x86_64`, `aarch64`.
    pub arch: String,
    /// Whether `tmux` is available.
    pub has_tmux: bool,
    /// The batch scheduler, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<Scheduler>,
    /// Whether the home directory is on a network filesystem (e.g. NFS). This changes storage
    /// rules; see ADR-0004.
    #[serde(default)]
    pub home_on_network_fs: bool,
}

/// A machine known to a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// Id.
    pub id: MachineId,
    /// Display name, e.g. "This PC" or "cluster".
    pub name: String,
    /// How it is reached.
    pub kind: MachineKind,
    /// Last reported facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<MachineInfo>,
    /// Whether it can be reached now.
    pub liveness: Liveness,
}

/// A folder (and optionally a git branch) on a machine. This is how sessions are linked to
/// projects and workstreams.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    /// The machine.
    pub machine: MachineId,
    /// Absolute path on that machine.
    pub path: String,
    /// The git branch, when the location is branch-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

// ─── Members, personas, teams ────────────────────────────────────────────────────────────────

/// Whether a member is a person or an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    /// A person.
    Human,
    /// An AI agent. It always has an owner, and its authority is bounded by the owner's.
    Agent,
}

/// A member of a workspace. People and agents share this shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// Id.
    pub id: MemberId,
    /// Person or agent.
    pub kind: MemberKind,
    /// Unique handle within the workspace, e.g. `@writer`.
    pub handle: String,
    /// Display name.
    pub name: String,
    /// The owning person. Required for agents; absent for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<MemberId>,
    /// The persona an agent was created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<PersonaId>,
}

impl Member {
    /// Checks the ownership invariant: every agent has an owner, and no person has one.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self.kind {
            MemberKind::Agent => self.owner.is_some_and(|o| o != self.id),
            MemberKind::Human => self.owner.is_none(),
        }
    }
}

/// How an agent CLI handles permission prompts. The default is the CLI's own behaviour, which
/// asks. Bypassing is an explicit per-workspace opt-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// The CLI's own default: it asks before risky actions.
    #[default]
    Default,
    /// Accept file edits without asking.
    AcceptEdits,
    /// Plan only; make no changes.
    Plan,
    /// Skip all permission prompts. Needs explicit opt-in, and every use is written to the audit
    /// log.
    BypassPermissions,
}

/// A reusable recipe for new agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Persona {
    /// Id.
    pub id: PersonaId,
    /// Name, e.g. "Writer".
    pub name: String,
    /// The CLI to run.
    pub engine: Engine,
    /// Model identifier passed to the CLI, if not its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Standing instructions given to every agent made from this persona.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Permission mode.
    #[serde(default)]
    pub permission_mode: PermissionMode,
}

/// A group of members with a lead, such as "Paper team".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    /// Id.
    pub id: TeamId,
    /// Name.
    pub name: String,
    /// The lead member.
    pub lead: MemberId,
    /// All members, including the lead.
    pub members: Vec<MemberId>,
}

// ─── Projects and workstreams ────────────────────────────────────────────────────────────────

/// A link to an item in an external system.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalRef {
    /// The system.
    pub system: ExternalSystem,
    /// The item's key in that system, e.g. `owner/repo#12`, `PROJ-7`, or a milestone name.
    pub key: String,
    /// A link to open it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// External systems PitCrew links to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExternalSystem {
    /// GitHub issues, pull requests, milestones.
    Github,
    /// Jira issues and epics.
    Jira,
    /// Linear issues and projects.
    Linear,
    /// GitLab issues and merge requests.
    Gitlab,
}

/// Status of a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// Being planned.
    Planning,
    /// Active.
    InProgress,
    /// Paused.
    OnHold,
    /// Finished.
    Completed,
}

/// A project: a deliverable such as a paper, a thesis part or a product.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Id.
    pub id: ProjectId,
    /// Key used in task keys.
    pub key: ProjectKey,
    /// Name.
    pub name: String,
    /// Status.
    pub status: ProjectStatus,
    /// The lead member.
    pub lead: MemberId,
    /// Members working on it.
    #[serde(default)]
    pub members: Vec<MemberId>,
    /// Start date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Date>,
    /// Due date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Date>,
    /// The root folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Location>,
    /// Linked external items, such as a repository or a Jira project.
    #[serde(default)]
    pub external: Vec<ExternalRef>,
}

/// Status of a workstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkstreamStatus {
    /// A candidate, not started.
    Idea,
    /// Being worked on.
    Active,
    /// Paused.
    Paused,
    /// Delivered.
    Shipped,
    /// Abandoned.
    Dropped,
}

/// Health of an active workstream. It is derived from events, and a person can override it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// On track.
    OnTrack,
    /// At risk.
    AtRisk,
    /// Blocked.
    Blocked,
}

/// A workstream: one line of work inside a project, such as "Seed campaign".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workstream {
    /// Id.
    pub id: WorkstreamId,
    /// The project it belongs to.
    pub project: ProjectId,
    /// Name.
    pub name: String,
    /// Status.
    pub status: WorkstreamStatus,
    /// Health.
    pub health: Health,
    /// Folders or branches whose sessions belong to this workstream.
    #[serde(default)]
    pub locations: Vec<Location>,
    /// Linked external items, such as a GitHub milestone or a Jira epic.
    #[serde(default)]
    pub external: Vec<ExternalRef>,
}

// ─── Tasks ───────────────────────────────────────────────────────────────────────────────────

/// Status of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not planned yet.
    Backlog,
    /// Planned.
    Todo,
    /// Someone is working on it.
    InProgress,
    /// Waiting for review.
    Review,
    /// Finished.
    Done,
    /// Won't be done.
    Canceled,
}

/// Who is asking to move a task. The hub checks every move with [`TaskStatus::can_move`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mover {
    /// A person.
    Person,
    /// An agent. `on_own_task` is true when the agent is the task's assignee or holds its active
    /// dispatch.
    Agent {
        /// Whether the task is the agent's own.
        on_own_task: bool,
    },
    /// The back office, acting on evidence. `accept_auto` is the task's acceptance policy.
    BackOffice {
        /// Whether the task may be accepted without a person.
        accept_auto: bool,
    },
    /// A tracker sync (GitHub, Jira, Linear) reflecting an upstream change.
    Sync,
}

impl TaskStatus {
    /// Whether `mover` may move a task from `self` to `to`.
    ///
    /// The rules are the product contract (see ADR-0007):
    /// - **People** may make any move.
    /// - **Agents** may move only their own task, and only forward: backlog or todo → in progress,
    ///   and in progress → review. An agent never moves a task to done or canceled.
    /// - **The back office** moves in progress → review on evidence, and review → done only when
    ///   the task allows automatic acceptance.
    /// - **Sync** mirrors an upstream close (to done) or reopen (done → todo). It never touches a
    ///   task that is in progress.
    #[must_use]
    pub fn can_move(self, to: TaskStatus, mover: Mover) -> bool {
        use TaskStatus::{Backlog, Canceled, Done, InProgress, Review, Todo};
        if self == to {
            return false;
        }
        match mover {
            Mover::Person => true,
            Mover::Agent { on_own_task } => {
                on_own_task
                    && matches!(
                        (self, to),
                        (Backlog | Todo, InProgress) | (InProgress, Review)
                    )
            }
            Mover::BackOffice { accept_auto } => match (self, to) {
                (InProgress, Review) => true,
                (Review, Done) => accept_auto,
                _ => false,
            },
            Mover::Sync => matches!(
                (self, to),
                (Backlog | Todo | Review | Canceled, Done) | (Done, Todo)
            ),
        }
    }
}

/// Priority of a task.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Urgent.
    Urgent,
    /// High.
    High,
    /// Medium.
    Medium,
    /// Low.
    Low,
    /// No priority set.
    #[default]
    None,
}

/// Where a subtask came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubtaskSource {
    /// Added by a person.
    Human,
    /// Mirrored from an agent's live plan (its todo list). It is updated as the agent works.
    AgentPlan {
        /// The agent whose plan this is.
        agent: MemberId,
    },
}

/// One checklist line on a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subtask {
    /// Id.
    pub id: SubtaskId,
    /// Text.
    pub text: String,
    /// Whether it is done.
    pub done: bool,
    /// Origin.
    pub source: SubtaskSource,
}

/// A task: something a person or an agent finishes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Id.
    pub id: TaskId,
    /// People-facing key, e.g. `CMP-104`.
    pub key: TaskKey,
    /// The project.
    pub project: ProjectId,
    /// The workstream, if any. Small projects may have tasks without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workstream: Option<WorkstreamId>,
    /// Title.
    pub title: String,
    /// Description, which is also the brief an agent reads first.
    #[serde(default)]
    pub description: String,
    /// Status.
    pub status: TaskStatus,
    /// Priority.
    #[serde(default)]
    pub priority: Priority,
    /// The assignee: a person or an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<MemberId>,
    /// Labels.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Start date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Date>,
    /// Due date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Date>,
    /// Tasks that must finish first.
    #[serde(default)]
    pub blocked_by: Vec<TaskId>,
    /// The external item this task mirrors, such as a GitHub issue or a Jira issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ExternalRef>,
    /// Whether review can complete without a person (see [`Mover::BackOffice`]).
    #[serde(default)]
    pub accept_auto: bool,
    /// Checklist.
    #[serde(default)]
    pub subtasks: Vec<Subtask>,
}

// ─── Sessions and dispatches ─────────────────────────────────────────────────────────────────

/// State of a session, derived from hooks, transcripts and the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Launched; no transcript yet.
    Starting,
    /// Running a turn.
    Working,
    /// Waiting for a person: a question, a permission prompt, or the end of a turn.
    Waiting,
    /// Alive, with nothing happening.
    Idle,
    /// The process has ended.
    Ended,
    /// Its machine cannot be reached; the last known state is kept.
    Unreachable,
}

/// Why a session is linked to a workstream or task. Links made by a person or a dispatch are never
/// overridden by inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkBasis {
    /// Started by a dispatch.
    Dispatch,
    /// The agent claimed the task itself.
    Claimed,
    /// Linked by a person.
    Manual,
    /// Its working folder is inside a workstream location.
    Folder,
    /// Its git branch matches a workstream location.
    Branch,
    /// Linked when importing existing sessions.
    Imported,
}

/// One CLI conversation on one machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Id.
    pub id: SessionId,
    /// The CLI.
    pub engine: Engine,
    /// The CLI's own session id, used to resume it.
    pub native_id: String,
    /// The machine it runs on.
    pub machine: MachineId,
    /// Working directory.
    pub cwd: String,
    /// Git branch at the start of the session, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Title, from the CLI or a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The agent member running it. Absent for unnamed runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<MemberId>,
    /// Linked workstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workstream: Option<WorkstreamId>,
    /// Linked task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// Why it is linked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_basis: Option<LinkBasis>,
    /// Current state.
    pub state: SessionState,
    /// A one-line status, e.g. "Running tests → 212 of 240".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_line: Option<String>,
    /// When it started.
    pub started: TimestampMs,
    /// Last observed activity.
    pub last_activity: TimestampMs,
    /// The terminal it runs in, if the runner owns one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalId>,
    /// For a sub-agent's session, the session that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
}

/// How a dispatch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchOutcome {
    /// The agent reports the work done.
    Succeeded,
    /// The agent reports it could not finish.
    Failed,
    /// Stopped by a person.
    Canceled,
}

/// One attempt at a task by one agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    /// Id.
    pub id: DispatchId,
    /// The task.
    pub task: TaskId,
    /// The agent.
    pub agent: MemberId,
    /// The session running it, once started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// The brief the agent was given.
    pub brief: String,
    /// Start time.
    pub started: TimestampMs,
    /// End time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<TimestampMs>,
    /// Outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<DispatchOutcome>,
    /// The agent's closing summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

// ─── Asks and receipts ───────────────────────────────────────────────────────────────────────

/// Evidence behind a claim. Every recap line and every "Where it stands" links to receipts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Receipt {
    /// A byte offset in a session's transcript.
    Transcript {
        /// The session.
        session: SessionId,
        /// Byte offset of the record.
        offset: u64,
    },
    /// A git commit.
    Commit {
        /// Repository, as a path or URL.
        repo: String,
        /// Commit hash.
        sha: String,
    },
    /// A pull or merge request.
    PullRequest {
        /// Link.
        url: String,
    },
    /// A batch job.
    Job {
        /// The scheduler.
        scheduler: Scheduler,
        /// The job id.
        id: String,
    },
    /// A file.
    File {
        /// Where it is.
        location: Location,
    },
    /// Another event.
    Event {
        /// The event id.
        id: EventId,
    },
}

/// Which brief ("Where it stands") something is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum BriefTarget {
    /// "Where the project stands".
    Project(ProjectId),
    /// "Where it stands" of a workstream.
    Workstream(WorkstreamId),
}

/// Who wrote the brief currently in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefSource {
    /// A person wrote or edited it.
    Person,
    /// The back office wrote it from events; it was accepted or applied automatically.
    BackOffice,
}

/// The "Where it stands" text of a project or workstream, as currently in force.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    /// Which brief.
    pub target: BriefTarget,
    /// The text.
    pub text: String,
    /// The next step, if stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Pinned by a person; the back office may then only propose changes.
    pub pinned: bool,
    /// Who wrote the version in force.
    pub source: BriefSource,
    /// When it was last updated.
    pub updated: TimestampMs,
    /// Evidence behind it.
    #[serde(default)]
    pub receipts: Vec<Receipt>,
}

/// The kind of ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    /// A question from an agent, such as Claude's AskUserQuestion or a terminal prompt.
    Question,
    /// A decision only a person may take.
    Decision,
    /// Finished work to accept or send back.
    Review,
    /// An outward action waiting for approval, such as a GitHub or Jira write, or GPU work.
    Approval,
    /// A mention of the member in a comment or room.
    Mention,
}

/// Lifecycle of an ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskState {
    /// Waiting for an answer.
    Open,
    /// Answered.
    Answered,
    /// No longer relevant.
    Withdrawn,
}

/// An answer to an ask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// Who answered. For decisions and approvals, always a person.
    pub by: MemberId,
    /// The chosen option's index, if the ask offered options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<usize>,
    /// Free-text answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// When.
    pub at: TimestampMs,
}

/// Something that needs a specific member's answer. Asks are what the Inbox lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    /// Id.
    pub id: AskId,
    /// Kind.
    pub kind: AskKind,
    /// Who is asking (an agent, the back office's member, or a person).
    pub from: MemberId,
    /// Who must answer.
    pub to: MemberId,
    /// Related task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// Related session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// One-line title.
    pub title: String,
    /// Context.
    #[serde(default)]
    pub body: String,
    /// Offered options, in order.
    #[serde(default)]
    pub options: Vec<String>,
    /// Evidence.
    #[serde(default)]
    pub receipts: Vec<Receipt>,
    /// Lifecycle.
    pub state: AskState,
    /// The answer, once given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// When it was raised.
    pub created: TimestampMs,
}
