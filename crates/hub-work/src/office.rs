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
//! # Running a range again is safe
//!
//! The events an action appends get ids derived from the store's log id, the run-log entry's
//! revision and place (`seq`), and the event's place within the action, and are appended with
//! `Store::append_new`. An action whose first event is already in the log was applied before: it
//! is reported as replayed and nothing is appended. So the daemon may re-run a range after a
//! restart, e.g. the last one it is not sure it finished.
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
//! - **answers**, as for any agent: only open questions and mentions addressed to the back office
//!   itself. Never a person's ask, and never a decision, approval or review;
//! - **asks** name a known addressee, task and session, and are never approvals;
//! - **comments** name a known task or workstream (a task in that workstream, when both) and known
//!   mentions;
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
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{AskId, EventId, MemberId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    Answer, Ask, AskKind, AskState, Member, MemberKind, Mover, Receipt, Task, TaskStatus,
    TimestampMs,
};
use pitcrew_recap::BriefProposal;
use pitcrew_store::sql::{Connection, OptionalExtension, params};
use pitcrew_store::{Projection, RevRange};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::sync::Arc;

type RuleSet = Arc<dyn Fn() -> Vec<Box<dyn Rule>> + Send + Sync>;

/// The back office of one hub: its member, settings and rules. Build it once; the store gets its
/// run log ([`projections_with_office`]) and [`WorkService::run_office`] applies what it emitted,
/// so both always use the same [`Config`].
pub struct BackOffice {
    member: MemberId,
    config: Config,
    rules: RuleSet,
    /// How many run-log entries to read at once: more than one revision can hold with these rules,
    /// so a full page always ends with whole revisions before its last one.
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
    /// `Ok` when the hub applied it, now or before; else why not: a shape `apply` refused, or the
    /// hub's refusal.
    pub result: std::result::Result<(), ApplyError<WorkError>>,
    /// Whether it had been applied before (a range run again): its events were already in the log,
    /// so nothing was appended.
    pub replayed: bool,
}

/// What [`WorkService::run_office`] did.
#[derive(Debug, Default)]
pub struct OfficeRun {
    /// Every emitted action of the revisions, in log order, with its result.
    pub actions: Vec<Applied>,
}

impl OfficeRun {
    /// The actions the hub applied in this run.
    pub fn applied(&self) -> impl Iterator<Item = &Applied> {
        self.actions
            .iter()
            .filter(|a| a.result.is_ok() && !a.replayed)
    }

    /// The actions an earlier run had applied already.
    pub fn replayed(&self) -> impl Iterator<Item = &Applied> {
        self.actions.iter().filter(|a| a.replayed)
    }

    /// The actions the hub did not apply.
    pub fn refused(&self) -> impl Iterator<Item = &Applied> {
        self.actions.iter().filter(|a| a.result.is_err())
    }
}

/// The run-log entry an action came from, so its events' ids can be derived from it.
#[derive(Debug)]
struct Origin {
    log: String,
    rev: u64,
    seq: u32,
    at: TimestampMs,
    /// The place of the action's next event.
    next: Cell<u32>,
}

impl Origin {
    fn new(log: &str, entry: &Entry) -> Self {
        Self {
            log: log.to_owned(),
            rev: entry.rev,
            seq: entry.seq,
            at: entry.at,
            next: Cell::new(0),
        }
    }

    /// The id of the action's `n`th event: a ULID with the entry's time, and 80 bits of the
    /// SHA-256 of the log id, the revision, the entry's place and `n`. The same run-log entry in
    /// the same log always gives the same ids.
    fn id(&self, n: u32) -> EventId {
        let mut hash = Sha256::new();
        hash.update(b"pitcrew.office.action.v1\0");
        hash.update(self.log.as_bytes());
        hash.update([0]);
        hash.update(self.rev.to_be_bytes());
        hash.update(self.seq.to_be_bytes());
        hash.update(n.to_be_bytes());
        let digest = hash.finalize();
        let mut random = [0u8; 16];
        random[6..].copy_from_slice(&digest[..10]);
        let ms = u64::try_from(self.at).unwrap_or(0);
        EventId(ulid::Ulid::from_parts(ms, u128::from_be_bytes(random)))
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
    /// While [`WorkService::run_office`] applies a run-log entry: where its event ids come from.
    origin: Option<Origin>,
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

/// Whether the log holds an event with this id (the log's `events.id` is unique).
fn in_log(conn: &Connection, id: &EventId) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM events WHERE id = ?1")?
        .query_row(params![id.0.to_string()], |_| Ok(()))
        .optional()?
        .is_some())
}

