//! Outward writes to GitHub and Jira, the hub's side (api-v1.md, "Outward writes"): the commands
//! a tracker sync proposes, starts and finishes writes through, and the reads of
//! [`crate::projection::Writes`].
//!
//! This crate never reaches GitHub or Jira: the daemon decides what a change implies, sends it,
//! and reports back through these commands. They are the gate the guarantee rests on:
//!
//! - [`SyncCommands::propose_write`] raises the approval ask and appends `write_proposed` in one
//!   append, so a write and its ask exist together or not at all; one cause proposes once;
//! - [`WorkService::request_retry`] appends `write_retry_requested`, by the person who asks to send
//!   a failed write again (one who may answer its ask);
//! - [`SyncCommands::start_write`] appends `write_started` only for a write whose ask this sync's
//!   member raised, answered with "Send" by a person ([`APPROVAL_OPTIONS`]), or for a failed write
//!   with a person's retry request no attempt has used yet; every other state is refused, so a
//!   write is never started twice at once, nor at all without that answer or request;
//! - [`SyncCommands::finish_write`] appends `write_finished`: `sent` or `failed` only after a
//!   start, `not_sent` only for a write never started since its last failure.

use crate::error::{Result, WorkError};
use crate::query::{self, TaskRef};
use crate::service::WorkService;
use crate::sync::{Outcome, SyncCommands, fit_title};
use pitcrew_protocol::api::Caller;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{AskId, EventId, MemberId, TaskId};
use pitcrew_protocol::model::{Ask, AskKind, AskState, MemberKind, Receipt};
use pitcrew_protocol::writes::{
    APPROVAL_OPTIONS, SEND_OPTION, UpstreamWrite, WriteProposal, WriteResult, WriteState,
};
use pitcrew_store::sql::{self, Connection, OptionalExtension, Row, params};

/// The longest approval ask title, in characters.
const ASK_TITLE_CHARS: usize = 200;

/// Which writes to list: those of `task` (when given) in any of `states` (all when empty).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteFilter {
    /// Only this task's.
    pub task: Option<TaskId>,
    /// Only these states.
    pub states: Vec<WriteState>,
}

const COLS: &str = "proposal, state, attempts, proposed_at, answered_at, answered_by, finished_at, \
     result, retry_requested_by";

fn conversion(idx: usize, e: impl std::error::Error + Send + Sync + 'static) -> sql::Error {
    sql::Error::FromSqlConversionFailure(idx, sql::types::Type::Text, Box::new(e))
}

fn json<T: serde::de::DeserializeOwned>(idx: usize, text: &str) -> sql::Result<T> {
    serde_json::from_str(text).map_err(|e| conversion(idx, e))
}

fn write_row(r: &Row<'_>) -> sql::Result<UpstreamWrite> {
    let proposal: String = r.get(0)?;
    let state: String = r.get(1)?;
    let answered_by: Option<String> = r.get(5)?;
    let result: Option<String> = r.get(7)?;
    let retry_requested_by: Option<String> = r.get(8)?;
    Ok(UpstreamWrite {
        proposal: json(0, &proposal)?,
        state: serde_json::from_value(serde_json::Value::String(state))
            .map_err(|e| conversion(1, e))?,
        attempts: r.get(2)?,
        proposed_at: r.get(3)?,
        answered_at: r.get(4)?,
        answered_by: answered_by
            .map(|id| id.parse::<MemberId>().map_err(|e| conversion(5, e)))
            .transpose()?,
        finished_at: r.get(6)?,
        result: result.map(|text| json(7, &text)).transpose()?,
        retry_requested_by: retry_requested_by
            .map(|id| id.parse::<MemberId>().map_err(|e| conversion(8, e)))
            .transpose()?,
    })
}

fn state_text(state: WriteState) -> Result<String> {
    Ok(crate::codec::enum_text(&state)?)
}

