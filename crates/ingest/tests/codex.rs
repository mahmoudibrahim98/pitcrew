//! Codex adapter: golden output, chunked reads, paging and edge cases.

use pitcrew_ingest::codex::{CodexAdapter, ReadReport};
use pitcrew_ingest::{MAX_REPORTED_SKIPS, SkipReason, SkippedLine};
use pitcrew_interfaces::source::{
    Cursor, SessionMeta, SourceAdapter, SourceError, TranscriptItem, TranscriptRef,
};
use pitcrew_protocol::model::Engine;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

fn fixture_path() -> PathBuf {
    pitcrew_fixtures::data_dir().join("transcripts/codex/rollout-demo.jsonl")
}

fn fixture() -> Vec<u8> {
    fs::read(fixture_path()).expect("fixture")
}

fn tref(path: &Path) -> TranscriptRef {
    TranscriptRef {
        engine: Engine::Codex,
        path: path.to_path_buf(),
        inner_id: None,
        size: 0,
        modified: 0,
    }
}

/// Everything one or more reads produced.
#[derive(Debug, Default)]
struct Collected {
    items: Vec<TranscriptItem>,
    meta: Option<SessionMeta>,
    skipped: Vec<SkippedLine>,
    skipped_total: u64,
    bytes_read: u64,
    cursor: Cursor,
}

impl Collected {
    fn absorb(&mut self, report: ReadReport) {
        self.items.extend(report.chunk.items);
        if report.chunk.meta.is_some() {
            self.meta = report.chunk.meta;
        }
        self.skipped.extend(report.skipped);
        self.skipped_total += report.skipped_total;
        self.bytes_read += report.bytes_read;
        self.cursor = report.chunk.cursor;
    }
}

fn read_all(path: &Path) -> Collected {
    let mut c = Collected::default();
    c.absorb(
        CodexAdapter
            .read(&tref(path), &Cursor::default())
            .expect("read"),
    );
    c
}

/// Writes `data` to a new file in `cuts.len() + 1` pieces, reading after each append.
fn read_in_chunks(dir: &Path, data: &[u8], cuts: &[usize]) -> Collected {
    // Same file name as the full reads, so the fallback session id matches.
    let path = dir.join("chunked").join("full.jsonl");
    fs::create_dir_all(dir.join("chunked")).expect("mkdir");
    fs::write(&path, b"").expect("create");
    let mut points: Vec<usize> = cuts.iter().map(|c| c % (data.len() + 1)).collect();
    points.push(data.len());
    points.sort_unstable();

    let mut c = Collected::default();
    let mut written = 0;
    for p in points {
        let mut f = OpenOptions::new().append(true).open(&path).expect("open");
        f.write_all(&data[written..p]).expect("append");
        drop(f);
        written = p;
        c.absorb(CodexAdapter.read(&tref(&path), &c.cursor).expect("read"));
    }
    c
}

