//! Claude Code adapter: golden output, chunked reads, paging and edge cases.

use pitcrew_ingest::claude::{ClaudeAdapter, ReadReport};
use pitcrew_ingest::{SkipReason, SkippedLine};
use pitcrew_interfaces::source::{
    Cursor, SessionMeta, SourceAdapter, TranscriptItem, TranscriptRef,
};
use pitcrew_protocol::model::Engine;
use proptest::prelude::*;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

fn fixture_path() -> PathBuf {
    pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl")
}

fn fixture() -> Vec<u8> {
    fs::read(fixture_path()).expect("fixture")
}

fn tref(path: &Path) -> TranscriptRef {
    TranscriptRef {
        engine: Engine::Claude,
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
        self.bytes_read += report.bytes_read;
        self.cursor = report.chunk.cursor;
    }
}

fn read_all(path: &Path) -> Collected {
    let mut c = Collected::default();
    c.absorb(
        ClaudeAdapter
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
        c.absorb(ClaudeAdapter.read(&tref(&path), &c.cursor).expect("read"));
    }
    c
}

/// Pages backwards from the end with `limit` items per page, oldest first overall.
fn page_all(path: &Path, limit: usize) -> Vec<TranscriptItem> {
    let t = tref(path);
    let mut pages = Vec::new();
    let mut before = None;
    for _ in 0..10_000 {
        let page = ClaudeAdapter.read_page(&t, before, limit).expect("page");
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

#[test]
fn golden_demo_session() {
    let c = read_all(&fixture_path());
    assert!(c.skipped.is_empty(), "{:?}", c.skipped);
    assert_eq!(c.cursor.offset, fixture().len() as u64);
    insta::assert_json_snapshot!(
        "demo_session",
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
    let page = ClaudeAdapter.read_page(&t, None, 2).expect("page");
    assert!(!page.at_start);
    assert_eq!(page.to, fixture().len() as u64);
    assert_eq!(page.items, full[full.len() - page.items.len()..]);
    assert_eq!(page.from, page.items[0].offset());

    let first = ClaudeAdapter
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

    let page = ClaudeAdapter
        .read_page(&tref(&path), None, 50)
        .expect("page");
    assert!(page.items.is_empty() && page.at_start);
    assert_eq!((page.from, page.to), (0, 0));
}

#[test]
fn truncated_last_line_is_completed_later() {
    let data = fixture();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("growing.jsonl");
    // Cut in the middle of line 12 (the last assistant text).
    let line12 = nth_line_start(&data, 11) + 40;
    assert!(line12 < data.len());
    fs::write(&path, &data[..line12]).expect("write");

    let first = ClaudeAdapter
        .read(&tref(&path), &Cursor::default())
        .expect("read");
    assert_eq!(first.chunk.cursor.offset, nth_line_start(&data, 11) as u64);
    assert!(first.skipped.is_empty());

    let mut f = OpenOptions::new().append(true).open(&path).expect("open");
    f.write_all(&data[line12..]).expect("append");
    drop(f);
    let second = ClaudeAdapter
        .read(&tref(&path), &first.chunk.cursor)
        .expect("read");

    let mut items = first.chunk.items;
    items.extend(second.chunk.items);
    assert_eq!(items, read_all(&fixture_path()).items);
    assert_eq!(first.bytes_read + second.bytes_read, data.len() as u64);
}

fn nth_line_start(data: &[u8], n: usize) -> usize {
    data.iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(n.wrapping_sub(1))
        .map_or(0, |(i, _)| i + 1)
}

fn prompt(text: &str, ts: &str) -> String {
    json!({"type": "user", "sessionId": "s-1", "cwd": "/w", "timestamp": ts,
           "message": {"role": "user", "content": text}})
    .to_string()
}

#[test]
fn oversized_line_is_skipped_and_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big.jsonl");
    let huge = prompt(&"x".repeat(20 * 1024 * 1024), "2026-01-01T00:00:01Z");
    let before = prompt("before", "2026-01-01T00:00:00Z");
    let after = prompt("after", "2026-01-01T00:00:02Z");
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

fn prompt_text(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::UserPrompt { text, .. } => text.clone(),
        other => format!("{other:?}"),
    }
}

#[test]
fn invalid_utf8_and_wrong_shapes_are_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.jsonl");
    let mut data = Vec::new();
    data.extend_from_slice(prompt("one", "2026-01-01T00:00:00Z").as_bytes());
    data.extend_from_slice(b"\n{\"type\":\"user\",\"message\":{\"content\":\"\xff\xfe\"}}\n");
    data.extend_from_slice(b"[1,2,3]\n");
    data.extend_from_slice(b"{\"type\":\"assistant\",\"message\":\"not an object\"}\n");
    data.extend_from_slice(b"{\"type\":\"user\",\"message\":{\"content\":42}}\n");
    data.extend_from_slice(
        b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":1}]}}\n",
    );
    data.extend_from_slice(b"not json at all\n\n   \n");
    data.extend_from_slice(prompt("two", "2026-01-01T00:00:01Z").as_bytes());
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
    assert_eq!(page_all(&path, 1), c.items);
}

#[test]
fn meta_comes_from_records_not_folders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let folder = dir.path().join("-some-encoded-folder");
    fs::create_dir(&folder).expect("mkdir");
    let path = folder.join("file.jsonl");
    let lines = [
        json!({"type": "user", "sessionId": "sid", "cwd": "/real/cwd", "gitBranch": "feat/a",
               "timestamp": "2026-01-01T00:00:00Z", "message": {"content": "  Fix   the\nparser  "}}),
        json!({"type": "assistant", "gitBranch": "feat/b", "timestamp": "2026-01-01T00:00:01Z",
               "message": {"model": "claude-x", "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn"}}),
        json!({"type": "assistant", "message": {"model": "<synthetic>", "content": []}}),
    ];
    let data: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(&path, &data).expect("write");
    let meta = read_all(&path).meta.expect("meta");
    assert_eq!(
        meta,
        SessionMeta {
            native_id: "sid".into(),
            cwd: Some("/real/cwd".into()),
            branch: Some("feat/b".into()),
            title: Some("Fix the parser".into()),
            model: Some("claude-x".into()),
            started: Some(1_767_225_600_000),
            is_subagent: false,
        }
    );

    // A summary outranks the first prompt, and a custom title outranks both.
    let with_summary = format!("{data}{}\n", json!({"type": "summary", "summary": "Sum"}));
    fs::write(&path, &with_summary).expect("write");
    assert_eq!(
        read_all(&path).meta.and_then(|m| m.title).as_deref(),
        Some("Sum")
    );
    let with_title = format!(
        "{with_summary}{}\n",
        json!({"type": "custom-title", "customTitle": "Mine"})
    );
    fs::write(&path, with_title).expect("write");
    assert_eq!(
        read_all(&path).meta.and_then(|m| m.title).as_deref(),
        Some("Mine")
    );
}

