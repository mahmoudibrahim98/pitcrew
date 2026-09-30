//! OpenCode adapter: golden output, streaming equivalence, paging, read-only access and edge
//! cases, on synthetic stores built from `tests/data/opencode/`.

use pitcrew_ingest::opencode::{OpenCodeAdapter, ReadReport, SETTLE_MS};
use pitcrew_ingest::{SkipReason, SkippedLine};
use pitcrew_interfaces::source::{
    Cursor, SessionMeta, SourceAdapter, SourceError, TranscriptItem, TranscriptRef,
};
use pitcrew_protocol::model::Engine;
use proptest::prelude::*;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/opencode")
}

fn schema() -> String {
    fs::read_to_string(data_dir().join("schema.sql")).expect("schema")
}

/// One row write: `{"table": ..., "row": {column: value}}`.
fn history() -> Vec<Value> {
    fs::read_to_string(data_dir().join("history.jsonl"))
        .expect("history")
        .lines()
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect()
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

fn apply(conn: &Connection, op: &Value) {
    let table = op["table"].as_str().expect("table");
    let row = op["row"].as_object().expect("row");
    let cols: Vec<&str> = row.keys().map(String::as_str).collect();
    let marks = vec!["?"; cols.len()].join(", ");
    // An upsert, as OpenCode writes: a REPLACE would delete the row and cascade to its children.
    let set: Vec<String> = cols.iter().map(|c| format!("{c} = excluded.{c}")).collect();
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({marks}) ON CONFLICT(id) DO UPDATE SET {}",
        cols.join(", "),
        set.join(", ")
    );
    conn.execute(&sql, params_from_iter(row.values().map(sql_value)))
        .expect("write");
}

/// A test writer: no fsync, since the files are thrown away.
fn writer(path: &Path) -> Connection {
    let conn = Connection::open(path).expect("open");
    conn.execute_batch("PRAGMA synchronous = OFF")
        .expect("pragma");
    conn
}

/// Applies `ops` in one transaction.
fn apply_all(conn: &Connection, ops: &[Value]) {
    conn.execute_batch("BEGIN").expect("begin");
    for op in ops {
        apply(conn, op);
    }
    conn.execute_batch("COMMIT").expect("commit");
}

/// A store with `schema` and the given writes applied.
fn store_with(dir: &Path, name: &str, schema: &str, ops: &[Value]) -> PathBuf {
    let path = dir.join(name);
    let conn = writer(&path);
    conn.execute_batch(schema).expect("schema");
    conn.execute_batch("BEGIN").expect("begin");
    for op in ops {
        apply(&conn, op);
    }
    conn.execute_batch("COMMIT").expect("commit");
    path
}

fn full_store(dir: &Path) -> PathBuf {
    store_with(dir, "opencode.db", &schema(), &history())
}

/// Session ids in the history: the main session first, then the sub-session.
fn session_ids() -> (String, String) {
    let ids: Vec<String> = history()
        .iter()
        .filter(|op| op["table"] == "session")
        .map(|op| op["row"]["id"].as_str().expect("id").to_owned())
        .fold(Vec::new(), |mut acc, id| {
            if !acc.contains(&id) {
                acc.push(id);
            }
            acc
        });
    (ids[0].clone(), ids[1].clone())
}

fn tref(db: &Path, session: &str) -> TranscriptRef {
    TranscriptRef {
        engine: Engine::OpenCode,
        path: db.to_path_buf(),
        inner_id: Some(session.to_owned()),
        size: 0,
        modified: 0,
    }
}

#[derive(Debug, Default)]
struct Collected {
    items: Vec<TranscriptItem>,
    meta: Option<SessionMeta>,
    skipped: Vec<SkippedLine>,
    cursor: Cursor,
    reads: usize,
}

impl Collected {
    fn absorb(&mut self, report: ReadReport) {
        self.items.extend(report.chunk.items);
        if report.chunk.meta.is_some() {
            self.meta = report.chunk.meta;
        }
        self.skipped.extend(report.skipped);
        self.cursor = report.chunk.cursor;
        self.reads += 1;
    }
}

fn read_all(db: &Path, session: &str) -> Collected {
    let mut c = Collected::default();
    c.absorb(
        OpenCodeAdapter
            .read(&tref(db, session), &Cursor::default())
            .expect("read"),
    );
    c
}

fn page_all(db: &Path, session: &str, limit: usize) -> Vec<TranscriptItem> {
    let t = tref(db, session);
    let mut pages = Vec::new();
    let mut before = None;
    for _ in 0..10_000 {
        let page = OpenCodeAdapter.read_page(&t, before, limit).expect("page");
        assert!(page.from <= page.to);
        if let Some(b) = before {
            assert_eq!(page.to, b, "pages must join up");
        }
        let done = page.at_start;
        before = Some(page.from);
        pages.push(page.items);
        if done {
            break;
        }
    }
    pages.into_iter().rev().flatten().collect()
}