/// Pages backwards from the end with `limit` items per page, oldest first overall.
fn page_all(path: &Path, limit: usize) -> Vec<TranscriptItem> {
    let t = tref(path);
    let mut pages = Vec::new();
    let mut before = None;
    for _ in 0..10_000 {
        let page = CodexAdapter.read_page(&t, before, limit).expect("page");
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
}

fn count(items: &[TranscriptItem], pred: impl Fn(&TranscriptItem) -> bool) -> usize {
    items.iter().filter(|i| pred(i)).count()
}

#[test]
fn golden_rollout_demo() {
    let c = read_all(&fixture_path());
    assert!(c.skipped.is_empty(), "{:?}", c.skipped);
    assert_eq!(c.cursor.offset, fixture().len() as u64);
    // Each prompt and reply is recorded twice; each appears once.
    assert_eq!(
        count(&c.items, |i| matches!(i, TranscriptItem::UserPrompt { .. })),
        1
    );
    assert_eq!(
        count(&c.items, |i| matches!(
            i,
            TranscriptItem::AssistantText { .. }
        )),
        1
    );
    insta::assert_json_snapshot!(
        "rollout_demo",
        Golden {
            meta: c.meta,
            items: c.items
        }
    );
}

#[test]
fn fixture_pages_join_up_to_the_full_read() {
    let full = read_all(&fixture_path()).items;
    for limit in 1..=full.len() + 1 {
        assert_eq!(page_all(&fixture_path(), limit), full, "limit {limit}");
    }
}

#[test]
fn newest_page_comes_first() {
    let t = tref(&fixture_path());
    let full = read_all(&fixture_path()).items;
    let page = CodexAdapter.read_page(&t, None, 2).expect("page");
    assert!(!page.at_start);
    assert_eq!(page.to, fixture().len() as u64);
    assert_eq!(page.items, full[full.len() - page.items.len()..]);
    assert_eq!(page.from, page.items[0].offset());

    let first = CodexAdapter
        .read_page(&t, Some(page.from), 1000)
        .expect("page");
    assert!(first.at_start);
    assert_eq!(first.items.len() + page.items.len(), full.len());
}

#[test]
fn empty_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.jsonl");
    fs::write(&path, b"").expect("write");
    let c = read_all(&path);
    assert!(c.items.is_empty());
    assert_eq!(c.cursor.offset, 0);
    assert_eq!(c.meta.map(|m| m.native_id).as_deref(), Some("empty"));

    let page = CodexAdapter
        .read_page(&tref(&path), None, 50)
        .expect("page");
    assert!(page.items.is_empty() && page.at_start);
    assert_eq!((page.from, page.to), (0, 0));
}

fn nth_line_start(data: &[u8], n: usize) -> usize {
    data.iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(n.wrapping_sub(1))
        .map_or(0, |(i, _)| i + 1)
}

#[test]
fn truncated_last_line_is_completed_later() {
    let data = fixture();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("growing.jsonl");
    // Cut in the middle of line 12 (the assistant message).
    let line12 = nth_line_start(&data, 11) + 40;
    assert!(line12 < data.len());
    fs::write(&path, &data[..line12]).expect("write");

    let first = CodexAdapter
        .read(&tref(&path), &Cursor::default())
        .expect("read");
    assert_eq!(first.chunk.cursor.offset, nth_line_start(&data, 11) as u64);
    assert!(first.skipped.is_empty());

    let mut f = OpenOptions::new().append(true).open(&path).expect("open");
    f.write_all(&data[line12..]).expect("append");
    drop(f);
    let second = CodexAdapter
        .read(&tref(&path), &first.chunk.cursor)
        .expect("read");

    let mut items = first.chunk.items;
    items.extend(second.chunk.items);
    assert_eq!(items, read_all(&fixture_path()).items);
    assert_eq!(first.bytes_read + second.bytes_read, data.len() as u64);
}

fn record(kind: &str, payload: Value) -> String {
    json!({"timestamp": "2026-01-01T00:00:00Z", "type": kind, "payload": payload}).to_string()
}

fn prompt(text: &str) -> String {
    record(
        "response_item",
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}),
    )
}

fn prompt_text(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::UserPrompt { text, .. } => text.clone(),
        other => format!("{other:?}"),
    }
}

#[test]
fn oversized_line_is_skipped_and_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big.jsonl");
    let huge = prompt(&"x".repeat(20 * 1024 * 1024));
    let before = prompt("before");
    let after = prompt("after");
    let data = format!("{before}\n{huge}\n{after}\n").into_bytes();

    // Written in two pieces, the second read resumes inside the huge line without buffering it.
    let split = before.len() + 1 + 10 * 1024 * 1024;
    let c = read_in_chunks(dir.path(), &data, &[split]);
    let texts: Vec<_> = c.items.iter().map(prompt_text).collect();
    assert_eq!(texts, ["before", "after"]);
    assert_eq!(
        c.skipped,
        vec![SkippedLine {
            offset: before.len() as u64 + 1,
            len: huge.len() as u64,
            reason: SkipReason::TooLong
        }]
    );
    assert!(c.bytes_read <= data.len() as u64);

    fs::write(&path, &data).expect("write");
    let paged: Vec<_> = page_all(&path, 1).iter().map(prompt_text).collect();
    assert_eq!(paged, ["before", "after"]);
}

