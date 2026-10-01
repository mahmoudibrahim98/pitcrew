//! `pitcrew_ingest::opencode::OpenCodeAdapter` on arbitrary OpenCode stores. The store is a
//! SQLite file the OpenCode CLI writes; its payloads hold model output and tool results (U2).
//!
//! Input: a mode byte, a page-size byte, then either
//! - (even mode) the database file's bytes, or
//! - (odd mode) row writes, one JSON object per line, `{"table": …, "row": {column: value}}`,
//!   applied to a fresh store with OpenCode's real schema (as `crates/ingest`'s own tests do).
//!   This reaches the reader's logic far more often than mutated database bytes.
//!
//! Checks, for every session `discover` finds (at most four), besides "no panic" and the item
//! caps every adapter promises:
//! - offsets are positions no larger than `MAX_POSITION`, and never go backwards in a read;
//! - reading again from the returned cursor, with nothing changed, finds nothing new;
//! - paging from the newest page back to the start ends, every page joins the next, its items
//!   lie inside it, and the pages together equal the full read.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::{check_items, fresh_dir};
use pitcrew_ingest::opencode::{MAX_POSITION, OpenCodeAdapter};
use pitcrew_interfaces::source::{Cursor, SourceAdapter, TranscriptItem, TranscriptRef};
use rusqlite::{Connection, params_from_iter};
use serde_json::Value;
use std::path::Path;

/// OpenCode's schema, as `crates/ingest`'s tests use it.
const SCHEMA: &str = include_str!("../../crates/ingest/tests/data/opencode/schema.sql");
const TABLES: [&str; 5] = ["project", "session", "message", "part", "todo"];

fuzz_target!(|input: &[u8]| {
    let Some((&[mode, limit], rest)) = input.split_first_chunk::<2>() else {
        return;
    };
    let home = fresh_dir("opencode-home");
    let db = home.join("opencode.db");
    if mode & 1 == 0 {
        std::fs::write(&db, rest).expect("write the store");
    } else if !build(&db, rest) {
        return;
    }
    let Ok(sessions) = OpenCodeAdapter.discover(&home) else {
        return;
    };
    for transcript in sessions.iter().take(4) {
        check_session(transcript, 1 + usize::from(limit % 16));
    }
});

fn check_session(transcript: &TranscriptRef, limit: usize) {
    let Ok(first) = OpenCodeAdapter.read(transcript, &Cursor::default()) else {
        return;
    };
    let items = &first.chunk.items;
    check_items(items, None);
    let mut last = 0;
    for item in items {
        let offset = item.offset();
        assert!(offset <= MAX_POSITION, "position {offset} over the cap");
        assert!(offset >= last, "offsets go backwards in one read");
        last = offset;
    }
    assert!(first.chunk.cursor.offset <= MAX_POSITION);

    if let Ok(again) = OpenCodeAdapter.read(transcript, &first.chunk.cursor) {
        assert!(
            again.chunk.items.is_empty(),
            "a second read of an unchanged store found {} new items",
            again.chunk.items.len()
        );
    }

    let mut pages: Vec<Vec<TranscriptItem>> = Vec::new();
    let mut before: Option<u64> = None;
    // Pages can be empty (parts that show nothing yet), but each moves back at least one
    // position, and a small store has few.
    for _ in 0..4096 {
        let Ok(page) = OpenCodeAdapter.read_page(transcript, before, limit) else {
            return;
        };
        assert!(page.from <= page.to, "a page ends before it starts");
        if let Some(b) = before {
            assert_eq!(page.to, b, "a page does not join the next one");
        }
        let mut last = page.from;
        for item in &page.items {
            let offset = item.offset();
            assert!(
                offset >= last && offset < page.to,
                "offset {offset} outside its page {}..{}",
                page.from,
                page.to
            );
            last = offset;
        }
        if !page.at_start {
            assert!(page.from < page.to, "a page makes no progress");
        }
        let done = page.at_start;
        before = Some(page.from);
        pages.push(page.items);
        if done {
            let paged: Vec<TranscriptItem> = pages.into_iter().rev().flatten().collect();
            assert_eq!(&paged, items, "the pages differ from the full read");
            return;
        }
    }
    panic!("paging did not reach the start");
}

/// Writes a store with OpenCode's schema and the row writes in `lines`. `false` if nothing usable
/// was written.
fn build(db: &Path, lines: &[u8]) -> bool {
    let Ok(conn) = Connection::open(db) else {
        return false;
    };
    if conn
        .execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")
        .is_err()
        || conn.execute_batch(SCHEMA).is_err()
    {
        return false;
    }
    let mut wrote = false;
    for line in String::from_utf8_lossy(lines).lines() {
        let Ok(op) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        wrote |= apply(&conn, &op);
    }
    wrote
}

/// One upsert, as OpenCode writes: a REPLACE would delete the row and cascade to its children.
/// Table and column names come from the input, so only known tables and plain names are used.
fn apply(conn: &Connection, op: &Value) -> bool {
    let (Some(table), Some(row)) = (op["table"].as_str(), op["row"].as_object()) else {
        return false;
    };
    if !TABLES.contains(&table)
        || row.is_empty()
        || !row
            .keys()
            .all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    {
        return false;
    }
    let cols: Vec<&str> = row.keys().map(String::as_str).collect();
    let marks = vec!["?"; cols.len()].join(", ");
    let set: Vec<String> = cols.iter().map(|c| format!("{c} = excluded.{c}")).collect();
    let conflict = if table == "todo" {
        "(session_id, position)"
    } else {
        "(id)"
    };
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({marks}) ON CONFLICT{conflict} DO UPDATE SET {}",
        cols.join(", "),
        set.join(", ")
    );
    conn.execute(&sql, params_from_iter(row.values().map(sql_value)))
        .is_ok()
}

fn sql_value(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map_or_else(|| V::Real(n.as_f64().unwrap_or(0.0)), V::Integer),
        Value::String(s) => V::Text(s.clone()),
        other => V::Text(other.to_string()),
    }
}
