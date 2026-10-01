//! The back office acting on the hub: [`OfficeCommands`] implements `pitcrew_office::Commands`
//! over [`WorkService`], and [`WorkService::run_office`] is the one entry point the daemon calls
//! after each append.
//!
//! # How the office runs
//!
//! The office's rules run inside the store, as its run log: [`BackOffice::run_log`] is
//! `pitcrew_office::RunLog` with the office's [`Config`] (`Config::office` = the back office's
//! member) and rules, and the store applies it to every append in the append's own transaction.
//! Each action a rule takes is logged in `office_runs` as `emitted`, `capped` or `refused` (by the
//! office's "never" list and move rules). The run log is the live office: there is no second copy
//! of its state that could disagree with what it logged.
//!
//! After an append commits, [`WorkService::run_office`] reads the run log's entries for the
//! appended revisions and hands the emitted ones to `pitcrew_office::apply`, which re-checks each
//! action's shape and calls [`OfficeCommands`]. Capped and refused entries are only logged.
//!
//! # The hub re-validates every action
//!
//! `apply` checks only an action's shape. [`OfficeCommands`] checks every action against the hub's
//! own tables, like any caller's, and appends its events authored by the back office's member, on
//! behalf of that member's owner:
//!
//! - **task moves** pass `TaskStatus::can_move` with `Mover::BackOffice` carrying the task's own
//!   `accept_auto` (whatever the action claimed), from the status the task is in now; a move to
//!   done needs `accept_auto`. Any other mover is refused;
//! - **answers** go only to open questions and mentions addressed to an agent: the back office
//!   itself or another agent of its owner. Never to a person, and never a decision, approval or
//!   review;
//! - **asks** name a known addressee, task and session, and are never approvals;
//! - **comments** name a known task or workstream and known mentions;
//! - **brief proposals** name a known project or workstream. `brief_proposed` is appended, and with
//!   it `brief_accepted` when the proposal says to accept it automatically (its policy), unless the
//!   brief in force is pinned: a pinned brief only ever gets proposals.
//!
//! Every action must cite receipts. An action the hub refuses appends nothing; `run_office` returns
//! the refusal and logs it as a warning.

use crate::commands::{brief_target_exists, known_member, not_empty};
use crate::error::{Result, WorkError};
use crate::projection::projections;
use crate::query::{self, TaskRef};
use crate::service::{WorkService, no_task};
use pitcrew_office::{
    ApplyError, AskDraft, Commands, Config, Entry, MAX_ACTIONS_PER_EVENT, Outcome, RUN_LOG, Rule,
    RunLog, apply, default_rules, read_runs,
};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{AskId, MemberId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    Answer, Ask, AskKind, AskState, MemberKind, Mover, Receipt, Task, TaskStatus,
};
use pitcrew_recap::BriefProposal;
use pitcrew_store::sql::{Connection, OptionalExtension};
use pitcrew_store::{Projection, RevRange};
use std::sync::Arc;

type RuleSet = Arc<dyn Fn() -> Vec<Box<dyn Rule>> + Send + Sync>;

/// The back office of one hub: its member, settings and rules. Build it once; the store gets its
/// run log ([`projections_with_office`]) and [`WorkService::run_office`] applies what it emitted,
/// so both always use the same [`Config`].
pub struct BackOffice {
    member: MemberId,
    config: Config,
    rules: RuleSet,
    /// How many run-log entries to read at once: more than one revision can hold, so a full page
    /// always ends with whole revisions before its last one.
    page: usize,
}

