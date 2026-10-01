//! The first rules. Each reacts to evidence in the events and cites it.

use crate::action::{Action, AskDraft};
use crate::rule::{Context, Rule};
use crate::world::{AskPos, QuietPos, TITLE_CHARS, small_receipts};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{SessionId, TaskId};
use pitcrew_protocol::model::{
    AskKind, DispatchOutcome, Mover, Receipt, SessionState, TaskStatus, TimestampMs,
    WorkstreamStatus,
};
use pitcrew_recap::{Check, classify, clean, date_of, mentions_divergence, propose_paused};
use serde::{Deserialize, Serialize};

/// Longest ask body written, in characters.
const BODY_CHARS: usize = 200;
/// Most asks or workstreams a time-based rule handles per event; the rest wait for the next one.
const DUE_PER_EVENT: usize = 16;
/// Receipts kept for a failing streak: the first failure and the latest one.
const STREAK_RECEIPTS: usize = 4;

/// The rules every office runs, in order.
#[must_use]
pub fn default_rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(DispatchToReview),
        Box::new(JobDiverged),
        Box::new(TestsFailing),
        Box::new(RemindStaleAsks),
        Box::new(QuietWorkstream),
    ]
}

fn event_receipt(event: &Event) -> Receipt {
    Receipt::Event { id: event.id }
}

/// A dispatch finished successfully: move its task to review, as the back office, when
/// `can_move` allows it (the task is in progress).
#[derive(Clone, Copy, Debug, Default)]
pub struct DispatchToReview;

impl Rule for DispatchToReview {
    fn name(&self) -> &'static str {
        "dispatch_to_review"
    }

    fn on_event(&mut self, ctx: &mut Context<'_>, event: &Event) -> Vec<Action> {
        let EventBody::DispatchFinished {
            dispatch,
            outcome: DispatchOutcome::Succeeded,
            ..
        } = &event.body
        else {
            return Vec::new();
        };
        if ctx.from_office(event) {
            return Vec::new();
        }
        let Some(d) = ctx.world.dispatch(*dispatch) else {
            return Vec::new();
        };
        let Some(task) = ctx.world.task(d.task) else {
            return Vec::new();
        };
        let mover = Mover::BackOffice {
            accept_auto: task.accept_auto,
        };
        if !task.status.can_move(TaskStatus::Review, mover) {
            return Vec::new();
        }
        vec![Action::Append {
            body: EventBody::TaskMoved {
                task: d.task,
                from: task.status,
                to: TaskStatus::Review,
                mover,
            },
            because: vec![event_receipt(event)],
        }]
    }
}

/// The session's agent if known, else the event's author.
fn worker(
    ctx: &Context<'_>,
    session: Option<SessionId>,
    event: &Event,
) -> pitcrew_protocol::ids::MemberId {
    session
        .and_then(|s| ctx.world.session(s))
        .and_then(|s| s.agent)
        .unwrap_or(event.author)
}

/// A job diverged (a tool's outcome or a dispatch's summary says so): ask the person who owns the
/// work for a decision. Once per session (or dispatch) until `diverged_repeat_ms` has passed.
#[derive(Clone, Copy, Debug, Default)]
pub struct JobDiverged;

impl Rule for JobDiverged {
    fn name(&self) -> &'static str {
        "job_diverged"
    }

    fn on_event(&mut self, ctx: &mut Context<'_>, event: &Event) -> Vec<Action> {
        if ctx.from_office(event) {
            return Vec::new();
        }
        let (key, session, task, text, receipts) = match &event.body {
            EventBody::ToolRan {
                session,
                outcome,
                receipt,
                ..
            } if mentions_divergence(outcome) => (
                format!("s/{}", session.0),
                Some(*session),
                ctx.world.session(*session).and_then(|s| s.task),
                outcome.as_str(),
                small_receipts([&event_receipt(event), receipt], 2),
            ),
            EventBody::DispatchFinished {
                dispatch,
                summary: Some(summary),
                ..
            } if mentions_divergence(summary) => {
                let d = ctx.world.dispatch(*dispatch);
                let session = d.and_then(|d| d.session);
                let key = match session {
                    Some(s) => format!("s/{}", s.0),
                    None => format!("d/{}", dispatch.0),
                };
                (
                    key,
                    session,
                    d.map(|d| d.task),
                    summary.as_str(),
                    vec![event_receipt(event)],
                )
            }
            _ => return Vec::new(),
        };
        let recent = ctx
            .memo
            .get::<TimestampMs>(&key)
            .is_some_and(|last| ctx.now.saturating_sub(last) < ctx.config.diverged_repeat_ms);
        if recent {
            return Vec::new();
        }
        let agent = worker(ctx, session, event);
        let Some(owner) = ctx.world.owner_of(agent, event.on_behalf_of) else {
            return Vec::new();
        };
        ctx.memo.put(&key, &ctx.now);
        vec![Action::RaiseAsk {
            ask: AskDraft {
                kind: AskKind::Decision,
                to: owner,
                task,
                session,
                title: "A run diverged: rerun it or drop it?".to_owned(),
                body: clean(text, BODY_CHARS),
                options: vec![
                    "Rerun it".to_owned(),
                    "Drop it".to_owned(),
                    "I'll look first".to_owned(),
                ],
                receipts,
            },
        }]
    }
}

/// A session's failing tests, in a row.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Streak {
    failed: u32,
    receipts: Vec<Receipt>,
    asked: bool,
}

/// Tests kept failing: after `failing_runs` failed test runs in a row in one session, ask the
/// person who owns the work for a decision. Once per streak; a passing run ends the streak.
#[derive(Clone, Copy, Debug, Default)]
pub struct TestsFailing;

