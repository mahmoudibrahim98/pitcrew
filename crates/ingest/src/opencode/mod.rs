//! OpenCode sessions, from its SQLite store `<home>/opencode.db`, where `home` is
//! `$XDG_DATA_HOME/opencode` or `~/.local/share/opencode` (also on Windows). One transcript per
//! session: [`TranscriptRef::path`] is the database, [`TranscriptRef::inner_id`] the session id.
//!
//! Verified against OpenCode 1.18 (sessions written by 1.3 to 1.18). Older versions kept JSON
//! files under `storage/`; current versions migrate them into the database, and that format is
//! not read here.
//!
//! # Offsets, for consumers
//!
//! Item offsets are **ordering positions, not byte offsets**:
//! - an offset is the part's creation time on OpenCode's id scale (see Positions);
//! - a tool call and its result share one offset, the tool part's, and so do its `PlanUpdated`,
//!   `Question` and `FileEdit` items;
//! - a result can arrive in a later read than its call, after items with higher offsets;
//! - two items can share an offset (the items of one part, a turn end after its last part, or two
//!   parts created at the same position), so an item is identified by its offset *and* its
//!   content, never by `(session, offset)` alone.
//!
//! [`Cursor::offset`] is the frontier's position: everything at or below it has been emitted. The
//! cursor itself resumes from `(offset, part id)`.
//!
//! # Positions
//!
//! A position is a part's creation time on OpenCode's id scale: milliseconds × 4096 plus a
//! counter. OpenCode ids (`prt_` + 12 hex digits + random) hold the low 48 bits of that number;
//! the full value is recovered with the row's `time_created`. So positions are stable, unique
//! within a session, and increase in creation order. A decoded value more than a day *after* the
//! row's `time_created` cannot be a creation time (ids are made before their rows are written),
//! so such ids, and ids of another shape, fall back to `time_created × 4096`. Earlier values are
//! kept: `opencode import` writes parts with their ids but the import time as `time_created`.
//! Times a payload lacks are taken from the position too. Parts that share a position are never
//! split across pages. Positions are capped at [`MAX_POSITION`] (2^53 − 1, exact as a JSON
//! number), which real times reach in 2039.
//!
//! # Incremental reads
//!
//! Parts change while a turn streams, so each part is shown in up to two steps (see
//! [`parse_part`]): a running tool's call as soon as its input is known, and its result (or a
//! text part, or anything else) once it is final. A part is final when its own state says so, or
//! when its message is settled: completed, failed, or followed by a newer **assistant** message.
//! (A prompt typed while the agent is busy is written at once but answered only after the running
//! message ends, so a newer user message settles nothing.) An unfinished tool of a settled message
//! is reported as interrupted. Each step is emitted once, with the part's position as its offset,
//! and **changes to a part after it is final are not re-emitted**. A tool part whose payload
//! becomes unreadable after its call was shown still gets a result saying so.
//!
//! A turn ends with a `step-finish` part whose reason is not `tool-calls`, or with an assistant
//! message that failed (an error or an abort). A failed message's `TurnEnded` follows its last
//! part, at that part's offset, or sits at the message's own position when it has no parts.
//!
//! The cursor keeps a frontier (the newest position below which everything is final and emitted)
//! and the parts above it already shown. The frontier only passes parts whose message is settled,
//! never passes a running assistant message, and only passes parts made more than [`SETTLE_MS`]
//! (by their positions) before the newest part was created or updated, so a part inserted a
//! little late is still read. So one read of a finished session gives exactly the items of
//! reading it in pieces, and reads during a turn give the same items, a result possibly arriving
//! after later calls.
//!
//! # Bounded work
//!
//! There is no index on part times, but `part_session_idx` orders a session's parts by rowid,
//! which is insertion order and follows creation order (a real store had no exceptions). Reads
//! walk that index newest first, in batches, and treat a part as next in position order once
//! every part not yet fetched is known to be older, allowing parts to be out of order by up to
//! [`SETTLE_MS`]. Messages without parts are found the same way through
//! `message_session_time_created_id_idx`. So a page, or a read with nothing new, touches only the
//! newest rows, and a page further back jumps there by binary search on rowids. Only a table
//! without rowids is listed whole.
//!
//! # Discovery
//!
//! [`TranscriptRef::size`] is a change counter, **not a size in bytes**: the highest rowid among
//! the session's parts (0 without parts). It grows when parts are added, does not change when a
//! part is updated in place, and can go down when parts are deleted (a revert), which is not a
//! truncation. [`TranscriptRef::modified`] is the session's `time_updated`. Neither reads part
//! payloads. A store that cannot be read is skipped with a warning, logged once per change.

mod parse;
mod store;

pub use crate::jsonl::ReadReport;
pub use parse::{MessageInfo, PartItems, Phase, Role, parse_part};