impl std::fmt::Debug for BackOffice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackOffice")
            .field("member", &self.member)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl BackOffice {
    /// The back office acting as `member` (an agent of the workspace), with the default settings
    /// and rules.
    #[must_use]
    pub fn new(member: MemberId) -> Self {
        Self::with_config(member, Config::default())
    }

    /// The back office acting as `member`, with `config` (its `office` is set to `member`).
    #[must_use]
    pub fn with_config(member: MemberId, config: Config) -> Self {
        Self::with_rules(member, config, default_rules)
    }

    /// The back office acting as `member`, with `config` and other rules. `rules` makes a fresh
    /// set each time the run log restores the office.
    #[must_use]
    pub fn with_rules(
        member: MemberId,
        mut config: Config,
        rules: impl Fn() -> Vec<Box<dyn Rule>> + Send + Sync + 'static,
    ) -> Self {
        config.office = Some(member);
        let per_revision = rules().len().max(1).saturating_mul(MAX_ACTIONS_PER_EVENT);
        Self {
            member,
            config,
            rules: Arc::new(rules),
            page: per_revision.saturating_add(1),
        }
    }

    /// The back office's member.
    #[must_use]
    pub fn member(&self) -> MemberId {
        self.member
    }

    /// The settings, as the run log uses them.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The office's run log as a store projection, with this office's settings and rules.
    /// Register it with `Store::open_with`, e.g. through [`projections_with_office`].
    #[must_use]
    pub fn run_log(&self) -> RunLog {
        let rules = Arc::clone(&self.rules);
        RunLog::with_rules(self.config.clone(), move || rules())
    }
}

/// Every projection of the work model ([`projections`]) and the back office's run log. Pass them
/// to `Store::open_with` when the hub runs a back office.
#[must_use]
pub fn projections_with_office(office: &BackOffice) -> Vec<Box<dyn Projection>> {
    let mut all = projections();
    all.push(Box::new(office.run_log()));
    all
}

/// One emitted action of the run log, and what the hub made of it.
#[derive(Debug)]
pub struct Applied {
    /// The run-log entry (its outcome is `emitted`).
    pub entry: Entry,
    /// `Ok` when the hub applied it; else why not: a shape `apply` refused, or the hub's refusal.
    pub result: std::result::Result<(), ApplyError<WorkError>>,
}

/// What [`WorkService::run_office`] did.
#[derive(Debug, Default)]
pub struct OfficeRun {
    /// Every emitted action of the revisions, in log order, with its result.
    pub actions: Vec<Applied>,
}

impl OfficeRun {
    /// The actions the hub applied.
    pub fn applied(&self) -> impl Iterator<Item = &Applied> {
        self.actions.iter().filter(|a| a.result.is_ok())
    }

    /// The actions the hub did not apply.
    pub fn refused(&self) -> impl Iterator<Item = &Applied> {
        self.actions.iter().filter(|a| a.result.is_err())
    }
}

/// `pitcrew_office::Commands` over the hub: each call re-validates the action against the hub's
/// tables and appends its events, authored by the back office's member on behalf of its owner.
/// See the [module docs](self). Get one with [`WorkService::office_commands`].
#[derive(Debug)]
pub struct OfficeCommands<'a> {
    work: &'a WorkService,
    member: MemberId,
    owner: Option<MemberId>,
}

fn evidence(receipts: &[Receipt]) -> Result<()> {
    if receipts.is_empty() {
        Err(WorkError::invalid(
            "The back office must cite evidence for every action.",
        ))
    } else {
        Ok(())
    }
}

fn status_name(s: TaskStatus) -> String {
    crate::codec::enum_text(&s).unwrap_or_default()
}

/// Whether the run log has applied every revision up to `to_rev`. It has, after an append, when
/// it is registered with the store; otherwise its checkpoint is behind or missing.
fn run_log_reached(conn: &Connection, to_rev: u64) -> Result<()> {
    let rev: Option<i64> = conn
        .prepare_cached("SELECT rev FROM projection_state WHERE name = ?1")?
        .query_row([RUN_LOG], |r| r.get(0))
        .optional()?;
    match rev.and_then(|r| u64::try_from(r).ok()) {
        Some(rev) if rev >= to_rev => Ok(()),
        _ => Err(WorkError::internal(format!(
            "the back office's run log ({RUN_LOG}) has not reached revision {to_rev}: register \
             BackOffice::run_log with the store (projections_with_office)"
        ))),
    }
}

