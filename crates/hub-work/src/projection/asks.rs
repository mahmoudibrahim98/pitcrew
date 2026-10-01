//! `work.asks`: what needs a member's answer.

use super::{clear, exec};
use crate::codec::{IdText, enum_text, json, opt_json, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::{Ask, AskState};
use pitcrew_store::sql::{Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Asks, open and answered.
#[derive(Debug, Clone, Copy, Default)]
pub struct Asks;

impl Asks {
    /// The projection's name.
    pub const NAME: &'static str = "work.asks";
    const VERSION: u32 = 1;
}

impl Projection for Asks {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(tx, &["work_asks"])
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        match &stored.event.body {
            EventBody::AskRaised { ask } => ask_raised(tx, sql_rev(stored.rev), ask),
            EventBody::AskAnswered { ask, answer } => {
                exec(
                    tx,
                    "UPDATE work_asks SET state = ?2, answer = ?3 WHERE id = ?1",
                    params![ask.text(), enum_text(&AskState::Answered)?, json(answer)?],
                )?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn ask_raised(tx: &Transaction<'_>, rev: i64, a: &Ask) -> Result<(), BoxError> {
    exec(
        tx,
        "INSERT INTO work_asks (id, rev, kind, from_member, to_member, task, session, title, body,
           options, receipts, state, answer, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, from_member = excluded.from_member,
           to_member = excluded.to_member, task = excluded.task, session = excluded.session,
           title = excluded.title, body = excluded.body, options = excluded.options,
           receipts = excluded.receipts, state = excluded.state, answer = excluded.answer,
           created = excluded.created",
        params![
            a.id.text(),
            rev,
            enum_text(&a.kind)?,
            a.from.text(),
            a.to.text(),
            opt_text(a.task.as_ref()),
            opt_text(a.session.as_ref()),
            a.title,
            a.body,
            json(&a.options)?,
            json(&a.receipts)?,
            enum_text(&a.state)?,
            opt_json(a.answer.as_ref())?,
            a.created,
        ],
    )?;
    Ok(())
}