#[test]
fn discovery_finds_sessions_and_subagents() {
    let home = tempfile::tempdir().expect("tempdir");
    let project = home.path().join("projects").join("-w-proj");
    let subagents = project.join("sess-1").join("subagents");
    fs::create_dir_all(&subagents).expect("mkdir");
    fs::write(
        project.join("sess-1.jsonl"),
        prompt("hi", "2026-01-01T00:00:00Z") + "\n",
    )
    .expect("write");
    let side = json!({"type": "user", "isSidechain": true, "sessionId": "sess-1",
                      "agentId": "ag-7", "message": {"content": "sub task"}});
    fs::write(subagents.join("agent-ag-7.jsonl"), format!("{side}\n")).expect("write");
    let old_side = json!({"type": "user", "isSidechain": true, "sessionId": "sess-0",
                          "message": {"content": "old sub task"}});
    fs::write(project.join("sess-0.jsonl"), format!("{old_side}\n")).expect("write");
    fs::write(project.join("notes.txt"), "x").expect("write");
    fs::write(home.path().join("projects").join("stray.jsonl"), "{}\n").expect("write");

    let found = ClaudeAdapter.discover(home.path()).expect("discover");
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
            Path::new("projects/-w-proj/sess-0.jsonl"),
            Path::new("projects/-w-proj/sess-1/subagents/agent-ag-7.jsonl"),
            Path::new("projects/-w-proj/sess-1.jsonl"),
        ]
    );
    assert!(
        found
            .iter()
            .all(|t| t.engine == Engine::Claude && t.size > 0 && t.modified > 0)
    );

    let metas: Vec<_> = found
        .iter()
        .map(|t| read_all(&t.path).meta.expect("meta"))
        .collect();
    let flags: Vec<_> = metas
        .iter()
        .map(|m| (m.native_id.as_str(), m.is_subagent))
        .collect();
    assert_eq!(flags, [("sess-0", true), ("ag-7", true), ("s-1", false)]);

    assert!(
        ClaudeAdapter
            .discover(&home.path().join("missing"))
            .expect("ok")
            .is_empty()
    );
}