#[derive(serde::Serialize)]
struct Golden {
    meta: Option<SessionMeta>,
    items: Vec<TranscriptItem>,
    sub_session: Option<SessionMeta>,
    sub_items: Vec<TranscriptItem>,
}

#[test]
fn golden_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = full_store(dir.path());
    let (main, child) = session_ids();
    let c = read_all(&db, &main);
    let sub = read_all(&db, &child);
    // One payload is not JSON; the tool part of the wrong shape is valid JSON and gives nothing.
    assert_eq!(
        c.skipped
            .iter()
            .map(|s| matches!(s.reason, SkipReason::Malformed(_)))
            .collect::<Vec<_>>(),
        [true]
    );
    assert!(sub.meta.as_ref().is_some_and(|m| m.is_subagent));
    insta::assert_json_snapshot!(
        "opencode_session",
        Golden {
            meta: c.meta,
            items: c.items,
            sub_session: sub.meta,
            sub_items: sub.items,
        }
    );
}

#[test]
fn pages_join_up_to_the_full_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = full_store(dir.path());
    let (main, child) = session_ids();
    for session in [&main, &child] {
        let full = read_all(&db, session).items;
        for limit in 1..=full.len() + 1 {
            assert_eq!(page_all(&db, session, limit), full, "limit {limit}");
        }
    }
}

#[test]
fn newest_page_first_and_before_between_positions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = full_store(dir.path());
    let (main, _) = session_ids();
    let t = tref(&db, &main);
    let full = read_all(&db, &main).items;
    let last = full.last().expect("items").offset() + 1;

    let page = OpenCodeAdapter.read_page(&t, None, 3).expect("page");
    assert!(!page.at_start);
    assert_eq!(page.to, last);
    assert_eq!(page.items, full[full.len() - page.items.len()..]);
    assert_eq!(page.from, page.items[0].offset());

    // `before` between two positions pages from there.
    let mid = full[full.len() / 2].offset() + 1;
    let page = OpenCodeAdapter
        .read_page(&t, Some(mid), 10_000)
        .expect("page");
    assert_eq!(page.to, mid);
    assert!(page.at_start);
    let expected: Vec<_> = full.iter().filter(|i| i.offset() < mid).cloned().collect();
    assert_eq!(page.items, expected);

    let past_end = OpenCodeAdapter
        .read_page(&t, Some(u64::MAX), 10_000)
        .expect("page");
    assert_eq!((past_end.items, past_end.to), (full, last));

    let empty = OpenCodeAdapter.read_page(&t, None, 0).expect("page");
    assert!(empty.items.is_empty());
    assert_eq!((empty.from, empty.to, empty.at_start), (last, last, false));
}

/// Row writes with their row times spread out `factor` times, so the settle window passes during
/// the session and the frontier moves.
fn stretched(ops: &[Value], factor: i64) -> Vec<Value> {
    let start = ops
        .iter()
        .filter_map(|op| op["row"]["time_created"].as_i64())
        .min()
        .unwrap_or(0);
    ops.iter()
        .map(|op| {
            let mut op = op.clone();
            for k in ["time_created", "time_updated"] {
                if let Some(t) = op["row"][k].as_i64() {
                    op["row"][k] = json!(start + (t - start) * factor);
                }
            }
            op
        })
        .collect()
}

/// Items sorted by position, keeping the order of items that share one (a part's call before
/// its result).
fn by_position(mut items: Vec<TranscriptItem>) -> Vec<TranscriptItem> {
    items.sort_by_key(TranscriptItem::offset);
    items
}

/// Applies `ops` in batches cut at `cuts`, reading each session after every batch.
fn read_while_writing(
    dir: &Path,
    ops: &[Value],
    cuts: &[usize],
) -> (Collected, Collected, PathBuf) {
    let path = dir.join("streaming.db");
    let writer = writer(&path);
    writer.execute_batch(&schema()).expect("schema");
    let (main, child) = session_ids();
    let mut points: Vec<usize> = cuts.to_vec();
    points.push(ops.len());
    points.sort_unstable();
    let (mut a, mut b) = (Collected::default(), Collected::default());
    let mut done = 0;
    for p in points {
        apply_all(&writer, &ops[done..p]);
        done = p;
        for (c, session) in [(&mut a, &main), (&mut b, &child)] {
            match OpenCodeAdapter.read(&tref(&path, session), &c.cursor) {
                Ok(report) => c.absorb(report),
                // Before the session row exists.
                Err(SourceError::Unreadable { .. }) => {}
                Err(e) => panic!("{e}"),
            }
        }
    }
    (a, b, path)
}

