//! Rules that turn blocks into drafts: which claims to make, in what words, and what each one
//! cites. Every clause is built from a block's counts or facts and carries their receipts.

use crate::block::{Block, Fact, FactKind};
use crate::checks::Check;
use crate::directory::Directory;
use crate::summary::{Clause, Draft, DraftKind, Sentence};
use crate::text::{NAME_CHARS, basename, clean, push_first};
use pitcrew_protocol::events::BriefTarget;
use pitcrew_protocol::ids::{MemberId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    AskKind, DispatchOutcome, Health, Receipt, TaskStatus, WorkstreamStatus,
};

/// Most clauses in a block's line, including the closing "and N more".
const LINE_CLAUSES: usize = 7;
/// Most blocks described one by one in a paragraph; the rest are counted.
const PARAGRAPH_BLOCKS: usize = 6;
/// Most receipts on a clause that gathers evidence from several places.
const CLAUSE_RECEIPTS: usize = 8;

/// How much to say.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// For a line: no quoted titles or summaries.
    Terse,
    /// For a paragraph: quote titles and closing summaries.
    Full,
}

/// The draft of a block's one-line summary.
#[must_use]
pub fn draft_line(block: &Block, directory: &Directory) -> Draft {
    Draft {
        kind: DraftKind::Line,
        sentences: vec![Sentence {
            clauses: block_clauses(block, directory, Detail::Terse),
        }],
    }
}

/// The draft of a paragraph over some blocks, e.g. one workstream's day: a sentence of totals
/// (when there is more than one block), then one sentence per block.
#[must_use]
pub fn draft_paragraph(blocks: &[&Block], directory: &Directory) -> Draft {
    let mut sentences = Vec::new();
    if blocks.len() > 1 {
        sentences.push(totals(blocks));
    }
    let shown = blocks.len().min(PARAGRAPH_BLOCKS);
    for block in blocks.iter().take(shown) {
        sentences.push(Sentence {
            clauses: block_clauses(block, directory, Detail::Full),
        });
    }
    let rest = blocks.get(shown..).unwrap_or_default();
    if !rest.is_empty() {
        let receipts = block_ids(rest);
        sentences.push(Sentence {
            clauses: vec![Clause {
                text: plural(rest.len(), "more burst of work", "more bursts of work"),
                receipts,
            }],
        });
    }
    Draft {
        kind: DraftKind::Paragraph,
        sentences,
    }
}

/// Who a clause is about: a member, or a session that runs as no agent, named by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actor {
    Member(MemberId),
    Session(SessionId),
}

/// A claim before its subject is attached.
struct Item {
    actor: Option<Actor>,
    text: String,
    receipts: Vec<Receipt>,
}

