//! Identifiers.
//!
//! Internal ids are [ULIDs](https://github.com/ulid/spec): globally unique, sortable by creation
//! time, and safe to create on any machine without coordination. That matters when runners on
//! several machines, and later several people, write to one log. On the wire an id is the bare
//! 26-character ULID. For people and logs it is shown with a short prefix (`tsk_01JB…`), and both
//! forms parse.
//!
//! People-facing keys are separate: [`ProjectKey`] (`CMP`) and [`TaskKey`] (`CMP-104`).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use ulid::Ulid;

/// Errors from parsing ids and keys.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The value is not a valid id or key of the named kind.
    #[error("invalid {kind}: {value:?}")]
    Invalid {
        /// The kind of id that was expected, e.g. `TaskId`.
        kind: &'static str,
        /// The rejected input.
        value: String,
    },
}

macro_rules! ulid_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        #[serde(transparent)]
        pub struct $name(pub Ulid);

        impl $name {
            /// The prefix used when this id is shown to people or logged.
            pub const PREFIX: &'static str = $prefix;

            /// Creates a new, time-ordered id.
            #[must_use]
            pub fn new() -> Self {
                Self(Ulid::generate())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}_{}", $prefix, self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            /// Accepts both `tsk_01JB…` and the bare ULID.
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let raw = s
                    .strip_prefix($prefix)
                    .and_then(|rest| rest.strip_prefix('_'))
                    .unwrap_or(s);
                Ulid::from_string(raw).map(Self).map_err(|_| IdError::Invalid {
                    kind: stringify!($name),
                    value: s.to_owned(),
                })
            }
        }
    };
}

ulid_id!(
    /// A workspace: one context such as "PhD research", hosted by one hub.
    WorkspaceId, "wsp"
);
ulid_id!(
    /// A machine with a runner: this PC, a WSL distro, a server, an HPC login node.
    MachineId, "mch"
);
ulid_id!(
    /// A member of a workspace: a person or an agent.
    MemberId, "mem"
);
ulid_id!(
    /// A persona: a reusable recipe for new agents.
    PersonaId, "per"
);
ulid_id!(
    /// A team of members with a lead.
    TeamId, "team"
);
ulid_id!(
    /// A project: a deliverable, such as a paper, a thesis part or a product.
    ProjectId, "prj"
);
ulid_id!(
    /// A workstream: one line of work inside a project.
    WorkstreamId, "wst"
);
ulid_id!(
    /// A task: something a person or an agent finishes.
    TaskId, "tsk"
);
ulid_id!(
    /// A subtask: one checklist line on a task.
    SubtaskId, "sub"
);
ulid_id!(
    /// A session: one CLI conversation (Claude Code, Codex, OpenCode) on one machine.
    SessionId, "ses"
);
ulid_id!(
    /// A dispatch: one attempt at a task by one agent.
    DispatchId, "dsp"
);
ulid_id!(
    /// An ask: something that needs a specific member's answer.
    AskId, "ask"
);
ulid_id!(
    /// An event in the append-only log.
    EventId, "evt"
);
ulid_id!(
    /// A command sent from a hub to a runner. It doubles as the idempotency key.
    CommandId, "cmd"
);
ulid_id!(
    /// A terminal owned by a runner's runtime (a tmux window or a PTY).
    TerminalId, "term"
);

/// A short project key used in task keys, such as `CMP` in `CMP-104`.
///
/// Rules: 2–10 characters, the first an uppercase ASCII letter, the rest uppercase letters or
/// digits.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct ProjectKey(String);

impl ProjectKey {
    /// Validates and wraps a project key.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        let mut chars = value.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_uppercase());
        let rest_ok = chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let len_ok = (2..=10).contains(&value.len());
        if first_ok && rest_ok && len_ok {
            Ok(Self(value))
        } else {
            Err(IdError::Invalid {
                kind: "ProjectKey",
                value,
            })
        }
    }

    /// The key as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProjectKey {
    type Error = IdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProjectKey> for String {
    fn from(key: ProjectKey) -> Self {
        key.0
    }
}

impl fmt::Display for ProjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A people-facing task key, such as `CMP-104`. The number is unique within its project.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct TaskKey {
    /// The project's key.
    pub project: ProjectKey,
    /// The task's number within the project; starts at 1.
    pub number: u32,
}

impl TaskKey {
    /// Builds a key from its parts. `number` must be at least 1.
    pub fn new(project: ProjectKey, number: u32) -> Result<Self, IdError> {
        if number == 0 {
            return Err(IdError::Invalid {
                kind: "TaskKey",
                value: format!("{project}-0"),
            });
        }
        Ok(Self { project, number })
    }
}

impl FromStr for TaskKey {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || IdError::Invalid {
            kind: "TaskKey",
            value: s.to_owned(),
        };
        let (project, number) = s.rsplit_once('-').ok_or_else(invalid)?;
        let project = ProjectKey::new(project).map_err(|_| invalid())?;
        let number: u32 = number.parse().map_err(|_| invalid())?;
        Self::new(project, number).map_err(|_| invalid())
    }
}

impl TryFrom<String> for TaskKey {
    type Error = IdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<TaskKey> for String {
    fn from(key: TaskKey) -> Self {
        key.to_string()
    }
}

impl fmt::Display for TaskKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.project, self.number)
    }
}
