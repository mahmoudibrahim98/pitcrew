//! "Where it stands": proposed briefs for workstreams and projects, from recent blocks.
//!
//! [`standing`] reads a workstream's recent blocks into what is true now: its state, where each
//! task is, how the checks stand, what is waiting on whom, and the most urgent next step. Every
//! point is a [`Clause`] carrying the receipts of the facts behind it. [`propose_workstream`]
//! writes a standing as a [`BriefProposal`] through a [`Summarizer`], and [`propose_project`]
//! rolls a project's standings up. [`propose_paused`] is the back office's "paused?" question
//! for a workstream that went quiet.
//!
//! Pinned briefs only ever get proposals ([`Disposition::Propose`]). An unpinned one may be
//! applied automatically ([`Disposition::AutoAccept`]) when the workspace's [`BriefPolicy`]
//! allows it. Either way the caller appends [`BriefProposal::body`]; this module writes nothing.

use crate::block::{Block, Fact, FactKind};
use crate::checks::Check;
use crate::directory::Directory;
use crate::draft::{plural, plural64};
use crate::summary::{
    Clause, Draft, DraftKind, RuleSummarizer, Sentence, Summarizer, Summary, SummaryError, verify,
};
use crate::text::{push_first, push_latest};
use crate::time::date_of;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{AskId, EventId, MemberId, ProjectId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    AskKind, Brief, BriefTarget, DispatchOutcome, Health, Receipt, TaskStatus, TimestampMs,
    WorkstreamStatus,
};
use serde::{Deserialize, Serialize};

/// Most tasks described one by one; the rest are counted.
const TASK_POINTS: usize = 5;
/// Most open asks listed as waiting.
const WAITING_POINTS: usize = 3;
/// Most clauses per workstream in a project roll-up.
const ROLLUP_POINTS: usize = 3;
/// Most receipts on one clause.
const CLAUSE_RECEIPTS: usize = 8;
/// Most receipts on a proposal.
const BRIEF_RECEIPTS: usize = 32;
/// Most tasks and asks followed per standing. Blocks are capped already; this bounds the rest.
const MAX_TRACKED: usize = 64;
const DAY_MS: i64 = 86_400_000;

/// What the workspace allows for briefs the back office proposes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefPolicy {
    /// Whether a proposal for an unpinned brief may be applied without a person. Off by default.
    pub auto_accept_unpinned: bool,
}

/// What the caller should do with a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Append it as a proposal for a person to accept. Always the case for a pinned brief.
    Propose,
    /// The brief is unpinned and the policy allows it: append the proposal, then accept it
    /// ([`BriefProposal::accepted_body`]).
    AutoAccept,
}

/// How pressing a next step is; the most pressing comes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    /// A person must decide or approve.
    Decide,
    /// A person must answer a question.
    Answer,
    /// Checks are failing, or a job diverged.
    Fix,
    /// Finished work waits for review.
    Review,
    /// A dispatch could not finish.
    Retry,
    /// Work in progress has steps left.
    Finish,
}

/// The next step of a standing, and how pressing it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextStep {
    /// How pressing it is.
    pub urgency: Urgency,
    /// What to do, with the receipts that make it the next step.
    pub clause: Clause,
}

/// Where one workstream stands, read from its recent blocks. Each point is a clause with
/// receipts; the prose comes from [`draft_brief`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    /// The workstream.
    pub workstream: WorkstreamId,
    /// Its name, cleaned, or "this workstream".
    pub name: String,
    /// Its project, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// Status and health, if the blocks changed them, e.g. "Seed runs is at risk".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Clause>,
    /// Where tasks are, e.g. "PAP-1 is in progress (2 of 4 steps done)".
    #[serde(default)]
    pub tasks: Vec<Clause>,
    /// Checks, jobs and decisions, e.g. "tests are failing".
    #[serde(default)]
    pub signals: Vec<Clause>,
    /// Open asks, e.g. "waiting on @sam to decide …".
    #[serde(default)]
    pub waiting: Vec<Clause>,
    /// The most pressing next step, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<NextStep>,
    /// The blocks read, in order.
    #[serde(default)]
    pub blocks: Vec<EventId>,
}