impl WorkService {
    /// The back office's [`Commands`] over this service, acting as `office`'s member.
    ///
    /// # Errors
    ///
    /// `invalid` when the member is not an agent of this workspace; database errors.
    pub fn office_commands(&self, office: &BackOffice) -> Result<OfficeCommands<'_>> {
        let id = office.member();
        match self.read(|c| query::member(c, &id))? {
            Some(m) if m.kind == MemberKind::Agent => Ok(OfficeCommands {
                work: self,
                member: m.id,
                owner: m.owner,
            }),
            Some(m) => Err(WorkError::invalid(format!(
                "The back office's member must be an agent; {} is a person.",
                m.handle
            ))),
            None => Err(WorkError::invalid(format!(
                "The back office's member {id} is not in this workspace."
            ))),
        }
    }

    /// Applies what the back office emitted for the revisions `revs`. **The daemon calls this after
    /// each append batch**, with the batch's revisions (e.g. each range from `Store::subscribe`),
    /// on the blocking pool: it reads the store and appends.
    ///
    /// It reads the run log's entries for those revisions (the store wrote them in the append's
    /// transaction) and applies the emitted ones through [`OfficeCommands`], in log order. Capped
    /// and refused entries are skipped: they are only logged. An action the hub refuses appends
    /// nothing and is logged as a warning. What the office appends is itself an append batch: pass
    /// its revisions here too (the rules ignore the office's own events, but the time-based ones
    /// may act on any event). A receiver that lagged can pass the whole range it missed.
    ///
    /// # Errors
    ///
    /// When the back office's member is not an agent of the workspace (nothing is applied), when
    /// the run log has not reached `revs.to_rev` (it is not registered with the store), or on
    /// database errors. Refused actions are not errors: see [`OfficeRun::refused`].
    pub fn run_office(&self, office: &BackOffice, revs: RevRange) -> Result<OfficeRun> {
        let mut run = OfficeRun::default();
        if revs.is_empty() {
            return Ok(run);
        }
        self.read(|c| run_log_reached(c, revs.to_rev))?;
        let mut commands = self.office_commands(office)?;
        let mut after = revs.from_rev.saturating_sub(1);
        loop {
            let page = self.read(|c| {
                read_runs(c, after, office.page)
                    .map_err(|e| WorkError::internal(format!("reading the run log: {e}")))
            })?;
            let read_all = page.len() < office.page;
            let mut batch: Vec<Entry> = page
                .into_iter()
                .take_while(|e| e.rev <= revs.to_rev)
                .collect();
            let done = read_all || batch.len() < office.page;
            if !done {
                // A full page may end part-way through its last revision: keep whole revisions,
                // and read that one again from its start.
                let last = batch.last().map_or(after, |e| e.rev);
                let whole = batch.iter().take_while(|e| e.rev < last).count();
                if whole > 0 {
                    batch.truncate(whole);
                } else {
                    tracing::warn!(rev = last, "one revision filled a page of the run log");
                }
                after = batch.last().map_or(last, |e| e.rev);
            }
            let emitted = batch.iter().filter(|e| e.outcome == Outcome::Emitted);
            let results = apply(&batch, &mut commands);
            for (entry, result) in emitted.zip(results) {
                if let Err(error) = &result {
                    tracing::warn!(
                        rule = %entry.rule,
                        rev = entry.rev,
                        seq = entry.seq,
                        error = ?error,
                        "the hub refused a back-office action"
                    );
                }
                run.actions.push(Applied {
                    entry: entry.clone(),
                    result,
                });
            }
            if done {
                return Ok(run);
            }
        }
    }
}