use crate::bound::{MAX_PATH_BYTES, MAX_TITLE_CHARS, bounded};
use crate::jsonl::Skips;
use crate::lines::{MAX_LINE_BYTES, SkipReason, SkippedLine};
use crate::text::title;
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SessionMeta, SourceAdapter, SourceError, TranscriptItem, TranscriptPage,
    TranscriptRef,
};
use pitcrew_protocol::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use store::{MessageKey, PartRow, Store};

/// The frontier only passes parts made at least this long before the newest part activity.
pub const SETTLE_MS: i64 = 60_000;
/// The largest position: 2^53 − 1, the largest integer a JSON number holds exactly.
pub const MAX_POSITION: u64 = (1 << 53) - 1;
/// Parts above the frontier the cursor remembers; past this, the frontier is forced forward.
const MAX_OPEN: usize = 4096;
/// OpenCode ids hold the low 48 bits of a position.
const ID_SPAN: u64 = 1 << 48;
/// How far out of rowid order a part's position may be: [`SETTLE_MS`] on the position scale.
const MARGIN: u64 = SETTLE_MS.unsigned_abs() * 4096;
/// How far an id's time may run ahead of its row's `time_created`: a day, for clock changes.
const MAX_ID_LEAD: u64 = 86_400_000 * 4096;
/// Parts fetched per step of a walk.
const BATCH: usize = 256;
/// Messages fetched per step of a walk.
const MESSAGE_BATCH: usize = 64;
/// Unreadable stores remembered for logging once; past this, the memory starts over.
const MAX_UNREADABLE_NOTES: usize = 1024;

/// Reads OpenCode sessions. Read-only: the store is opened with `SQLITE_OPEN_READ_ONLY`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    /// A new adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The OpenCode data folder on this machine: `$XDG_DATA_HOME/opencode`, else
    /// `~/.local/share/opencode`.
    #[must_use]
    pub fn default_home() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir).join("opencode"));
        }
        crate::user_home().map(|home| home.join(".local").join("share").join("opencode"))
    }

    /// [`SourceAdapter::read_from`], also returning skipped parts and the payload bytes read.
    /// Skipped parts are reported with their position as `offset` and their payload size as
    /// `len`.
    ///
    /// # Errors
    ///
    /// [`SourceError::Unreadable`] if the session or a required table is missing;
    /// [`SourceError::Io`] with [`io::ErrorKind::WouldBlock`] if a writer holds the store or
    /// changed it during the read (retry later).
    pub fn read(
        &self,
        transcript: &TranscriptRef,
        cursor: &Cursor,
    ) -> Result<ReadReport, SourceError> {
        let session_id = session_id(transcript)?;
        let store = Store::open(&transcript.path)?;
        store.require_transcripts()?;
        let session = store
            .session(session_id)?
            .ok_or_else(|| missing_session(transcript))?;
        let first_read = cursor.state.is_none();
        let mut state: ReadState = cursor
            .state
            .clone()
            .and_then(|s| serde_json::from_value(s).ok())
            .unwrap_or_default();

        // Every entry above the frontier, oldest first.
        let mut walker = Walker::new(&store, session_id, u64::MAX, None, None);
        let mut entries = Vec::new();
        while let Some(e) = walker.next()? {
            if let Some(frontier) = &state.frontier
                && e.key() <= (frontier.0, frontier.1.as_str())
            {
                break;
            }
            entries.push(e);
        }
        entries.reverse();
        // The settle clock is the parts' own: message and session times can run ahead of parts
        // still being written.
        let now = walker.newest_activity;
        let mut messages = Messages::load(&store, session_id)?;

        let open: HashMap<String, Shown> = std::mem::take(&mut state.open).into_iter().collect();
        let mut shown: HashMap<&str, Shown> = HashMap::new();
        let mut items = Vec::new();
        let mut skips = Skips::default();
        let mut bytes_read = 0u64;
        for e in &entries {
            let before = open.get(e.id());
            if before == Some(&Shown::Done) {
                shown.insert(e.id(), Shown::Done);
                continue;
            }
            let row = match &e.entry {
                Entry::Failed { at, .. } => {
                    items.push(TranscriptItem::TurnEnded {
                        at: *at,
                        offset: e.offset,
                    });
                    shown.insert(e.id(), Shown::Done);
                    continue;
                }
                Entry::Part(row) => row,
            };
            // Shown whole while its message was open: only the message is looked at again.
            if let Some(&Shown::Emitted(ended)) = before {
                let msg = messages.get(&store, &row.message_id)?;
                let next = if msg.info.settled {
                    let end = failed_turn_end(&store, &mut messages, &msg, row, e.offset, ended)?;
                    items.extend(end);
                    Shown::Done
                } else {
                    Shown::Emitted(ended)
                };
                shown.insert(e.id(), next);
                continue;
            }
            let PartNow { mut got, skip, msg } =
                examine(&store, e.offset, row, &mut messages, &mut bytes_read)?;
            if let Some(skip) = skip {
                // A call already shown still gets a result.
                if let Some(Shown::Started(call)) = before {
                    got.tail
                        .push(unread_result(call, row, e.offset, &skip.reason));
                }
                skips.note(&transcript.path, skip);
            }
            let next = match got.phase {
                Phase::Waiting => before.cloned(),
                Phase::Started => match before {
                    Some(started) => Some(started.clone()),
                    None => {
                        let call = started_call(&got.head);
                        items.extend(got.head);
                        Some(Shown::Started(call))
                    }
                },
                Phase::Done => {
                    let ended = ends_turn(&got);
                    if before.is_none() {
                        items.extend(got.head);
                    }
                    items.extend(got.tail);
                    if msg.info.settled {
                        let end =
                            failed_turn_end(&store, &mut messages, &msg, row, e.offset, ended)?;
                        items.extend(end);
                        Some(Shown::Done)
                    } else {
                        // Its message may still fail, which would end the turn after it.
                        Some(Shown::Emitted(ended))
                    }
                }
            };
            if let Some(next) = next {
                shown.insert(e.id(), next);
            }
        }

        // The frontier passes the leading entries that are done and quiet, and stops below a
        // running assistant message.
        let running = messages.running(&store)?;
        let mut passed = 0;
        for e in &entries {
            let quiet = id_time(e.offset) <= now.saturating_sub(SETTLE_MS);
            let before_running = running.is_none_or(|r| e.offset < r);
            if !(quiet && before_running && shown.get(e.id()) == Some(&Shown::Done)) {
                break;
            }
            passed += 1;
        }
        // A store with thousands of unfinished parts would grow the cursor without bound: force
        // the frontier on, giving up the results those parts may still get.
        while shown.len() > MAX_OPEN && passed < entries.len() {
            shown.remove(entries[passed].id());
            passed += 1;
        }
        for e in &entries[..passed] {
            shown.remove(e.id());
            state.frontier = Some((e.offset, e.id().to_owned()));
        }
        state.open = entries[passed..]
            .iter()
            .filter_map(|e| shown.get(e.id()).map(|s| (e.id().to_owned(), s.clone())))
            .collect();

        let had_prompt = state.first_prompt.is_some();
        if !had_prompt {
            state.first_prompt = items.iter().find_map(|item| match item {
                TranscriptItem::UserPrompt { text, .. } => Some(title(text, MAX_TITLE_CHARS)),
                _ => None,
            });
        }
        let model = match store.latest_model(session_id)? {
            Some(m) => Some(m),
            None => session.model.as_deref().and_then(parse::model_from_json),
        };
        store.finish()?;
        let meta_changed = first_read
            || state.session_updated != Some(session.updated)
            || state.model != model
            || (!had_prompt && state.first_prompt.is_some());
        state.session_updated = Some(session.updated);
        state.model = model;
        let meta = meta_changed.then(|| session_meta(&session, &state));

        let (skipped, skipped_total) = skips.finish(&transcript.path);
        let offset = state.frontier.as_ref().map_or(0, |f| f.0);
        let state = serde_json::to_value(&state).map_err(|e| SourceError::Unreadable {
            path: transcript.path.clone(),
            reason: format!("cannot encode the cursor: {e}"),
        })?;
        Ok(ReadReport {
            chunk: ParseChunk {
                cursor: Cursor {
                    offset,
                    state: Some(state),
                },
                meta,
                items,
            },
            skipped,
            skipped_total,
            bytes_read,
        })
    }
}