fn block_clauses(block: &Block, dir: &Directory, detail: Detail) -> Vec<Clause> {
    let names = Names(dir);
    // A session's work is its agent's, or else the session's own: never the person the runner's
    // events are stamped with (api-v1.md, "Sessions": "Who did it").
    let worker = match block.session {
        Some(s) => Some(block.agent.map_or(Actor::Session(s), Actor::Member)),
        None => block
            .agent
            .or_else(|| block.actors.first().copied())
            .map(Actor::Member),
    };
    let started_by_person = block
        .session
        .and_then(|s| dir.session(s))
        .is_some_and(|s| s.person_start);
    let fact_item = |f: &Fact| Item {
        actor: actor_of(f, worker, started_by_person),
        text: fact_text(f, &names, detail),
        receipts: f.receipts.clone(),
    };

    let mut items: Vec<Item> = block
        .facts
        .iter()
        .filter(|f| is_opening(&f.kind))
        .map(fact_item)
        .collect();
    items.extend(files_item(block).map(|(text, receipts)| Item {
        actor: worker,
        text,
        receipts,
    }));
    if block.counts.tools_run > 0 && !block.tool_receipts.is_empty() {
        let tools = counted(u64::from(block.counts.tools_run), "a tool", "tools");
        let text = match block.counts.tools_failed {
            0 => format!("ran {tools}"),
            f => format!("ran {tools} ({f} failed)"),
        };
        items.push(Item {
            actor: worker,
            text,
            receipts: block.tool_receipts.clone(),
        });
    }
    if block.counts.turns > 0 && !block.turn_receipts.is_empty() {
        items.push(Item {
            actor: worker,
            text: format!(
                "finished {}",
                counted(u64::from(block.counts.turns), "a turn", "turns")
            ),
            receipts: block.turn_receipts.clone(),
        });
    }
    items.extend(
        block
            .facts
            .iter()
            .filter(|f| !is_opening(&f.kind))
            .map(fact_item),
    );
    items.retain(|i| !i.receipts.is_empty());

    let span_ends = || {
        let mut r = vec![Receipt::Event { id: block.id }];
        if block.last != block.id {
            r.push(Receipt::Event { id: block.last });
        }
        r
    };
    if items.is_empty() {
        items.push(Item {
            actor: worker,
            text: format!(
                "was active ({})",
                plural64(u64::from(block.counts.events), "event", "events")
            ),
            receipts: span_ends(),
        });
    }

    let omitted = block.facts_omitted as usize;
    if items.len() + usize::from(omitted > 0) > LINE_CLAUSES {
        let keep = LINE_CLAUSES - 1;
        let rest = items.split_off(keep.min(items.len()));
        let mut receipts = Vec::new();
        for item in &rest {
            push_first(&mut receipts, &item.receipts, CLAUSE_RECEIPTS);
        }
        if receipts.is_empty() {
            receipts = span_ends();
        }
        items.push(Item {
            actor: None,
            text: format!("and {} more", rest.len() + omitted),
            receipts,
        });
    } else if omitted > 0 {
        items.push(Item {
            actor: None,
            text: format!("and {omitted} more"),
            receipts: span_ends(),
        });
    }

    // Name the actor whenever it changes, so each clause says who did it.
    let mut last: Option<Actor> = None;
    items
        .into_iter()
        .map(|item| {
            let text = match item.actor {
                Some(a) if Some(a) != last => format!("{} {}", names.actor(a), item.text),
                _ => item.text,
            };
            last = item.actor;
            Clause {
                text,
                receipts: item.receipts,
            }
        })
        .collect()
}

/// A paragraph's first sentence: how many bursts, edits, runs, moves and asks.
fn totals(blocks: &[&Block]) -> Sentence {
    let mut clauses = vec![Clause {
        text: plural(blocks.len(), "burst of work", "bursts of work"),
        receipts: block_ids(blocks),
    }];
    let mut add = |n: u64, text: String, receipts: Vec<Receipt>| {
        if n > 0 && !receipts.is_empty() {
            clauses.push(Clause { text, receipts });
        }
    };

    let edits = sum(blocks, |b| u64::from(b.counts.file_edits));
    let added = sum(blocks, |b| b.counts.lines_added);
    let removed = sum(blocks, |b| b.counts.lines_removed);
    add(
        edits,
        format!(
            "{} (+{added} −{removed})",
            plural64(edits, "file edit", "file edits")
        ),
        gather(blocks, |b| {
            b.files.iter().filter_map(|f| f.receipts.first()).collect()
        }),
    );
    let runs = sum(blocks, |b| u64::from(b.counts.tools_run));
    let failed = sum(blocks, |b| u64::from(b.counts.tools_failed));
    let runs_text = plural64(runs, "tool run", "tool runs");
    add(
        runs,
        if failed > 0 {
            format!("{runs_text} ({failed} failed)")
        } else {
            runs_text
        },
        gather(blocks, |b| b.tool_receipts.iter().take(2).collect()),
    );
    let moves = sum(blocks, |b| u64::from(b.counts.task_moves));
    add(
        moves,
        plural64(moves, "task move", "task moves"),
        fact_receipts(blocks, |k| matches!(k, FactKind::TaskMoved { .. })),
    );
    let raised = sum(blocks, |b| u64::from(b.counts.asks_raised));
    add(
        raised,
        plural64(raised, "ask raised", "asks raised"),
        fact_receipts(blocks, |k| matches!(k, FactKind::AskRaised { .. })),
    );
    let answered = sum(blocks, |b| u64::from(b.counts.asks_answered));
    add(
        answered,
        plural64(answered, "ask answered", "asks answered"),
        fact_receipts(blocks, |k| matches!(k, FactKind::AskAnswered { .. })),
    );
    Sentence { clauses }
}

