//! OpenCode sessions, from its SQLite store `<home>/opencode.db`, where `home` is
//! `$XDG_DATA_HOME/opencode` or `~/.local/share/opencode` (also on Windows). One transcript per
//! session: [`TranscriptRef::path`] is the database, [`TranscriptRef::inner_id`] the session id.
//!
//! Verified against OpenCode 1.18 (sessions written by 1.3 to 1.18). Older versions kept JSON
//! files under `storage/`; current versions migrate them into the database, and that format is
//! not read here.
//!
//! # Positions
//!
//! Items carry an `offset` like the JSONL adapters, but here it is a position, not a byte offset:
//! the part's creation time on OpenCode's id scale (milliseconds × 4096 plus a counter). OpenCode
//! ids (`prt_` + 12 hex digits + random) hold the low 48 bits of that number; the full value is
//! recovered with the row's `time_created`. So positions are stable, unique within a session, and
//! increase in creation order. Ids of another shape fall back to `time_created × 4096`; parts
//! that share a position are never split across pages. Positions stay below 2^53 (safe as JSON
//! numbers) until 2039.
//!
//! # Incremental reads
//!
//! Parts change while a turn streams, so each part is shown in up to two steps (see
//! [`parse_part`]): a running tool's call as soon as its input is known, and its result (or a
//! text part, or anything else) once it is final. A part is final when its own state says so, or
//! when its message is settled (completed, failed, or followed by a newer message); an unfinished
//! tool is then reported as interrupted. Each step is emitted once, with the part's position as
//! its offset, and **changes to a part after it is final are not re-emitted**.
//!
//! The cursor keeps a frontier (the newest position below which everything is final and emitted)
//! and the parts above it already shown. The frontier only passes parts created more than
//! [`SETTLE_MS`] before the newest part was created or updated, so a part inserted a little late is still
//! read. So one read of a finished session gives exactly the items of reading it in pieces, and
//! reads during a turn give the same items, a result possibly arriving after later calls.
//!
//! # Bounded work
//!
//! There is no index on part times, but `part_session_idx` orders a session's parts by rowid,
//! which is insertion order and follows creation order (a real store had no exceptions). Reads
//! walk that index newest first, in batches, and treat a part as next in position order once
//! every part not yet fetched is known to be older, allowing parts to be out of order by up to
//! [`SETTLE_MS`]. So a page, or a read with nothing new, touches only the newest parts, and a page
//! further back jumps there by binary search on rowids. Only a table without rowids is listed
//! whole.

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
use store::{PartRow, Store};

