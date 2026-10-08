//! # pitcrew-office
//!
//! The back office: small, deterministic rules that act on evidence in the event log, with caps,
//! a run log, and a hard "never" list. Model calls come later, behind recap's `Summarizer`.
//!
//! **Owned by stream F.** The work packages are in `docs/build/streams/F.md`.
//!
//! ## How it works
//!
//! An [`Office`] reads events in log order ([`Office::on_event`]). It first learns from each one
//! ([`World`]: members, tasks, dispatches, sessions, open asks, workstreams), then runs its
//! [`Rule`]s. A rule returns [`Action`]s: append an event, raise an ask, or propose a brief.
//! Every action is checked against the "never" list and the move rules, then against the caps
//! (per rule and over all rules, per hour), and logged as an [`Entry`] with its [`Outcome`]:
//! emitted, capped or refused. The caller applies the emitted ones through its [`Commands`]
//! ([`apply`]); the office never writes the store itself and never calls anything outside.
//!
//! Time is the office's clock, the latest event time seen, never the wall clock. Given the same
//! events, the office gives the same entries, and an event it has seen (by revision) is ignored,
//! so a replay never acts twice. Its state saves row by row and restores exactly.
//!
//! [`RunLog`] is the run log as a store projection (`office.runs`): it replays the rules inside
//! each append, so `office_runs` rebuilds identically from the log.
//!
//! ## The first rules
//!
//! - [`DispatchToReview`]: a dispatch finished successfully → move its task to review, as
//!   `Mover::BackOffice`, when `can_move` allows it.
//! - [`JobDiverged`] and [`TestsFailing`]: a job diverged, or tests kept failing → a decision ask
//!   to the person who owns the work, with receipts.
//! - [`RemindStaleAsks`]: an ask open too long → one reminder mention to its addressee.
//! - [`QuietWorkstream`]: an active workstream with no activity for days → a "paused?" brief
//!   proposal.
//!
//! ## Never
//!
//! Checked in code for every action, whatever a rule or an event says: never send anything
//! outward (no approval asks, only internal events), never mark a task done unless it allows
//! automatic acceptance, never answer an ask addressed to a person.
//!
//! ## Prompts and what they send
//!
//! [`prompts`] holds the versioned prompt templates (`prompts/<name>/v<n>.md`); [`board`] builds
//! the bounded, redacted summary a board draft sends its agent, and its cost; [`orchestrator`]
//! builds the Orchestrator's prompt and finds what its answers cite and suggest; [`redact`] is
//! what never leaves in a prompt.

#![forbid(unsafe_code)]

mod action;
pub mod board;
mod caps;
mod commands;
mod guard;
mod office;
pub mod orchestrator;
pub mod prompts;
pub mod redact;
mod rule;
mod rules;
mod runlog;
mod state;
mod world;

pub use action::{Action, AskDraft, CapScope, Entry, Outcome, Refusal};
pub use commands::{ApplyError, Commands, apply};
pub use office::{Config, MAX_ACTIONS_PER_EVENT, Office, RestoreError};
pub use rule::{Context, Memo, Rule};
pub use rules::{
    DispatchToReview, JobDiverged, QuietWorkstream, RemindStaleAsks, TestsFailing, default_rules,
};
pub use runlog::{RUN_LOG, ReadError, RunLog, read_runs};
pub use state::StateRow;
pub use world::{
    AskInfo, AskPos, DispatchInfo, MemberInfo, QuietPos, SessionInfo, TaskInfo, WorkstreamInfo,
    World,
};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
