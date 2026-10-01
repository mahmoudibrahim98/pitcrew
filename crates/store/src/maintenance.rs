//! Maintenance: integrity checks, and the JSON-line encoding [`Store::export`](crate::Store) and
//! [`Store::import`](crate::Store) share. `Store::snapshot`, `Store::export` and `Store::import`
//! themselves live on `Store` (in `store.rs`), since they need its connections; this module holds
//! the parts that do not.

use crate::error::{Error, Result};
use crate::sql::Connection;
use pitcrew_protocol::events::Event;
use std::io::BufRead;

/// The result of `PRAGMA quick_check` or `PRAGMA integrity_check`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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

/// The inverse of [`encode_line`]. `line_no` (1-based, counting every line `read_bounded_line`
/// returns, blank ones included, the way a text editor numbers them) is folded into the error so
/// a failed import names which line was bad.
pub(crate) fn decode_line(line: &str, line_no: u64) -> Result<Event> {
    serde_json::from_str(line).map_err(|e| Error::Corrupt {
        rev: 0,
        reason: format!("import line {line_no}: {e}"),
    })
}

/// The longest single line [`Store::import`](crate::Store::import) accepts: 16 MiB, the same cap
/// the ingest crate uses for hostile transcript lines (`docs/security/threat-model.md`, T20). A
/// corrupt or hostile export must not force unbounded memory before [`decode_line`] ever gets a
/// chance to reject it.
const MAX_IMPORT_LINE: usize = 16 * 1024 * 1024;

/// Reads one line from `reader`, the way [`std::io::BufRead::lines`] would (a trailing `\r\n` or
/// `\n` stripped; `Ok(None)` at a clean end of input), except bounded: a line over
/// [`MAX_IMPORT_LINE`] bytes is refused as [`Error::Corrupt`] without ever buffering past the cap
/// — unlike `lines()`, which grows its `String` without limit until it finds a newline or runs
/// out of input, so one adversarial or corrupt line with no newline for miles would otherwise
/// defeat `import`'s "memory stays bounded" design (the crate README, "Maintenance") even though
/// the 1,000-line batches themselves are bounded.
///
/// # Errors
///
/// [`Error::Io`] reading `reader`, or [`Error::Corrupt`] for a line over the cap or one that is
/// not valid UTF-8.
pub(crate) fn read_bounded_line(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(Error::Io)?;
        if available.is_empty() {
            // Clean end of input: the last line even without a trailing newline, as `lines()`
            // yields it, or nothing left to return.
            return if buf.is_empty() {
                Ok(None)
            } else {
                Ok(Some(finish_line(buf)?))
            };
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                if buf.len() + pos > MAX_IMPORT_LINE {
                    return Err(line_too_long());
                }
                buf.extend_from_slice(&available[..pos]);
                reader.consume(pos + 1);
                return Ok(Some(finish_line(buf)?));
            }
            None => {
                if buf.len() + available.len() > MAX_IMPORT_LINE {
                    return Err(line_too_long());
                }
                buf.extend_from_slice(available);
                let n = available.len();
                reader.consume(n);
            }
        }
    }
}

fn line_too_long() -> Error {
    Error::Corrupt {
        rev: 0,
        reason: format!("import line over {MAX_IMPORT_LINE} bytes"),
    }
}

/// Strips a trailing `\r` (left over from a `\r\n` ending; the `\n` itself is already consumed by
/// the caller) and checks the result is UTF-8, as [`std::io::BufRead::lines`] does.
fn finish_line(mut buf: Vec<u8>) -> Result<String> {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|e| Error::Corrupt {
        rev: 0,
        reason: format!("import line: invalid UTF-8: {e}"),
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

    #[test]
    fn read_bounded_line_matches_lines_for_ordinary_input() {
        let mut r = std::io::Cursor::new(b"a\r\nbb\nccc".as_slice());
        assert_eq!(read_bounded_line(&mut r).expect("1").as_deref(), Some("a"));
        assert_eq!(read_bounded_line(&mut r).expect("2").as_deref(), Some("bb"));
        // The last line, with no trailing newline, is still returned once.
        assert_eq!(
            read_bounded_line(&mut r).expect("3").as_deref(),
            Some("ccc")
        );
        assert_eq!(read_bounded_line(&mut r).expect("eof"), None);
    }

    #[test]
    fn read_bounded_line_refuses_a_line_over_the_cap_without_buffering_it() {
        // One line longer than the cap, spread across many `fill_buf` calls (`Cursor` over a
        // `Vec` hands back everything at once, so chunk it by hand through a `Take`-wrapped
        // reader to also prove the check runs as data arrives, not only once a whole line is
        // already in memory).
        let huge = vec![b'x'; MAX_IMPORT_LINE + 1];
        let mut r = std::io::BufReader::with_capacity(8192, std::io::Cursor::new(huge));
        let err = read_bounded_line(&mut r).expect_err("must refuse");
        assert!(
            matches!(&err, Error::Corrupt { reason, .. } if reason.contains("over")),
            "{err:?}"
        );
    }

    #[test]
    fn read_bounded_line_accepts_a_line_exactly_at_the_cap() {
        let exact = vec![b'y'; MAX_IMPORT_LINE];
        let mut r = std::io::Cursor::new(exact.clone());
        let line = read_bounded_line(&mut r).expect("ok").expect("a line");
        assert_eq!(line.as_bytes(), exact.as_slice());
    }
}