#[test]
fn invalid_utf8_and_wrong_shapes_are_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.jsonl");
    let mut data = Vec::new();
    data.extend_from_slice(prompt("one").as_bytes());
    data.extend_from_slice(b"\n{\"type\":\"response_item\",\"payload\":{\"text\":\"\xff\xfe\"}}\n");
    data.extend_from_slice(b"[1,2,3]\n");
    data.extend_from_slice(b"{\"type\":\"response_item\",\"payload\":\"not an object\"}\n");
    data.extend_from_slice(b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":42}}\n");
    data.extend_from_slice(
        b"{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",\"call_id\":7}}\n",
    );
    data.extend_from_slice(
        b"{\"type\":\"session_meta\",\"payload\":{\"id\":[1],\"git\":\"main\"}}\n",
    );
    data.extend_from_slice(b"not json at all\n\n   \n");
    data.extend_from_slice(prompt("two").as_bytes());
    data.push(b'\n');
    fs::write(&path, &data).expect("write");

    let c = read_all(&path);
    let texts: Vec<_> = c.items.iter().map(prompt_text).collect();
    assert_eq!(texts, ["one", "two"]);
    let reasons: Vec<_> = c.skipped.iter().map(|s| &s.reason).collect();
    assert!(matches!(
        reasons[..],
        [
            SkipReason::InvalidUtf8,
            SkipReason::Malformed(_),
            SkipReason::Malformed(_)
        ]
    ));
    assert_eq!(c.skipped_total, 3);
    assert_eq!(page_all(&path, 1), c.items);
}

#[test]
fn a_flood_of_junk_lines_reports_only_the_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("junk.jsonl");
    let mut data = format!("{}\n", prompt("go"));
    for _ in 0..1000 {
        data.push_str("junk\n");
    }
    fs::write(&path, &data).expect("write");
    let c = read_all(&path);
    assert_eq!(c.items.len(), 1);
    assert_eq!(c.skipped.len(), MAX_REPORTED_SKIPS);
    assert_eq!(c.skipped_total, 1000);
    assert_eq!(c.skipped[0].offset, prompt("go").len() as u64 + 1);
}

#[test]
fn meta_comes_from_session_meta_and_turn_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rollout-x.jsonl");
    let lines = [
        record(
            "session_meta",
            json!({"id": "sid", "timestamp": "2026-01-01T00:00:00Z", "cwd": "/real/cwd",
                   "git": {"branch": "feat/a"}}),
        ),
        record("turn_context", json!({"cwd": "/other", "model": "m-1"})),
        prompt("<environment_context>\n<cwd>/real/cwd</cwd>\n</environment_context>"),
        prompt("  Fix   the\nparser  "),
        record("turn_context", json!({"model": "m-2"})),
    ];
    let data: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(&path, &data).expect("write");
    assert_eq!(
        read_all(&path).meta.expect("meta"),
        SessionMeta {
            native_id: "sid".into(),
            cwd: Some("/real/cwd".into()),
            branch: Some("feat/a".into()),
            title: Some("Fix the parser".into()),
            model: Some("m-2".into()),
            started: Some(1_767_225_600_000),
            is_subagent: false,
        }
    );
}