impl Standing {
    /// Whether there is nothing to say.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.tasks.is_empty()
            && self.signals.is_empty()
            && self.waiting.is_empty()
    }
}

/// A proposed "Where it stands", ready to append.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefProposal {
    /// Which brief.
    pub target: BriefTarget,
    /// Where it stands, without the next step. Every clause is a span with receipts.
    pub summary: Summary,
    /// The next step, if any, as one span with receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Summary>,
    /// Every receipt the text cites, without repeats (capped).
    pub receipts: Vec<Receipt>,
    /// Propose it, or apply it too.
    pub disposition: Disposition,
}

impl BriefProposal {
    fn new(
        target: BriefTarget,
        summary: Summary,
        next: Option<Summary>,
        disposition: Disposition,
    ) -> Self {
        let mut receipts = Vec::new();
        push_first(&mut receipts, summary.receipts(), BRIEF_RECEIPTS);
        if let Some(n) = &next {
            push_first(&mut receipts, n.receipts(), BRIEF_RECEIPTS);
        }
        Self {
            target,
            summary,
            next,
            receipts,
            disposition,
        }
    }

    /// The full text: the summary, then "Next: …". The protocol's `BriefProposed` has no `next`
    /// field yet, so the next step travels in the text.
    #[must_use]
    pub fn text(&self) -> String {
        let summary = self.summary.text.as_str();
        match self.next.as_ref().map(|n| n.text.as_str()) {
            Some(next) if !next.is_empty() => {
                let next = end_sentence(&format!("Next: {next}"));
                if summary.is_empty() {
                    next
                } else {
                    format!("{summary} {next}")
                }
            }
            _ => summary.to_owned(),
        }
    }

    /// The `BriefProposed` event body. The caller appends it.
    #[must_use]
    pub fn body(&self) -> EventBody {
        EventBody::BriefProposed {
            target: self.target,
            text: self.text(),
            receipts: self.receipts.clone(),
        }
    }

    /// For [`Disposition::AutoAccept`], the `BriefAccepted` body that applies the proposal
    /// (unpinned). `None` when a person must accept it.
    #[must_use]
    pub fn accepted_body(&self) -> Option<EventBody> {
        (self.disposition == Disposition::AutoAccept).then(|| EventBody::BriefAccepted {
            target: self.target,
            text: self.text(),
            pinned: false,
        })
    }
}

/// Adds a full stop unless the text already ends a sentence.
fn end_sentence(s: &str) -> String {
    if s.ends_with(['.', '?', '!']) {
        s.to_owned()
    } else {
        format!("{s}.")
    }
}

/// Pinned briefs only get proposals; unpinned ones follow the policy.
fn disposition(current: Option<&Brief>, policy: BriefPolicy) -> Disposition {
    let pinned = current.is_some_and(|b| b.pinned);
    if !pinned && policy.auto_accept_unpinned {
        Disposition::AutoAccept
    } else {
        Disposition::Propose
    }
}

// ─── Reading the blocks ──────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct TaskView {
    status: Option<(TaskStatus, Vec<Receipt>)>,
    plan: Option<(u32, u32, Vec<Receipt>)>,
    started: Option<(MemberId, Vec<Receipt>)>,
    finished: Option<(DispatchOutcome, Vec<Receipt>)>,
}

struct OpenAsk {
    ask: AskId,
    kind: AskKind,
    to: MemberId,
    title: String,
    receipts: Vec<Receipt>,
}

struct CheckView {
    failures: u32,
    last_failed: bool,
    receipts: Vec<Receipt>,
}