impl SourceAdapter for OpenCodeAdapter {
    fn engine(&self) -> Engine {
        Engine::OpenCode
    }

    /// One transcript per session of every `opencode*.db` in `home`. `modified` is the session's
    /// `time_updated`; `size` is a change counter, not bytes (see the module docs).
    fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        let entries = match fs::read_dir(home) {
            Ok(entries) => entries,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Regular files only: links are not followed out of the home.
            if !entry.file_type().is_ok_and(|t| t.is_file())
                || !name.starts_with("opencode")
                || !name.ends_with(".db")
            {
                continue;
            }
            let path = entry.path();
            // A locked or changing store fails the whole call, to be retried; one that cannot be
            // read is skipped.
            let listed = Store::open(&path).and_then(|store| {
                let sessions = if store.has_sessions() {
                    store.sessions()?
                } else {
                    Vec::new()
                };
                store.finish()?;
                Ok(sessions)
            });
            let sessions = match listed {
                Ok(sessions) => {
                    note_unreadable(&path, None);
                    sessions
                }
                Err(SourceError::Io(e)) => return Err(SourceError::Io(e)),
                Err(e) => {
                    if note_unreadable(&path, Some(&e.to_string())) {
                        tracing::warn!(path = %path.display(), error = %e, "skipped an unreadable OpenCode store");
                    }
                    continue;
                }
            };
            for (session, change) in sessions {
                out.push(TranscriptRef {
                    engine: Engine::OpenCode,
                    path: path.clone(),
                    inner_id: Some(session.id),
                    size: change,
                    modified: session.updated,
                });
            }
        }
        out.sort_by(|a, b| (&a.path, &a.inner_id).cmp(&(&b.path, &b.inner_id)));
        Ok(out)
    }

    fn read_from(
        &self,
        transcript: &TranscriptRef,
        cursor: &Cursor,
    ) -> Result<ParseChunk, SourceError> {
        self.read(transcript, cursor).map(|r| r.chunk)
    }

    /// Items of the current state, newest first by position: a running tool shows its call,
    /// unfinished text shows nothing yet.
    fn read_page(
        &self,
        transcript: &TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, SourceError> {
        let session_id = session_id(transcript)?;
        let store = Store::open(&transcript.path)?;
        store.require_transcripts()?;
        if store.session(session_id)?.is_none() {
            return Err(missing_session(transcript));
        }
        let mut messages = Messages::load(&store, session_id)?;

        // The newest entry gives the end of the transcript.
        let mut walker = Walker::new(&store, session_id, u64::MAX, None, None);
        let newest = walker.next()?;
        // Positions are capped well below `u64::MAX`, so this never saturates.
        let last = newest.as_ref().map_or(0, |p| p.offset.saturating_add(1));
        let end = before.map_or(last, |b| b.min(last));
        if end == last {
            walker.end = last;
            walker.put_back(newest);
        } else {
            let parts = start_below(&store, session_id, end.saturating_add(MARGIN))?;
            let from = id_time(end).saturating_add(SETTLE_MS);
            let msgs = store
                .messages_from(session_id, from, |id, created| position(id, created) >= end)?;
            walker = Walker::new(&store, session_id, end, parts, msgs);
        }

        // Whole groups of entries, newest first, until `limit` items; entries that share a
        // position stay on one page.
        let mut records: Vec<(u64, Vec<TranscriptItem>)> = Vec::new();
        let mut kept = 0usize;
        let mut skips = Skips::default();
        let mut bytes = 0u64;
        let mut next = walker.next()?;
        while let Some(p) = next.take() {
            if kept >= limit && records.last().is_none_or(|r| r.0 != p.offset) {
                next = Some(p);
                break;
            }
            let got = visible(
                &store,
                &p,
                &mut messages,
                &mut bytes,
                &mut skips,
                transcript,
            )?;
            if !got.is_empty() {
                kept += got.len();
                records.push((p.offset, got));
            }
            next = walker.next()?;
        }
        // Anything older with items?
        let mut at_start = true;
        while let Some(p) = next {
            if !visible(
                &store,
                &p,
                &mut messages,
                &mut bytes,
                &mut skips,
                transcript,
            )?
            .is_empty()
            {
                at_start = false;
                break;
            }
            next = walker.next()?;
        }
        store.finish()?;
        skips.finish(&transcript.path);
        let from = records
            .last()
            .map_or(if at_start { 0 } else { end }, |r| r.0);
        Ok(TranscriptPage {
            items: records.into_iter().rev().flat_map(|r| r.1).collect(),
            from,
            to: end,
            at_start,
        })
    }
}