impl OfficeCommands<'_> {
    /// The back office's member.
    #[must_use]
    pub fn member(&self) -> MemberId {
        self.member
    }

    fn event(&self, body: EventBody) -> pitcrew_protocol::events::Event {
        self.work.event(self.member, self.owner, body)
    }

    /// A move as the back office, with `can_move` and the task's own `accept_auto`.
    fn move_task(
        &self,
        id: TaskId,
        from: TaskStatus,
        to: TaskStatus,
        mover: Mover,
    ) -> Result<Task> {
        if !matches!(mover, Mover::BackOffice { .. }) {
            return Err(WorkError::forbidden(
                "The back office moves tasks only as the back office.",
            ));
        }
        let w = self.work;
        let _guard = w.lock();
        let task = w
            .read(|c| query::task(c, &TaskRef::Id(id)))?
            .ok_or_else(|| no_task(&TaskRef::Id(id)))?;
        // The task's own policy, whatever the action claimed.
        let ours = Mover::BackOffice {
            accept_auto: task.accept_auto,
        };
        if task.status != from {
            return Err(WorkError::conflict(format!(
                "{} is {} now, not {}.",
                task.key,
                status_name(task.status),
                status_name(from)
            )));
        }
        if to == TaskStatus::Done && !task.accept_auto {
            return Err(WorkError::conflict(format!(
                "{} does not allow automatic acceptance, so only a person marks it done.",
                task.key
            )));
        }
        if !task.status.can_move(to, ours) {
            return Err(WorkError::conflict(format!(
                "The back office moves a task only from in_progress to review, or from review to \
                 done when it allows automatic acceptance; not {} → {}.",
                status_name(task.status),
                status_name(to)
            )));
        }
        let body = EventBody::TaskMoved {
            task: id,
            from,
            to,
            mover: ours,
        };
        w.append(&[self.event(body)])?;
        w.reload_moved(id, to)
    }

    /// An answer to an open question or mention of an agent of the office's owner.
    fn answer(&self, id: AskId, answer: &Answer) -> Result<Ask> {
        let w = self.work;
        let text = answer.text.clone().filter(|t| !t.trim().is_empty());
        let _guard = w.lock();
        let ask = w.read(|c| {
            let ask =
                query::ask(c, &id)?.ok_or_else(|| WorkError::not_found(format!("No ask {id}.")))?;
            let to = query::member(c, &ask.to)?.filter(|m| m.kind == MemberKind::Agent);
            let Some(to) = to else {
                return Err(WorkError::forbidden(
                    "The back office never answers an ask addressed to a person.",
                ));
            };
            if !matches!(ask.kind, AskKind::Question | AskKind::Mention) {
                let kind = crate::codec::enum_text(&ask.kind).unwrap_or_default();
                return Err(WorkError::forbidden(format!(
                    "A {kind} is answered only by a person."
                )));
            }
            if to.id != self.member && (self.owner.is_none() || to.owner != self.owner) {
                return Err(WorkError::forbidden(
                    "The back office answers only asks addressed to itself or to its owner's \
                     agents.",
                ));
            }
            if answer.option.is_none() && text.is_none() {
                return Err(WorkError::invalid(
                    "Give an option, a text that is not empty, or both.",
                ));
            }
            if let Some(option) = answer.option
                && option >= ask.options.len()
            {
                return Err(WorkError::invalid(format!(
                    "option must be below {}.",
                    ask.options.len()
                )));
            }
            Ok(ask)
        })?;
        if ask.state != AskState::Open {
            let state = crate::codec::enum_text(&ask.state).unwrap_or_default();
            return Err(WorkError::conflict(format!("This ask is already {state}.")));
        }
        let answer = Answer {
            by: self.member,
            option: answer.option,
            text,
            at: w.now(),
        };
        w.append(&[self.event(EventBody::AskAnswered { ask: id, answer })])?;
        w.ask(&id)
    }

    /// A comment on a known task or workstream.
    fn comment(
        &self,
        task: Option<TaskId>,
        workstream: Option<WorkstreamId>,
        text: &str,
        mentions: &[MemberId],
    ) -> Result<()> {
        if task.is_none() && workstream.is_none() {
            return Err(WorkError::invalid(
                "A comment belongs to a task or a workstream.",
            ));
        }
        not_empty(text, "text")?;
        let w = self.work;
        let _guard = w.lock();
        w.read(|c| {
            if let Some(id) = &task
                && query::task(c, &TaskRef::Id(*id))?.is_none()
            {
                return Err(no_task(&TaskRef::Id(*id)));
            }
            if let Some(id) = &workstream
                && query::workstream(c, id)?.is_none()
            {
                return Err(WorkError::not_found(format!("No workstream {id}.")));
            }
            for (i, id) in mentions.iter().enumerate() {
                known_member(c, id, &format!("mentions[{i}]"))?;
            }
            Ok(())
        })?;
        w.append(&[self.event(EventBody::CommentPosted {
            task,
            workstream,
            text: text.to_owned(),
            mentions: mentions.to_vec(),
        })])?;
        Ok(())
    }
}