/// What the facts of a workstream's blocks add up to, latest first where it matters.
#[derive(Default)]
struct Collected {
    state: Option<(WorkstreamStatus, Health, Vec<Receipt>)>,
    tasks: Vec<(TaskId, TaskView)>,
    asks: Vec<OpenAsk>,
    checks: [Option<CheckView>; 3],
    diverged: Option<(Vec<String>, Vec<Receipt>)>,
    decision: Option<(String, Vec<Receipt>)>,
}

fn check_index(check: Check) -> usize {
    match check {
        Check::Tests => 0,
        Check::Lint => 1,
        Check::Build => 2,
    }
}

impl Collected {
    fn task(&mut self, task: TaskId) -> Option<&mut TaskView> {
        let i = match self.tasks.iter().position(|(t, _)| *t == task) {
            Some(i) => i,
            None if self.tasks.len() < MAX_TRACKED => {
                self.tasks.push((task, TaskView::default()));
                self.tasks.len() - 1
            }
            None => return None,
        };
        self.tasks.get_mut(i).map(|(_, v)| v)
    }

    fn read(&mut self, workstream: WorkstreamId, fact: &Fact) {
        let r = || fact.receipts.clone();
        match &fact.kind {
            FactKind::WorkstreamChanged {
                workstream: w,
                status,
                health,
            } if *w == workstream => self.state = Some((*status, *health, r())),
            FactKind::TaskMoved { task, to, .. } => {
                if let Some(v) = self.task(*task) {
                    v.status = Some((*to, r()));
                }
            }
            FactKind::PlanUpdated { task, done, total } => {
                if let Some(v) = self.task(*task) {
                    v.plan = Some((*done, *total, r()));
                }
            }
            FactKind::DispatchStarted { task, agent } => {
                if let Some(v) = self.task(*task) {
                    v.started = Some((*agent, r()));
                    v.finished = None;
                }
            }
            FactKind::DispatchFinished {
                task: Some(task),
                outcome,
                ..
            } => {
                if let Some(v) = self.task(*task) {
                    v.finished = Some((*outcome, r()));
                }
            }
            FactKind::Checks {
                check,
                failures,
                last_failed,
                ..
            } => {
                let slot = &mut self.checks[check_index(*check)];
                let view = slot.get_or_insert_with(|| CheckView {
                    failures: 0,
                    last_failed: false,
                    receipts: Vec::new(),
                });
                view.failures = view.failures.saturating_add(*failures);
                view.last_failed = *last_failed;
                push_latest(&mut view.receipts, &fact.receipts, CLAUSE_RECEIPTS);
            }
            FactKind::JobDiverged { jobs } => {
                let (known, receipts) = self
                    .diverged
                    .get_or_insert_with(|| (Vec::new(), Vec::new()));
                for job in jobs {
                    if known.len() < 4 && !known.contains(job) {
                        known.push(job.clone());
                    }
                }
                push_latest(receipts, &fact.receipts, CLAUSE_RECEIPTS);
            }
            FactKind::AskRaised {
                ask,
                ask_kind,
                to,
                title,
            } if self.asks.len() < MAX_TRACKED && !self.asks.iter().any(|a| a.ask == *ask) => {
                self.asks.push(OpenAsk {
                    ask: *ask,
                    kind: *ask_kind,
                    to: *to,
                    title: title.clone(),
                    receipts: r(),
                });
            }
            FactKind::AskAnswered { ask } => self.asks.retain(|a| a.ask != *ask),
            FactKind::DecisionRecorded { text } => self.decision = Some((text.clone(), r())),
            _ => {}
        }
    }
}

/// Names for prose, with plain fallbacks.
fn task_key(dir: &Directory, t: TaskId) -> &str {
    dir.task_key(t).unwrap_or("a task")
}

fn handle(dir: &Directory, m: MemberId) -> &str {
    dir.handle(m).unwrap_or("someone")
}

fn clause(text: String, receipts: Vec<Receipt>) -> Clause {
    Clause { text, receipts }
}

