//! # pitcrew-fixtures
//!
//! Shared, synthetic test data. Every stream builds and tests against the same demo workspace, so
//! the UI, the mock hub and the Rust crates agree on what "a workspace with some work in it" looks
//! like.
//!
//! - [`demo_workspace`]: a small lab with two projects, four workstreams, ten tasks, six sessions
//!   on three machines, open asks and briefs, and a slice of the event log.
//! - [`demo_recaps`]: the recaps of that slice, as the recap engine writes them with its rules.
//!   The mock hub serves its recap routes from them.
//! - [`data_dir`]: the folder holding the JSON and the sample transcripts (`transcripts/claude`,
//!   `transcripts/codex`, `transcripts/opencode`).
//!
//! **Everything here is made up.** Never add real transcripts, host names, paths or people. Real
//! samples for local testing go in `data/private/`, which is ignored by git.
//!
//! Owned by stream 0. Other streams may add files under `data/` through a contract change
//! (`s/0/contract-…`).

#![forbid(unsafe_code)]

use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::ProjectId;
use pitcrew_protocol::model::{
    Ask, Brief, Dispatch, Machine, Member, Persona, Project, Session, Task, Team, Workspace,
    Workstream,
};
use pitcrew_protocol::recap::{DayRecap, RecapBlock};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The demo workspace as JSON, embedded at build time.
pub const DEMO_WORKSPACE_JSON: &str = include_str!("../data/demo-workspace.json");

/// The demo workspace's recaps as JSON, embedded at build time.
pub const DEMO_RECAPS_JSON: &str = include_str!("../data/demo-recaps.json");

/// The folder holding the fixture files.
#[must_use]
pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}

/// A whole workspace: every list is what the hub's projections would return, and `events` is a
/// recent slice of the log (not the full history).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoWorkspace {
    /// The workspace.
    pub workspace: Workspace,
    /// Machines.
    pub machines: Vec<Machine>,
    /// People and agents.
    pub members: Vec<Member>,
    /// Personas.
    #[serde(default)]
    pub personas: Vec<Persona>,
    /// Teams.
    #[serde(default)]
    pub teams: Vec<Team>,
    /// Projects.
    pub projects: Vec<Project>,
    /// Workstreams.
    pub workstreams: Vec<Workstream>,
    /// Tasks, with their subtasks.
    pub tasks: Vec<Task>,
    /// Sessions.
    pub sessions: Vec<Session>,
    /// Dispatches.
    #[serde(default)]
    pub dispatches: Vec<Dispatch>,
    /// Asks, open and answered.
    pub asks: Vec<Ask>,
    /// "Where it stands" briefs in force.
    pub briefs: Vec<Brief>,
    /// A recent slice of the event log, oldest first.
    pub events: Vec<Event>,
}

/// Parses the demo workspace.
///
/// # Errors
///
/// Returns an error if the embedded JSON does not match the protocol types, which the tests in
/// this crate rule out.
pub fn demo_workspace() -> Result<DemoWorkspace, serde_json::Error> {
    serde_json::from_str(DEMO_WORKSPACE_JSON)
}

/// The recaps of the demo workspace's events, as the recap engine (`pitcrew-recap`) writes them
/// with its default settings and its rules (`RuleSummarizer`), its directory seeded from the
/// demo's lists. `tests/recaps.rs` regenerates and checks it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoRecaps {
    /// Where days begin, in minutes east of UTC (`tz` on `GET /v1/recaps/days`). Always 0.
    pub tz: i32,
    /// Every block with its line, in the engine's order: by start, then by id.
    pub blocks: Vec<RecapBlock>,
    /// Every project's day paragraphs, projects in id order.
    pub projects: Vec<ProjectDays>,
}

/// One project's day paragraphs: what `GET /v1/recaps/days?project=` pages through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDays {
    /// The project.
    pub project: ProjectId,
    /// A paragraph per workstream per day, plus one per day for the project's work outside any
    /// workstream, from the blocks whose `project` is this one. In the engine's order: by date;
    /// within a date, the one without a workstream first, then by workstream id.
    pub days: Vec<DayRecap>,
}

/// Parses the demo workspace's recaps.
///
/// # Errors
///
/// Returns an error if the embedded JSON does not match the protocol types, which the tests in
/// this crate rule out.
pub fn demo_recaps() -> Result<DemoRecaps, serde_json::Error> {
    serde_json::from_str(DEMO_RECAPS_JSON)
}
