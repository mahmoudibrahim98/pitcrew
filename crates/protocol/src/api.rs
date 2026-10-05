//! The daemon's API as seen by the desktop: host info, errors, token scopes, shared request bodies,
//! and the delta stream.
//!
//! - Endpoints live under [`API_PREFIX`]. The endpoint list is in `docs/build/contracts/api-v1.md`,
//!   and `apps/mock-hub` implements it with fixture data.
//! - Authentication is `Authorization: Bearer <token>` only, never a query string (ADR-0006).
//! - Live changes arrive on one WebSocket, `GET /v1/stream?since=<rev>`, as [`StreamFrame`]s.

use crate::events::Event;
use crate::ids::{MemberId, ProjectId, ProjectKey, WorkstreamId};
use crate::model::{
    Date, Location, Machine, MachineInfo, Member, Priority, ProjectStatus, TaskStatus, TimestampMs,
    Workspace, WorkstreamStatus,
};
use crate::runner::Capability;
use serde::{Deserialize, Serialize};

/// Launch choices advertised by a reachable machine's runner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionOptions {
    /// Absolute path syntax: `windows` or `unix`.
    pub platform: String,
    /// Executable agent CLIs, with their supported permission modes.
    pub engines: Vec<SessionEngine>,
}

/// One installed CLI's launch options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionEngine {
    /// CLI engine.
    pub engine: crate::model::Engine,
    /// Modes supported by this engine and allowed by this runner.
    pub permission_modes: Vec<crate::model::PermissionMode>,
}

/// The revision a person has read in a workspace, project or workstream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReadCursor {
    /// `workspace`, `project:<id>` or `workstream:<id>`.
    pub scope: String,
    /// Last seen log revision; only moves forward.
    pub rev: u64,
}

/// Request to advance a person's cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MoveCursor {
    /// Last seen log revision.
    pub rev: u64,
}

/// Prefix of every API route.
pub const API_PREFIX: &str = "/v1";

/// The roles a daemon plays (ADR-0009). A solo workspace runs both in one process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HostRole {
    /// Holds the workspace's shared state: projects, tasks, events, recaps.
    Hub,
    /// Runs and watches agent sessions on its machine.
    Runner,
}

/// `GET /v1/host/info`: available before authentication, so version skew can be detected early.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostInfo {
    /// Always `pitcrewd`.
    pub name: String,
    /// Daemon version.
    pub version: String,
    /// Protocol version it speaks.
    pub protocol: u32,
    /// Oldest protocol it accepts.
    pub protocol_min: u32,
    /// Roles.
    pub roles: Vec<HostRole>,
    /// Machine facts.
    pub machine: MachineInfo,
    /// Runner capabilities.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

/// The two kinds of token (ADR-0006).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// A person's desktop. It may take every action, including those only a person may take.
    Device,
    /// An agent or hook. It is limited to agent verbs, for its owner's workspace.
    Agent,
}

/// Machine-readable error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No valid token.
    Unauthorized,
    /// The token's scope does not allow this.
    Forbidden,
    /// No such resource.
    NotFound,
    /// A conflicting change, such as a move the rules do not allow.
    Conflict,
    /// A malformed request.
    Invalid,
    /// The resource's machine is unreachable.
    Unavailable,
    /// File or body exceeds the files API cap.
    TooLarge,
    /// File access on another machine is not implemented.
    Unsupported,
    /// An unexpected failure.
    Internal,
}

impl ErrorCode {
    /// The HTTP status that carries this code (`docs/build/contracts/api-v1.md`).
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::Invalid => 400,
            Self::Unavailable => 503,
            Self::TooLarge => 413,
            Self::Unsupported => 501,
            Self::Internal => 500,
        }
    }
}

/// Who is calling, as established by the API layer (stream H) from the bearer token.
///
/// Domain crates never parse tokens. Their `routes()` read the caller from the request's
/// extensions (`axum::Extension<Caller>`), which the API layer inserts after authentication, and
/// stamp events with `author = member` and `on_behalf_of`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Caller {
    /// The member the token belongs to: a person (device token) or an agent (agent token).
    pub member: MemberId,
    /// The token's scope.
    pub scope: TokenScope,
    /// For an agent, its owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub on_behalf_of: Option<MemberId>,
}

impl Caller {
    /// Whether this caller may take person-only actions (decisions, approvals, settings).
    #[must_use]
    pub fn is_person(&self) -> bool {
        self.scope == TokenScope::Device
    }
}

