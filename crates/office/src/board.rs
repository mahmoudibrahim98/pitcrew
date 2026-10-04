//! What a board draft sends its agent: a bounded, redacted summary of a workstream's sessions,
//! inside the versioned prompt ([`crate::prompts::DRAFT_BOARD`]), and the estimate shown first.
//!
//! **What goes in**, per session, most recently active first: its id (for the proposal's
//! evidence), CLI, state, the dates it ran, its branch and linked task, its title, its counts
//! (turns, tool runs, failures, file edits), up to [`MAX_FILES`] files it edited and up to
//! [`MAX_RECAP_LINES`] recap lines (the recap engine's one-line summaries of its blocks of work);
//! and the workstream's tasks already on the board. **Never** a transcript, a prompt, a tool's
//! output, or a secret: every text goes through [`crate::redact::line`], and the caller hands in
//! only those facts.
//!
//! **Bounds**: [`MAX_SESSIONS`] sessions, [`MAX_TASKS`] tasks (in at most [`MAX_TASKS_BYTES`]),
//! every text cut to its own length, and the whole summary at most [`MAX_SUMMARY_BYTES`]: the
//! least recently active sessions that do not fit are left out, and counted.
//!
//! **The estimate** ([`DraftCost::estimate`]): the agent reads the prompt (a token for every 4
//! bytes, rounded up) and its CLI's own instructions ([`CLI_OVERHEAD_TOKENS`], a fixed
//! allowance); its answer is at most the proposal's bound (`MAX_PROPOSAL_BYTES / 4` tokens).

use crate::prompts::DRAFT_BOARD;
use crate::redact::{self, Redacted};
use pitcrew_protocol::board::{
    DraftCost, MAX_PROPOSAL_BYTES, MAX_PROPOSED_TASKS, MAX_PROPOSED_TITLE, UsageEstimate,
};
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::{Engine, SessionState, TaskStatus, TimestampMs};
use std::fmt::Write as _;

/// The most sessions a summary covers.
pub const MAX_SESSIONS: usize = 40;
/// The most tasks a summary lists.
pub const MAX_TASKS: usize = 60;
/// The most bytes the task list takes.
pub const MAX_TASKS_BYTES: usize = 4 * 1024;
/// The longest summary, in bytes.
pub const MAX_SUMMARY_BYTES: usize = 12 * 1024;
/// The longest workstream or project name, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// The longest session or task title, in characters.
pub const MAX_TITLE_CHARS: usize = 120;
/// The most recap lines per session.
pub const MAX_RECAP_LINES: usize = 3;
/// The longest recap line, in characters.
pub const MAX_RECAP_CHARS: usize = 200;
/// The most files per session.
pub const MAX_FILES: usize = 5;
/// The longest file path or branch, in characters.
pub const MAX_PATH_CHARS: usize = 100;
/// What an agent CLI's own instructions and tools are allowed for in the estimate, in tokens.
pub const CLI_OVERHEAD_TOKENS: u32 = 15_000;

/// A workstream's facts for its draft. Built by the hub from what its routes would show the
/// person: sessions hidden by the import choice, and the drafting sessions, are left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftFacts {
    /// The workstream's name.
    pub workstream: String,
    /// Its project's name.
    pub project: String,
    /// The tasks on its board, in board order.
    pub tasks: Vec<TaskFacts>,
    /// Its sessions, in any order.
    pub sessions: Vec<SessionFacts>,
}

/// A task already on the board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskFacts {
    /// Its key, e.g. `PAP-4`.
    pub key: String,
    /// Where it stands.
    pub status: TaskStatus,
    /// Its title.
    pub title: String,
}

/// What a session did, as the hub knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFacts {
    /// Its id, which the proposal cites as evidence.
    pub id: SessionId,
    /// Its CLI.
    pub engine: Engine,
    /// Its title.
    pub title: Option<String>,
    /// Its state.
    pub state: SessionState,
    /// Its branch.
    pub branch: Option<String>,
    /// When it started.
    pub started: TimestampMs,
    /// When it was last active.
    pub last_activity: TimestampMs,
    /// The key of the task it is linked to.
    pub task: Option<String>,
    /// Turns it finished.
    pub turns: u32,
    /// Tools it ran.
    pub tools: u32,
    /// Of those, the ones that failed.
    pub tools_failed: u32,
    /// File edits it made.
    pub edits: u32,
    /// Files it edited, most edited first.
    pub files: Vec<String>,
    /// Recap lines of its blocks of work, newest first.
    pub recaps: Vec<String>,
}

