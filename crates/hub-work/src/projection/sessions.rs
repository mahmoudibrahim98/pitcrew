//! `work.sessions`: sessions as the hub sees them, and dispatches.

use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::{Dispatch, Session, SessionState, TimestampMs};
use pitcrew_store::sql::{Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Sessions and dispatches.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sessions;

impl Sessions {
    /// The projection's name.
    pub const NAME: &'static str = "work.sessions";
    const VERSION: u32 = 1;
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
            } => {
                exec(
                    tx,
                    "UPDATE work_sessions SET title = COALESCE(?2, title),
                       branch = COALESCE(?3, branch)
                     WHERE id = ?1",
                    params![session.text(), title, branch],
                )?;
                Ok(())
            }
            EventBody::SessionLinked {
                session,
                workstream,
                task,
                basis,
            } => {
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
    exec(
        tx,
        "INSERT INTO work_sessions (id, rev, engine, native_id, machine, cwd, branch, title, agent,
           workstream, task, link_basis, state, status_line, started, last_activity, terminal,
           parent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT (id) DO UPDATE SET engine = excluded.engine, native_id = excluded.native_id,
           machine = excluded.machine, cwd = excluded.cwd, branch = excluded.branch,
           title = excluded.title, agent = excluded.agent, workstream = excluded.workstream,
           task = excluded.task, link_basis = excluded.link_basis, state = excluded.state,
           status_line = excluded.status_line, started = excluded.started,
           last_activity = excluded.last_activity, terminal = excluded.terminal,
           parent = excluded.parent",
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