#[test]
fn discovery_finds_dated_and_archived_rollouts() {
    let home = tempfile::tempdir().expect("tempdir");
    let day = home.path().join("sessions/2026/09/29");
    let archived = home.path().join("archived_sessions");
    fs::create_dir_all(&day).expect("mkdir");
    fs::create_dir_all(&archived).expect("mkdir");
    fs::create_dir_all(home.path().join("sessions/2026/09/29/deeper")).expect("mkdir");
    let line = format!("{}\n", prompt("hi"));
    fs::write(day.join("rollout-a.jsonl"), &line).expect("write");
    fs::write(day.join("notes.jsonl"), &line).expect("write");
    fs::write(day.join("rollout-b.json"), &line).expect("write");
    fs::write(day.join("deeper/rollout-too-deep.jsonl"), &line).expect("write");
    fs::write(archived.join("rollout-old.jsonl"), &line).expect("write");

    #[cfg(unix)]
    {
        // A link out of the home is not followed.
        let outside = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(outside.path().join("x")).expect("mkdir");
        fs::write(outside.path().join("x/rollout-outside.jsonl"), &line).expect("write");
        std::os::unix::fs::symlink(outside.path(), home.path().join("sessions/2026/linked"))
            .expect("symlink");
        std::os::unix::fs::symlink(
            outside.path().join("x/rollout-outside.jsonl"),
            day.join("rollout-link.jsonl"),
        )
        .expect("symlink");
        std::os::unix::fs::symlink(outside.path(), home.path().join("archived_sessions/x"))
            .expect("symlink");
        let other_home = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(outside.path(), other_home.path().join("sessions"))
            .expect("symlink");
        assert!(
            CodexAdapter
                .discover(other_home.path())
                .expect("discover")
                .is_empty()
        );
    }

    let found = CodexAdapter.discover(home.path()).expect("discover");
    let names: Vec<_> = found
        .iter()
        .map(|t| {
            t.path
                .strip_prefix(home.path())
                .expect("under home")
                .to_path_buf()
        })
        .collect();
    assert_eq!(
        names,
        [
            Path::new("archived_sessions/rollout-old.jsonl"),
            Path::new("sessions/2026/09/29/rollout-a.jsonl"),
        ]
    );
    assert!(
        found
            .iter()
            .all(|t| t.engine == Engine::Codex && t.size > 0 && t.modified > 0)
    );
    assert!(
        CodexAdapter
            .discover(&home.path().join("missing"))
            .expect("ok")
            .is_empty()
    );
}

#[test]
fn a_patch_across_several_files_and_a_malformed_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("patches.jsonl");
    let good = "*** Begin Patch\n*** Add File: a.txt\n+1\n+2\n*** Update File: b.txt\n@@\n-x\n+y\n*** Delete File: c.txt\n*** End Patch";
    let lines = [
        record(
            "response_item",
            json!({"type": "custom_tool_call", "name": "apply_patch", "call_id": "p1", "input": good}),
        ),
        record(
            "response_item",
            json!({"type": "custom_tool_call", "name": "apply_patch", "call_id": "p2", "input": "*** Update File: nope\n+x"}),
        ),
        record(
            "response_item",
            json!({"type": "custom_tool_call_output", "call_id": "p2",
                   "output": json!({"output": "invalid patch", "metadata": {"exit_code": 1}}).to_string()}),
        ),
    ];
    let data: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(&path, &data).expect("write");
    let items = read_all(&path).items;
    let edits: Vec<_> = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::FileEdit {
                path,
                added,
                removed,
                ..
            } => Some((path.as_str(), *added, *removed)),
            _ => None,
        })
        .collect();
    assert_eq!(edits, [("a.txt", 2, 0), ("b.txt", 1, 1), ("c.txt", 0, 0)]);
    assert!(matches!(
        items.last(),
        Some(TranscriptItem::ToolResult { call_id, is_error: true, .. }) if call_id == "p2"
    ));
    assert_eq!(page_all(&path, 1), items);
}