fn check_streaming(ops: &[Value], cuts: &[usize]) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let final_db = store_with(dir.path(), "final.db", &schema(), ops);
    let (main, child) = session_ids();
    let (a, b, _) = read_while_writing(dir.path(), ops, cuts);
    for (got, session) in [(a, &main), (b, &child)] {
        let want = read_all(&final_db, session);
        prop_assert_eq!(by_position(got.items), want.items);
        prop_assert_eq!(got.meta, want.meta);
        let state = serde_json::to_string(&got.cursor.state).expect("state");
        prop_assert!(
            state.len() < 8 * 1024,
            "cursor state is {} bytes",
            state.len()
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Streaming updates read in random batches give the items of one read of the final state,
    /// each once.
    #[test]
    fn random_batches_match_one_read(cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..16)) {
        let ops = history();
        let cuts: Vec<usize> = cuts.iter().map(|i| i.index(ops.len() + 1)).collect();
        check_streaming(&ops, &cuts)?;
    }

    /// The same with the session spread over minutes, so the frontier moves between reads.
    #[test]
    fn random_batches_match_one_read_with_a_moving_frontier(cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..16)) {
        let ops = stretched(&history(), 20);
        let cuts: Vec<usize> = cuts.iter().map(|i| i.index(ops.len() + 1)).collect();
        check_streaming(&ops, &cuts)?;
    }
}

#[test]
fn reading_after_every_write_moves_the_frontier_and_keeps_the_cursor_small() {
    let ops = stretched(&history(), 20);
    let dir = tempfile::tempdir().expect("tempdir");
    let cuts: Vec<usize> = (1..ops.len()).collect();
    let (a, _, path) = read_while_writing(dir.path(), &ops, &cuts);
    let (main, _) = session_ids();
    assert_eq!(by_position(a.items), read_all(&path, &main).items);
    assert!(a.cursor.offset > 0, "the frontier moved");
    let state = a.cursor.state.expect("state");
    assert!(
        state["open"].as_array().map_or(0, Vec::len) < 20,
        "only recent parts are remembered: {state}"
    );
}

#[test]
fn a_running_tool_shows_its_call_then_its_result() {
    let ops = history();
    let dir = tempfile::tempdir().expect("tempdir");
    // Stop right after the question tool starts running.
    let running = ops
        .iter()
        .position(|op| {
            op["row"]["data"]["tool"] == "question"
                && op["row"]["data"]["state"]["status"] == "running"
        })
        .expect("question runs")
        + 1;
    let db = store_with(dir.path(), "live.db", &schema(), &ops[..running]);
    let (main, _) = session_ids();
    let first = read_all(&db, &main);
    assert!(matches!(
        &first.items[first.items.len() - 2..],
        [TranscriptItem::ToolUse { tool, .. }, TranscriptItem::Question { .. }] if tool == "question"
    ));
    let page = OpenCodeAdapter
        .read_page(&tref(&db, &main), None, 2)
        .expect("page");
    assert_eq!(page.items, first.items[first.items.len() - 2..]);

    apply_all(&writer(&db), &ops[running..]);
    let next = OpenCodeAdapter
        .read(&tref(&db, &main), &first.cursor)
        .expect("read");
    assert!(matches!(
        &next.chunk.items[0],
        TranscriptItem::ToolResult { summary, .. } if summary == "User answered: Yes"
    ));
    assert!(
        !next
            .chunk
            .items
            .iter()
            .any(|i| matches!(i, TranscriptItem::Question { .. })),
        "the question is not repeated"
    );
}

#[test]
fn an_interrupted_tool_is_reported_as_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = full_store(dir.path());
    let (main, _) = session_ids();
    let items = read_all(&db, &main).items;
    assert!(matches!(
        &items[items.len() - 2..],
        [TranscriptItem::ToolUse { tool, .. }, TranscriptItem::ToolResult { is_error: true, summary, .. }]
            if tool == "bash" && summary.starts_with("interrupted")
    ));
}

/// The file's bytes and mtime, and the side files next to it.
fn snapshot(db: &Path) -> (Vec<u8>, SystemTime, Vec<String>) {
    let bytes = fs::read(db).expect("read");
    let mtime = fs::metadata(db).and_then(|m| m.modified()).expect("mtime");
    let mut names: Vec<String> = fs::read_dir(db.parent().expect("dir"))
        .expect("list")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    (bytes, mtime, names)
}