impl TestsFailing {
    fn key(session: SessionId) -> String {
        format!("s/{}", session.0)
    }
}

impl Rule for TestsFailing {
    fn name(&self) -> &'static str {
        "tests_failing"
    }

    fn on_event(&mut self, ctx: &mut Context<'_>, event: &Event) -> Vec<Action> {
        let (session, target, failed, receipt) = match &event.body {
            EventBody::SessionEnded { session }
            | EventBody::SessionStateChanged {
                session,
                to: SessionState::Ended,
                ..
            } => {
                ctx.memo.remove(&Self::key(*session));
                return Vec::new();
            }
            EventBody::ToolRan {
                session,
                tool,
                target,
                failed,
                receipt,
                ..
            } if classify(tool, target) == Some(Check::Tests) && !ctx.from_office(event) => {
                (*session, target, *failed, receipt)
            }
            _ => return Vec::new(),
        };
        let key = Self::key(session);
        if !failed {
            ctx.memo.remove(&key);
            return Vec::new();
        }
        let mut streak: Streak = ctx.memo.get(&key).unwrap_or_default();
        streak.failed = streak.failed.saturating_add(1);
        if streak.receipts.len() >= STREAK_RECEIPTS {
            streak.receipts.truncate(2);
        }
        streak
            .receipts
            .extend(small_receipts([&event_receipt(event), receipt], 2));
        let task: Option<TaskId> = ctx.world.session(session).and_then(|s| s.task);
        let owner = ctx
            .world
            .owner_of(worker(ctx, Some(session), event), event.on_behalf_of);
        let ask = match owner {
            Some(owner) if streak.failed >= ctx.config.failing_runs && !streak.asked => {
                streak.asked = true;
                Some(AskDraft {
                    kind: AskKind::Decision,
                    to: owner,
                    task,
                    session: Some(session),
                    title: format!(
                        "Tests failed {} times in a row: keep going or step in?",
                        streak.failed
                    ),
                    body: clean(&format!("The latest run: {target}"), BODY_CHARS),
                    options: vec!["Keep going".to_owned(), "I'll step in".to_owned()],
                    receipts: streak.receipts.clone(),
                })
            }
            _ => None,
        };
        ctx.memo.put(&key, &streak);
        ask.map(|ask| vec![Action::RaiseAsk { ask }])
            .unwrap_or_default()
    }
}

/// How long something has been open, in words.
fn age(ms: i64) -> String {
    let hours = ms.max(0) / 3_600_000;
    match hours {
        0 => "less than an hour".to_owned(),
        1 => "1 hour".to_owned(),
        2..48 => format!("{hours} hours"),
        _ => format!("{} days", hours / 24),
    }
}

/// An ask open for more than `remind_after_ms`: mention its addressee once. Mentions themselves
/// are not reminded, nor asks addressed to the office.
#[derive(Clone, Copy, Debug, Default)]
pub struct RemindStaleAsks;

impl Rule for RemindStaleAsks {
    fn name(&self) -> &'static str {
        "remind_stale_asks"
    }

    fn on_event(&mut self, ctx: &mut Context<'_>, _event: &Event) -> Vec<Action> {
        let cursor: Option<AskPos> = ctx.memo.get("cursor");
        let until = ctx.now.saturating_sub(ctx.config.remind_after_ms);
        let mut last = None;
        let mut out = Vec::new();
        for (pos, ask) in ctx.world.asks_due(cursor, until).take(DUE_PER_EVENT) {
            last = Some(pos);
            if ask.kind == AskKind::Mention || ctx.config.office == Some(ask.to) {
                continue;
            }
            let mut receipts = vec![Receipt::Event { id: ask.event }];
            receipts.extend(small_receipts(&ask.receipts, 7));
            let since = date_of(ask.at, ctx.config.utc_offset_minutes).0;
            out.push(Action::RaiseAsk {
                ask: AskDraft {
                    kind: AskKind::Mention,
                    to: ask.to,
                    task: ask.task,
                    session: ask.session,
                    title: clean(&format!("Reminder: {}", ask.title), TITLE_CHARS),
                    body: format!(
                        "Open for {}, since {since}.",
                        age(ctx.now.saturating_sub(ask.at))
                    ),
                    options: Vec::new(),
                    receipts,
                },
            });
        }
        if let Some(pos) = last {
            ctx.memo.put("cursor", &pos);
        }
        out
    }
}

/// An active workstream with no activity for `quiet_after_ms`: propose "paused?" in a brief
/// proposal. Once per quiet spell; new activity starts a new one.
#[derive(Clone, Copy, Debug, Default)]
pub struct QuietWorkstream;

impl Rule for QuietWorkstream {
    fn name(&self) -> &'static str {
        "quiet_workstream"
    }

    fn on_event(&mut self, ctx: &mut Context<'_>, _event: &Event) -> Vec<Action> {
        let cursor: Option<QuietPos> = ctx.memo.get("cursor");
        let until = ctx.now.saturating_sub(ctx.config.quiet_after_ms);
        let mut last = None;
        let mut out = Vec::new();
        for (pos, w) in ctx.world.workstreams_due(cursor, until).take(DUE_PER_EVENT) {
            last = Some(pos);
            if w.status != WorkstreamStatus::Active {
                continue;
            }
            out.push(Action::ProposeBrief {
                proposal: propose_paused(
                    pos.2,
                    w.last_event,
                    w.last_at,
                    ctx.now,
                    ctx.config.utc_offset_minutes,
                ),
            });
        }
        if let Some(pos) = last {
            ctx.memo.put("cursor", &pos);
        }
        out
    }
}