fn session_id(transcript: &TranscriptRef) -> Result<&str, SourceError> {
    transcript
        .inner_id
        .as_deref()
        .ok_or_else(|| SourceError::Unreadable {
            path: transcript.path.clone(),
            reason: "an OpenCode transcript needs a session id (inner_id)".into(),
        })
}

fn missing_session(transcript: &TranscriptRef) -> SourceError {
    SourceError::Unreadable {
        path: transcript.path.clone(),
        reason: format!(
            "no session {} in the store",
            transcript.inner_id.as_deref().unwrap_or("")
        ),
    }
}

/// Stores last reported unreadable, with the reason.
static UNREADABLE: Mutex<BTreeMap<PathBuf, String>> = Mutex::new(BTreeMap::new());

/// Records whether the store at `path` is unreadable (`Some(reason)`) or readable (`None`).
/// Returns whether that is news worth a warning: a new or changed reason.
fn note_unreadable(path: &Path, reason: Option<&str>) -> bool {
    let mut notes = UNREADABLE.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(reason) = reason else {
        notes.remove(path);
        return false;
    };
    if notes.get(path).is_some_and(|old| old == reason) {
        return false;
    }
    if notes.len() >= MAX_UNREADABLE_NOTES && !notes.contains_key(path) {
        notes.clear();
    }
    notes.insert(path.to_path_buf(), reason.to_owned());
    true
}

/// What a walk yields.
enum Entry {
    Part(PartRow),
    /// A failed assistant message without parts, ended at `at`: its turn's end.
    Failed {
        id: String,
        at: TimestampMs,
    },
}

/// An entry with its position.
struct Positioned {
    offset: u64,
    entry: Entry,
}

impl Positioned {
    fn id(&self) -> &str {
        match &self.entry {
            Entry::Part(row) => &row.id,
            Entry::Failed { id, .. } => id,
        }
    }

    fn key(&self) -> (u64, &str) {
        (self.offset, self.id())
    }
}