fn read_everything(db: &Path) {
    let home = db.parent().expect("dir");
    let found = OpenCodeAdapter.discover(home).expect("discover");
    assert!(!found.is_empty());
    for t in &found {
        OpenCodeAdapter.read(t, &Cursor::default()).expect("read");
        OpenCodeAdapter.read_page(t, None, 5).expect("page");
    }
}

#[test]
fn reading_changes_nothing_on_disk() {
    for wal in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = full_store(dir.path());
        if wal {
            // A WAL-mode store after OpenCode closed it: the header says WAL, no -wal file.
            let conn = Connection::open(&db).expect("open");
            let mode: String = conn
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                .expect("wal");
            assert_eq!(mode, "wal");
            drop(conn);
        }
        let before = snapshot(&db);
        assert_eq!(before.2, ["opencode.db"], "wal: {wal}");
        std::thread::sleep(Duration::from_millis(20));
        read_everything(&db);
        let after = snapshot(&db);
        assert!(before.0 == after.0, "bytes changed (wal: {wal})");
        assert_eq!(before.1, after.1, "mtime changed (wal: {wal})");
        assert_eq!(after.2, ["opencode.db"], "side files created (wal: {wal})");
    }
}

#[test]
fn reads_while_a_wal_writer_is_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ops = history();
    let half = ops.len() / 2;
    let db = store_with(dir.path(), "opencode.db", &schema(), &ops[..half]);
    let writer = writer(&db);
    writer
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
        .expect("wal");
    apply_all(&writer, &ops[half..]);
    // An open write transaction does not block WAL readers.
    writer.execute_batch("BEGIN IMMEDIATE").expect("begin");
    let (main, _) = session_ids();
    let started = Instant::now();
    let c = read_all(&db, &main);
    assert!(started.elapsed() < Duration::from_secs(2));
    let final_dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        c.items,
        read_all(&full_store(final_dir.path()), &main).items
    );
    writer.execute_batch("ROLLBACK").expect("rollback");
}

#[test]
fn a_locked_store_fails_fast_to_retry_later() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = full_store(dir.path());
    let (main, _) = session_ids();
    let writer = Connection::open(&db).expect("open");
    writer.execute_batch("BEGIN EXCLUSIVE").expect("lock");

    let started = Instant::now();
    let err = OpenCodeAdapter
        .read(&tref(&db, &main), &Cursor::default())
        .expect_err("locked");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(&err, SourceError::Io(e) if e.kind() == io::ErrorKind::WouldBlock),
        "{err}"
    );
    assert!(matches!(
        OpenCodeAdapter.discover(dir.path()),
        Err(SourceError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock
    ));

    writer.execute_batch("COMMIT").expect("unlock");
    assert!(
        !read_all(&db, &main).items.is_empty(),
        "a later retry reads"
    );
}

#[test]
fn empty_and_foreign_databases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = dir.path().join("opencode.db");
    fs::write(&empty, b"").expect("write");
    assert!(
        OpenCodeAdapter
            .discover(dir.path())
            .expect("discover")
            .is_empty()
    );
    assert!(matches!(
        OpenCodeAdapter.read(&tref(&empty, "ses_x"), &Cursor::default()),
        Err(SourceError::Unreadable { .. })
    ));

    let junk = dir.path().join("opencode-junk.db");
    fs::write(&junk, b"this is not a database at all, just text").expect("write");
    assert!(
        OpenCodeAdapter
            .discover(dir.path())
            .expect("discover")
            .is_empty()
    );
    assert!(matches!(
        OpenCodeAdapter.read(&tref(&junk, "ses_x"), &Cursor::default()),
        Err(SourceError::Unreadable { .. })
    ));

    let other = tempfile::tempdir().expect("tempdir");
    let db = full_store(other.path());
    assert!(matches!(
        OpenCodeAdapter.read(&tref(&db, "ses_missing"), &Cursor::default()),
        Err(SourceError::Unreadable { .. })
    ));
    let mut no_inner = tref(&db, "x");
    no_inner.inner_id = None;
    assert!(OpenCodeAdapter.read_page(&no_inner, None, 5).is_err());
}

