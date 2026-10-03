//! Read cursors use commands and a projection, so replay and live devices agree.

use crate::codec::IdText;
use crate::commands::require_person;
use crate::{Result, WorkError, WorkService, query};
use pitcrew_protocol::api::{Caller, ReadCursor};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{ProjectId, WorkstreamId};
use pitcrew_store::sql::params;

impl WorkService {
    /// All cursors belonging to this person.
    ///
    /// # Errors
    /// Agents are forbidden; database failures are internal errors.
    pub fn cursors(&self, caller: &Caller) -> Result<Vec<ReadCursor>> {
        require_person(caller, "Reading cursors")?;
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT scope, rev FROM work_read_cursors WHERE member = ?1 ORDER BY scope",
            )?;
            let rows = stmt.query_map(params![caller.member.text()], |row| {
                Ok(ReadCursor {
                    scope: row.get(0)?,
                    rev: row.get::<_, i64>(1)?.unsigned_abs(),
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Advance a scope to a revision already present in the log.
    ///
    /// # Errors
    /// Forbidden for agents, invalid scope/revision, unknown scope or database error.
    pub fn move_cursor(&self, caller: &Caller, scope: &str, rev: u64) -> Result<ReadCursor> {
        require_person(caller, "Moving a cursor")?;
        let _guard = self.lock();
        self.read(|conn| {
            if scope == "workspace" {
                return Ok(());
            }
            if let Some(id) = scope.strip_prefix("project:") {
                let id: ProjectId = id
                    .parse()
                    .map_err(|_| WorkError::invalid("Invalid cursor scope."))?;
                if id.text() != scope[8..] {
                    return Err(WorkError::invalid("Use a bare project id."));
                }
                if query::project(conn, &id)?.is_none() {
                    return Err(WorkError::not_found("Unknown project."));
                }
            } else if let Some(id) = scope.strip_prefix("workstream:") {
                let id: WorkstreamId = id
                    .parse()
                    .map_err(|_| WorkError::invalid("Invalid cursor scope."))?;
                if id.text() != scope[11..] {
                    return Err(WorkError::invalid("Use a bare workstream id."));
                }
                if query::workstream(conn, &id)?.is_none() {
                    return Err(WorkError::not_found("Unknown workstream."));
                }
            } else {
                return Err(WorkError::invalid("Invalid cursor scope."));
            }
            Ok(())
        })?;
        if rev > self.store().latest_rev()? {
            return Err(WorkError::invalid("Revision is ahead of the log."));
        }
        let current = self
            .cursors(caller)?
            .into_iter()
            .find(|c| c.scope == scope)
            .map_or(0, |c| c.rev);
        if rev > current {
            self.append(&[self.by(
                caller,
                EventBody::CursorMoved {
                    scope: scope.to_owned(),
                    rev,
                },
            )])?;
        }
        Ok(ReadCursor {
            scope: scope.to_owned(),
            rev: current.max(rev),
        })
    }
}