/// One of a walk's sources, fetched newest first in batches.
struct Source<K> {
    /// Rows not fetched yet sort below this; `None` is the top.
    below: Option<K>,
    exhausted: bool,
    /// The lowest position fetched: every row not fetched yet is below `floor + MARGIN`.
    floor: u64,
}

impl<K> Source<K> {
    fn new(below: Option<K>) -> Self {
        Self {
            below,
            exhausted: false,
            floor: u64::MAX,
        }
    }

    /// Whether a row not fetched yet may be newer than `top`.
    fn may_pass(&self, top: Option<u64>) -> bool {
        !self.exhausted && top.is_none_or(|t| self.floor.saturating_add(MARGIN) > t)
    }
}

/// A session's parts, and its failed messages without parts, in position order, newest first:
/// parts from the rowid index, messages from the `(time_created, id)` index.
struct Walker<'s> {
    store: &'s Store,
    session: &'s str,
    parts: Source<i64>,
    messages: Source<MessageKey>,
    /// Only entries below this are returned.
    end: u64,
    pool: BTreeMap<(u64, String), Entry>,
    /// The latest time any fetched part was created or updated.
    newest_activity: i64,
}

impl<'s> Walker<'s> {
    fn new(
        store: &'s Store,
        session: &'s str,
        end: u64,
        parts_below: Option<i64>,
        messages_below: Option<MessageKey>,
    ) -> Self {
        Self {
            store,
            session,
            parts: Source::new(parts_below),
            messages: Source::new(messages_below),
            end,
            pool: BTreeMap::new(),
            newest_activity: 0,
        }
    }

    fn fetch_parts(&mut self) -> Result<(), SourceError> {
        let rows = if self.store.parts_have_rowid() {
            let rows = self
                .store
                .parts_below(self.session, self.parts.below, BATCH)?;
            self.parts.exhausted = rows.len() < BATCH;
            rows
        } else {
            self.parts.exhausted = true;
            self.store
                .parts(self.session)?
                .into_iter()
                .map(|row| (0, Some(row)))
                .collect()
        };
        for (rowid, row) in rows {
            self.parts.below = Some(rowid);
            let Some(row) = row else { continue };
            let offset = position(&row.id, row.created);
            self.parts.floor = self.parts.floor.min(offset);
            self.newest_activity = self.newest_activity.max(row.created).max(row.updated);
            if offset < self.end {
                self.pool.insert((offset, row.id.clone()), Entry::Part(row));
            }
        }
        Ok(())
    }

    fn fetch_messages(&mut self) -> Result<(), SourceError> {
        let rows =
            self.store
                .messages_below(self.session, self.messages.below.as_ref(), MESSAGE_BATCH)?;
        self.messages.exhausted = rows.len() < MESSAGE_BATCH;
        for step in rows {
            self.messages.below = Some(step.key);
            let offset = position(&step.id, step.created);
            self.messages.floor = self.messages.floor.min(offset);
            if !step.partless || offset >= self.end {
                continue;
            }
            let facts = self.store.message_facts(&step.id)?;
            if facts.failed && facts.role.as_deref() == Some("assistant") {
                let at = facts.ended.unwrap_or(step.created);
                self.pool
                    .insert((offset, step.id.clone()), Entry::Failed { id: step.id, at });
            }
        }
        Ok(())
    }

    /// The next entry, newest first, once no row still unfetched can be newer.
    fn next(&mut self) -> Result<Option<Positioned>, SourceError> {
        loop {
            let top = self.pool.last_key_value().map(|(k, _)| k.0);
            match (self.parts.may_pass(top), self.messages.may_pass(top)) {
                (false, false) => {
                    return Ok(self
                        .pool
                        .pop_last()
                        .map(|((offset, _), entry)| Positioned { offset, entry }));
                }
                (true, false) => self.fetch_parts()?,
                (false, true) => self.fetch_messages()?,
                (true, true) if self.parts.floor >= self.messages.floor => self.fetch_parts()?,
                (true, true) => self.fetch_messages()?,
            }
        }
    }

    fn put_back(&mut self, entry: Option<Positioned>) {
        if let Some(p) = entry.filter(|p| p.offset < self.end) {
            self.pool.insert((p.offset, p.id().to_owned()), p.entry);
        }
    }
}

