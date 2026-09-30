//! The daemon's API as seen by the desktop: host info, errors, token scopes, and the delta stream.
//!
//! - Endpoints live under [`API_PREFIX`]. The endpoint list is in `docs/build/contracts/api-v1.md`,
//!   and `apps/mock-hub` implements it with fixture data.
//! - Authentication is `Authorization: Bearer <token>` only, never a query string (ADR-0006).
//! - Live changes arrive on one WebSocket, `GET /v1/stream?since=<rev>`, as [`StreamFrame`]s.

use crate::events::Event;
use crate::ids::MemberId;
use crate::model::{MachineInfo, TimestampMs};
use crate::runner::Capability;
use serde::{Deserialize, Serialize};

/// Prefix of every API route.
pub const API_PREFIX: &str = "/v1";

/// The roles a daemon plays (ADR-0009). A solo workspace runs both in one process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRole {
    /// Holds the workspace's shared state: projects, tasks, events, recaps.
    Hub,
    /// Runs and watches agent sessions on its machine.
    Runner,
}

/// `GET /v1/host/info`: available before authentication, so version skew can be detected early.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// A person's desktop. It may take every action, including those only a person may take.
    Device,
    /// An agent or hook. It is limited to agent verbs, for its owner's workspace.
    Agent,
}

/// Machine-readable error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
pub struct Caller {
    /// The member the token belongs to: a person (device token) or an agent (agent token).
    pub member: MemberId,
    /// The token's scope.
    pub scope: TokenScope,
    /// For an agent, its owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
pub struct ApiError {
    /// Code.
    pub code: ErrorCode,
    /// A sentence for people.
    pub message: String,
}

/// `GET /v1/events`: a page of the activity log, oldest first within the page.
///
/// Page backwards by passing `from_rev` as `before`. Only `at_start` ends paging: with filters, a
/// page may hold fewer than `limit` events (even none) while older matches still exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsPage {
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