/// A summary, and what it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoardSummary {
    /// The text the agent reads.
    pub text: String,
    /// Sessions it covers.
    pub sessions: u32,
    /// Sessions left out by the bounds.
    pub sessions_left_out: u32,
    /// Tasks it lists.
    pub tasks: u32,
    /// Things replaced by redaction.
    pub redacted: u32,
}

/// The prompt a draft starts its agent with, and its cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftPrompt {
    /// The whole prompt.
    pub text: String,
    /// The summary in it.
    pub summary: BoardSummary,
    /// Sizes and the estimate.
    pub cost: DraftCost,
}

/// Counts redactions as texts are cleaned.
#[derive(Default)]
struct Cleaner {
    redacted: u32,
}

impl Cleaner {
    /// One redacted line of at most `max` characters, its `<` and `>` as `‹` and `›`: no text
    /// from a session can close the prompt's `<summary>` and speak outside it.
    fn line(&mut self, text: &str, max: usize) -> String {
        let Redacted { text, count } = redact::line(text, max);
        self.redacted += count;
        text.replace('<', "‹").replace('>', "›")
    }
}

fn status_word(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Backlog => "backlog",
        TaskStatus::Todo => "todo",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Review => "review",
        TaskStatus::Done => "done",
        TaskStatus::Canceled => "canceled",
    }
}

fn state_word(state: SessionState) -> &'static str {
    match state {
        SessionState::Starting => "starting",
        SessionState::Working => "working",
        SessionState::Waiting => "waiting",
        SessionState::Idle => "idle",
        SessionState::Ended => "ended",
        SessionState::Unreachable => "unreachable",
    }
}

fn engine_word(engine: Engine) -> &'static str {
    match engine {
        Engine::Claude => "Claude Code",
        Engine::Codex => "Codex",
        Engine::OpenCode => "OpenCode",
        _ => "an agent CLI",
    }
}

fn day(at: TimestampMs) -> String {
    pitcrew_recap::date_of(at, 0).0
}

/// One session's lines.
fn session_block(session: &SessionFacts, clean: &mut Cleaner) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Session {}", session.id.0);
    let (from, to) = (day(session.started), day(session.last_activity));
    let when = if from == to {
        from
    } else {
        format!("{from} to {to}")
    };
    let mut facts = format!(
        "  {}, {}, {when}",
        engine_word(session.engine),
        state_word(session.state)
    );
    if let Some(branch) = &session.branch {
        let _ = write!(facts, ", branch {}", clean.line(branch, MAX_PATH_CHARS));
    }
    if let Some(task) = &session.task {
        let _ = write!(facts, ", task {}", clean.line(task, MAX_NAME_CHARS));
    }
    let _ = writeln!(out, "{facts}");
    if let Some(title) = session.title.as_deref().filter(|t| !t.trim().is_empty()) {
        let _ = writeln!(out, "  Title: {}", clean.line(title, MAX_TITLE_CHARS));
    }
    let _ = writeln!(
        out,
        "  Work: {} turns, {} tool runs ({} failed), {} file edits",
        session.turns, session.tools, session.tools_failed, session.edits
    );
    let files: Vec<String> = session
        .files
        .iter()
        .take(MAX_FILES)
        .map(|f| clean.line(f, MAX_PATH_CHARS))
        .filter(|f| !f.is_empty())
        .collect();
    if !files.is_empty() {
        let more = session.files.len().saturating_sub(MAX_FILES);
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        let _ = writeln!(out, "  Files: {}{more}", files.join(", "));
    }
    let recaps: Vec<String> = session
        .recaps
        .iter()
        .take(MAX_RECAP_LINES)
        .map(|r| clean.line(r, MAX_RECAP_CHARS))
        .filter(|r| !r.is_empty())
        .collect();
    if !recaps.is_empty() {
        let _ = writeln!(out, "  Recent work:");
        for recap in recaps {
            let _ = writeln!(out, "  - {recap}");
        }
    }
    out
}

