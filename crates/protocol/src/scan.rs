//! The machine scan, as the API serves it: `POST /v1/machines/{id}/scan`
//! (`docs/build/contracts/api-v1.md`, "Machine scan").
//!
//! A scan is a read-only, bounded walk of a machine's agent homes (`pitcrew_ingest::scan`): it
//! counts the sessions it finds and suggests projects and workstreams from their folders and
//! branches. It never reads a whole transcript and never copies prompt text: only paths, branches,
//! times and counts.
//!
//! The answer is a stream of [`ScanFrame`]s, one JSON object per line: [`ScanFrame::Progress`]
//! while the walk runs, then exactly one [`ScanFrame::Done`] carrying the [`ScanReport`], or one
//! [`ScanFrame::Error`] if the scan failed once the answer had begun.
//!
//! Paths are the machine's own, as its CLIs wrote them (a scan of a Windows machine has `\`
//! separators). They are the person's: the route serves them to a device token only.

use crate::api::ErrorCode;
use crate::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};

/// One frame of a scan's answer: a line of JSON, tagged by `type`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScanFrame {
    /// How far the walk has got. The first frame, sent as the scan starts, has `scanned: 0` and
    /// no `total`; later ones come at most every 100 ms, and the last one before `done` has
    /// `scanned == total`.
    Progress(ScanProgress),
    /// The result: the last frame of a scan that finished.
    Done {
        /// Counts and suggestions.
        report: ScanReport,
    },
    /// The scan failed after its answer began (its status was already `200`): the last frame.
    Error {
        /// What kind of failure, as in an `ApiError`.
        code: ErrorCode,
        /// A sentence for people.
        message: String,
    },
}

/// One progress tick.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScanProgress {
    /// Transcripts looked at so far.
    pub scanned: usize,
    /// Total transcripts discovered, once discovery (a directory walk) has finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub total: Option<usize>,
    /// A path recently finished, for a "scanning …" line. Best-effort: a tick can land between
    /// transcripts and carry nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub path: Option<String>,
}

/// Sessions found for one engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EngineCount {
    /// The CLI.
    pub engine: Engine,
    /// Sessions found.
    pub count: usize,
}

/// Sessions found under one account home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HomeCount {
    /// The CLI.
    pub engine: Engine,
    /// The home folder (`~/.claude`, `~/.codex`, OpenCode's data folder, or one an environment
    /// variable names).
    pub home: String,
    /// Sessions found.
    pub count: usize,
}

/// Sessions found with one working directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FolderCount {
    /// The `cwd`.
    pub path: String,
    /// Sessions found.
    pub count: usize,
}

/// Sessions started in one calendar month, UTC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MonthCount {
    /// `YYYY-MM`.
    pub month: String,
    /// Sessions found.
    pub count: usize,
}

/// Counts from a scan. `by_engine`, `by_home`, `by_folder` and `by_month` cover ordinary sessions
/// only: a sub-agent session shares its parent's folder and month, so folding it in would double
/// those buckets without adding information. Sub-agent sessions are counted once, separately, in
/// `subagent_sessions`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScanCounts {
    /// Ordinary (non-sub-agent) sessions found.
    pub sessions: usize,
    /// Sub-agent sessions found, counted separately.
    pub subagent_sessions: usize,
    /// Per engine.
    pub by_engine: Vec<EngineCount>,
    /// Per account home.
    pub by_home: Vec<HomeCount>,
    /// Per folder, busiest first. Folders that are the same real place but spelled differently
    /// (a case-insensitive filesystem) are counted together, under one of their spellings.
    pub by_folder: Vec<FolderCount>,
    /// Per month, most recent first.
    pub by_month: Vec<MonthCount>,
    /// The earliest session start found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub first_activity: Option<TimestampMs>,
    /// The most recent activity found (a transcript's modification time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_activity: Option<TimestampMs>,
}

/// A suggested workstream inside a [`Suggestion`]'s project: either an active sub-folder
/// (`branch` absent, named after the folder) or a non-default branch (`branch` set, named after
/// it). A project can suggest both kinds, and a session can count toward one of each.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WorkstreamSuggestion {
    /// Stable within its project. For a sub-folder, the folder's own path (the project's `path`
    /// joined with `name`, with the machine's separator): also where it is. For a branch,
    /// `<project path>#<branch>`.
    pub id: String,
    /// Display name: the folder's name, or the branch.
    pub name: String,
    /// Set for a branch-based suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub branch: Option<String>,
    /// Sessions in it.
    pub session_count: usize,
    /// Of those, in the last 30 days.
    pub recent_30d: usize,
    /// Of those, in the last 90 days.
    pub recent_90d: usize,
}

/// A suggested project: a repository root (the nearest ancestor with a `.git`), or, for folders
/// with no `.git` above them, a folder shared by several of them. Never the person's home
/// directory, a scanned agent home, or a well-known system folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Suggestion {
    /// Stable across a re-scan: the root's path.
    pub id: String,
    /// Display name (the root folder's name).
    pub name: String,
    /// The root folder.
    pub path: String,
    /// Whether a `.git` was found at or above it (versus a grouped folder without one).
    pub is_git: bool,
    /// Sessions under it (any depth), excluding sub-agents.
    pub session_count: usize,
    /// Of those, in the last 30 days.
    pub recent_30d: usize,
    /// Of those, in the last 90 days.
    pub recent_90d: usize,
    /// Suggested workstreams inside it, ranked the same way as projects.
    pub workstreams: Vec<WorkstreamSuggestion>,
}

/// The result of a scan.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScanReport {
    /// Counts.
    pub counts: ScanCounts,
    /// Suggested projects, most recently active first.
    pub suggestions: Vec<Suggestion>,
    /// Folders or transcripts skipped because they could not be read (permission denied, a
    /// vanished file, a locked store). Not fatal: the rest of the scan still ran.
    pub unreadable: u64,
}