/// Where a walk for parts below `target - MARGIN` can start: just under the lowest-rowid part
/// whose position is at least `target`. Parts above it are newer than `target - MARGIN`. Found
/// by binary search on rowids; `None` starts at the top.
fn start_below(store: &Store, session: &str, target: u64) -> Result<Option<i64>, SourceError> {
    if !store.parts_have_rowid() {
        return Ok(None);
    }
    let Some((mut lo, mut hi)) = store.part_rowid_bounds(session)? else {
        return Ok(None);
    };
    let at = |rowid: i64| -> Result<Option<(i64, u64)>, SourceError> {
        Ok(store
            .part_at_or_below(session, rowid)?
            .map(|(r, id, created)| (r, position(&id, created))))
    };
    if at(hi)?.is_none_or(|(_, p)| p < target) {
        return Ok(None);
    }
    while lo < hi {
        let mid = midpoint(lo, hi);
        if at(mid)?.is_some_and(|(_, p)| p >= target) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(match at(lo)? {
        Some((rowid, p)) if p >= target => Some(rowid),
        _ => None,
    })
}

/// The midpoint of `lo..=hi`, rounded down, for any two rowids.
fn midpoint(lo: i64, hi: i64) -> i64 {
    let mid = (i128::from(lo) + i128::from(hi)).div_euclid(2);
    i64::try_from(mid).unwrap_or(lo)
}

/// A row's position: its creation time on OpenCode's id scale, recovered from the id's low 48
/// bits and the row's `time_created` (see the module docs).
#[must_use]
pub fn position(id: &str, created: TimestampMs) -> u64 {
    let base = u64::try_from(created)
        .unwrap_or(0)
        .saturating_mul(4096)
        .min(MAX_POSITION);
    let decoded = id_bits(id).map(|bits| {
        let near = (base & !(ID_SPAN - 1)) | bits;
        [
            near.checked_sub(ID_SPAN),
            Some(near),
            near.checked_add(ID_SPAN),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|c| c.abs_diff(base))
        .unwrap_or(near)
    });
    let pos = match decoded {
        // Without a creation time (no `time_created` column), the id's bits are all there is.
        Some(d) if created <= 0 || d <= base.saturating_add(MAX_ID_LEAD) => d,
        _ => base,
    };
    pos.min(MAX_POSITION)
}

/// The time, in milliseconds, of a position.
fn id_time(position: u64) -> TimestampMs {
    TimestampMs::try_from(position / 4096).unwrap_or(TimestampMs::MAX)
}

/// The 48 bits after an id's `prefix_`.
fn id_bits(id: &str) -> Option<u64> {
    let (_, rest) = id.split_once('_')?;
    let hex = rest.get(..12)?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

/// What a part needs to know about its message, and what the frontier does.
#[derive(Clone, Debug)]
struct MessageState {
    info: MessageInfo,
    /// It is an assistant message that failed (an abort included).
    failed: bool,
    /// When it completed or failed, if recorded.
    ended: Option<TimestampMs>,
}

/// Message facts, loaded as parts need them.
struct Messages {
    /// The newest assistant message: its position and id.
    newest_assistant: Option<(u64, String)>,
    info: HashMap<String, MessageState>,
    /// Each failed message's last part.
    last_parts: HashMap<String, Option<(u64, String)>>,
}

impl Messages {
    fn load(store: &Store, session: &str) -> Result<Self, SourceError> {
        let newest = store.newest_assistant(session)?;
        Ok(Self {
            newest_assistant: newest.map(|m| (position(&m.id, m.created), m.id)),
            info: HashMap::new(),
            last_parts: HashMap::new(),
        })
    }

    fn get(&mut self, store: &Store, id: &str) -> Result<MessageState, SourceError> {
        if let Some(state) = self.info.get(id) {
            return Ok(state.clone());
        }
        let state = match store.message_created(id)? {
            // A part whose message is gone will not change again.
            None => MessageState {
                info: MessageInfo {
                    settled: true,
                    ..MessageInfo::default()
                },
                failed: false,
                ended: None,
            },
            Some(created) => {
                let pos = position(id, created);
                let facts = store.message_facts(id)?;
                let role = match facts.role.as_deref() {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    _ => Role::Unknown,
                };
                // Only a newer assistant message settles: a newer prompt may be queued while
                // this message is still running.
                let followed = self
                    .newest_assistant
                    .as_ref()
                    .is_some_and(|(newest, _)| *newest > pos);
                MessageState {
                    info: MessageInfo {
                        role,
                        settled: facts.completed || facts.failed || followed,
                        model: facts.model,
                    },
                    failed: facts.failed && role == Role::Assistant,
                    ended: facts.ended,
                }
            }
        };
        self.info.insert(id.to_owned(), state.clone());
        Ok(state)
    }

    /// The newest assistant message's position while it is still running: the frontier stays
    /// below it, so its parts can still be told that it failed.
    fn running(&mut self, store: &Store) -> Result<Option<u64>, SourceError> {
        let Some((pos, id)) = self.newest_assistant.clone() else {
            return Ok(None);
        };
        Ok((!self.get(store, &id)?.info.settled).then_some(pos))
    }

    /// Whether the part at `key` is the last of `message`'s parts.
    fn is_last_part(
        &mut self,
        store: &Store,
        message: &str,
        key: (u64, &str),
    ) -> Result<bool, SourceError> {
        if !self.last_parts.contains_key(message) {
            let last = store
                .message_parts(message)?
                .into_iter()
                .map(|(id, created)| (position(&id, created), id))
                .max();
            self.last_parts.insert(message.to_owned(), last);
        }
        Ok(self
            .last_parts
            .get(message)
            .and_then(Option::as_ref)
            .is_some_and(|(p, id)| (*p, id.as_str()) == key))
    }
}

/// What one part gives now.
struct PartNow {
    /// Its items; a part whose payload cannot be used gives none.
    got: PartItems,
    /// Why its payload could not be used.
    skip: Option<SkippedLine>,
    /// Its message.
    msg: MessageState,
}

fn examine(
    store: &Store,
    offset: u64,
    row: &PartRow,
    messages: &mut Messages,
    bytes_read: &mut u64,
) -> Result<PartNow, SourceError> {
    let msg = messages.get(store, &row.message_id)?;
    let (got, skip) = match load_part(store, offset, row, &msg.info, bytes_read)? {
        Ok(got) => (got, None),
        Err(skip) => (
            PartItems {
                phase: Phase::Done,
                head: Vec::new(),
                tail: Vec::new(),
            },
            Some(skip),
        ),
    };
    Ok(PartNow { got, skip, msg })
}

/// Whether a part's own items end the turn (a final `step-finish`).
fn ends_turn(got: &PartItems) -> bool {
    matches!(
        got.head.iter().chain(&got.tail).last(),
        Some(TranscriptItem::TurnEnded { .. })
    )
}

/// The end of a failed turn, after the part at `offset`: when its message is settled and failed,
/// the part is the message's last, and its own items (`ended`) did not end the turn already.
fn failed_turn_end(
    store: &Store,
    messages: &mut Messages,
    msg: &MessageState,
    row: &PartRow,
    offset: u64,
    ended: bool,
) -> Result<Option<TranscriptItem>, SourceError> {
    if ended
        || !(msg.info.settled && msg.failed)
        || !messages.is_last_part(store, &row.message_id, (offset, &row.id))?
    {
        return Ok(None);
    }
    Ok(Some(TranscriptItem::TurnEnded {
        at: msg.ended.unwrap_or(row.updated.max(row.created)),
        offset,
    }))
}

/// A part's items, or why it was skipped. Store errors are the outer `Err`.
fn load_part(
    store: &Store,
    offset: u64,
    row: &PartRow,
    msg: &MessageInfo,
    bytes_read: &mut u64,
) -> Result<Result<PartItems, SkippedLine>, SourceError> {
    let skip = |len: u64, reason| {
        Ok(Err(SkippedLine {
            offset,
            len,
            reason,
        }))
    };
    let size = row.size.unwrap_or(0);
    if size > MAX_LINE_BYTES as u64 {
        return skip(size, SkipReason::TooLong);
    }
    let Some(data) = store.part_data(&row.id)? else {
        return skip(0, SkipReason::Malformed("no payload".into()));
    };
    *bytes_read += data.len() as u64;
    // Times missing from the payload come from the id, which an import keeps, rather than from
    // `time_created`, which it does not.
    Ok(
        parse_part(&data, msg, offset, id_time(offset)).map_err(|reason| SkippedLine {
            offset,
            len: data.len() as u64,
            reason,
        }),
    )
}

/// The items a page shows for an entry now.
fn visible(
    store: &Store,
    p: &Positioned,
    messages: &mut Messages,
    bytes: &mut u64,
    skips: &mut Skips,
    transcript: &TranscriptRef,
) -> Result<Vec<TranscriptItem>, SourceError> {
    let row = match &p.entry {
        Entry::Failed { at, .. } => {
            return Ok(vec![TranscriptItem::TurnEnded {
                at: *at,
                offset: p.offset,
            }]);
        }
        Entry::Part(row) => row,
    };
    let PartNow { got, skip, msg } = examine(store, p.offset, row, messages, bytes)?;
    if let Some(skip) = skip {
        skips.note(&transcript.path, skip);
    }
    let (done, ended) = (got.phase == Phase::Done, ends_turn(&got));
    let mut items = got.visible();
    if done {
        items.extend(failed_turn_end(
            store, messages, &msg, row, p.offset, ended,
        )?);
    }
    Ok(items)
}

/// The call id of a started tool part (its first item is the call).
fn started_call(head: &[TranscriptItem]) -> String {
    match head.first() {
        Some(TranscriptItem::ToolUse { call_id, .. }) => call_id.clone(),
        _ => String::new(),
    }
}

/// The result of a call whose part can no longer be read. Its outcome is unknown, so it is not
/// marked an error: an oversized part is usually a large output.
fn unread_result(call: &str, row: &PartRow, offset: u64, reason: &SkipReason) -> TranscriptItem {
    let why = match reason {
        SkipReason::TooLong => "the part is too large",
        SkipReason::InvalidUtf8 => "the part is not UTF-8",
        SkipReason::Malformed(_) => "the part is not readable",
    };
    TranscriptItem::ToolResult {
        at: row.updated.max(row.created),
        call_id: call.to_owned(),
        is_error: false,
        summary: format!("result not read: {why}"),
        offset,
    }
}

/// How much of an entry above the frontier has been emitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Shown {
    /// A tool's call, with its call id: the result is still to come.
    Started(String),
    /// Everything, but its message was still open, so a failure would add a turn end; `true` if
    /// the part's own items already ended the turn.
    Emitted(bool),
    /// Everything.
    Done,
}

/// What the cursor carries between reads.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ReadState {
    /// Position and id of the newest entry below which everything is final and emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frontier: Option<(u64, String)>,
    /// Entries above the frontier already shown, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    open: Vec<(String, Shown)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_updated: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_prompt: Option<String>,
}