#[test]
fn missing_tables_and_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (main, _) = session_ids();
    // Sessions but no parts: listed, not readable.
    let only_sessions =
        "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, time_updated INTEGER);";
    let sessions: Vec<Value> = history()
        .into_iter()
        .filter(|op| op["table"] == "session")
        .map(|mut op| {
            let row = op["row"].as_object().expect("row").clone();
            op["row"] = json!({"id": row["id"], "title": row["title"], "time_updated": row["time_updated"]});
            op
        })
        .collect();
    let db = store_with(dir.path(), "opencode.db", only_sessions, &sessions);
    let found = OpenCodeAdapter.discover(dir.path()).expect("discover");
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|t| t.size == 0 && t.modified > 0));
    let err = OpenCodeAdapter
        .read(&tref(&db, &main), &Cursor::default())
        .expect_err("no parts");
    assert!(
        err.to_string().contains("message") || err.to_string().contains("part"),
        "{err}"
    );

    // Only the required columns: everything optional missing still reads.
    let minimal = "CREATE TABLE session (id TEXT PRIMARY KEY);
        CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, data TEXT);
        CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);";
    let keep = |table: &str, cols: &[&str], op: &Value| -> Option<Value> {
        (op["table"] == table).then(|| {
            let row: serde_json::Map<String, Value> = cols
                .iter()
                .map(|c| ((*c).to_owned(), op["row"][*c].clone()))
                .collect();
            json!({"table": table, "row": row})
        })
    };
    let ops: Vec<Value> = history()
        .iter()
        .filter_map(|op| {
            keep("session", &["id"], op)
                .or_else(|| keep("message", &["id", "session_id", "data"], op))
                .or_else(|| keep("part", &["id", "message_id", "session_id", "data"], op))
        })
        .collect();
    let other = tempfile::tempdir().expect("tempdir");
    let db = store_with(other.path(), "opencode.db", minimal, &ops);
    let c = read_all(&db, &main);
    let full_dir = tempfile::tempdir().expect("tempdir");
    let full = read_all(&full_store(full_dir.path()), &main);
    // Same items, apart from times that came from row columns.
    assert_eq!(c.items.len(), full.items.len());
    let meta = c.meta.expect("meta");
    assert_eq!((meta.cwd, meta.started), (None, None));
    assert_eq!(page_all(&db, &main, 4), c.items);
}

#[test]
fn the_stream_zero_fixture_reads() {
    // The approximate schema in crates/fixtures: fewer columns than a real store, and ids that
    // do not carry a position.
    let fixtures = pitcrew_fixtures::data_dir().join("transcripts/opencode");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("opencode.db");
    let conn = Connection::open(&path).expect("create");
    conn.execute_batch(&fs::read_to_string(fixtures.join("schema.sql")).expect("schema"))
        .expect("schema");
    conn.execute_batch(&fs::read_to_string(fixtures.join("seed.sql")).expect("seed"))
        .expect("seed");
    drop(conn);
    let found = OpenCodeAdapter.discover(dir.path()).expect("discover");
    assert_eq!(found.len(), 1);
    let c = read_all(&path, found[0].inner_id.as_deref().expect("id"));
    let kinds: Vec<&str> = c
        .items
        .iter()
        .map(|i| match i {
            TranscriptItem::UserPrompt { .. } => "prompt",
            TranscriptItem::AssistantText { .. } => "text",
            TranscriptItem::ToolUse { .. } => "use",
            TranscriptItem::ToolResult { .. } => "result",
            TranscriptItem::PlanUpdated { .. } => "plan",
            TranscriptItem::TurnEnded { .. } => "end",
            _ => "other",
        })
        .collect();
    // The fixture's todo part has no counterpart in real stores (todos are a tool there).
    assert_eq!(
        kinds,
        ["prompt", "text", "use", "result", "use", "result", "end"]
    );
    assert_eq!(
        c.meta.map(|m| (m.title, m.model)),
        Some((
            Some("Codex rollout parser".into()),
            Some("demo-model".into())
        ))
    );
}

