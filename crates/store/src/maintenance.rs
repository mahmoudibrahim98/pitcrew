//! Maintenance: integrity checks, and the JSON-line encoding [`Store::export`](crate::Store) and
//! [`Store::import`](crate::Store) share. `Store::snapshot`, `Store::export` and `Store::import`
//! themselves live on `Store` (in `store.rs`), since they need its connections; this module holds
//! the parts that do not.

use crate::error::{Error, Result};
use crate::sql::Connection;
use pitcrew_protocol::events::Event;

/// The result of `PRAGMA quick_check` or `PRAGMA integrity_check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityReport {
    /// No problems found.
    Ok,
    /// Problems found: one message per row SQLite reported, or, for a database too damaged for
    /// the pragma itself to run, that error's message.
    Failed(Vec<String>),
}

/// Runs `PRAGMA quick_check` (or, if `full`, the slower `PRAGMA integrity_check`) on `conn` and
/// reports the result plainly. Takes a plain connection, not a [`Store`](crate::Store), so
/// support tooling can check a snapshot or a raw copy of a database file directly, without
/// opening it (which would run migrations against a possibly-corrupt file).
///
/// # Errors
///
/// Never: a corrupt or unreadable database is reported as `IntegrityReport::Failed`, not an
/// `Err`. Kept fallible for symmetry with the rest of the crate.
pub fn integrity_check(conn: &Connection, full: bool) -> Result<IntegrityReport> {
    let pragma = if full {
        "integrity_check"
    } else {
        "quick_check"
    };
    let rows = conn
        .prepare(&format!("PRAGMA {pragma}"))
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        });
    Ok(match rows {
        Ok(rows) if rows.len() == 1 && rows[0].eq_ignore_ascii_case("ok") => IntegrityReport::Ok,
        Ok(rows) => IntegrityReport::Failed(rows),
        Err(e) => IntegrityReport::Failed(vec![e.to_string()]),
    })
}

/// One `export` line: an event, with no revision (import assigns fresh ones in the same order).
pub(crate) fn encode_line(event: &Event) -> Result<String> {
    Ok(serde_json::to_string(event)?)
}

/// The inverse of [`encode_line`].
pub(crate) fn decode_line(line: &str) -> Result<Event> {
    serde_json::from_str(line).map_err(|e| Error::Corrupt {
        rev: 0,
        reason: format!("export line: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_check_reports_ok_for_a_fresh_database() {
        let conn = Connection::open_in_memory().expect("open");
        assert_eq!(
            integrity_check(&conn, false).expect("check"),
            IntegrityReport::Ok
        );
        assert_eq!(
            integrity_check(&conn, true).expect("check"),
            IntegrityReport::Ok
        );
    }
}