/// Writes matching `filter`, oldest first.
///
/// # Errors
///
/// Database errors.
pub fn writes(conn: &Connection, filter: &WriteFilter) -> Result<Vec<UpstreamWrite>> {
    let mut clauses = Vec::new();
    let mut values: Vec<String> = Vec::new();
    if let Some(task) = &filter.task {
        values.push(task.0.to_string());
        clauses.push(format!("task = ?{}", values.len()));
    }
    if !filter.states.is_empty() {
        let mut marks = Vec::new();
        for state in &filter.states {
            values.push(state_text(*state)?);
            marks.push(format!("?{}", values.len()));
        }
        clauses.push(format!("state IN ({})", marks.join(", ")));
    }
    let filter = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM work_writes {filter} ORDER BY rev, ask"
    ))?;
    let rows = stmt.query_map(sql::params_from_iter(values.iter()), write_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// One write, by its ask.
///
/// # Errors
///
/// Database errors.
pub fn write(conn: &Connection, ask: &AskId) -> Result<Option<UpstreamWrite>> {
    Ok(conn
        .prepare_cached(&format!("SELECT {COLS} FROM work_writes WHERE ask = ?1"))?
        .query_row(params![ask.0.to_string()], write_row)
        .optional()?)
}

/// Whether a write was proposed for `cause`.
fn proposed_for(conn: &Connection, cause: &EventId) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM work_writes WHERE cause = ?1 LIMIT 1")?
        .query_row(params![cause.0.to_string()], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Why a failed write may not start again, if it may not: a person must have asked since its
/// last attempt.
fn retry_refusal(c: &Connection, by: Option<&MemberId>) -> Result<Option<String>> {
    let Some(by) = by else {
        return Ok(Some(
            "This write failed, and no one asked to send it again since.".into(),
        ));
    };
    let person = query::member(c, by)?.is_some_and(|m| m.kind == MemberKind::Human);
    Ok((!person).then(|| "Its retry was not asked for by a person.".into()))
}

fn no_write(ask: &AskId) -> WorkError {
    WorkError::not_found(format!("No write {ask}."))
}

fn state_name(state: WriteState) -> String {
    crate::codec::enum_text(&state).unwrap_or_default()
}

impl WorkService {
    /// Writes matching `filter`, oldest first.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn writes(&self, filter: &WriteFilter) -> Result<Vec<UpstreamWrite>> {
        self.read(|c| writes(c, filter))
    }

    /// The write whose approval ask is `ask`.
    ///
    /// # Errors
    ///
    /// `not_found`; database errors.
    pub fn write(&self, ask: &AskId) -> Result<UpstreamWrite> {
        self.read(|c| write(c, ask))?.ok_or_else(|| no_write(ask))
    }

    /// `POST /v1/writes/{id}/retry`: records that `caller` asks to send the failed write `ask`
    /// again (`write_retry_requested`, authored by the caller). A request already waiting is
    /// kept, and nothing more is appended. The sync sends it on its next pass
    /// ([`SyncCommands::start_write`] uses the request).
    ///
    /// # Errors
    ///
    /// As [`WorkService::check_retry`] (and `forbidden` for a reader token); database errors.
    pub fn request_retry(&self, caller: &Caller, ask: &AskId) -> Result<UpstreamWrite> {
        crate::commands::require_writer(caller)?;
        let _guard = self.lock();
        let found = self.check_retry(caller, ask)?;
        if found.retry_requested_by.is_some() {
            return Ok(found);
        }
        self.append(&[self.by(
            caller,
            EventBody::WriteRetryRequested {
                ask: *ask,
                task: found.proposal.task,
                by: caller.member,
            },
        )])?;
        self.write(ask)
    }

    /// The write `ask`, when `caller` may retry it (`POST /v1/writes/{id}/retry`): a person who
    /// may answer its ask, and a write whose last attempt failed.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown write; `forbidden` for a caller who may not answer its ask;
    /// `conflict` for a write that did not fail.
    pub fn check_retry(&self, caller: &Caller, ask: &AskId) -> Result<UpstreamWrite> {
        self.read(|c| {
            let found = write(c, ask)?.ok_or_else(|| no_write(ask))?;
            let raised = query::ask(c, ask)?
                .ok_or_else(|| WorkError::internal("a write without its ask"))?;
            if let Some(refusal) = crate::commands::answer_refusal(c, caller, &raised)? {
                return Err(WorkError::forbidden(refusal));
            }
            if found.state != WriteState::Failed {
                return Err(WorkError::conflict(format!(
                    "Only a failed write is sent again; this one is {}.",
                    state_name(found.state)
                )));
            }
            Ok(found)
        })
    }
}

impl SyncCommands<'_> {
    /// Proposes `write` (its `ask` is replaced by the new ask's id): raises an approval ask from
    /// the sync to `to`, with the task, `title`, `body` and [`APPROVAL_OPTIONS`] (and the cause as
    /// a receipt), and appends `write_proposed` with it. `None` when a write was already proposed
    /// for the same cause.
    ///
    /// # Errors
    ///
    /// `invalid` when `to` is not a person, the task is unknown, or the write sends nothing;
    /// database errors.
    pub fn propose_write(
        &self,
        to: MemberId,
        mut write: WriteProposal,
        title: &str,
        body: &str,
    ) -> Result<Option<UpstreamWrite>> {
        if write.after.is_empty() {
            return Err(WorkError::invalid("A write must send at least one field."));
        }
        let title = fit_title(title, "Send a change upstream?");
        let title: String = title.chars().take(ASK_TITLE_CHARS).collect();
        let _guard = self.work().lock();
        let duplicate = self.work().read(|c| {
            match query::member(c, &to)? {
                Some(m) if m.kind == MemberKind::Human => {}
                _ => {
                    return Err(WorkError::invalid(format!(
                        "{to} is not a person; only a person approves a write."
                    )));
                }
            }
            if let Some(id) = &write.task {
                query::task(c, &TaskRef::Id(*id))?
                    .ok_or_else(|| WorkError::invalid(format!("task: no task {id}.")))?;
            }
            match &write.cause {
                Some(cause) => proposed_for(c, cause),
                None => Ok(false),
            }
        })?;
        if duplicate {
            return Ok(None);
        }
        let ask = Ask {
            id: AskId::new(),
            kind: AskKind::Approval,
            from: self.member(),
            to,
            task: write.task,
            session: None,
            title,
            body: body.to_owned(),
            options: APPROVAL_OPTIONS.iter().map(|o| (*o).to_owned()).collect(),
            receipts: write
                .cause
                .iter()
                .map(|id| Receipt::Event { id: *id })
                .collect(),
            state: AskState::Open,
            answer: None,
            created: self.work().now(),
        };
        write.ask = ask.id;
        let id = ask.id;
        self.work().append(&[
            self.sync_event(EventBody::AskRaised { ask }),
            self.sync_event(EventBody::WriteProposed {
                write: Box::new(write),
            }),
        ])?;
        self.work().write(&id).map(Some)
    }

    /// Starts sending the write `ask` (`write_started`): only when its ask is this sync's own
    /// approval ask, answered "Send" by a person, and it was never started; or when its last
    /// attempt failed and a person asked to retry it since (`write_retry_requested`, used by this
    /// start). Refused otherwise, with nothing appended.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown write; database errors.
    pub fn start_write(&self, ask: &AskId) -> Result<Outcome<UpstreamWrite>> {
        let _guard = self.work().lock();
        let (found, refusal) = self.work().read(|c| {
            let found = write(c, ask)?.ok_or_else(|| no_write(ask))?;
            let refusal = match found.state {
                WriteState::Approved => self.approval_refusal(c, ask)?,
                WriteState::Failed => retry_refusal(c, found.retry_requested_by.as_ref())?,
                other => Some(format!(
                    "This write is {}; only an approved or failed write is sent.",
                    state_name(other)
                )),
            };
            Ok((found, refusal))
        })?;
        if let Some(reason) = refusal {
            return Ok(Outcome::Refused(reason));
        }
        self.work()
            .append(&[self.sync_event(EventBody::WriteStarted {
                ask: *ask,
                task: found.proposal.task,
                attempt: found.attempts.saturating_add(1),
            })])?;
        Ok(Outcome::Changed(self.work().write(ask)?))
    }

    /// Why the approved write `ask` may not be sent, if it may not: its ask must be the sync's
    /// own approval ask, answered with "Send" by a person of this workspace.
    fn approval_refusal(&self, c: &Connection, ask: &AskId) -> Result<Option<String>> {
        let Some(raised) = query::ask(c, ask)? else {
            return Ok(Some("Its approval ask is missing.".into()));
        };
        if raised.kind != AskKind::Approval || raised.from != self.member() {
            return Ok(Some("Its ask is not the sync's approval ask.".into()));
        }
        let Some(answer) = raised.answer.as_ref() else {
            return Ok(Some("Its approval ask is not answered.".into()));
        };
        if answer.option != Some(SEND_OPTION) {
            return Ok(Some("Its approval ask was not answered \"Send\".".into()));
        }
        let by_person = query::member(c, &answer.by)?.is_some_and(|m| m.kind == MemberKind::Human);
        Ok((!by_person).then(|| "Its approval ask was not answered by a person.".into()))
    }

    /// Records what came of the write `ask` (`write_finished`): `sent` or `failed` only while it
    /// is being sent; `not_sent` only while it is pending, answered or failed. Refused otherwise,
    /// with nothing appended.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown write; database errors.
    pub fn finish_write(&self, ask: &AskId, result: WriteResult) -> Result<Outcome<UpstreamWrite>> {
        let _guard = self.work().lock();
        let found = self
            .work()
            .read(|c| write(c, ask)?.ok_or_else(|| no_write(ask)))?;
        let allowed = match &result {
            WriteResult::Sent { .. } | WriteResult::Failed { .. } => {
                found.state == WriteState::Sending
            }
            WriteResult::NotSent { .. } => matches!(
                found.state,
                WriteState::Pending
                    | WriteState::Approved
                    | WriteState::Denied
                    | WriteState::Failed
            ),
        };
        if !allowed {
            return Ok(Outcome::Refused(format!(
                "This write is {}.",
                state_name(found.state)
            )));
        }
        self.work()
            .append(&[self.sync_event(EventBody::WriteFinished {
                ask: *ask,
                task: found.proposal.task,
                result,
            })])?;
        Ok(Outcome::Changed(self.work().write(ask)?))
    }
}