#[test]
fn before_inside_a_line_pages_from_the_line_boundary() {
    let t = tref(&fixture_path());
    let data = fixture();
    let full = read_all(&fixture_path()).items;
    let boundary = nth_line_start(&data, 7) as u64; // start of line 8
    let page = CodexAdapter
        .read_page(&t, Some(boundary + 17), 1000)
        .expect("page");
    assert_eq!(page.to, boundary);
    assert!(page.at_start);
    let expected: Vec<_> = full
        .iter()
        .filter(|i| i.offset() < boundary)
        .cloned()
        .collect();
    assert_eq!(page.items, expected);

    let past_end = CodexAdapter
        .read_page(&t, Some(u64::MAX), 1000)
        .expect("page");
    assert_eq!((past_end.items, past_end.to), (full, data.len() as u64));
}

#[test]
fn limit_zero_gives_an_empty_page() {
    let len = fixture().len() as u64;
    let page = CodexAdapter
        .read_page(&tref(&fixture_path()), None, 0)
        .expect("page");
    assert!(page.items.is_empty());
    assert_eq!((page.from, page.to, page.at_start), (len, len, false));
}

#[test]
fn a_cursor_past_the_end_is_an_error() {
    let t = tref(&fixture_path());
    let cursor = Cursor {
        offset: fixture().len() as u64 + 1,
        state: None,
    };
    assert!(matches!(
        CodexAdapter.read(&t, &cursor),
        Err(SourceError::Unreadable { .. })
    ));

    // A corrupt carried line whose length overflows is ignored, not a panic.
    let cursor = Cursor {
        offset: 10,
        state: Some(json!({"pending": {"len": u64::MAX, "too_long": true}})),
    };
    let report = CodexAdapter.read(&t, &cursor).expect("read");
    assert_eq!(report.chunk.cursor.offset, fixture().len() as u64);
}

/// Items with their offsets removed, for comparing files whose line endings differ.
fn without_offsets(items: &[TranscriptItem]) -> Vec<Value> {
    items
        .iter()
        .map(|i| {
            let mut v = serde_json::to_value(i).expect("json");
            v.as_object_mut().expect("object").remove("offset");
            v
        })
        .collect()
}

#[test]
fn crlf_files_give_the_same_items() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("crlf.jsonl");
    let crlf = String::from_utf8(fixture())
        .expect("utf8")
        .replace('\n', "\r\n");
    fs::write(&path, &crlf).expect("write");
    let got = read_all(&path);
    let want = read_all(&fixture_path());
    assert!(got.skipped.is_empty());
    assert_eq!(without_offsets(&got.items), without_offsets(&want.items));
    assert_eq!(got.meta, want.meta);
    assert_eq!(page_all(&path, 3), got.items);
    let chunked = read_in_chunks(dir.path(), crlf.as_bytes(), &[100, 1001, 1002, 4000]);
    assert_eq!(chunked.items, got.items);
}