/// The frontier only passes parts created at least this long before the newest part activity.
pub const SETTLE_MS: i64 = 60_000;
/// Parts above the frontier the cursor remembers; past this, the frontier is forced forward.
const MAX_OPEN: usize = 4096;
/// OpenCode ids hold the low 48 bits of a position.
const ID_SPAN: u64 = 1 << 48;
/// How far out of rowid order a part's position may be: [`SETTLE_MS`] on the position scale.
const MARGIN: u64 = SETTLE_MS.unsigned_abs() * 4096;
/// Parts fetched per step of a walk.
const BATCH: usize = 256;

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
    /// [`SourceError::Io`] with [`io::ErrorKind::WouldBlock`] if a writer holds the store (retry
    /// later).
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

        // Every part above the frontier, oldest first.
        let mut walker = Walker::new(&store, session_id, u64::MAX, None);
        let mut parts = Vec::new();
        while let Some(p) = walker.next()? {
            if let Some(frontier) = &state.frontier
                && p.key() <= (frontier.0, frontier.1.as_str())
            {
                break;
            }
            parts.push(p);
        }
        parts.reverse();
        let mut messages = Messages::load(&store, session_id)?;
        // The settle clock is the parts' own: message and session times can run ahead of parts
        // still being written.
        let now = walker.newest_activity;

        let open: HashMap<String, Shown> = std::mem::take(&mut state.open).into_iter().collect();
        let mut shown: HashMap<&str, Shown> = HashMap::new();
        let mut items = Vec::new();
        let mut skips = Skips::default();
        let mut bytes_read = 0u64;
        for p in &parts {
            let before = open.get(&p.row.id).copied();
            if before == Some(Shown::Done) {
                shown.insert(&p.row.id, Shown::Done);
                continue;
            }
            match load_part(&store, p, &mut messages, &mut bytes_read)? {
                Err(skip) => {
                    skips.note(&transcript.path, skip);
                    shown.insert(&p.row.id, Shown::Done);
                }
                Ok(got) => match got.phase {
                    Phase::Done => {
                        if before != Some(Shown::Started) {
                            items.extend(got.head);
                        }
                        items.extend(got.tail);
                        shown.insert(&p.row.id, Shown::Done);
                    }
                    Phase::Started => {
                        if before.is_none() {
                            items.extend(got.head);
                        }
                        shown.insert(&p.row.id, Shown::Started);
                    }
                    Phase::Waiting => {
                        if let Some(before) = before {
                            shown.insert(&p.row.id, before);
                        }
                    }
                },
            }
        }

        // The frontier passes the leading parts that are done and quiet.
        let mut passed = 0;
        for p in &parts {
            let quiet = p.row.created <= now.saturating_sub(SETTLE_MS);
            if !(quiet && shown.get(p.row.id.as_str()) == Some(&Shown::Done)) {
                break;
            }
            passed += 1;
        }
        // A store with thousands of unfinished parts would grow the cursor without bound: force
        // the frontier on, giving up the results those parts may still get.
        while shown.len() > MAX_OPEN && passed < parts.len() {
            shown.remove(parts[passed].row.id.as_str());
            passed += 1;
        }
        for p in &parts[..passed] {
            shown.remove(p.row.id.as_str());
            state.frontier = Some((p.offset, p.row.id.clone()));
        }
        state.open = parts[passed..]
            .iter()
            .filter_map(|p| shown.get(p.row.id.as_str()).map(|s| (p.row.id.clone(), *s)))
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
    /// `time_updated`; `size` is the bytes of its parts' payloads.
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
            let store = match Store::open(&path) {
                Ok(store) => store,
                Err(SourceError::Io(e)) => return Err(SourceError::Io(e)),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipped an unreadable OpenCode store");
                    continue;
                }
            };
            if !store.has_sessions() {
                continue;
            }
            for (session, size) in store.sessions()? {
                out.push(TranscriptRef {
                    engine: Engine::OpenCode,
                    path: path.clone(),
                    inner_id: Some(session.id),
                    size,
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

        // The newest part gives the end of the transcript.
        let mut walker = Walker::new(&store, session_id, u64::MAX, None);
        let newest = walker.next()?;
        let last = newest.as_ref().map_or(0, |p| p.offset.saturating_add(1));
        let end = before.map_or(last, |b| b.min(last));
        if end == last {
            walker.end = last;
            walker.put_back(newest);
        } else {
            let start = start_below(&store, session_id, end.saturating_add(MARGIN))?;
            walker = Walker::new(&store, session_id, end, start);
        }

        // Whole groups of parts, newest first, until `limit` items; parts that share a position
        // stay on one page.
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
                &transcript.path,
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
                &transcript.path,
            )?
            .is_empty()
            {
                at_start = false;
                break;
            }
            next = walker.next()?;
        }
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

/// A part row with its position.
struct Positioned {
    row: PartRow,
    offset: u64,
}

impl Positioned {
    fn key(&self) -> (u64, &str) {
        (self.offset, self.row.id.as_str())
    }
}

/// A session's parts in position order, newest first, fetched from the rowid index in batches.
struct Walker<'s> {
    store: &'s Store,
    session: &'s str,
    /// Parts not fetched yet have a rowid below this; `None` is the top.
    below: Option<i64>,
    exhausted: bool,
    /// The lowest position fetched: every part not fetched yet is below `floor + MARGIN`.
    floor: u64,
    /// Only parts below this are returned.
    end: u64,
    pool: BTreeMap<(u64, String), PartRow>,
    /// The latest time any fetched part was created or updated.
    newest_activity: i64,
}

impl<'s> Walker<'s> {
    fn new(store: &'s Store, session: &'s str, end: u64, below: Option<i64>) -> Self {
        Self {
            store,
            session,
            below,
            exhausted: false,
            floor: u64::MAX,
            end,
            pool: BTreeMap::new(),
            newest_activity: 0,
        }
    }

    fn fetch(&mut self) -> Result<(), SourceError> {
        let rows = if self.store.parts_have_rowid() {
            let rows = self.store.parts_below(self.session, self.below, BATCH)?;
            self.exhausted = rows.len() < BATCH;
            rows
        } else {
            self.exhausted = true;
            self.store
                .parts(self.session)?
                .into_iter()
                .map(|row| (0, Some(row)))
                .collect()
        };
        for (rowid, row) in rows {
            self.below = Some(rowid);
            let Some(row) = row else { continue };
            let offset = position(&row.id, row.created);
            self.floor = self.floor.min(offset);
            self.newest_activity = self.newest_activity.max(row.created).max(row.updated);
            if offset < self.end {
                self.pool.insert((offset, row.id.clone()), row);
            }
        }
        Ok(())
    }