#[test]
fn malformed_and_huge_payloads_are_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = full_store(dir.path());
    let (main, _) = session_ids();
    let before = read_all(&db, &main).items;

    let conn = Connection::open(&db).expect("open");
    let msg: String = conn
        .query_row(
            "SELECT id FROM message WHERE session_id = ?1 ORDER BY time_created DESC LIMIT 1",
            [&main],
            |r| r.get(0),
        )
        .expect("message");
    let add = |id: &str, t: i64, data: rusqlite::types::Value| {
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![id, msg, main, t, data],
        )
        .expect("insert");
    };
    let t = 1_790_800_000_000i64;
    use rusqlite::types::Value as V;
    add(
        "prt_bad_utf8",
        t,
        V::Blob(b"{\"type\":\"text\",\"text\":\"\xff\"}".to_vec()),
    );
    add("prt_bad_array", t + 1, V::Text("[1,2,3]".into()));
    let huge = json!({"type": "tool", "tool": "read", "callID": "c", "state": {"status": "completed", "input": {}, "output": "y".repeat(17 * 1024 * 1024)}});
    add("prt_huge", t + 2, V::Text(huge.to_string()));
    let big = json!({"type": "tool", "tool": "read", "callID": "big", "state": {"status": "completed", "input": {"filePath": "/big"}, "output": "z".repeat(5 * 1024 * 1024)}});
    add("prt_big", t + 3, V::Text(big.to_string()));
    drop(conn);

    let c = read_all(&db, &main);
    let reasons: Vec<&SkipReason> = c.skipped.iter().map(|s| &s.reason).collect();
    assert!(
        matches!(
            reasons[..],
            [
                SkipReason::Malformed(_),
                SkipReason::InvalidUtf8,
                SkipReason::Malformed(_),
                SkipReason::TooLong
            ]
        ),
        "{reasons:?}"
    );
    assert_eq!(&c.items[..before.len()], &before[..]);
    assert!(matches!(
        &c.items[before.len()..],
        [TranscriptItem::ToolUse { target, .. }, TranscriptItem::ToolResult { summary, .. }]
            if target == "/big" && summary.len() <= 250
    ));
    assert_eq!(page_all(&db, &main, 3), c.items);
}

#[test]
fn discovery_lists_sessions_of_every_store() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = full_store(home.path());
    fs::copy(&db, home.path().join("opencode-dev.db")).expect("copy");
    fs::copy(&db, home.path().join("other.db")).expect("copy");
    fs::write(home.path().join("opencode.db-notes"), "x").expect("write");
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().expect("tempdir");
        let far = full_store(outside.path());
        std::os::unix::fs::symlink(&far, home.path().join("opencode-link.db")).expect("link");
    }
    let found = OpenCodeAdapter.discover(home.path()).expect("discover");
    let (main, child) = session_ids();
    let names: Vec<(String, String)> = found
        .iter()
        .map(|t| {
            (
                t.path
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned(),
                t.inner_id.clone().expect("id"),
            )
        })
        .collect();
    let mut want = vec![
        ("opencode-dev.db".to_owned(), main.clone()),
        ("opencode-dev.db".to_owned(), child.clone()),
        ("opencode.db".to_owned(), main.clone()),
        ("opencode.db".to_owned(), child.clone()),
    ];
    want.sort();
    assert_eq!(names, want);
    assert!(
        found
            .iter()
            .all(|t| t.engine == Engine::OpenCode && t.size > 0 && t.modified > 1_700_000_000_000)
    );
    let sub = found
        .iter()
        .find(|t| t.inner_id.as_deref() == Some(child.as_str()))
        .expect("child");
    assert!(read_all(&sub.path, &child).meta.expect("meta").is_subagent);
    assert!(
        OpenCodeAdapter
            .discover(&home.path().join("missing"))
            .expect("ok")
            .is_empty()
    );
}

/// An OpenCode-style id for creation time `ms` (the low 48 bits of `ms × 4096 + n`).
fn ascending_id(prefix: &str, ms: i64, n: u64) -> String {
    let full = u64::try_from(ms).expect("positive") * 4096 + n;
    format!("{prefix}_{:012x}Synthetic{n:05}", full % (1 << 48))
}

/// A long session: `messages` assistant messages of ten parts each, a second apart, inserted
/// in rowid order shuffled within windows of `window` parts (so out of order by up to `window`
/// seconds).
fn long_session(messages: usize, window: usize, seed: u64) -> (Vec<Value>, String) {
    let session = "ses_long".to_owned();
    let start = 1_790_756_400_000i64;
    let mut ops = vec![
        json!({"table": "session", "row": {"id": session, "project_id": "p", "slug": "s",
        "directory": "/w", "title": "Long", "version": "1.18.30", "time_created": start, "time_updated": start}}),
    ];
    let mut parts = Vec::new();
    let mut n = 0u64;
    for m in 0..messages {
        let t = start + i64::try_from(m).expect("fits") * 10_000;
        let mid = ascending_id("msg", t, 1);
        ops.push(json!({"table": "message", "row": {"id": mid, "session_id": session, "time_created": t,
            "time_updated": t, "data": {"role": "assistant", "time": {"created": t, "completed": t + 9000}}}}));
        for k in 0..10i64 {
            let pt = t + k * 1000;
            n += 1;
            let data = if k % 3 == 0 {
                json!({"type": "tool", "tool": "bash", "callID": format!("c{n}"), "state": {"status": "completed",
                    "input": {"command": format!("echo {n}")}, "output": format!("{n}"), "metadata": {"exit": 0}}})
            } else {
                json!({"type": "text", "text": format!("part {n}"), "time": {"start": pt, "end": pt + 10}})
            };
            parts.push(
                json!({"table": "part", "row": {"id": ascending_id("prt", pt, n), "message_id": mid,
                "session_id": session, "time_created": pt, "time_updated": pt, "data": data}}),
            );
        }
    }
    // Deterministic shuffle within each window.
    let mut state = seed | 1;
    for chunk in parts.chunks_mut(window.max(1)) {
        for i in (1..chunk.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let j = usize::try_from(state % (i as u64 + 1)).expect("fits");
            chunk.swap(i, j);
        }
    }
    ops.extend(parts);
    (ops, session)
}