fn sum(blocks: &[&Block], f: impl Fn(&Block) -> u64) -> u64 {
    blocks.iter().map(|&b| f(b)).fold(0u64, u64::saturating_add)
}

/// Receipts picked from each block, the first few overall.
fn gather<'a>(blocks: &[&'a Block], f: impl Fn(&'a Block) -> Vec<&'a Receipt>) -> Vec<Receipt> {
    let mut out = Vec::new();
    for &b in blocks {
        push_first(&mut out, f(b), CLAUSE_RECEIPTS);
    }
    out
}

/// The first receipt of each matching fact.
fn fact_receipts(blocks: &[&Block], pick: fn(&FactKind) -> bool) -> Vec<Receipt> {
    gather(blocks, |b| {
        b.facts
            .iter()
            .filter(|f| pick(&f.kind))
            .filter_map(|f| f.receipts.first())
            .collect()
    })
}

fn block_ids(blocks: &[&Block]) -> Vec<Receipt> {
    blocks
        .iter()
        .take(CLAUSE_RECEIPTS)
        .map(|b| Receipt::Event { id: b.id })
        .collect()
}

fn files_item(block: &Block) -> Option<(String, Vec<Receipt>)> {
    let files = &block.files;
    let first = files.first()?;
    let lines = format!(
        "(+{} −{})",
        block.counts.lines_added, block.counts.lines_removed
    );
    let name = |p: &str| clean(basename(p), NAME_CHARS);
    let text = if block.files_omitted > 0 {
        format!("edited more than {} files {lines}", files.len())
    } else {
        match files.as_slice() {
            [_] => format!("edited {} {lines}", name(&first.path)),
            [a, b] => format!("edited {} and {} {lines}", name(&a.path), name(&b.path)),
            _ => format!("edited {} files {lines}", files.len()),
        }
    };
    let mut receipts = Vec::new();
    push_first(
        &mut receipts,
        files.iter().filter_map(|f| f.receipts.first()),
        CLAUSE_RECEIPTS,
    );
    Some((text, receipts))
}

/// Facts that open a piece of work; they come before the work in a line.
fn is_opening(kind: &FactKind) -> bool {
    matches!(
        kind,
        FactKind::SessionStarted { .. }
            | FactKind::SessionLinked { .. }
            | FactKind::DispatchStarted { .. }
            | FactKind::TaskCreated { .. }
            | FactKind::TaskAssigned { .. }
            | FactKind::TaskMoved {
                to: TaskStatus::Backlog | TaskStatus::Todo | TaskStatus::InProgress,
                ..
            }
    )
}

/// Who a fact is about. Checks and divergences are statements about the work, by no one. A
/// session's start, waits and end are the session's doing (its agent's, or its own), except a
/// start a person made from PitCrew, which is theirs.
fn actor_of(fact: &Fact, worker: Option<Actor>, started_by_person: bool) -> Option<Actor> {
    match fact.kind {
        FactKind::Checks { .. } | FactKind::JobDiverged { .. } => None,
        FactKind::SessionStarted { .. } if started_by_person => Some(Actor::Member(fact.by)),
        FactKind::SessionStarted { .. }
        | FactKind::SessionWaiting { .. }
        | FactKind::SessionEnded => worker.or(Some(Actor::Member(fact.by))),
        _ => Some(Actor::Member(fact.by)),
    }
}