/// The handle of the back office's member, `@office`.
pub const OFFICE_HANDLE: &str = "@office";
/// The display name [`WorkService::ensure_office_member`] gives a member it adds.
pub const OFFICE_NAME: &str = "Back office";

impl WorkService {
    /// The back office's member for a workspace whose person is `owner`, found or added: the
    /// member holding [`OFFICE_HANDLE`] when it is an agent of `owner`, else, when no member holds
    /// the handle, a new one (agent, [`OFFICE_NAME`], owned by `owner`) appended in a
    /// `member_added` authored by `owner`. Through the one writer, so it may run while the hub
    /// serves, e.g. right after `set_up`.
    ///
    /// Found or added once, and reused for the life of the store: the run log's settings name the
    /// member, and must stay the same.
    ///
    /// # Errors
    ///
    /// `invalid` when `owner` is not a person of this workspace; `conflict` when `@office` is
    /// held by a person, by another person's agent, or by an agent of no one (the back office never
    /// acts as a person, nor for someone else); database errors.
    pub fn ensure_office_member(&self, owner: MemberId) -> Result<Member> {
        let _guard = self.lock();
        let (person, found) = self.read(|c| {
            Ok((
                query::member(c, &owner)?,
                query::member_with_handle(c, OFFICE_HANDLE)?,
            ))
        })?;
        if !person.is_some_and(|p| p.kind == MemberKind::Human) {
            return Err(WorkError::invalid(format!(
                "{owner} is not a person of this workspace; only a person owns the back office."
            )));
        }
        if let Some(found) = found {
            return match (found.kind, found.owner) {
                (MemberKind::Agent, Some(of)) if of == owner => Ok(found),
                (MemberKind::Agent, Some(of)) => Err(WorkError::conflict(format!(
                    "{OFFICE_HANDLE} is an agent of {of}, not of the workspace's person {owner}."
                ))),
                (MemberKind::Agent, None) => Err(WorkError::conflict(format!(
                    "{OFFICE_HANDLE} is an agent of no one."
                ))),
                (MemberKind::Human, _) => Err(WorkError::conflict(format!(
                    "{OFFICE_HANDLE} is a person in this workspace."
                ))),
            };
        }
        let office = Member {
            id: MemberId::new(),
            kind: MemberKind::Agent,
            handle: OFFICE_HANDLE.to_owned(),
            name: OFFICE_NAME.to_owned(),
            owner: Some(owner),
            persona: None,
        };
        self.append(&[self.event(
            owner,
            None,
            EventBody::MemberAdded {
                member: office.clone(),
            },
        )])?;
        Ok(office)
    }

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
                origin: None,
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
    /// may act on any event).
    ///
    /// **Running a range again is safe**: an action already applied is reported as
    /// [`Applied::replayed`] and appends nothing (see the [module docs](self)). After a restart,
    /// the daemon may re-run the range it is not sure it finished.
    ///
    /// # Errors
    ///
    /// When the back office's member is not an agent of the workspace (nothing is applied), when
    /// the run log has not reached `revs.to_rev` (it is not registered with the store), when a
    /// revision has more entries than this office's rules can make (the store's run log has other
    /// rules), or on database errors. Refused actions are not errors: see [`OfficeRun::refused`].
    pub fn run_office(&self, office: &BackOffice, revs: RevRange) -> Result<OfficeRun> {
        let mut run = OfficeRun::default();
        if revs.is_empty() {
            return Ok(run);
        }
        self.read(|c| run_log_reached(c, revs.to_rev))?;
        let mut commands = self.office_commands(office)?;
        let log = self.store().log_id().to_owned();
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
                if whole == 0 {
                    return Err(WorkError::internal(format!(
                        "revision {last} has more run-log entries than the back office's {} \
                         rules can make: the store's run log was registered with other rules",
                        office.page.saturating_sub(1) / MAX_ACTIONS_PER_EVENT
                    )));
                }
                batch.truncate(whole);
                after = batch.last().map_or(last, |e| e.rev);
            }
            for entry in batch.iter().filter(|e| e.outcome == Outcome::Emitted) {
                let origin = Origin::new(&log, entry);
                let first = origin.id(0);
                let applied = if self.read(|c| in_log(c, &first))? {
                    Applied {
                        entry: entry.clone(),
                        result: Ok(()),
                        replayed: true,
                    }
                } else {
                    commands.origin = Some(origin);
                    let result = apply(std::slice::from_ref(entry), &mut commands)
                        .pop()
                        .unwrap_or(Ok(()));
                    commands.origin = None;
                    if let Err(error) = &result {
                        tracing::warn!(
                            rule = %entry.rule,
                            rev = entry.rev,
                            seq = entry.seq,
                            error = ?error,
                            "the hub refused a back-office action"
                        );
                    }
                    Applied {
                        entry: entry.clone(),
                        result,
                        replayed: false,
                    }
                };
                run.actions.push(applied);
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

    /// An event of the office's, now. Its id comes from the run-log entry being applied, if any.
    fn event(&self, body: EventBody) -> Event {
        let mut event = self.work.event(self.member, self.owner, body);
        if let Some(origin) = &self.origin {
            let n = origin.next.get();
            origin.next.set(n.saturating_add(1));
            event.id = origin.id(n);
        }
        event
    }

    /// Appends the office's events in one transaction; any already in the log are skipped.
    fn append_events(&self, events: &[Event]) -> Result<()> {
        self.work.append_new(events).map(drop)
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
        if to == TaskStatus::Done && !w.auto_accept_allowed(self.member)? {
            return Err(WorkError::conflict(
                "Workspace safety settings disable or cap automatic acceptance.",
            ));
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
        self.append_events(&[self.event(body)])?;
        w.reload_moved(id, to)
    }

    /// An answer to an open question or mention addressed to the back office itself, as any
    /// agent may answer (api-v1, "Who may answer an ask").
    fn answer(&self, id: AskId, answer: &Answer) -> Result<Ask> {
        let w = self.work;
        let text = answer.text.clone().filter(|t| !t.trim().is_empty());
        let _guard = w.lock();
        let ask = w
            .read(|c| query::ask(c, &id))?
            .ok_or_else(|| WorkError::not_found(format!("No ask {id}.")))?;
        if ask.to != self.member {
            return Err(WorkError::forbidden(
                "The back office answers only asks addressed to itself.",
            ));
        }
        if !matches!(ask.kind, AskKind::Question | AskKind::Mention) {
            let kind = crate::codec::enum_text(&ask.kind).unwrap_or_default();
            return Err(WorkError::forbidden(format!(
                "A {kind} is answered only by a person."
            )));
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
        self.append_events(&[self.event(EventBody::AskAnswered { ask: id, answer })])?;
        w.ask(&id)
    }

    /// A comment on a known task or workstream; naming both, the task must be in that workstream.
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
            let found = match &task {
                Some(id) => Some(
                    query::task(c, &TaskRef::Id(*id))?.ok_or_else(|| no_task(&TaskRef::Id(*id)))?,
                ),
                None => None,
            };
            if let Some(id) = &workstream
                && query::workstream(c, id)?.is_none()
            {
                return Err(WorkError::not_found(format!("No workstream {id}.")));
            }
            if let (Some(found), Some(id)) = (&found, &workstream)
                && found.workstream != Some(*id)
            {
                return Err(WorkError::invalid(format!(
                    "{} is not in workstream {id}.",
                    found.key
                )));
            }
            if let Some((i, id)) = query::first_unknown_member(c, mentions)? {
                return Err(WorkError::invalid(format!(
                    "mentions[{i}]: no member {id}."
                )));
            }
            Ok(())
        })?;
        self.append_events(&[self.event(EventBody::CommentPosted {
            task,
            workstream,
            text: text.to_owned(),
            mentions: mentions.to_vec(),
        })])
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
                "The back office appends only task moves, answers to its own asks and comments.",
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
        self.append_events(&[self.event(EventBody::AskRaised { ask })])
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
            Some(accepted) if !pinned && w.auto_accept_allowed(self.member)? => {
                events.push(self.event(accepted))
            }
            Some(_) => tracing::info!(
                brief = ?proposal.target,
                "the brief is pinned or the safety budget forbids automatic acceptance: keeping the proposal"
            ),
            None => {}
        }
        self.append_events(&events)
    }
}