impl Commands for OfficeCommands<'_> {
    type Error = WorkError;

    fn append(&mut self, body: &EventBody, because: &[Receipt]) -> Result<()> {
        evidence(because)?;
        match body {
            EventBody::TaskMoved {
                task,
                from,
                to,
                mover,
            } => self.move_task(*task, *from, *to, *mover).map(drop),
            EventBody::AskAnswered { ask, answer } => self.answer(*ask, answer).map(drop),
            EventBody::CommentPosted {
                task,
                workstream,
                text,
                mentions,
            } => self.comment(*task, *workstream, text, mentions),
            _ => Err(WorkError::forbidden(
                "The back office appends only task moves, answers to agents and comments.",
            )),
        }
    }

    fn raise_ask(&mut self, draft: &AskDraft) -> Result<()> {
        if draft.kind == AskKind::Approval {
            return Err(WorkError::forbidden(
                "The back office never raises approvals: they are how outward writes are \
                 requested.",
            ));
        }
        evidence(&draft.receipts)?;
        not_empty(&draft.title, "title")?;
        let w = self.work;
        let _guard = w.lock();
        w.read(|c| {
            known_member(c, &draft.to, "to")?;
            if let Some(id) = &draft.task
                && query::task(c, &TaskRef::Id(*id))?.is_none()
            {
                return Err(WorkError::invalid(format!("task: no task {id}.")));
            }
            if let Some(id) = &draft.session
                && query::session(c, id)?.is_none()
            {
                return Err(WorkError::invalid(format!("session: no session {id}.")));
            }
            Ok(())
        })?;
        let ask = Ask {
            id: AskId::new(),
            kind: draft.kind,
            from: self.member,
            to: draft.to,
            task: draft.task,
            session: draft.session,
            title: draft.title.clone(),
            body: draft.body.clone(),
            options: draft.options.clone(),
            receipts: draft.receipts.clone(),
            state: AskState::Open,
            answer: None,
            created: w.now(),
        };
        w.append(&[self.event(EventBody::AskRaised { ask })])?;
        Ok(())
    }

    fn propose_brief(&mut self, proposal: &BriefProposal) -> Result<()> {
        evidence(&proposal.receipts)?;
        not_empty(proposal.text(), "text")?;
        let w = self.work;
        let _guard = w.lock();
        let pinned = w.read(|c| {
            brief_target_exists(c, &proposal.target)?;
            Ok(query::brief(c, &proposal.target)?.is_some_and(|b| b.pinned))
        })?;
        let mut events = vec![self.event(proposal.body())];
        match proposal.accepted_body() {
            Some(accepted) if !pinned => events.push(self.event(accepted)),
            Some(_) => tracing::info!(
                brief = ?proposal.target,
                "the brief is pinned: the back office's proposal is only proposed"
            ),
            None => {}
        }
        w.append(&events)?;
        Ok(())
    }
}