/// Receipts of several facts, the first few of each.
fn joined<'a>(lists: impl IntoIterator<Item = &'a Vec<Receipt>>) -> Vec<Receipt> {
    let mut out = Vec::new();
    for list in lists {
        push_first(&mut out, list.iter().take(3), CLAUSE_RECEIPTS);
    }
    out
}

/// Where a workstream stands, from its recent blocks. Blocks of other workstreams are ignored.
/// The caller picks how far back to look (e.g. the last few days).
#[must_use]
pub fn standing(workstream: WorkstreamId, blocks: &[&Block], dir: &Directory) -> Standing {
    let mut mine: Vec<&Block> = blocks
        .iter()
        .copied()
        .filter(|b| b.workstream == Some(workstream))
        .collect();
    mine.sort_by_key(|b| (b.start, b.id));
    let mut c = Collected::default();
    for block in &mine {
        for fact in &block.facts {
            c.read(workstream, fact);
        }
    }
    let name = dir
        .workstream_name(workstream)
        .unwrap_or("this workstream")
        .to_owned();
    let project = mine.iter().find_map(|b| b.project);

    let state = c.state.as_ref().map(|(status, health, receipts)| {
        let word = match status {
            WorkstreamStatus::Active => match health {
                Health::OnTrack => "on track",
                Health::AtRisk => "at risk",
                Health::Blocked => "blocked",
            },
            WorkstreamStatus::Idea => "an idea",
            WorkstreamStatus::Paused => "paused",
            WorkstreamStatus::Shipped => "shipped",
            WorkstreamStatus::Dropped => "dropped",
        };
        clause(format!("{name} is {word}"), receipts.clone())
    });

    let mut tasks = task_points(&c, dir);
    let signals = signal_points(&c);
    let waiting = waiting_points(&c, dir);
    let next = next_step(&c, dir);

    // Something always happened in a block; say so when no rule has more to say.
    if state.is_none()
        && tasks.is_empty()
        && signals.is_empty()
        && waiting.is_empty()
        && let Some(activity) = activity_point(&mine)
    {
        tasks.push(activity);
    }

    Standing {
        workstream,
        name,
        project,
        state,
        tasks,
        signals,
        waiting,
        next,
        blocks: mine.iter().map(|b| b.id).collect(),
    }
}

fn task_points(c: &Collected, dir: &Directory) -> Vec<Clause> {
    let mut out = Vec::new();
    for (task, v) in &c.tasks {
        let key = task_key(dir, *task);
        let plan = v
            .plan
            .as_ref()
            .filter(|(_, total, _)| *total > 0)
            .map(|(done, total, _)| format!(" ({done} of {total} steps done)"))
            .unwrap_or_default();
        let main = match (&v.status, &v.started) {
            (Some((status, _)), _) => Some(match status {
                TaskStatus::Done => format!("{key} is done"),
                TaskStatus::Review => format!("{key} is in review"),
                TaskStatus::InProgress => format!("{key} is in progress{plan}"),
                TaskStatus::Todo => format!("{key} is in todo"),
                TaskStatus::Backlog => format!("{key} is in the backlog"),
                TaskStatus::Canceled => format!("{key} was canceled"),
            }),
            (None, Some((agent, _))) => {
                Some(format!("{} is working on {key}{plan}", handle(dir, *agent)))
            }
            (None, None) if !plan.is_empty() => Some(format!("{key} has{plan}")),
            (None, None) => None,
        };
        let receipts = |extra: Option<&Vec<Receipt>>| {
            joined(
                [
                    v.status.as_ref().map(|s| &s.1),
                    v.plan.as_ref().map(|p| &p.2),
                    v.started.as_ref().map(|s| &s.1),
                    extra,
                ]
                .into_iter()
                .flatten(),
            )
        };
        match (&v.finished, main) {
            (Some((DispatchOutcome::Succeeded, r)), Some(text)) => {
                out.push(clause(text, receipts(Some(r))));
            }
            (Some((DispatchOutcome::Succeeded, r)), None) => {
                out.push(clause(format!("the dispatch on {key} finished"), r.clone()));
            }
            (Some((outcome, r)), main) => {
                if let Some(text) = main {
                    out.push(clause(text, receipts(None)));
                }
                let text = match outcome {
                    DispatchOutcome::Canceled => format!("the dispatch on {key} was stopped"),
                    _ => format!("the dispatch on {key} could not finish"),
                };
                out.push(clause(text, r.clone()));
            }
            (None, Some(text)) => out.push(clause(text, receipts(None))),
            (None, None) => {}
        }
    }
    out.retain(|c| !c.receipts.is_empty());
    if out.len() > TASK_POINTS {
        let rest = out.split_off(TASK_POINTS - 1);
        out.push(clause(
            plural(rest.len(), "more task changed", "more tasks changed"),
            joined(rest.iter().map(|c| &c.receipts)),
        ));
    }
    out
}

