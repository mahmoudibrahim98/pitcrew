//! `work.briefs`: the "Where it stands" in force for each project and workstream, and each one's
//! pending proposal. The rules are api-v1's ("Briefs"), applied in revision order:
//!
//! - The brief in force is the one the newest `brief_accepted` put there, with its `text`, `next`,
//!   `pinned` and `receipts` as the event carries them, and `updated` the event's time.
//! - The **pending proposal** is the newest `brief_proposed` for the target, if no `brief_accepted`
//!   for the target came after it. `work_brief_proposals` holds exactly those: a `brief_proposed`
//!   replaces the target's row, and a `brief_accepted` removes it.
//! - A `brief_accepted` is the back office's (`source: back_office`) when its `text` and `next`
//!   both equal the pending proposal's (a person accepted it unchanged; a missing `next` equals only
//!   a missing `next`), or when an agent wrote it (the back office applying a brief itself, so
//!   `on_behalf_of` is set). Otherwise it is the person's.
//!
//! "Keep current" is a `brief_accepted` of the current text: it is newer than the proposal, so it
//! clears it, and the brief becomes the person's.

use super::{clear, exec};
use crate::codec::{IdText, enum_text, json, sql_rev};
use pitcrew_protocol::events::{BriefTarget, EventBody};
use pitcrew_protocol::model::BriefSource;
use pitcrew_store::sql::{OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Briefs in force and their pending proposals.
#[derive(Debug, Clone, Copy, Default)]
pub struct Briefs;

impl Briefs {
    /// The projection's name.
    pub const NAME: &'static str = "work.briefs";
    /// 2: `next` and receipts come from the events, and only the pending proposal is kept.
    const VERSION: u32 = 2;
}

/// `(target_kind, target_id)` columns of a target.
pub(crate) fn target_columns(target: &BriefTarget) -> (&'static str, String) {
    match target {
        BriefTarget::Project(id) => ("project", id.text()),
        BriefTarget::Workstream(id) => ("workstream", id.text()),
    }
}

impl Projection for Briefs {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(tx, &["work_brief_proposals", "work_briefs"])
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let e = &stored.event;
        let rev = sql_rev(stored.rev);
        match &e.body {
            EventBody::BriefProposed {
                target,
                text,
                next,
                receipts,
            } => {
                let (kind, id) = target_columns(target);
                exec(
                    tx,
                    "INSERT INTO work_brief_proposals (target_kind, target_id, event, rev, at,
                       author, text, next, receipts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT (target_kind, target_id) DO UPDATE SET event = excluded.event,
                       rev = excluded.rev, at = excluded.at, author = excluded.author,
                       text = excluded.text, next = excluded.next, receipts = excluded.receipts",
                    params![
                        kind,
                        id,
                        e.id.text(),
                        rev,
                        e.at,
                        e.author.text(),
                        text,
                        next,
                        json(receipts)?
                    ],
                )?;
                Ok(())
            }
            EventBody::BriefAccepted {
                target,
                text,
                next,
                pinned,
                receipts,
            } => {
                let accepts_proposal = accepts_pending(tx, target, text, next.as_deref())?;
                let source = if accepts_proposal || e.on_behalf_of.is_some() {
                    BriefSource::BackOffice
                } else {
                    BriefSource::Person
                };
                let (kind, id) = target_columns(target);
                exec(
                    tx,
                    "INSERT INTO work_briefs (target_kind, target_id, rev, text, next, pinned,
                       source, updated, receipts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT (target_kind, target_id) DO UPDATE SET text = excluded.text,
                       next = excluded.next, pinned = excluded.pinned, source = excluded.source,
                       updated = excluded.updated, receipts = excluded.receipts",
                    params![
                        kind,
                        id,
                        rev,
                        text,
                        next,
                        pinned,
                        enum_text(&source)?,
                        e.at,
                        json(receipts)?
                    ],
                )?;
                // The brief just put in force is newer than any proposal: nothing is pending.
                exec(
                    tx,
                    "DELETE FROM work_brief_proposals WHERE target_kind = ?1 AND target_id = ?2",
                    params![kind, id],
                )?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Whether `text` and `next` are exactly the target's pending proposal.
fn accepts_pending(
    tx: &Transaction<'_>,
    target: &BriefTarget,
    text: &str,
    next: Option<&str>,
) -> Result<bool, BoxError> {
    let (kind, id) = target_columns(target);
    let pending: Option<(String, Option<String>)> = tx
        .prepare_cached(
            "SELECT text, next FROM work_brief_proposals WHERE target_kind = ?1 AND target_id = ?2",
        )?
        .query_row(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(pending.is_some_and(|(t, n)| t == text && n.as_deref() == next))
}