/// One or two synthetic lines per choice, covering every item kind, the duplicated records, and
/// junk.
fn synthetic_line(kind: u8, n: usize, text: &str) -> Vec<u8> {
    let ts = format!("2026-01-01T00:{:02}:{:02}.000Z", (n / 60) % 60, n % 60);
    let rec = |kind: &str, payload: Value| {
        json!({"timestamp": ts, "type": kind, "payload": payload}).to_string()
    };
    let lines = match kind % 12 {
        0 => vec![rec(
            "session_meta",
            json!({"id": "gen", "timestamp": ts, "cwd": "/g", "git": {"branch": text}}),
        )],
        1 => vec![rec("turn_context", json!({"cwd": "/g", "model": text}))],
        2 => vec![
            rec(
                "response_item",
                json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}),
            ),
            rec(
                "event_msg",
                json!({"type": "user_message", "message": text, "kind": "plain"}),
            ),
        ],
        3 => vec![
            rec(
                "event_msg",
                json!({"type": "agent_message", "message": text}),
            ),
            rec(
                "response_item",
                json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}),
            ),
        ],
        4 => vec![rec(
            "response_item",
            json!({"type": "function_call", "name": "shell", "call_id": format!("c{n}"),
                   "arguments": json!({"command": ["bash", "-lc", text]}).to_string()}),
        )],
        5 => vec![rec(
            "response_item",
            json!({"type": "function_call_output", "call_id": format!("c{n}"),
                   "output": json!({"output": text, "metadata": {"exit_code": n % 2}}).to_string()}),
        )],
        6 => vec![rec(
            "response_item",
            json!({"type": "custom_tool_call", "name": "apply_patch", "call_id": format!("p{n}"),
                   "input": format!("*** Begin Patch\n*** Update File: a\n@@\n-{text}\n+b\n*** Add File: c\n+{text}\n*** End Patch")}),
        )],
        7 => vec![rec(
            "response_item",
            json!({"type": "custom_tool_call_output", "call_id": format!("p{n}"), "output": text}),
        )],
        8 => vec![rec(
            "response_item",
            json!({"type": "function_call", "name": "update_plan", "call_id": format!("u{n}"),
                   "arguments": json!({"plan": [{"step": text, "status": "in_progress"}, {"step": "b", "status": "completed"}]}).to_string()}),
        )],
        9 => vec![
            rec(
                "event_msg",
                json!({"type": "token_count", "info": {"total_token_usage": {"total_tokens": n}}}),
            ),
            rec(
                "event_msg",
                json!({"type": "task_complete", "last_agent_message": text}),
            ),
        ],
        10 if n.is_multiple_of(2) => return b"{\"broken\": ".to_vec(),
        10 => return vec![b'{', 0xff, b'}'],
        _ => vec![rec(
            "response_item",
            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": text}]}),
        )],
    };
    lines.join("\n").into_bytes()
}

fn synthetic(lines: &[(u8, String)], trailing_newline: bool) -> Vec<u8> {
    let mut data = Vec::new();
    for (n, (kind, text)) in lines.iter().enumerate() {
        data.extend(synthetic_line(*kind, n, text));
        data.push(b'\n');
    }
    if !trailing_newline {
        data.pop();
    }
    data
}

fn text_strategy() -> impl Strategy<Value = String> {
    prop_oneof![Just("§ ünïcode".to_string()), "[a-z \\n]{0,40}", "x{0,300}"]
}

fn resolve(cuts: &[prop::sample::Index], data: &[u8]) -> Vec<usize> {
    cuts.iter().map(|i| i.index(data.len() + 1)).collect()
}

/// Length of the incomplete line a read stopping at `p` would leave behind.
fn partial_len(data: &[u8], p: usize) -> u64 {
    let start = data[..p]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    (p - start) as u64
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn fixture_in_random_chunks_matches_one_read(cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..12)) {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = fixture();
        let cuts = resolve(&cuts, &data);
        let full = read_all(&fixture_path());
        let chunked = read_in_chunks(dir.path(), &data, &cuts);
        prop_assert_eq!(&chunked.items, &full.items);
        prop_assert_eq!(&chunked.meta, &full.meta);
        prop_assert!(chunked.bytes_read <= data.len() as u64);
    }

    #[test]
    fn generated_in_random_chunks_matches_one_read(
        lines in prop::collection::vec((0u8..12, text_strategy()), 0..40),
        trailing_newline in any::<bool>(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..12),
        limit in 1usize..6,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = synthetic(&lines, trailing_newline);
        let cuts = resolve(&cuts, &data);
        let path = dir.path().join("full.jsonl");
        fs::write(&path, &data).expect("write");
        let full = read_all(&path);
        let chunked = read_in_chunks(dir.path(), &data, &cuts);
        prop_assert_eq!(&chunked.items, &full.items);
        prop_assert_eq!(&chunked.meta, &full.meta);
        prop_assert_eq!(chunked.cursor.offset, full.cursor.offset);
        prop_assert!(chunked.bytes_read <= data.len() as u64);
        prop_assert_eq!(page_all(&path, limit), full.items.clone());

        // Duplicated prompt and reply records give one item each. (Without a final newline the
        // last line is incomplete and not read, so only count complete files.)
        if trailing_newline {
            let prompts = lines.iter().filter(|(k, t)| k % 12 == 2 && !t.trim().is_empty()).count();
            let replies = lines.iter().filter(|(k, t)| k % 12 == 3 && !t.trim().is_empty()).count();
            prop_assert_eq!(count(&full.items, |i| matches!(i, TranscriptItem::UserPrompt { .. })), prompts);
            prop_assert_eq!(count(&full.items, |i| matches!(i, TranscriptItem::AssistantText { .. })), replies);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// A line between 64 KiB and 16 MiB, cut by writes, is not carried in the cursor. Once complete,
    /// only the part read by earlier calls is read again.
    #[test]
    fn generated_with_a_large_line_in_random_chunks(
        before in prop::collection::vec((0u8..12, text_strategy()), 0..10),
        after in prop::collection::vec((0u8..12, text_strategy()), 0..10),
        big in 70_000usize..1_500_000,
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 1..6),
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut data = synthetic(&before, true);
        data.extend(prompt(&"b".repeat(big)).into_bytes());
        data.push(b'\n');
        data.extend(synthetic(&after, true));
        let cuts = resolve(&cuts, &data);
        let path = dir.path().join("full.jsonl");
        fs::write(&path, &data).expect("write");
        let full = read_all(&path);
        let chunked = read_in_chunks(dir.path(), &data, &cuts);
        prop_assert_eq!(&chunked.items, &full.items);
        prop_assert_eq!(&chunked.meta, &full.meta);
        let longest_partial = cuts.iter().map(|&p| partial_len(&data, p)).max().unwrap_or(0);
        prop_assert!(chunked.bytes_read <= data.len() as u64 + longest_partial);
    }
}