fn signal_points(c: &Collected) -> Vec<Clause> {
    let mut out = Vec::new();
    for (check, view) in [Check::Tests, Check::Lint, Check::Build]
        .into_iter()
        .zip(&c.checks)
    {
        let Some(view) = view else { continue };
        let (fail, again, pass) = match check {
            Check::Tests => ("tests are failing", "tests pass again", "tests pass"),
            Check::Lint => ("lint is failing", "lint passes again", "lint passes"),
            Check::Build => (
                "the build is failing",
                "the build passes again",
                "the build passes",
            ),
        };
        let text = if view.last_failed {
            fail
        } else if view.failures > 0 {
            again
        } else {
            pass
        };
        out.push(clause(text.to_owned(), view.receipts.clone()));
    }
    if let Some((jobs, receipts)) = &c.diverged {
        let text = match jobs.as_slice() {
            [] => "a job diverged".to_owned(),
            [one] => format!("job {one} diverged"),
            [a, b] => format!("jobs {a} and {b} diverged"),
            more => format!("{} jobs diverged", more.len()),
        };
        out.push(clause(text, receipts.clone()));
    }
    if let Some((text, receipts)) = &c.decision {
        out.push(clause(
            format!("the latest decision: \"{text}\""),
            receipts.clone(),
        ));
    }
    out.retain(|c| !c.receipts.is_empty());
    out
}

fn waiting_points(c: &Collected, dir: &Directory) -> Vec<Clause> {
    c.asks
        .iter()
        .filter(|a| a.kind != AskKind::Mention && !a.receipts.is_empty())
        .take(WAITING_POINTS)
        .map(|a| {
            let to = handle(dir, a.to);
            let what = match a.kind {
                AskKind::Question => "to answer",
                AskKind::Decision => "to decide",
                AskKind::Review => "to review",
                AskKind::Approval => "to approve",
                AskKind::Mention => "to reply to",
            };
            clause(
                format!("waiting on {to} {what} \"{}\"", a.title),
                a.receipts.clone(),
            )
        })
        .collect()
}

