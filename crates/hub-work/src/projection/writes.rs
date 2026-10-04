//! `work.writes`: outward writes to GitHub and Jira, each approved by a person first (api-v1.md,
//! "Outward writes"). One row per write, keyed by its approval ask:
//!
//! - `write_proposed` adds it, `pending`;
//! - `ask_answered` on its ask makes it `approved` (the first option, "Send") or `denied`, once;
//! - `write_started` makes an `approved` or `failed` write `sending`, one attempt more;
//! - `write_finished` records its result: `sent`, `failed` or `not_sent`. A `sent` or `not_sent`
//!   write never changes again.
//!
//! An event about a write the table does not hold, or one that does not fit its state, changes
//! nothing. The commands (`crate::writes`) check the same transitions before appending, so an
//! event the projection ignores only comes from a second writer.

use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, json, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::writes::{SEND_OPTION, WriteProposal, WriteResult, WriteState};
use pitcrew_store::sql::{Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Outward writes and where each stands.
#[derive(Debug, Clone, Copy, Default)]
pub struct Writes;

impl Writes {
    /// The projection's name.
    pub const NAME: &'static str = "work.writes";
    const VERSION: u32 = 1;
}

impl Projection for Writes {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(tx, &["work_writes"])
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let at = stored.event.at;
        match &stored.event.body {
            EventBody::WriteProposed { write } => proposed(tx, sql_rev(stored.rev), at, write),
            EventBody::AskAnswered { ask, answer } => {
                let state = if answer.option == Some(SEND_OPTION) {
                    WriteState::Approved
                } else {
                    WriteState::Denied
                };
                exec(
                    tx,
                    "UPDATE work_writes SET state = ?2, answered_at = ?3, answered_by = ?4
                     WHERE ask = ?1 AND state = ?5",
                    params![
                        ask.text(),
                        enum_text(&state)?,
                        answer.at,
                        answer.by.text(),
                        enum_text(&WriteState::Pending)?
                    ],
                )?;
                Ok(())
            }
            EventBody::WriteStarted { ask, .. } => {
                exec(
                    tx,
                    "UPDATE work_writes SET state = ?2, attempts = attempts + 1
                     WHERE ask = ?1 AND state IN (?3, ?4)",
                    params![
                        ask.text(),
                        enum_text(&WriteState::Sending)?,
                        enum_text(&WriteState::Approved)?,
                        enum_text(&WriteState::Failed)?
                    ],
                )?;
                Ok(())
            }
            EventBody::WriteFinished { ask, result, .. } => {
                let state = match result {
                    WriteResult::Sent { .. } => WriteState::Sent,
                    WriteResult::Failed { .. } => WriteState::Failed,
                    WriteResult::NotSent { .. } => WriteState::NotSent,
                };
                exec(
                    tx,
                    "UPDATE work_writes SET state = ?2, result = ?3, finished_at = ?4
                     WHERE ask = ?1 AND state NOT IN (?5, ?6)",
                    params![
                        ask.text(),
                        enum_text(&state)?,
                        json(result)?,
                        at,
                        enum_text(&WriteState::Sent)?,
                        enum_text(&WriteState::NotSent)?
                    ],
                )?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn proposed(tx: &Transaction<'_>, rev: i64, at: i64, w: &WriteProposal) -> Applied {
    exec(
        tx,
        "INSERT INTO work_writes (ask, rev, task, integration, cause, proposal, state, attempts,
           proposed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)
         ON CONFLICT (ask) DO UPDATE SET task = excluded.task,
           integration = excluded.integration, cause = excluded.cause,
           proposal = excluded.proposal",
        params![
            w.ask.text(),
            rev,
            opt_text(w.task.as_ref()),
            w.integration.text(),
            opt_text(w.cause.as_ref()),
            json(w)?,
            enum_text(&WriteState::Pending)?,
            at,
        ],
    )?;
    Ok(())
}
