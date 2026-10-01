//! Transcript pages (`RunnerHandle::transcript_page`) with the real Claude adapter, as api-v1's
//! "Transcript paging" says: tail-first back to the start, whole records, a partial last line
//! left out, a transcript that grows between two pages, the limits, and the two errors.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, FIXTURE_ID, append, claude_file, config, discovered, fixture_lines};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptItem, TranscriptPage, TranscriptRef,
};
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::Engine;
use pitcrew_runner::{DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, PageError, RunnerHandle};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Longest wait for a discovery; it ends as soon as the session is discovered.
const CEILING: Duration = Duration::from_secs(60);

/// A runner watching `home` with `adapter`, once it has discovered its first session.
fn watch_with(
    home: &Path,
    state: &Path,
    adapter: Arc<dyn SourceAdapter>,
) -> (RunnerHandle, SessionId) {
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(config(home, state), vec![adapter], sink.clone()).unwrap();
    sink.wait_for(1, CEILING).expect("discovery");
    let session = discovered(&sink.events()).id;
    (runner, session)
}

fn watch(home: &Path, state: &Path) -> (RunnerHandle, SessionId) {
    watch_with(home, state, Arc::new(ClaudeAdapter::new()))
}

fn len(bytes: &[Vec<u8>]) -> u64 {
    bytes.iter().map(|l| l.len() as u64).sum()
}

/// A user prompt record, one item.
fn prompt_line(i: usize) -> String {
    format!(
        r#"{{"parentUuid":null,"isSidechain":false,"cwd":"/w/paper","sessionId":"{FIXTURE_ID}","type":"user","uuid":"u-{i}","timestamp":"2026-09-30T08:00:00.000Z","message":{{"role":"user","content":"prompt {i}"}}}}"#
    ) + "\n"
}

fn prompt_text(item: &TranscriptItem) -> &str {
    match item {
        TranscriptItem::UserPrompt { text, .. } => text,
        other => panic!("not a prompt: {other:?}"),
    }
}

#[test]
fn pages_go_back_from_the_tail_to_the_start() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let lines = fixture_lines();
    std::fs::write(claude_file(home.path(), FIXTURE_ID), lines.concat()).unwrap();
    let (runner, s) = watch(home.path(), state.path());

    let whole = runner
        .transcript_page(s, None, Some(MAX_PAGE_LIMIT))
        .unwrap();
    assert!(whole.at_start);
    assert_eq!((whole.from, whole.to), (0, len(&lines)));
    assert!(whole.items.len() > 6, "{}", whole.items.len());

    // Back from the newest page, a few items at a time: each page ends where the newer one
    // began, and the last says nothing older exists.
    let mut pages: Vec<TranscriptPage> = Vec::new();
    let mut before = None;
    loop {
        let page = runner.transcript_page(s, before, Some(3)).unwrap();
        assert_eq!(page.to, before.unwrap_or(whole.to));
        assert!(!page.items.is_empty());
        let done = page.at_start;
        before = Some(page.from);
        pages.push(page);
        if done {
            break;
        }
        assert!(pages.len() <= whole.items.len(), "paging back never ends");
    }
    assert!(pages.len() > 2, "{}", pages.len());
    assert!(pages[..pages.len() - 1].iter().all(|p| !p.at_start));
    assert_eq!(pages.last().map(|p| p.from), Some(0));
    let items: Vec<TranscriptItem> = pages.iter().rev().flat_map(|p| p.items.clone()).collect();
    assert_eq!(items, whole.items);

    // Pages hold whole records: the second line's record is one, with more than one item.
    let third_line = len(&lines[..2]);
    let one = runner
        .transcript_page(s, Some(third_line), Some(1))
        .unwrap();
    assert_eq!((one.from, one.to), (len(&lines[..1]), third_line));
    assert!(one.items.len() > 1, "{:?}", one.items);
    assert!(!one.at_start);
    runner.stop();
}

#[test]
fn a_partial_last_line_is_left_out_until_it_is_whole() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let lines = fixture_lines();
    let path = claude_file(home.path(), FIXTURE_ID);
    let (head, tail) = lines[5].split_at(lines[5].len() / 2);
    std::fs::write(&path, [lines[..5].concat(), head.to_vec()].concat()).unwrap();
    let (runner, s) = watch(home.path(), state.path());

    let page = runner.transcript_page(s, None, None).unwrap();
    assert_eq!((page.from, page.to), (0, len(&lines[..5])));
    assert!(page.at_start);
    assert!(!page.items.is_empty());

    // The line is finished: the newest page now ends after it, and before it nothing changed.
    append(&path, tail);
    let grown = runner.transcript_page(s, None, None).unwrap();
    assert_eq!(grown.to, len(&lines[..6]));
    assert!(grown.items.len() > page.items.len());
    assert_eq!(grown.items[..page.items.len()], page.items[..]);
    assert_eq!(
        runner.transcript_page(s, Some(page.to), None).unwrap(),
        page
    );
    runner.stop();
}

#[test]
fn a_transcript_that_grows_between_two_pages_pages_on_from_where_it_was() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let lines = fixture_lines();
    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, lines[..10].concat()).unwrap();
    let (runner, s) = watch(home.path(), state.path());
    let first_ten = runner.transcript_page(s, None, None).unwrap();
    assert!(first_ten.at_start);

    let newest = runner.transcript_page(s, None, Some(2)).unwrap();
    assert!(!newest.at_start);
    assert_eq!(newest.to, len(&lines[..10]));
    // The session goes on before the client asks for the page before.
    append(&path, &lines[10..].concat());
    let older = runner.transcript_page(s, Some(newest.from), None).unwrap();
    assert_eq!(older.to, newest.from);
    assert!(older.at_start);
    let joined: Vec<TranscriptItem> = older.items.iter().chain(&newest.items).cloned().collect();
    assert_eq!(joined, first_ten.items);

    // The newest page now ends at the new end.
    let now = runner.transcript_page(s, None, None).unwrap();
    assert_eq!(now.to, len(&lines));
    assert!(now.items.len() > first_ten.items.len());
    runner.stop();
}

