//! Test helpers: a collecting sink, a source that logs its reads, and a fast configuration.

#![allow(dead_code, clippy::unwrap_used)]

use pitcrew_interfaces::fake::FakeSource;
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptItem, TranscriptPage, TranscriptRef,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
use pitcrew_protocol::model::{Engine, Receipt};
use pitcrew_runner::{EventSink, RunnerConfig, SinkError, Timing};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Collects events; can be closed so `accept` blocks (to test backpressure).
#[derive(Debug, Default)]
pub struct CollectSink {
    events: Mutex<Vec<(Instant, Event)>>,
    closed: Mutex<bool>,
    cv: Condvar,
}

impl CollectSink {
    pub fn close(&self) {
        *self.closed.lock().unwrap() = true;
    }

    pub fn open(&self) {
        *self.closed.lock().unwrap() = false;
        self.cv.notify_all();
    }

    pub fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(_, e)| e.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Waits until at least `n` events have arrived; returns when the n-th arrived.
    pub fn wait_for(&self, n: usize, timeout: Duration) -> Option<Instant> {
        let end = Instant::now() + timeout;
        let mut events = self.events.lock().unwrap();
        while events.len() < n {
            let now = Instant::now();
            if now >= end {
                return None;
            }
            events = self.cv.wait_timeout(events, end - now).unwrap().0;
        }
        Some(events[n - 1].0)
    }
}

impl EventSink for CollectSink {
    fn accept(&self, events: &[Event]) -> Result<(), SinkError> {
        let mut closed = self.closed.lock().unwrap();
        while *closed {
            closed = self.cv.wait(closed).unwrap();
        }
        drop(closed);
        let now = Instant::now();
        self.events
            .lock()
            .unwrap()
            .extend(events.iter().cloned().map(|e| (now, e)));
        self.cv.notify_all();
        Ok(())
    }
}

/// Wraps a [`FakeSource`] whose items can be replaced (to "append"), and logs every
/// `read_from` cursor offset. Every transcript serves the same items.
#[derive(Debug)]
pub struct LoggedFake {
    transcripts: Vec<TranscriptRef>,
    inner: Mutex<FakeSource>,
    pub reads: Mutex<Vec<u64>>,
    /// Items served per read (the plain fake serves one).
    per_read: usize,
    /// How long each read takes.
    delay: Duration,
    /// Reads of this transcript panic.
    panic_on: Mutex<Option<PathBuf>>,
}

impl LoggedFake {
    pub fn new(transcripts: Vec<TranscriptRef>, items: Vec<TranscriptItem>) -> Self {
        Self {
            inner: Mutex::new(FakeSource::new(Engine::Claude, transcripts.clone(), items)),
            transcripts,
            reads: Mutex::new(Vec::new()),
            per_read: 1,
            delay: Duration::ZERO,
            panic_on: Mutex::new(None),
        }
    }

    /// Serves up to `n` items per read.
    pub fn per_read(mut self, n: usize) -> Self {
        self.per_read = n.max(1);
        self
    }

    /// Makes every read take `d`.
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }

    /// Makes reads of `path` panic.
    pub fn panic_on(&self, path: &Path) {
        *self.panic_on.lock().unwrap() = Some(path.canonicalize().unwrap());
    }

    pub fn set_items(&self, items: Vec<TranscriptItem>) {
        *self.inner.lock().unwrap() =
            FakeSource::new(Engine::Claude, self.transcripts.clone(), items);
    }

    pub fn reads(&self) -> Vec<u64> {
        self.reads.lock().unwrap().clone()
    }
}

impl SourceAdapter for LoggedFake {
    fn engine(&self) -> Engine {
        Engine::Claude
    }

    fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        self.inner.lock().unwrap().discover(home)
    }

    fn read_from(&self, t: &TranscriptRef, cursor: &Cursor) -> Result<ParseChunk, SourceError> {
        self.reads.lock().unwrap().push(cursor.offset);
        let bad = self.panic_on.lock().unwrap().as_deref() == Some(t.path.as_path());
        assert!(!bad, "fake adapter: cannot parse {}", t.path.display());
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let inner = self.inner.lock().unwrap();
        let mut chunk = inner.read_from(t, cursor)?;
        for _ in 1..self.per_read {
            let next = inner.read_from(t, &chunk.cursor)?;
            if next.items.is_empty() {
                break;
            }
            chunk.items.extend(next.items);
            chunk.cursor = next.cursor;
        }
        Ok(chunk)
    }

    fn read_page(
        &self,
        t: &TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, SourceError> {
        self.inner.lock().unwrap().read_page(t, before, limit)
    }
}