/// The most pressing next step: an open decision, then a question, failing checks or a diverged
/// job, a task in review, a failed dispatch, and last a task with steps left. Only candidates with
/// receipts count: one without cannot be stated, so the next evidenced one is taken instead (an
/// unevidenced decision must not hide a failing build).
fn next_step(c: &Collected, dir: &Directory) -> Option<NextStep> {
    let step = |urgency, text: String, receipts: &[Receipt]| {
        Some(NextStep {
            urgency,
            clause: clause(text, receipts.to_vec()),
        })
    };
    let ask = |kinds: &[AskKind]| {
        c.asks
            .iter()
            .find(|a| kinds.contains(&a.kind) && !a.receipts.is_empty())
    };
    if let Some(a) = ask(&[AskKind::Decision, AskKind::Approval]) {
        let verb = if a.kind == AskKind::Approval {
            "approve"
        } else {
            "decide"
        };
        let to = handle(dir, a.to);
        return step(
            Urgency::Decide,
            format!("{to} to {verb} \"{}\"", a.title),
            &a.receipts,
        );
    }
    if let Some(a) = ask(&[AskKind::Question]) {
        let to = handle(dir, a.to);
        return step(
            Urgency::Answer,
            format!("{to} to answer \"{}\"", a.title),
            &a.receipts,
        );
    }
    for (check, view) in [Check::Tests, Check::Lint, Check::Build]
        .into_iter()
        .zip(&c.checks)
    {
        if let Some(view) = view
            .as_ref()
            .filter(|v| v.last_failed && !v.receipts.is_empty())
        {
            let what = match check {
                Check::Tests => "fix the failing tests",
                Check::Lint => "fix the lint errors",
                Check::Build => "fix the build",
            };
            return step(Urgency::Fix, what.to_owned(), &view.receipts);
        }
    }
    if let Some((jobs, receipts)) = c.diverged.as_ref().filter(|(_, r)| !r.is_empty()) {
        let text = match jobs.as_slice() {
            [one] => format!("decide whether to rerun job {one}"),
            _ => "decide whether to rerun the diverged jobs".to_owned(),
        };
        return step(Urgency::Fix, text, receipts);
    }
    if let Some(a) = ask(&[AskKind::Review]) {
        let to = handle(dir, a.to);
        return step(
            Urgency::Review,
            format!("{to} to review \"{}\"", a.title),
            &a.receipts,
        );
    }
    let in_review = c.tasks.iter().find_map(|(t, v)| match &v.status {
        Some((TaskStatus::Review, r)) if !r.is_empty() => Some((t, r)),
        _ => None,
    });
    if let Some((task, receipts)) = in_review {
        return step(
            Urgency::Review,
            format!("review {}", task_key(dir, *task)),
            receipts,
        );
    }
    let failed = c.tasks.iter().find_map(|(t, v)| match &v.finished {
        Some((DispatchOutcome::Failed, r)) if !r.is_empty() => Some((t, r)),
        _ => None,
    });
    if let Some((task, receipts)) = failed {
        return step(
            Urgency::Retry,
            format!("retry or reassign {}", task_key(dir, *task)),
            receipts,
        );
    }
    let left = c.tasks.iter().find_map(|(t, v)| {
        let in_progress =
            matches!(&v.status, Some((TaskStatus::InProgress, _))) || v.status.is_none();
        match &v.plan {
            Some((done, total, r)) if in_progress && done < total && !r.is_empty() => {
                Some((t, total - done, r))
            }
            _ => None,
        }
    });
    if let Some((task, n, receipts)) = left {
        return step(
            Urgency::Finish,
            format!(
                "finish {} ({} left)",
                task_key(dir, *task),
                plural64(u64::from(n), "step", "steps")
            ),
            receipts,
        );
    }
    None
}

fn activity_point(blocks: &[&Block]) -> Option<Clause> {
    let first = blocks.first()?;
    let edits: u64 = blocks.iter().map(|b| u64::from(b.counts.file_edits)).sum();
    let runs: u64 = blocks.iter().map(|b| u64::from(b.counts.tools_run)).sum();
    let mut text = format!(
        "work went on: {}",
        plural(blocks.len(), "burst of work", "bursts of work")
    );
    if edits > 0 || runs > 0 {
        text.push_str(&format!(
            " ({}, {})",
            plural64(edits, "file edit", "file edits"),
            plural64(runs, "tool run", "tool runs")
        ));
    }
    let mut receipts = vec![Receipt::Event { id: first.id }];
    for b in blocks.iter().skip(1).take(CLAUSE_RECEIPTS - 1) {
        receipts.push(Receipt::Event { id: b.id });
    }
    Some(clause(text, receipts))
}

// ─── Writing ─────────────────────────────────────────────────────────────────────────────────