/// The error body of every failed request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ApiError {
    /// Code.
    pub code: ErrorCode,
    /// A sentence for people.
    pub message: String,
}

// ─── Request bodies ──────────────────────────────────────────────────────────────────────────
// Shared by the hub, the CLI and the mock hub. A field left out (or `null`) takes the default
// that `docs/build/contracts/api-v1.md` gives it; the hub applies it.

/// `POST /v1/tasks`: a new task. The hub assigns the id and the next key in the project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewTask {
    /// The project.
    pub project: ProjectId,
    /// The workstream; it must belong to the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub workstream: Option<WorkstreamId>,
    /// Title.
    pub title: String,
    /// Description; empty if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub description: Option<String>,
    /// Status; `todo` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub status: Option<TaskStatus>,
    /// Priority; `none` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub priority: Option<Priority>,
    /// Assignee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub assignee: Option<MemberId>,
    /// Labels; none if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub labels: Option<Vec<String>>,
    /// Due date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub due: Option<Date>,
}

/// `POST /v1/projects`: a new project. The hub assigns the id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewProject {
    /// Key used in task keys; unique in the workspace.
    pub key: ProjectKey,
    /// Name.
    pub name: String,
    /// The lead; the caller if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub lead: Option<MemberId>,
    /// Members; the lead alone if absent. The lead is always a member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub members: Option<Vec<MemberId>>,
    /// Status; `in_progress` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub status: Option<ProjectStatus>,
    /// Start date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub start: Option<Date>,
    /// Due date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub due: Option<Date>,
    /// The root folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub root: Option<Location>,
}

/// `POST /v1/workstreams`: a new workstream in a project. The hub assigns the id; its health
/// starts `on_track`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewWorkstream {
    /// The project.
    pub project: ProjectId,
    /// Name.
    pub name: String,
    /// Status; `active` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub status: Option<WorkstreamStatus>,
    /// Folders or branches whose sessions belong to it; none if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub locations: Option<Vec<Location>>,
}

/// `POST /v1/setup`: the first run of a fresh hub (api-v1.md, "The first run"). The person is the
/// device token's own member; the machine is the hub's own, local one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Setup {
    /// The workspace's name, 1–80 characters.
    pub workspace_name: String,
    /// The person setting the workspace up.
    pub person: SetupPerson,
    /// This machine's display name, 1–60 characters.
    pub machine_name: String,
}

/// The person in a [`Setup`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SetupPerson {
    /// Display name, 1–80 characters.
    pub name: String,
    /// `@` followed by 1–32 of `a-z 0-9 _ -`.
    pub handle: String,
}

/// The answer to `POST /v1/setup`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SetupDone {
    /// The workspace, with its new name.
    pub workspace: Workspace,
    /// The person, as `GET /v1/me` answers from now on.
    pub me: Member,
    /// The hub's own machine.
    pub machine: Machine,
}

/// `GET /v1/events`: a page of the activity log, oldest first within the page.
///
/// Page backwards by passing `from_rev` as `before`. Only `at_start` ends paging: with filters, a
/// page may hold fewer than `limit` events (even none) while older matches still exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventsPage {
    /// Actual log revisions, in the same order as `events`.
    pub revisions: Vec<u64>,
    /// The events, oldest first.
    pub events: Vec<Event>,
    /// Revision of the first returned event; with no events, where the scan stopped (0 at the
    /// start of the log).
    pub from_rev: u64,
    /// Revision of the last returned event, or 0 when the page is empty.
    pub to_rev: u64,
    /// True when no older matching event exists.
    pub at_start: bool,
}

/// Frames on `GET /v1/stream?since=<rev>`. `rev` is the hub's event revision. A client that
/// reconnects with `since` receives exactly what it missed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamFrame {
    /// First frame: the current revision, and which event log it counts in.
    Hello {
        /// Current revision.
        rev: u64,
        /// Identifies this hub's event log; it is created with the store and never changes.
        /// Revisions only mean something within one log: a client whose cached state came from
        /// a different `log` must drop it and refetch.
        log: String,
    },
    /// New events, covering revisions `from_rev..=to_rev`.
    Events {
        /// First revision in the batch.
        from_rev: u64,
        /// Last revision in the batch.
        to_rev: u64,
        /// The events, in order.
        events: Vec<Event>,
    },
    /// Keep-alive.
    Ping {
        /// Server time.
        at: TimestampMs,
    },
}