/// Names for prose, with plain fallbacks for anything the directory does not know.
struct Names<'a>(&'a Directory);

impl Names<'_> {
    fn member(&self, id: MemberId) -> String {
        self.0.handle(id).unwrap_or("someone").to_owned()
    }

    fn actor(&self, actor: Actor) -> String {
        match actor {
            Actor::Member(id) => self.member(id),
            Actor::Session(id) => self
                .0
                .session_name(id)
                .unwrap_or_else(|| "a session".to_owned()),
        }
    }

    fn task(&self, id: TaskId) -> String {
        self.0.task_key(id).unwrap_or("a task").to_owned()
    }

    fn workstream(&self, id: WorkstreamId) -> String {
        self.0
            .workstream_name(id)
            .unwrap_or("a workstream")
            .to_owned()
    }

    fn list(&self, ids: &[MemberId]) -> String {
        let names: Vec<String> = ids.iter().map(|m| self.member(*m)).collect();
        match names.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [init @ .., last] => format!("{} and {last}", init.join(", ")),
        }
    }
}

fn quoted(base: String, extra: Option<&str>, detail: Detail) -> String {
    match extra {
        Some(q) if detail == Detail::Full && !q.is_empty() => format!("{base} (\"{q}\")"),
        _ => base,
    }
}