#[test]
fn end_turn_then_turn_duration_is_one_turn_end() {
    let full = read_all(&fixture_path()).items;
    let ends = full
        .iter()
        .filter(|i| matches!(i, TranscriptItem::TurnEnded { .. }))
        .count();
    assert_eq!(ends, 1);
}

/// One synthetic record per choice, covering every item kind plus junk.
fn synthetic_line(kind: u8, n: usize, text: &str) -> Vec<u8> {
    let ts = format!("2026-01-01T00:{:02}:{:02}.000Z", (n / 60) % 60, n % 60);
    let v = match kind % 10 {
        0 => json!({"type": "user", "sessionId": "gen", "cwd": "/g", "timestamp": ts,
                    "message": {"content": text}}),
        1 => json!({"type": "assistant", "timestamp": ts, "message": {"model": "m",
                    "content": [{"type": "text", "text": text}], "stop_reason": "end_turn"}}),
        2 => json!({"type": "assistant", "timestamp": ts, "message": {"content": [
                    {"type": "text", "text": text},
                    {"type": "tool_use", "id": format!("t{n}"), "name": "Bash", "input": {"command": text}}]}}),
        3 => json!({"type": "user", "timestamp": ts, "message": {"content": [
                    {"type": "tool_result", "tool_use_id": format!("t{n}"), "content": text, "is_error": n % 2 == 0}]},
                    "toolUseResult": {"filePath": "/f", "structuredPatch": [
                        {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-a", "+b"]}]}}),
        4 => {
            json!({"type": "system", "subtype": "turn_duration", "timestamp": ts, "durationMs": 5})
        }
        5 => json!({"type": "assistant", "timestamp": ts, "message": {"content": [
                    {"type": "tool_use", "id": format!("p{n}"), "name": "TodoWrite", "input": {"todos": [
                        {"content": text, "status": "in_progress"}, {"content": "b", "status": "completed"}]}}]}}),
        6 => json!({"type": "custom-title", "customTitle": text}),
        7 => return b"{\"broken\": ".to_vec(),
        8 => return vec![b'{', 0xff, b'}'],
        _ => json!({"type": "assistant", "timestamp": ts, "message": {"content": [
                    {"type": "tool_use", "id": format!("q{n}"), "name": "AskUserQuestion", "input": {"questions": [
                        {"question": text, "options": [{"label": "A"}, {"label": "B"}]}]}}]}}),
    };
    v.to_string().into_bytes()
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
        lines in prop::collection::vec((0u8..10, text_strategy()), 0..40),
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
        prop_assert_eq!(page_all(&path, limit), full.items);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// A line between 64 KiB and 16 MiB, cut by writes, is not carried in the cursor. Once complete,
    /// only the part read by earlier calls is read again.
    #[test]
    fn generated_with_a_large_line_in_random_chunks(
        before in prop::collection::vec((0u8..10, text_strategy()), 0..10),
        after in prop::collection::vec((0u8..10, text_strategy()), 0..10),
        big in 70_000usize..1_500_000,
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 1..6),
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut data = synthetic(&before, true);
        data.extend(prompt(&"b".repeat(big), "2026-01-01T00:00:00Z").into_bytes());
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

#[test]
fn huge_summary_then_blank_lines_is_fast_and_short() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("attack.jsonl");
    let summary = json!({"type": "summary", "summary": "a".repeat(15 * 1024 * 1024)});
    let mut data = format!("{summary}\n").into_bytes();
    data.extend(std::iter::repeat_n(b'\n', 100_000));
    fs::write(&path, &data).expect("write");

    let start = std::time::Instant::now();
    let c = read_all(&path);
    let took = start.elapsed();
    assert!(took.as_secs() < 5, "took {took:?}");
    let title = c.meta.and_then(|m| m.title).expect("title");
    assert!(
        title.chars().count() <= 121,
        "{} chars",
        title.chars().count()
    );
    let state = serde_json::to_string(&c.cursor.state).expect("state");
    assert!(state.len() < 2048, "cursor state is {} bytes", state.len());
}

#[test]
fn a_flood_of_turn_duration_records_pages_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("turns.jsonl");
    let turn = json!({"type": "system", "subtype": "turn_duration", "durationMs": 1}).to_string();
    let mut data = format!("{}\n", prompt("go", "2026-01-01T00:00:00Z"));
    for _ in 0..20_000 {
        data.push_str(&turn);
        data.push('\n');
    }
    fs::write(&path, &data).expect("write");
    let full = read_all(&path).items;
    assert_eq!(full.len(), 2, "one prompt, one turn end");
    // Deciding the kept turn end needs the prompt before it, and pages hold whole records.
    let page = ClaudeAdapter
        .read_page(&tref(&path), None, 1)
        .expect("page");
    assert_eq!(page.items, full);
    assert!(page.at_start);
}

#[test]
fn a_home_under_a_subagents_folder_is_not_a_subagent() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("subagents").join("claude");
    let project = home.join("projects").join("-w");
    fs::create_dir_all(&project).expect("mkdir");
    fs::write(
        project.join("s.jsonl"),
        prompt("hi", "2026-01-01T00:00:00Z") + "\n",
    )
    .expect("write");
    let found = ClaudeAdapter.discover(&home).expect("discover");
    assert_eq!(found.len(), 1);
    assert!(!read_all(&found[0].path).meta.expect("meta").is_subagent);
}

#[test]
fn before_inside_a_line_pages_from_the_line_boundary() {
    let t = tref(&fixture_path());
    let data = fixture();
    let full = read_all(&fixture_path()).items;
    let boundary = nth_line_start(&data, 5) as u64; // start of line 6
    let page = ClaudeAdapter
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

    let past_end = ClaudeAdapter
        .read_page(&t, Some(u64::MAX), 1000)
        .expect("page");
    assert_eq!((past_end.items, past_end.to), (full, data.len() as u64));
}

#[test]
fn limit_zero_gives_an_empty_page() {
    let len = fixture().len() as u64;
    let page = ClaudeAdapter
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
        ClaudeAdapter.read(&t, &cursor),
        Err(pitcrew_interfaces::source::SourceError::Unreadable { .. })
    ));

    // A corrupt carried line whose length overflows is ignored, not a panic.
    let cursor = Cursor {
        offset: 10,
        state: Some(json!({"pending": {"len": u64::MAX, "too_long": true}})),
    };
    let report = ClaudeAdapter.read(&t, &cursor).expect("read");
    assert_eq!(report.chunk.cursor.offset, fixture().len() as u64);
}

/// Items with their offsets removed, for comparing files whose line endings differ.
fn without_offsets(items: &[TranscriptItem]) -> Vec<serde_json::Value> {
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
    assert_eq!(chunked.items.len(), got.items.len());
}

#[test]
fn a_large_partial_line_round_trips_through_the_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("large.jsonl");
    let first = prompt("first", "2026-01-01T00:00:00Z");
    let large = prompt(&"q".repeat(100_000), "2026-01-01T00:00:01Z");
    let data = format!("{first}\n{large}\n");
    let cut = first.len() + 1 + 80_000;
    fs::write(&path, &data.as_bytes()[..cut]).expect("write");

    let one = ClaudeAdapter
        .read(&tref(&path), &Cursor::default())
        .expect("read");
    let state = serde_json::to_string(&one.chunk.cursor.state).expect("state");
    assert!(
        state.len() < 1024,
        "large partial lines are not carried: {state}"
    );
    assert_eq!(one.chunk.cursor.offset, first.len() as u64 + 1);

    let mut f = OpenOptions::new().append(true).open(&path).expect("open");
    f.write_all(&data.as_bytes()[cut..]).expect("append");
    drop(f);
    let two = ClaudeAdapter
        .read(&tref(&path), &one.chunk.cursor)
        .expect("read");
    assert_eq!(two.chunk.items.len(), 1);
    assert_eq!(prompt_text(&two.chunk.items[0]).len(), 100_000);
    assert_eq!(
        one.bytes_read + two.bytes_read,
        data.len() as u64 + 80_000,
        "only the part read before is read again"
    );
}

/// `cargo test -p pitcrew-ingest --release -- --ignored --nocapture read_page_on_200mb`
#[test]
#[ignore = "benchmark: writes a 200 MB file"]
fn read_page_on_200mb() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big.jsonl");
    let mut block = Vec::new();
    for n in 0..1000 {
        let text = format!("step {n}: {}", "lorem ipsum ".repeat(60));
        block.extend(synthetic_line((n % 6) as u8, n, &text));
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
    let page = ClaudeAdapter.read_page(&t, None, 200).expect("page");
    let newest = start.elapsed();
    let start = std::time::Instant::now();
    let older = ClaudeAdapter
        .read_page(&t, Some(page.from), 200)
        .expect("page");
    let second = start.elapsed();
    let start = std::time::Instant::now();
    let middle = ClaudeAdapter
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
    assert!(page.items.len() >= 200);
    assert!(newest.as_millis() < 50, "newest page took {newest:?}");
}