/// The summary of `facts`: see the [module docs](self) for what goes in and the bounds.
#[must_use]
pub fn summarize(facts: &DraftFacts) -> BoardSummary {
    let mut clean = Cleaner::default();
    let mut tasks_text = String::new();
    let mut listed = 0usize;
    for task in facts.tasks.iter().take(MAX_TASKS) {
        let line = format!(
            "- {} [{}] {}\n",
            clean.line(&task.key, MAX_NAME_CHARS),
            status_word(task.status),
            clean.line(&task.title, MAX_TITLE_CHARS)
        );
        if tasks_text.len() + line.len() > MAX_TASKS_BYTES {
            break;
        }
        tasks_text.push_str(&line);
        listed += 1;
    }
    let unlisted = facts.tasks.len() - listed;
    if listed == 0 {
        tasks_text.push_str("(none)\n");
    }
    if unlisted > 0 {
        let _ = writeln!(tasks_text, "(and {unlisted} more not listed)");
    }

    let mut sessions: Vec<&SessionFacts> = facts.sessions.iter().collect();
    sessions.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| b.id.cmp(&a.id))
    });
    let total = sessions.len();
    let mut blocks = Vec::new();
    // Room for the header (with the largest counts) and the closing line.
    let mut used = tasks_text.len() + 400;
    for session in sessions.iter().take(MAX_SESSIONS) {
        let block = session_block(session, &mut clean);
        if used + block.len() + 1 > MAX_SUMMARY_BYTES {
            break;
        }
        used += block.len() + 1;
        blocks.push(block);
    }
    let covered = blocks.len();
    let left_out = total - covered;

    let mut text = String::new();
    let _ = writeln!(
        text,
        "Sessions: {covered} of {total}, most recently active first{}.",
        if left_out > 0 {
            format!("; the {left_out} least recently active are left out")
        } else {
            String::new()
        }
    );
    let _ = writeln!(
        text,
        "\nTasks already on the board ({listed} of {}):",
        facts.tasks.len()
    );
    text.push_str(&tasks_text);
    if blocks.is_empty() {
        text.push_str("\n(No sessions are linked to this workstream yet.)\n");
    }
    for block in blocks {
        text.push('\n');
        text.push_str(&block);
    }
    let text = text.trim_end().to_owned();
    BoardSummary {
        text,
        sessions: u32::try_from(covered).unwrap_or(u32::MAX),
        sessions_left_out: u32::try_from(left_out).unwrap_or(u32::MAX),
        tasks: u32::try_from(listed).unwrap_or(u32::MAX),
        redacted: clean.redacted,
    }
}

/// The prompt for `facts`' draft `draft` (its id as shown, `drf_…`, which the agent's submit
/// names), its summary and its cost.
#[must_use]
pub fn draft_prompt(facts: &DraftFacts, draft: &str) -> DraftPrompt {
    let summary = summarize(facts);
    let mut clean = Cleaner::default();
    let workstream = clean.line(&facts.workstream, MAX_NAME_CHARS);
    let project = clean.line(&facts.project, MAX_NAME_CHARS);
    let (max_tasks, max_title) = (
        MAX_PROPOSED_TASKS.to_string(),
        MAX_PROPOSED_TITLE.to_string(),
    );
    let text = DRAFT_BOARD.render(&[
        ("workstream", workstream.as_str()),
        ("project", project.as_str()),
        ("draft", draft),
        ("max_tasks", max_tasks.as_str()),
        ("max_title", max_title.as_str()),
        ("summary", summary.text.as_str()),
    ]);
    let prompt_bytes = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let cost = DraftCost {
        sessions: summary.sessions,
        sessions_left_out: summary.sessions_left_out,
        tasks: summary.tasks,
        summary_bytes: u32::try_from(summary.text.len()).unwrap_or(u32::MAX),
        prompt_bytes,
        redacted: summary.redacted + clean.redacted,
        estimate: UsageEstimate {
            input_tokens: CLI_OVERHEAD_TOKENS.saturating_add(prompt_bytes.div_ceil(4)),
            output_tokens: u32::try_from(MAX_PROPOSAL_BYTES / 4).unwrap_or(u32::MAX),
        },
    };
    DraftPrompt {
        text,
        summary,
        cost,
    }
}

/// The longest prompt [`draft_prompt`] can make, in bytes: the template, the longest names and the
/// longest summary. It goes on the agent CLI's command line, which every platform allows.
pub const MAX_PROMPT_BYTES: usize = DRAFT_BOARD.template.len()
    + MAX_SUMMARY_BYTES
    + 2 * MAX_NAME_CHARS * 4
    + 64;