/// The paragraph for a workstream's brief: its state and tasks, then checks, jobs and decisions,
/// then what is waiting on whom. The next step is separate ([`Standing::next`]).
#[must_use]
pub fn draft_brief(standing: &Standing) -> Draft {
    let mut first: Vec<Clause> = standing.state.iter().cloned().collect();
    first.extend(standing.tasks.iter().cloned());
    let sentences = [first, standing.signals.clone(), standing.waiting.clone()]
        .into_iter()
        .filter(|clauses| !clauses.is_empty())
        .map(|clauses| Sentence { clauses })
        .collect();
    Draft {
        kind: DraftKind::Paragraph,
        sentences,
    }
}

/// The paragraph for a project's brief: one sentence per workstream with something to say,
/// leading with its state (or its name) and then its most important points.
#[must_use]
pub fn draft_rollup(standings: &[&Standing]) -> Draft {
    let mut sentences = Vec::new();
    for s in standings {
        let points: Vec<&Clause> = s
            .state
            .iter()
            .chain(&s.tasks)
            .chain(&s.signals)
            .take(ROLLUP_POINTS)
            .chain(s.waiting.first())
            .collect();
        let Some((lead, rest)) = points.split_first() else {
            continue;
        };
        // The state names the workstream already ("Seed runs is at risk"); anything else is
        // prefixed with its name.
        let lead = if s.state.is_some() {
            (*lead).clone()
        } else {
            clause(format!("{}: {}", s.name, lead.text), lead.receipts.clone())
        };
        let mut clauses = vec![lead];
        clauses.extend(rest.iter().map(|c| (*c).clone()));
        sentences.push(Sentence { clauses });
    }
    Draft {
        kind: DraftKind::Paragraph,
        sentences,
    }
}

fn line(c: &Clause) -> Draft {
    Draft {
        kind: DraftKind::Line,
        sentences: vec![Sentence {
            clauses: vec![c.clone()],
        }],
    }
}

fn write(
    target: BriefTarget,
    draft: &Draft,
    next: Option<&Clause>,
    current: Option<&Brief>,
    policy: BriefPolicy,
    summarizer: &dyn Summarizer,
) -> Result<Option<BriefProposal>, SummaryError> {
    let summary = summarizer.summarize(draft)?;
    verify(&summary, draft)?;
    if summary.spans.is_empty() {
        return Ok(None);
    }
    let next = match next {
        Some(c) => {
            let d = line(c);
            let s = summarizer.summarize(&d)?;
            verify(&s, &d)?;
            Some(s).filter(|s| !s.spans.is_empty())
        }
        None => None,
    };
    let proposal = BriefProposal::new(target, summary, next, disposition(current, policy));
    // Nothing new: the brief in force already says this.
    if current.is_some_and(|b| b.target == target && b.text == proposal.text()) {
        return Ok(None);
    }
    Ok(Some(proposal))
}

/// Proposes a workstream's brief from its standing. `current` is the brief in force, if any: a
/// pinned one only gets a proposal, and a proposal that says the same is not made (`None`).
///
/// # Errors
///
/// The summarizer's error, or what [`verify`] found wrong with its output.
pub fn propose_workstream(
    standing: &Standing,
    current: Option<&Brief>,
    policy: BriefPolicy,
    summarizer: &dyn Summarizer,
) -> Result<Option<BriefProposal>, SummaryError> {
    if standing.is_empty() {
        return Ok(None);
    }
    write(
        BriefTarget::Workstream(standing.workstream),
        &draft_brief(standing),
        standing.next.as_ref().map(|n| &n.clause),
        current,
        policy,
        summarizer,
    )
}