    /// The next part, newest first, once no part still unfetched can be newer.
    fn next(&mut self) -> Result<Option<Positioned>, SourceError> {
        loop {
            if let Some(entry) = self.pool.last_entry() {
                if self.exhausted || self.floor.saturating_add(MARGIN) <= entry.key().0 {
                    let ((offset, _), row) = entry.remove_entry();
                    return Ok(Some(Positioned { row, offset }));
                }
            } else if self.exhausted {
                return Ok(None);
            }
            self.fetch()?;
        }
    }

    fn put_back(&mut self, part: Option<Positioned>) {
        if let Some(p) = part.filter(|p| p.offset < self.end) {
            self.pool.insert((p.offset, p.row.id.clone()), p.row);
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
        let mid = lo + (hi - lo) / 2;
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

/// A row's position: its creation time on OpenCode's id scale, recovered from the id's low 48
/// bits and the row's `time_created` (see the module docs).
#[must_use]
pub fn position(id: &str, created: TimestampMs) -> u64 {
    let base = u64::try_from(created).unwrap_or(0).saturating_mul(4096);
    let Some(bits) = id_bits(id) else {
        return base;
    };
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

/// Message facts, loaded as parts need them.
struct Messages {
    newest: Option<u64>,
    info: HashMap<String, MessageInfo>,
}

impl Messages {
    fn load(store: &Store, session: &str) -> Result<Self, SourceError> {
        let newest = store.newest_message(session)?;
        Ok(Self {
            newest: newest.as_ref().map(|m| position(&m.id, m.created)),
            info: HashMap::new(),
        })
    }

    fn get(&mut self, store: &Store, id: &str) -> Result<MessageInfo, SourceError> {
        if let Some(info) = self.info.get(id) {
            return Ok(info.clone());
        }
        let info = match store.message_created(id)? {
            // A part whose message is gone will not change again.
            None => MessageInfo {
                settled: true,
                ..MessageInfo::default()
            },
            Some(created) => {
                let pos = position(id, created);
                let facts = store.message_facts(id)?;
                let role = match facts.role.as_deref() {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    _ => Role::Unknown,
                };
                MessageInfo {
                    role,
                    settled: facts.completed
                        || facts.failed
                        || self.newest.is_some_and(|newest| newest > pos),
                    model: facts.model,
                }
            }
        };
        self.info.insert(id.to_owned(), info.clone());
        Ok(info)
    }
}

/// A part's items, or why it was skipped. Store errors are the outer `Err`.
fn load_part(
    store: &Store,
    p: &Positioned,
    messages: &mut Messages,
    bytes_read: &mut u64,
) -> Result<Result<PartItems, SkippedLine>, SourceError> {
    let skip = |len: u64, reason| {
        Ok(Err(SkippedLine {
            offset: p.offset,
            len,
            reason,
        }))
    };
    let size = p.row.size.unwrap_or(0);
    if size > MAX_LINE_BYTES as u64 {
        return skip(size, SkipReason::TooLong);
    }
    let Some(data) = store.part_data(&p.row.id)? else {
        return skip(0, SkipReason::Malformed("no payload".into()));
    };
    *bytes_read += data.len() as u64;
    let msg = messages.get(store, &p.row.message_id)?;
    Ok(
        parse_part(&data, &msg, p.offset, p.row.created).map_err(|reason| SkippedLine {
            offset: p.offset,
            len: data.len() as u64,
            reason,
        }),
    )
}

fn visible(
    store: &Store,
    p: &Positioned,
    messages: &mut Messages,
    bytes: &mut u64,
    skips: &mut Skips,
    path: &Path,
) -> Result<Vec<TranscriptItem>, SourceError> {
    Ok(match load_part(store, p, messages, bytes)? {
        Ok(got) => got.visible(),
        Err(skip) => {
            skips.note(path, skip);
            Vec::new()
        }
    })
}

/// How much of a part above the frontier has been emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Shown {
    Started,
    Done,
}

/// What the cursor carries between reads.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ReadState {
    /// Position and id of the newest part below which everything is final and emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frontier: Option<(u64, String)>,
    /// Parts above the frontier already shown, oldest first.
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
    use super::{ID_SPAN, position};

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
}