/// OpenCode names new sessions `New session - <time>` (and sub-sessions `Child session - …`)
/// until it generates a title; those fall back to the first prompt.
fn session_meta(session: &store::SessionRow, state: &ReadState) -> SessionMeta {
    let placeholder =
        |t: &str| t.starts_with("New session - ") || t.starts_with("Child session - ");
    let title = session
        .title
        .as_deref()
        .filter(|t| !t.trim().is_empty() && !placeholder(t))
        .map(|t| title(t, MAX_TITLE_CHARS))
        .or_else(|| state.first_prompt.clone());
    SessionMeta {
        native_id: session.id.clone(),
        cwd: bounded(session.directory.as_deref(), MAX_PATH_BYTES),
        branch: None,
        title,
        model: state.model.clone(),
        started: (session.created > 0).then_some(session.created),
        is_subagent: session.parent_id.as_deref().is_some_and(|p| !p.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::{ID_SPAN, MAX_ID_LEAD, MAX_POSITION, midpoint, note_unreadable, position};
    use std::path::Path;

    #[test]
    fn positions_come_from_ids_and_survive_the_48_bit_wrap() {
        let ms: i64 = 1_790_756_400_000;
        let full = u64::try_from(ms).expect("positive") * 4096 + 7;
        let id = format!("prt_{:012x}AbCdEfGhIjKlMn", full % ID_SPAN);
        assert_eq!(position(&id, ms), full);
        // time_created a few seconds after the id was made still finds the same value.
        assert_eq!(position(&id, ms + 5000), full);
        // Across a wrap of the low 48 bits, order is kept.
        let wrap_ms = i64::try_from((full / ID_SPAN + 1) * ID_SPAN / 4096).expect("fits");
        let a = u64::try_from(wrap_ms - 1).expect("positive") * 4096;
        let b = u64::try_from(wrap_ms + 1).expect("positive") * 4096;
        let (ida, idb) = (
            format!("prt_{:012x}x", a % ID_SPAN),
            format!("prt_{:012x}x", b % ID_SPAN),
        );
        assert!(ida > idb, "the ids themselves wrap");
        assert!(position(&ida, wrap_ms - 1) < position(&idb, wrap_ms + 1));
        // Other ids fall back to the creation time.
        assert_eq!(position("prt_demo_0001", 10), 40_960);
        assert_eq!(position("x", -5), 0);
        assert_eq!(position("prt_+00000000001", 1), 4096);
    }

    #[test]
    fn positions_are_capped_and_ids_far_ahead_of_their_rows_are_not_trusted() {
        let ms: i64 = 1_790_756_400_000;
        let base = u64::try_from(ms).expect("positive") * 4096;
        let id_at = |pos: u64| format!("prt_{:012x}Synthetic", pos % ID_SPAN);
        // An imported row: written weeks after its id was made. The id is kept.
        let weeks = 30 * 86_400_000 * 4096;
        assert_eq!(position(&id_at(base - weeks), ms), base - weeks);
        // An id hours ahead of its row is clock drift; days ahead is not a creation time.
        let hours = 3 * 3_600_000 * 4096;
        assert_eq!(position(&id_at(base + hours), ms), base + hours);
        assert_eq!(position(&id_at(base + MAX_ID_LEAD + 4096), ms), base);
        // Without a creation time the id's 48 bits are used as they are.
        assert_eq!(position(&id_at(base), 0), base % ID_SPAN);
        // Huge creation times are capped, so `offset + 1` never overflows.
        for created in [i64::MAX, i64::MAX / 4096, 1 << 41] {
            assert!(position(&id_at(base), created) <= MAX_POSITION);
            assert!(position("prt_x", created) <= MAX_POSITION);
        }
    }

    #[test]
    fn midpoints_never_overflow() {
        assert_eq!(midpoint(i64::MIN, i64::MAX), -1);
        assert_eq!(midpoint(i64::MAX - 1, i64::MAX), i64::MAX - 1);
        assert_eq!(midpoint(i64::MIN, i64::MIN + 1), i64::MIN);
        assert_eq!(midpoint(-3, -2), -3);
        assert_eq!(midpoint(2, 5), 3);
    }

    #[test]
    fn an_unreadable_store_is_reported_once_per_change() {
        let p = Path::new("/nowhere/opencode-n7-test.db");
        assert!(note_unreadable(p, Some("file is not a database")));
        assert!(!note_unreadable(p, Some("file is not a database")));
        assert!(note_unreadable(p, Some("no such table: session")));
        assert!(!note_unreadable(p, None), "readable again: nothing to warn");
        assert!(note_unreadable(p, Some("no such table: session")));
        note_unreadable(p, None);
    }
}