/// Rolls a project's workstream standings up into the project's brief. Standings known to
/// belong to another project are left out. The next step is the most pressing of theirs.
///
/// # Errors
///
/// The summarizer's error, or what [`verify`] found wrong with its output.
pub fn propose_project(
    project: ProjectId,
    standings: &[&Standing],
    current: Option<&Brief>,
    policy: BriefPolicy,
    summarizer: &dyn Summarizer,
) -> Result<Option<BriefProposal>, SummaryError> {
    let mine: Vec<&Standing> = standings
        .iter()
        .copied()
        .filter(|s| s.project.is_none_or(|p| p == project) && !s.is_empty())
        .collect();
    if mine.is_empty() {
        return Ok(None);
    }
    // The first of the most pressing, so ties keep the caller's order.
    let next = mine
        .iter()
        .filter_map(|s| s.next.as_ref())
        .reduce(|best, n| if n.urgency < best.urgency { n } else { best })
        .map(|n| &n.clause);
    write(
        BriefTarget::Project(project),
        &draft_rollup(&mine),
        next,
        current,
        policy,
        summarizer,
    )
}

/// The back office's question for an active workstream with no activity since `last_at`: "No
/// activity for 4 days (since 2026-09-26), paused?". It cites the last activity, and is always
/// only a proposal.
#[must_use]
pub fn propose_paused(
    workstream: WorkstreamId,
    last_event: EventId,
    last_at: TimestampMs,
    now: TimestampMs,
    utc_offset_minutes: i32,
) -> BriefProposal {
    let receipts = vec![Receipt::Event { id: last_event }];
    let days = now.saturating_sub(last_at).max(0) / DAY_MS;
    let quiet = match days {
        0 => "less than a day".to_owned(),
        n => plural64(u64::try_from(n).unwrap_or(0), "day", "days"),
    };
    let since = date_of(last_at, utc_offset_minutes).0;
    let draft = Draft {
        kind: DraftKind::Paragraph,
        sentences: vec![Sentence {
            clauses: vec![
                clause(
                    format!("no activity for {quiet} (since {since})"),
                    receipts.clone(),
                ),
                clause("paused?".to_owned(), receipts.clone()),
            ],
        }],
    };
    let next = clause(
        "mark it paused, or give it a next step".to_owned(),
        receipts,
    );
    BriefProposal::new(
        BriefTarget::Workstream(workstream),
        RuleSummarizer.render(&draft),
        Some(RuleSummarizer.render(&line(&next))),
        Disposition::Propose,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(n: u128) -> Receipt {
        Receipt::Event {
            id: EventId(ulid::Ulid::from(n)),
        }
    }

    fn decision(n: u128, title: &str, receipts: Vec<Receipt>) -> OpenAsk {
        OpenAsk {
            ask: AskId(ulid::Ulid::from(n)),
            kind: AskKind::Decision,
            to: MemberId(ulid::Ulid::from(n)),
            title: title.into(),
            receipts,
        }
    }

    fn failing_tests(receipts: Vec<Receipt>) -> Option<CheckView> {
        Some(CheckView {
            failures: 1,
            last_failed: true,
            receipts,
        })
    }

    #[test]
    fn an_unevidenced_candidate_gives_way_to_the_next_evidenced_one() {
        let dir = Directory::default();
        let mut c = Collected {
            asks: vec![decision(1, "which dataset", vec![])],
            ..Collected::default()
        };
        c.checks[0] = failing_tests(vec![ev(2)]);
        let next = next_step(&c, &dir).expect("the failing tests are evidenced");
        assert_eq!(next.urgency, Urgency::Fix);
        assert_eq!(next.clause.text, "fix the failing tests");
        assert_eq!(next.clause.receipts, vec![ev(2)]);

        c.asks.push(decision(3, "which seed", vec![ev(3)]));
        let next = next_step(&c, &dir).expect("the second decision is evidenced");
        assert_eq!(next.urgency, Urgency::Decide);
        assert_eq!(next.clause.text, "someone to decide \"which seed\"");
        assert_eq!(next.clause.receipts, vec![ev(3)]);
    }

    #[test]
    fn no_next_step_without_evidence() {
        let mut c = Collected {
            asks: vec![decision(1, "which dataset", vec![])],
            diverged: Some((vec!["42".into()], vec![])),
            ..Collected::default()
        };
        c.checks[0] = failing_tests(vec![]);
        assert_eq!(next_step(&c, &Directory::default()), None);
    }
}