fn fact_text(fact: &Fact, n: &Names<'_>, detail: Detail) -> String {
    match &fact.kind {
        FactKind::SessionStarted { title } => match title.as_deref() {
            Some(t) if detail == Detail::Full && !t.is_empty() => {
                format!("started the session \"{t}\"")
            }
            _ => "started a session".into(),
        },
        FactKind::SessionLinked { workstream, task } => match (task, workstream) {
            (Some(t), _) => format!("linked the session to {}", n.task(*t)),
            (None, Some(w)) => format!("linked the session to {}", n.workstream(*w)),
            (None, None) => "linked the session".into(),
        },
        FactKind::SessionWaiting { status_line } => quoted(
            "stopped to wait for an answer".into(),
            status_line.as_deref(),
            detail,
        ),
        FactKind::SessionEnded => "ended the session".into(),
        FactKind::DispatchStarted { task, agent } => {
            format!("dispatched {} to {}", n.member(*agent), n.task(*task))
        }
        FactKind::DispatchFinished {
            task,
            outcome,
            summary,
        } => {
            let t = task.map_or_else(|| "a dispatch".to_owned(), |t| n.task(t));
            let base = match outcome {
                DispatchOutcome::Succeeded => format!("finished {t}"),
                DispatchOutcome::Failed => format!("could not finish {t}"),
                DispatchOutcome::Canceled => format!("stopped work on {t}"),
            };
            quoted(base, summary.as_deref(), detail)
        }
        FactKind::TaskCreated { task } => format!("created {}", n.task(*task)),
        FactKind::TaskMoved { task, to, .. } => {
            format!("moved {} to {}", n.task(*task), task_status(*to))
        }
        FactKind::TaskAssigned { task, assignee } => match assignee {
            Some(a) => format!("assigned {} to {}", n.task(*task), n.member(*a)),
            None => format!("unassigned {}", n.task(*task)),
        },
        FactKind::PlanUpdated { task, done, total } => {
            format!(
                "updated the plan for {} ({done} of {total} done)",
                n.task(*task)
            )
        }
        FactKind::Checks {
            check,
            runs,
            failures,
            last_failed,
        } => {
            let what = match check {
                Check::Tests => "tests",
                Check::Lint => "lint",
                Check::Build => "the build",
            };
            match (*failures, *last_failed) {
                (0, _) => format!("{what} passed"),
                (_, false) => format!("{what} failed then passed"),
                (f, true) if f >= *runs => format!("{what} failed"),
                (_, true) => format!("{what} passed, then failed again"),
            }
        }
        FactKind::JobDiverged { jobs } => match jobs.as_slice() {
            [] => "a job diverged".into(),
            [one] => format!("job {one} diverged"),
            [a, b] => format!("jobs {a} and {b} diverged"),
            more => format!("{} jobs diverged", more.len()),
        },
        FactKind::AskRaised {
            ask_kind,
            to,
            title,
            ..
        } => {
            let to = n.member(*to);
            let base = match ask_kind {
                AskKind::Question => format!("asked {to} a question"),
                AskKind::Decision => format!("asked {to} for a decision"),
                AskKind::Review => format!("asked {to} for a review"),
                AskKind::Approval => format!("asked {to} for approval"),
                AskKind::Mention => format!("mentioned {to}"),
            };
            quoted(base, Some(title), detail)
        }
        FactKind::AskAnswered { ask } => match n.0.ask(*ask) {
            Some((kind, from)) => {
                let from = n.member(from);
                match kind {
                    AskKind::Question => format!("answered a question from {from}"),
                    AskKind::Decision => format!("decided on a question from {from}"),
                    AskKind::Review => format!("reviewed work from {from}"),
                    AskKind::Approval => format!("answered an approval request from {from}"),
                    AskKind::Mention => format!("replied to a mention from {from}"),
                }
            }
            None => "answered an ask".into(),
        },
        FactKind::Commented {
            task,
            workstream,
            mentions,
        } => {
            let on = task
                .map(|t| n.task(t))
                .or_else(|| workstream.map(|w| n.workstream(w)));
            let base = match on {
                Some(on) => format!("commented on {on}"),
                None => "commented".into(),
            };
            if mentions.is_empty() {
                base
            } else {
                format!("{base} and mentioned {}", n.list(mentions))
            }
        }
        FactKind::DecisionRecorded { text } => {
            quoted("recorded a decision".into(), Some(text), detail)
        }
        FactKind::WorkstreamCreated { workstream } => {
            format!("created the workstream {}", n.workstream(*workstream))
        }
        FactKind::WorkstreamChanged {
            workstream,
            status,
            health,
        } => {
            let state = match status {
                WorkstreamStatus::Active => health_word(*health),
                other => workstream_status(*other),
            };
            format!("marked {} {state}", n.workstream(*workstream))
        }
        FactKind::BriefAccepted { target, pinned } => {
            let what = match target {
                BriefTarget::Workstream(w) => format!("the brief for {}", n.workstream(*w)),
                BriefTarget::Project(_) => "the project brief".into(),
            };
            if *pinned {
                format!("pinned {what}")
            } else {
                format!("accepted {what}")
            }
        }
    }
}

fn task_status(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Backlog => "backlog",
        TaskStatus::Todo => "todo",
        TaskStatus::InProgress => "in progress",
        TaskStatus::Review => "review",
        TaskStatus::Done => "done",
        TaskStatus::Canceled => "canceled",
    }
}

fn workstream_status(s: WorkstreamStatus) -> &'static str {
    match s {
        WorkstreamStatus::Idea => "an idea",
        WorkstreamStatus::Active => "active",
        WorkstreamStatus::Paused => "paused",
        WorkstreamStatus::Shipped => "shipped",
        WorkstreamStatus::Dropped => "dropped",
    }
}

fn health_word(h: Health) -> &'static str {
    match h {
        Health::OnTrack => "on track",
        Health::AtRisk => "at risk",
        Health::Blocked => "blocked",
    }
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    plural64(u64::try_from(n).unwrap_or(u64::MAX), one, many)
}

pub(crate) fn plural64(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Like [`plural64`], but `one` is said in full ("a tool").
fn counted(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        one.to_owned()
    } else {
        format!("{n} {many}")
    }
}