#[test]
fn long_sessions_page_by_rowid_with_parts_out_of_order() {
    let (ops, session) = long_session(120, 40, 99);
    let dir = tempfile::tempdir().expect("tempdir");
    let db = store_with(dir.path(), "opencode.db", &schema(), &ops);
    let full = read_all(&db, &session).items;
    // Ten parts a message, four of them tools (a call and a result each).
    assert_eq!(full.len(), 120 * 14);
    assert!(full.windows(2).all(|w| w[0].offset() <= w[1].offset()));
    for limit in [1, 7, 199, 256, 257, 5000] {
        assert_eq!(page_all(&db, &session, limit), full, "limit {limit}");
    }
    // Pages from many points jump straight there.
    let t = tref(&db, &session);
    for k in (0..full.len()).step_by(97) {
        let before = full[k].offset();
        let page = OpenCodeAdapter
            .read_page(&t, Some(before), 50)
            .expect("page");
        let older: Vec<_> = full
            .iter()
            .filter(|i| i.offset() < before)
            .cloned()
            .collect();
        assert_eq!(page.to, before);
        assert_eq!(
            page.items,
            older[older.len().saturating_sub(page.items.len())..],
            "before {before}"
        );
        assert_eq!(page.at_start, page.items.len() == older.len());
    }
    // Streaming the same writes in batches gives the same items.
    let cuts: Vec<usize> = (1..8).map(|i| i * ops.len() / 8).collect();
    let path = dir.path().join("streaming.db");
    let w = writer(&path);
    w.execute_batch(&schema()).expect("schema");
    let mut c = Collected::default();
    let mut done = 0;
    for p in cuts.into_iter().chain([ops.len()]) {
        apply_all(&w, &ops[done..p]);
        done = p;
        if let Ok(report) = OpenCodeAdapter.read(&tref(&path, &session), &c.cursor) {
            c.absorb(report);
        }
    }
    assert_eq!(by_position(c.items), full);
}

#[test]
fn a_part_table_without_rowids_is_listed_whole() {
    let (ops, session) = long_session(30, 1, 1);
    let dir = tempfile::tempdir().expect("tempdir");
    let schema = schema().replace(
        "CONSTRAINT `fk_part_message_id_message_id_fk` FOREIGN KEY (`message_id`) REFERENCES `message`(`id`) ON DELETE CASCADE\n);",
        "CONSTRAINT `fk_part_message_id_message_id_fk` FOREIGN KEY (`message_id`) REFERENCES `message`(`id`) ON DELETE CASCADE\n) WITHOUT ROWID;",
    );
    assert!(schema.contains("WITHOUT ROWID"));
    let db = store_with(dir.path(), "opencode.db", &schema, &ops);
    let full = read_all(&db, &session).items;
    assert_eq!(full.len(), 30 * 14);
    assert_eq!(page_all(&db, &session, 9), full);
    let mid = full[200].offset();
    let page = OpenCodeAdapter
        .read_page(&tref(&db, &session), Some(mid), 10)
        .expect("page");
    assert_eq!(page.items, full[190..200]);
}

#[test]
fn settle_window_is_what_the_docs_say() {
    assert_eq!(SETTLE_MS, 60_000);
}

