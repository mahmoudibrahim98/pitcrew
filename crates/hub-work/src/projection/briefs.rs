//! `work.briefs`: the "Where it stands" in force for each project and workstream, and the latest
//! proposal for each.
//!
//! A `brief_accepted` event carries only the text and the pin, so the rest is derived here:
//! - `updated` is the event's time;
//! - it is the back office's (`source: back_office`) when the back office applied it itself (an
//!   agent's event, so `on_behalf_of` is set) or when its text is exactly the latest proposal's,
//!   i.e. a person accepted the proposal unchanged. Otherwise a person wrote it;
//! - an accepted proposal keeps the proposal's receipts; a person's own text has none.
//! - `next` is not in the event yet (a contract gap), so it is always empty.

use super::{clear, exec};
use crate::codec::{IdText, enum_text, json, sql_rev};
use pitcrew_protocol::events::{BriefTarget, EventBody};
use pitcrew_protocol::model::BriefSource;
use pitcrew_store::sql::{OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Briefs in force and their latest proposals.
#[derive(Debug, Clone, Copy, Default)]
pub struct Briefs;

impl Briefs {
    /// The projection's name.
    pub const NAME: &'static str = "work.briefs";
    const VERSION: u32 = 1;
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
                receipts,
                ..
            } => {
                let (kind, id) = target_columns(target);
                exec(
                    tx,
                    "INSERT INTO work_brief_proposals (target_kind, target_id, event, rev, at,
                       author, text, receipts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (target_kind, target_id) DO UPDATE SET event = excluded.event,
                       rev = excluded.rev, at = excluded.at, author = excluded.author,
                       text = excluded.text, receipts = excluded.receipts",
                    params![
                        kind,
                        id,
                        e.id.text(),
                        rev,
                        e.at,
                        e.author.text(),
                        text,
                        json(receipts)?
                    ],
                )?;
                Ok(())
            }
            EventBody::BriefAccepted {
                target,
                text,
                pinned,
                ..
            } => {
                let (kind, id) = target_columns(target);
                let proposal: Option<(String, String)> = tx
                    .prepare_cached(
                        "SELECT text, receipts FROM work_brief_proposals
                         WHERE target_kind = ?1 AND target_id = ?2",
                    )?
                    .query_row(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                let accepted = proposal.filter(|(proposed, _)| proposed == text);
                let source = if accepted.is_some() || e.on_behalf_of.is_some() {
                    BriefSource::BackOffice
                } else {
                    BriefSource::Person
                };
                let receipts = accepted.map_or_else(|| "[]".to_owned(), |(_, r)| r);
                exec(
                    tx,
                    "INSERT INTO work_briefs (target_kind, target_id, rev, text, next, pinned,
                       source, updated, receipts)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8)
                     ON CONFLICT (target_kind, target_id) DO UPDATE SET text = excluded.text,
                       next = excluded.next, pinned = excluded.pinned, source = excluded.source,
                       updated = excluded.updated, receipts = excluded.receipts",
                    params![
                        kind,
                        id,
                        rev,
                        text,
                        pinned,
                        enum_text(&source)?,
                        e.at,
                        receipts
                    ],
                )?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