/// `cargo test -p pitcrew-ingest --release --test codex -- --ignored --nocapture on_200mb`
#[test]
#[ignore = "benchmark: writes a 200 MB file"]
fn read_page_and_full_read_on_200mb() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rollout-big.jsonl");
    let mut block = Vec::new();
    for n in 0..1000 {
        let text = format!("step {n}: {}", "lorem ipsum ".repeat(60));
        block.extend(synthetic_line((n % 12) as u8, n, &text));
        block.push(b'\n');
    }
    let mut f = fs::File::create(&path).expect("create");
    let mut size = 0usize;
    while size < 200 * 1024 * 1024 {
        f.write_all(&block).expect("write");
        size += block.len();
    }
    f.sync_all().expect("sync");
    drop(f);

    let t = tref(&path);
    let start = std::time::Instant::now();
    let page = CodexAdapter.read_page(&t, None, 200).expect("page");
    let newest = start.elapsed();
    let start = std::time::Instant::now();
    let older = CodexAdapter
        .read_page(&t, Some(page.from), 200)
        .expect("page");
    let second = start.elapsed();
    let start = std::time::Instant::now();
    let middle = CodexAdapter
        .read_page(&t, Some(size as u64 / 2), 200)
        .expect("page");
    let mid = start.elapsed();
    println!(
        "file {} MB: newest page {} items in {newest:?}; previous page {} items in {second:?}; middle page {} items in {mid:?}",
        size / (1024 * 1024),
        page.items.len(),
        older.items.len(),
        middle.items.len()
    );

    let start = std::time::Instant::now();
    let full = CodexAdapter.read(&t, &Cursor::default()).expect("read");
    let whole = start.elapsed();
    println!(
        "full read_from: {} items, {} skipped ({} listed), {} MB read in {whole:?} ({:.0} MB/s)",
        full.chunk.items.len(),
        full.skipped_total,
        full.skipped.len(),
        full.bytes_read / (1024 * 1024),
        full.bytes_read as f64 / (1024.0 * 1024.0) / whole.as_secs_f64()
    );
    assert!(page.items.len() >= 200);
    assert!(full.skipped.len() <= MAX_REPORTED_SKIPS);
    assert!(newest.as_millis() < 50, "newest page took {newest:?}");
}