/// `cargo test -p pitcrew-ingest --release --test opencode -- --ignored --nocapture large_session`
#[test]
#[ignore = "benchmark: writes a large store"]
fn large_session_timings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = history();
    let (main, _) = session_ids();
    // The main session's writes, repeated with fresh ids and later times.
    let main_ops: Vec<&Value> = base
        .iter()
        .filter(|op| op["row"]["session_id"] == main.as_str())
        .collect();
    let path = dir.path().join("opencode.db");
    let conn = Connection::open(&path).expect("create");
    conn.execute_batch(&schema()).expect("schema");
    conn.execute_batch("BEGIN").expect("begin");
    for op in base.iter().filter(|op| op["table"] == "session") {
        apply(&conn, op);
    }
    let filler = "lorem ipsum dolor sit amet ".repeat(80);
    let rounds = 2000u64;
    for round in 0..rounds {
        let shift = i64::try_from(round).expect("fits") * 3_600_000;
        for op in &main_ops {
            let mut op = (*op).clone();
            let row = op["row"].as_object_mut().expect("row");
            for k in ["time_created", "time_updated"] {
                let t = row[k].as_i64().expect("time") + shift;
                row.insert(k.into(), json!(t));
            }
            for k in ["id", "message_id"] {
                if let Some(id) = row.get(k).and_then(Value::as_str) {
                    // Same position bits shifted by an hour per round, fresh random tail.
                    let (prefix, rest) = id.split_at(4);
                    let bits = u64::from_str_radix(&rest[..12], 16).expect("hex");
                    let bits = (bits + round * 3_600_000 * 4096) % (1 << 48);
                    row.insert(k.into(), json!(format!("{prefix}{bits:012x}r{round:013}")));
                }
            }
            if let Some(text) = row.get_mut("data").and_then(|d| d.get_mut("text")) {
                *text = json!(format!("{}{filler}", text.as_str().unwrap_or("")));
            }
            apply(&conn, &op);
        }
    }
    conn.execute_batch("COMMIT").expect("commit");
    drop(conn);
    let size = fs::metadata(&path).expect("size").len();
    let t = tref(&path, &main);
    let parts: i64 = Connection::open(&path)
        .expect("open")
        .query_row(
            "SELECT count(*) FROM part WHERE session_id = ?1",
            [&main],
            |r| r.get(0),
        )
        .expect("count");

    let start = Instant::now();
    let page = OpenCodeAdapter.read_page(&t, None, 200).expect("page");
    let newest = start.elapsed();
    let start = Instant::now();
    let older = OpenCodeAdapter
        .read_page(&t, Some(page.from), 200)
        .expect("page");
    let second = start.elapsed();
    let start = Instant::now();
    let full = OpenCodeAdapter.read(&t, &Cursor::default()).expect("read");
    let whole = start.elapsed();
    let before = full.chunk.items[full.chunk.items.len() / 2].offset();
    let start = Instant::now();
    let middle = OpenCodeAdapter
        .read_page(&t, Some(before), 200)
        .expect("page");
    let mid = start.elapsed();
    println!(
        "a page from the middle of the history: {} items in {mid:?}",
        middle.items.len()
    );
    assert!(middle.items.len() >= 200);
    let start = Instant::now();
    let again = OpenCodeAdapter.read(&t, &full.chunk.cursor).expect("read");
    let idle = start.elapsed();
    println!(
        "store {} MB, session of {parts} parts: newest page {} items in {newest:?}; previous page {} items in {second:?}",
        size / (1024 * 1024),
        page.items.len(),
        older.items.len(),
    );
    println!(
        "full read_from: {} items, {} MB of payloads in {whole:?}; a read with nothing new: {} items in {idle:?}",
        full.chunk.items.len(),
        full.bytes_read / (1024 * 1024),
        again.chunk.items.len(),
    );
    assert!(page.items.len() >= 200);
    assert!(again.chunk.items.is_empty());
}

/// Checks the adapter against a real store without copying anything: counts and timings only.
/// `OPENCODE_DB=/path/to/opencode.db cargo test -p pitcrew-ingest --release --test opencode -- --ignored --nocapture real_store`
#[test]
#[ignore = "needs a real OpenCode store in OPENCODE_DB"]
fn real_store() {
    let Some(db) = std::env::var_os("OPENCODE_DB") else {
        return;
    };
    let db = PathBuf::from(db);
    let home = db.parent().expect("dir");
    let start = Instant::now();
    let found: Vec<_> = OpenCodeAdapter
        .discover(home)
        .expect("discover")
        .into_iter()
        .filter(|t| t.path == db)
        .collect();
    println!(
        "discover: {} sessions in {:?}",
        found.len(),
        start.elapsed()
    );
    let (mut items, mut skipped, mut slowest) = (0usize, 0u64, Duration::ZERO);
    for t in &found {
        let start = Instant::now();
        let full = OpenCodeAdapter.read(t, &Cursor::default()).expect("read");
        slowest = slowest.max(start.elapsed());
        items += full.chunk.items.len();
        skipped += full.skipped_total;
        let paged = page_all(&db, t.inner_id.as_deref().expect("id"), 50);
        assert_eq!(paged.len(), full.chunk.items.len(), "session paging");
        let again = OpenCodeAdapter.read(t, &full.chunk.cursor).expect("read");
        assert!(
            again.chunk.items.len() <= 2,
            "a quiet session gives nothing new"
        );
    }
    println!("read {items} items ({skipped} skipped parts); slowest full read {slowest:?}");
}
