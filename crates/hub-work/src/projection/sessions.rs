//! `work.sessions`: sessions as the hub sees them, and dispatches.
//!
//! **Firm links stay.** A link made by a dispatch, a person or the agent itself (`dispatch`,
//! `manual`, `claimed`, `imported`) is never replaced by an inferred one (`folder`, `branch`) or
//! by a re-stated `session_discovered` that carries no link. The runner re-states sessions as it
//! learns more about them, and must not undo a dispatch's link by doing so.
//!
//! **Agents stay too.** A re-stated session without an `agent` keeps the agent it had (the one a
//! dispatch named, say); one that names an agent takes it.
//!
//! **An ended session stays ended.** A re-stated `session_discovered` does not bring back a
//! session that has ended (one the hub ended because its CLI did not start, whose CLI then turns
//! up after all): it keeps its state and status line. A later `session_state_changed` still
//! applies (a resumed CLI works in its session again).

use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, opt_enum_col, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::{Dispatch, LinkBasis, Session, SessionState, TimestampMs};
use pitcrew_store::sql::{OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Sessions and dispatches.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sessions;

impl Sessions {
    /// The projection's name.
    pub const NAME: &'static str = "work.sessions";
    /// 4: imported links are firm, as in the runner. 5: a session's model and account.
    const VERSION: u32 = 5;
}

/// Whether a link made by a dispatch, a person or the agent itself.
fn is_firm(basis: Option<LinkBasis>) -> bool {
    matches!(
        basis,
        Some(LinkBasis::Dispatch | LinkBasis::Manual | LinkBasis::Claimed | LinkBasis::Imported)
    )
}

/// Whether a link with basis `incoming` may replace one with basis `existing`: anything replaces
/// an inferred link or none, and only a firm link replaces a firm one.
pub(crate) fn replaces_link(existing: Option<LinkBasis>, incoming: Option<LinkBasis>) -> bool {
    !is_firm(existing) || is_firm(incoming)
}

impl Projection for Sessions {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(tx, &["work_dispatches", "work_sessions"])
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let rev = sql_rev(stored.rev);
        let at = stored.event.at;
        match &stored.event.body {
            EventBody::SessionDiscovered { session } => session_discovered(tx, rev, session),
            EventBody::SessionStateChanged {
                session,
                to,
                status_line,
                ..
            } => {
                exec(
                    tx,
                    "UPDATE work_sessions SET state = ?2, status_line = ?3,
                       last_activity = MAX(last_activity, ?4)
                     WHERE id = ?1",
                    params![session.text(), enum_text(to)?, status_line, at],
                )?;
                Ok(())
            }
            EventBody::TurnEnded { session, .. }
            | EventBody::ToolRan { session, .. }
            | EventBody::FileEdited { session, .. } => touch(tx, session, at),
            EventBody::SessionUpdated {
                session,
                title,
                branch,
                model,
            } => {
                exec(
                    tx,
                    "UPDATE work_sessions SET title = COALESCE(?2, title),
                       branch = COALESCE(?3, branch), model = COALESCE(?4, model)
                     WHERE id = ?1",
                    params![session.text(), title, branch, model],
                )?;
                Ok(())
            }
            EventBody::SessionLinked {
                session,
                workstream,
                task,
                basis,
            } => {
                if !replaces_link(link_basis(tx, session)?, Some(*basis)) {
                    return Ok(());
                }
                exec(
                    tx,
                    "UPDATE work_sessions SET workstream = ?2, task = ?3, link_basis = ?4
                     WHERE id = ?1",
                    params![
                        session.text(),
                        opt_text(workstream.as_ref()),
                        opt_text(task.as_ref()),
                        enum_text(basis)?
                    ],
                )?;
                Ok(())
            }
            EventBody::SessionEnded { session } => {
                exec(
                    tx,
                    "UPDATE work_sessions SET state = ?2, last_activity = MAX(last_activity, ?3)
                     WHERE id = ?1",
                    params![session.text(), enum_text(&SessionState::Ended)?, at],
                )?;
                Ok(())
            }
            EventBody::DispatchStarted { dispatch } => dispatch_started(tx, rev, dispatch),
            EventBody::DispatchFinished {
                dispatch,
                outcome,
                summary,
            } => {
                exec(
                    tx,
                    "UPDATE work_dispatches SET ended = ?2, outcome = ?3, summary = ?4
                     WHERE id = ?1",
                    params![dispatch.text(), at, enum_text(outcome)?, summary],
                )?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// The basis of a known session's link; `None` for an unlinked or unknown session.
fn link_basis(tx: &Transaction<'_>, session: &SessionId) -> Result<Option<LinkBasis>, BoxError> {
    let basis = tx
        .prepare_cached("SELECT link_basis FROM work_sessions WHERE id = ?1")?
        .query_row(params![session.text()], |r| opt_enum_col(r, 0))
        .optional()?;
    Ok(basis.flatten())
}

/// Activity in a session moves its `last_activity` forward, never back.
fn touch(tx: &Transaction<'_>, session: &SessionId, at: TimestampMs) -> Applied {
    exec(
        tx,
        "UPDATE work_sessions SET last_activity = MAX(last_activity, ?2) WHERE id = ?1",
        params![session.text(), at],
    )?;
    Ok(())
}

fn session_discovered(tx: &Transaction<'_>, rev: i64, s: &Session) -> Applied {
    // A re-statement keeps a firm link it would otherwise lose, an agent it does not name, and an
    // end.
    let keep_link = !replaces_link(link_basis(tx, &s.id)?, s.link_basis);
    exec(
        tx,
        "INSERT INTO work_sessions (id, rev, engine, native_id, machine, cwd, branch, title, agent,
           workstream, task, link_basis, state, status_line, started, last_activity, terminal,
           parent, model, account)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
           ?21, ?22)
         ON CONFLICT (id) DO UPDATE SET engine = excluded.engine, native_id = excluded.native_id,
           machine = excluded.machine, cwd = excluded.cwd, branch = excluded.branch,
           title = excluded.title, agent = COALESCE(excluded.agent, agent),
           workstream = CASE WHEN ?19 THEN workstream ELSE excluded.workstream END,
           task = CASE WHEN ?19 THEN task ELSE excluded.task END,
           link_basis = CASE WHEN ?19 THEN link_basis ELSE excluded.link_basis END,
           state = CASE WHEN state = ?20 THEN state ELSE excluded.state END,
           status_line = CASE WHEN state = ?20 THEN status_line ELSE excluded.status_line END,
           started = excluded.started, last_activity = excluded.last_activity,
           terminal = excluded.terminal, parent = excluded.parent,
           model = COALESCE(excluded.model, model), account = COALESCE(excluded.account, account)",
        params![
            s.id.text(),
            rev,
            enum_text(&s.engine)?,
            s.native_id,
            s.machine.text(),
            s.cwd,
            s.branch,
            s.title,
            opt_text(s.agent.as_ref()),
            opt_text(s.workstream.as_ref()),
            opt_text(s.task.as_ref()),
            s.link_basis.as_ref().map(enum_text).transpose()?,
            enum_text(&s.state)?,
            s.status_line,
            s.started,
            s.last_activity,
            opt_text(s.terminal.as_ref()),
            opt_text(s.parent.as_ref()),
            keep_link,
            enum_text(&SessionState::Ended)?,
            s.recorded.as_ref().and_then(|r| r.model.as_deref()),
            s.recorded.as_ref().and_then(|r| r.account.as_deref()),
        ],
    )?;
    Ok(())
}

fn dispatch_started(tx: &Transaction<'_>, rev: i64, d: &Dispatch) -> Applied {
    exec(
        tx,
        "INSERT INTO work_dispatches (id, rev, task, agent, session, brief, started, ended,
           outcome, summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET task = excluded.task, agent = excluded.agent,
           session = excluded.session, brief = excluded.brief, started = excluded.started,
           ended = excluded.ended, outcome = excluded.outcome, summary = excluded.summary",
        params![
            d.id.text(),
            rev,
            d.task.text(),
            d.agent.text(),
            opt_text(d.session.as_ref()),
            d.brief,
            d.started,
            d.ended,
            d.outcome.as_ref().map(enum_text).transpose()?,
            d.summary,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firm_links_are_replaced_only_by_firm_links() {
        use LinkBasis::{Branch, Claimed, Dispatch, Folder, Imported, Manual};
        let firm = [Dispatch, Manual, Claimed, Imported];
        let inferred = [Folder, Branch];
        for existing in firm {
            for incoming in firm {
                assert!(replaces_link(Some(existing), Some(incoming)));
            }
            for incoming in inferred {
                assert!(!replaces_link(Some(existing), Some(incoming)));
            }
            assert!(!replaces_link(Some(existing), None));
        }
        for existing in inferred.map(Some).into_iter().chain([None]) {
            for incoming in firm.into_iter().chain(inferred).map(Some).chain([None]) {
                assert!(replaces_link(existing, incoming));
            }
        }
    }
}