#[test]
fn limits_default_to_200_and_stop_at_1000() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let records = 1500;
    let text: String = (0..records).map(prompt_line).collect();
    std::fs::write(claude_file(home.path(), FIXTURE_ID), text).unwrap();
    let (runner, s) = watch(home.path(), state.path());

    let count = |limit| runner.transcript_page(s, None, limit).unwrap().items.len();
    assert_eq!(DEFAULT_PAGE_LIMIT, 200);
    assert_eq!(MAX_PAGE_LIMIT, 1000);
    assert_eq!(count(None), DEFAULT_PAGE_LIMIT);
    assert_eq!(count(Some(7)), 7);
    assert_eq!(count(Some(1500)), MAX_PAGE_LIMIT);
    assert_eq!(count(Some(usize::MAX)), MAX_PAGE_LIMIT);
    // Zero counts as one, so paging back always moves.
    assert_eq!(count(Some(0)), 1);

    // The newest first, and nothing older left out between pages.
    let newest = runner.transcript_page(s, None, None).unwrap();
    assert!(!newest.at_start);
    assert_eq!(prompt_text(&newest.items[0]), "prompt 1300");
    assert_eq!(prompt_text(&newest.items[199]), "prompt 1499");
    let older = runner
        .transcript_page(s, Some(newest.from), Some(MAX_PAGE_LIMIT))
        .unwrap();
    assert_eq!(prompt_text(&older.items[0]), "prompt 300");
    assert_eq!(prompt_text(&older.items[999]), "prompt 1299");
    assert!(!older.at_start);
    runner.stop();
}

/// Claude's adapter, except that its pages fail.
#[derive(Debug, Default)]
struct BrokenPages(ClaudeAdapter);

impl SourceAdapter for BrokenPages {
    fn engine(&self) -> Engine {
        self.0.engine()
    }
    fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        self.0.discover(home)
    }
    fn read_from(&self, t: &TranscriptRef, cursor: &Cursor) -> Result<ParseChunk, SourceError> {
        self.0.read_from(t, cursor)
    }
    fn read_page(
        &self,
        _: &TranscriptRef,
        _: Option<u64>,
        _: usize,
    ) -> Result<TranscriptPage, SourceError> {
        panic!("test double: pages cannot be read");
    }
}

fn unavailable(r: &Result<TranscriptPage, PageError>, s: SessionId) -> bool {
    matches!(r, Err(PageError::Unavailable { session, .. }) if *session == s)
}

#[test]
fn an_unknown_session_and_a_transcript_that_cannot_be_read_are_different_errors() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let lines = fixture_lines();
    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, lines[..5].concat()).unwrap();
    let (runner, s) = watch(home.path(), state.path());
    let stranger = SessionId::new();
    assert!(matches!(
        runner.transcript_page(stranger, None, None),
        Err(PageError::UnknownSession(id)) if id == stranger
    ));

    // Deleted: unavailable, whether or not the watcher has noticed yet.
    std::fs::remove_file(&path).unwrap();
    let gone = runner.transcript_page(s, None, None);
    assert!(unavailable(&gone, s), "{gone:?}");
    runner.stop();

    // After a restart the index still has the session, not its transcript.
    let (runner, sink) = {
        let sink = Arc::new(CollectSink::default());
        let runner = pitcrew_runner::start(
            config(home.path(), state.path()),
            vec![Arc::new(ClaudeAdapter::new())],
            sink.clone(),
        )
        .unwrap();
        (runner, sink)
    };
    let after = runner.transcript_page(s, None, None);
    assert!(unavailable(&after, s), "{after:?}");
    assert!(matches!(
        runner.transcript_page(stranger, None, None),
        Err(PageError::UnknownSession(_))
    ));
    runner.stop();
    assert_eq!(sink.len(), 0);

    // An adapter whose pages fail (here, panic): unavailable, and the runner goes on.
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(claude_file(home.path(), FIXTURE_ID), lines.concat()).unwrap();
    let (runner, s) = watch_with(home.path(), state.path(), Arc::new(BrokenPages::default()));
    let broken = runner.transcript_page(s, None, None);
    assert!(unavailable(&broken, s), "{broken:?}");
    let again = runner.transcript_page(s, None, None);
    assert!(unavailable(&again, s), "{again:?}");
    runner.stop();
}

/// What the API route holds: transcripts taken from the handle, read on other threads, the same
/// pages as the handle's; and still readable after the runner stops, like its hooks.
#[test]
fn transcripts_taken_from_the_handle_read_the_same_pages() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(
        claude_file(home.path(), FIXTURE_ID),
        fixture_lines().concat(),
    )
    .unwrap();
    let (runner, s) = watch(home.path(), state.path());
    let transcripts = runner.transcripts();
    let reader = std::thread::spawn({
        let transcripts = transcripts.clone();
        move || transcripts.transcript_page(s, None, Some(4)).unwrap()
    });
    let page = runner.transcript_page(s, None, Some(4)).unwrap();
    assert_eq!(reader.join().unwrap(), page);
    runner.stop();
    assert_eq!(transcripts.transcript_page(s, None, Some(4)).unwrap(), page);
}