pub fn turn(i: u64) -> TranscriptItem {
    TranscriptItem::TurnEnded {
        at: 1_790_000_000_000 + i64::try_from(i).unwrap(),
        offset: i,
    }
}

pub fn prompt(i: u64) -> TranscriptItem {
    TranscriptItem::UserPrompt {
        at: 1_790_000_000_000 + i64::try_from(i).unwrap(),
        text: "go on".into(),
        offset: i,
    }
}

/// Polls `f` until it is true or `timeout` passes.
pub fn eventually(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

/// Sets a file's mtime to `ago` before now.
pub fn age(path: &Path, ago: Duration) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() - ago).unwrap();
}

pub fn transcript_ref(path: &Path) -> TranscriptRef {
    TranscriptRef {
        engine: Engine::Claude,
        path: path.to_path_buf(),
        inner_id: None,
        size: 0,
        modified: 0,
    }
}

/// A configuration with the default 100 ms debounce, and slow sweeps so notifications must do
/// the work.
pub fn config(home: &Path, state: &Path) -> RunnerConfig {
    let mut c = RunnerConfig::new(WorkspaceId::new(), MachineId::new(), MemberId::new(), state)
        .with_home(Engine::Claude, home);
    c.timing = Timing {
        cold_interval: Duration::from_secs(600),
        rediscover_interval: Duration::from_secs(600),
        ..Timing::default()
    };
    c
}

/// Puts a whole file at `path` at once: written beside it under a name no adapter discovers,
/// then renamed into place, so a discovery never sees it half written.
pub fn place(path: &Path, bytes: &[u8]) {
    let part = path.with_extension("part");
    std::fs::write(&part, bytes).unwrap();
    std::fs::rename(&part, path).unwrap();
}

pub fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
    f.sync_all().unwrap();
}

/// A short label per event, for comparing sequences.
pub fn label(e: &Event) -> String {
    match &e.body {
        EventBody::SessionDiscovered { session } => format!("discovered:{:?}", session.state),
        EventBody::SessionStateChanged { to, .. } => format!("state:{to:?}"),
        EventBody::ToolRan { tool, .. } => format!("tool:{tool}"),
        EventBody::FileEdited { path, .. } => format!("edit:{path}"),
        EventBody::TurnEnded {
            receipt: Receipt::Transcript { offset, .. },
            ..
        } => format!("turn@{offset}"),
        EventBody::SessionEnded { .. } => "ended".into(),
        EventBody::SessionLinked { basis, .. } => format!("linked:{basis:?}"),
        other => format!("{other:?}"),
    }
}

pub fn labels(events: &[Event]) -> Vec<String> {
    events.iter().map(label).collect()
}

pub fn fixture_transcript() -> PathBuf {
    pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl")
}

/// The Claude fixture's session id.
pub const FIXTURE_ID: &str = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";

/// The fixture's lines, each with its newline.
pub fn fixture_lines() -> Vec<Vec<u8>> {
    std::fs::read(fixture_transcript())
        .unwrap()
        .split_inclusive(|b| *b == b'\n')
        .map(<[u8]>::to_vec)
        .collect()
}

/// The fixture's lines, as the session `id`.
pub fn fixture_lines_as(id: &str) -> Vec<Vec<u8>> {
    fixture_lines()
        .into_iter()
        .map(|l| {
            String::from_utf8(l)
                .unwrap()
                .replace(FIXTURE_ID, id)
                .into_bytes()
        })
        .collect()
}

/// Where Claude keeps the transcript of session `id` in `home`; the folder is created.
pub fn claude_file(home: &Path, id: &str) -> PathBuf {
    let dir = home.join("projects").join("-w-paper");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{id}.jsonl"))
}

/// The session an event is about.
pub fn session_of(e: &Event) -> Option<pitcrew_protocol::ids::SessionId> {
    match &e.body {
        EventBody::SessionDiscovered { session } => Some(session.id),
        EventBody::SessionStateChanged { session, .. }
        | EventBody::ToolRan { session, .. }
        | EventBody::FileEdited { session, .. }
        | EventBody::TurnEnded { session, .. }
        | EventBody::SessionEnded { session }
        | EventBody::SessionLinked { session, .. } => Some(*session),
        _ => None,
    }
}

/// The session in the first `session_discovered` event.
pub fn discovered(events: &[Event]) -> pitcrew_protocol::model::Session {
    events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::SessionDiscovered { session } => Some(session.clone()),
            _ => None,
        })
        .unwrap()
}
