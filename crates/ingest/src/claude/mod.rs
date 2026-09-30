//! Claude Code transcripts: `<home>/projects/<encoded cwd>/<session>.jsonl`, where `home` is
//! `~/.claude` or a `CLAUDE_CONFIG_DIR`. Sub-agent transcripts live in
//! `<session>/subagents/*.jsonl` next to their parent, or (in older versions) as top-level files
//! whose records say `isSidechain: true`.

mod parse;

pub use parse::{ClaudeRecord, RecordFacts, parse_line};

use crate::lines::{self, Backward, Line, OwnedLine, Pending, SkipReason, SkippedLine};
use crate::text::{from_hex, title, to_hex};
use parse::MAX_TITLE_CHARS;
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SessionMeta, SourceAdapter, SourceError, TranscriptItem, TranscriptPage,
    TranscriptRef,
};
use pitcrew_protocol::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Reads Claude Code transcripts. Read-only: files are only ever opened for reading.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeAdapter;

/// One incremental read, with what the trait's [`ParseChunk`] leaves out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    /// The chunk the trait returns.
    pub chunk: ParseChunk,
    /// Lines skipped because they were too long, not UTF-8 or not a JSON object.
    pub skipped: Vec<SkippedLine>,
    /// Bytes this read took from the file.
    pub bytes_read: u64,
}

impl ClaudeAdapter {
    /// A new adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The Claude home on this machine: `CLAUDE_CONFIG_DIR` if set, else `~/.claude`.
    #[must_use]
    pub fn default_home() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir));
        }
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|d| !d.is_empty())
            .map(|home| PathBuf::from(home).join(".claude"))
    }

    /// [`SourceAdapter::read_from`], also returning skipped lines and the bytes read.
    ///
    /// # Errors
    ///
    /// I/O errors, or [`SourceError::Unreadable`] if the file is now shorter than the cursor.
    pub fn read(
        &self,
        transcript: &TranscriptRef,
        cursor: &Cursor,
    ) -> Result<ReadReport, SourceError> {
        let mut file = File::open(&transcript.path)?;
        let len = file.metadata()?.len();
        if cursor.offset > len {
            return Err(SourceError::Unreadable {
                path: transcript.path.clone(),
                reason: format!(
                    "file is {len} bytes but the cursor is at {}; it was truncated or replaced",
                    cursor.offset
                ),
            });
        }

        let first_read = cursor.state.is_none();
        let mut state: ReadState = cursor
            .state
            .clone()
            .and_then(|s| serde_json::from_value(s).ok())
            .unwrap_or_default();
        // A carried line that no longer fits the file (or whose length overflows) is dropped and
        // read again from the cursor.
        let resume = state
            .pending
            .take()
            .and_then(CarriedLine::into_pending)
            .filter(|p| {
                cursor
                    .offset
                    .checked_add(p.len)
                    .is_some_and(|end| end <= len)
            });

        let mut items = Vec::new();
        let mut skipped = Vec::new();
        let mut meta_changed = first_read;
        let fwd =
            lines::read_forward(
                &mut file,
                cursor.offset,
                resume,
                |offset, line| match parse_framed(offset, line) {
                    Ok(rec) => {
                        meta_changed |= state.meta.absorb(&rec);
                        push_record(&mut state.last, rec.items, rec.soft_turn_end, &mut items);
                    }
                    Err(skip) => skipped.push(skip),
                },
            )?;
        let mut log = SkipLog::default();
        for skip in &skipped {
            log.note(&transcript.path, skip);
        }
        log.finish(&transcript.path);

        state.pending = fwd.pending.map(CarriedLine::from);
        let meta = meta_changed.then(|| state.meta.to_meta(&transcript.path));
        let state = serde_json::to_value(&state).map_err(|e| SourceError::Unreadable {
            path: transcript.path.clone(),
            reason: format!("cannot encode the cursor: {e}"),
        })?;
        Ok(ReadReport {
            chunk: ParseChunk {
                cursor: Cursor {
                    offset: fwd.end,
                    state: Some(state),
                },
                meta,
                items,
            },
            skipped,
            bytes_read: fwd.bytes_read,
        })
    }
}

impl SourceAdapter for ClaudeAdapter {
    fn engine(&self) -> Engine {
        Engine::Claude
    }

    fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        let mut out = Vec::new();
        let Some(projects) = read_dir_or_empty(&home.join("projects"))? else {
            return Ok(out);
        };
        for project in projects {
            if !project.is_dir() {
                continue;
            }
            // A project folder that vanished or cannot be listed is skipped, not fatal.
            let Ok(Some(entries)) = read_dir_or_empty(&project) else {
                continue;
            };
            for entry in entries {
                if entry.is_dir() {
                    let Ok(Some(subs)) = read_dir_or_empty(&entry.join("subagents")) else {
                        continue;
                    };
                    out.extend(subs.iter().filter_map(|p| transcript_ref(p)));
                } else if let Some(t) = transcript_ref(&entry) {
                    out.push(t);
                }
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn read_from(
        &self,
        transcript: &TranscriptRef,
        cursor: &Cursor,
    ) -> Result<ParseChunk, SourceError> {
        self.read(transcript, cursor).map(|r| r.chunk)
    }

    fn read_page(
        &self,
        transcript: &TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, SourceError> {
        let mut file = File::open(&transcript.path)?;
        let len = file.metadata()?.len();
        let mut back = Backward::new(&mut file, len);
        let end = back.align(before.unwrap_or(len))?;

        // Collect item-bearing records newest first until `limit` items are certain to be kept.
        // A turn-duration record is undecided until the record before it is known; one that is
        // certain to be dropped is removed at once, so at most `limit + 1` records are held.
        let mut records: Vec<PageRecord> = Vec::new();
        let mut kept = 0usize;
        let mut pos = end;
        let mut exhausted = false;
        let mut log = SkipLog::default();
        while kept < limit {
            let Some(rec) = prev_record(&mut back, &mut pos, &transcript.path, &mut log)? else {
                exhausted = true;
                break;
            };
            if records.last().is_some_and(|later| later.soft) {
                if rec.ends_turn() {
                    records.pop();
                } else {
                    kept += 1;
                }
            }
            if !rec.soft {
                kept += rec.items.len();
            }
            records.push(rec);
        }

        // The record just before the page decides a leading turn-duration record, and whether
        // anything older exists.
        let context = if exhausted {
            None
        } else {
            prev_record(&mut back, &mut pos, &transcript.path, &mut log)?
        };
        log.finish(&transcript.path);
        let at_start = context.is_none();
        let mut last = context.map(|rec| rec.last_kind());

        records.reverse();
        let from = records
            .first()
            .map_or(if at_start { 0 } else { end }, |rec| rec.offset);
        let mut items = Vec::new();
        for rec in records {
            push_record(&mut last, rec.items, rec.soft, &mut items);
        }
        Ok(TranscriptPage {
            items,
            from,
            to: end,
            at_start,
        })
    }
}

fn read_dir_or_empty(dir: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(Some(
            entries.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        )),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn transcript_ref(path: &Path) -> Option<TranscriptRef> {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return None;
    }
    let meta = fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| {
            TimestampMs::try_from(d.as_millis()).unwrap_or(TimestampMs::MAX)
        });
    Some(TranscriptRef {
        engine: Engine::Claude,
        path: path.to_path_buf(),
        inner_id: None,
        size: meta.len(),
        modified,
    })
}

fn parse_framed(offset: u64, line: Line<'_>) -> Result<ClaudeRecord, SkippedLine> {
    match line {
        Line::TooLong(len) => Err(SkippedLine {
            offset,
            len,
            reason: SkipReason::TooLong,
        }),
        Line::Data(bytes) => parse_line(bytes, offset).map_err(|reason| SkippedLine {
            offset,
            len: bytes.len() as u64,
            reason,
        }),
    }
}

/// Skipped lines for one call: one warning with the count, details at debug level.
#[derive(Debug, Default)]
struct SkipLog {
    count: usize,
    first: Option<u64>,
}

impl SkipLog {
    fn note(&mut self, path: &Path, skip: &SkippedLine) {
        self.count += 1;
        self.first.get_or_insert(skip.offset);
        tracing::debug!(path = %path.display(), offset = skip.offset, len = skip.len, reason = ?skip.reason, "skipped transcript line");
    }

    fn finish(&self, path: &Path) {
        if let Some(first) = self.first {
            tracing::warn!(path = %path.display(), count = self.count, first_offset = first, "skipped transcript lines");
        }
    }
}

/// A record held while building a page: just what the page needs.
#[derive(Debug)]
struct PageRecord {
    offset: u64,
    items: Vec<TranscriptItem>,
    soft: bool,
}

impl PageRecord {
    fn ends_turn(&self) -> bool {
        ends_turn(&self.items)
    }

    fn last_kind(&self) -> LastKind {
        last_kind(&self.items)
    }
}

/// The next older record that yields items, moving `pos` to its start.
fn prev_record<F: io::Read + io::Seek>(
    back: &mut Backward<'_, F>,
    pos: &mut u64,
    path: &Path,
    log: &mut SkipLog,
) -> io::Result<Option<PageRecord>> {
    while let Some((start, line)) = back.prev_line(*pos)? {
        *pos = start;
        let parsed = match &line {
            OwnedLine::Data(bytes) => parse_framed(start, Line::Data(bytes)),
            OwnedLine::TooLong(len) => parse_framed(start, Line::TooLong(*len)),
        };
        match parsed {
            Ok(rec) if !rec.items.is_empty() => {
                return Ok(Some(PageRecord {
                    offset: start,
                    items: rec.items,
                    soft: rec.soft_turn_end,
                }));
            }
            Ok(_) => {}
            Err(skip) => log.note(path, &skip),
        }
    }
    Ok(None)
}

/// The kind of the last item seen, which decides whether a turn-duration record ends a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LastKind {
    TurnEnded,
    Other,
}

fn ends_turn(items: &[TranscriptItem]) -> bool {
    matches!(items.last(), Some(TranscriptItem::TurnEnded { .. }))
}

fn last_kind(items: &[TranscriptItem]) -> LastKind {
    if ends_turn(items) {
        LastKind::TurnEnded
    } else {
        LastKind::Other
    }
}

/// Appends a record's items. A turn-duration (`soft`) record ends the turn only if something
/// happened since the last `TurnEnded`, so `end_turn` then a turn-duration record is one turn end.
fn push_record(
    last: &mut Option<LastKind>,
    items: Vec<TranscriptItem>,
    soft: bool,
    out: &mut Vec<TranscriptItem>,
) {
    if items.is_empty() {
        return;
    }
    let kind = last_kind(&items);
    if !soft || *last == Some(LastKind::Other) {
        out.extend(items);
    }
    *last = Some(kind);
}

/// What the cursor carries between reads.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ReadState {
    #[serde(default)]
    meta: MetaAcc,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last: Option<LastKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<CarriedLine>,
}

/// An incomplete last line: its bytes (hex) when small, or a note that it is too long.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CarriedLine {
    len: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hex: Option<String>,
    #[serde(default)]
    too_long: bool,
}

impl CarriedLine {
    fn into_pending(self) -> Option<Pending> {
        let bytes = match self.hex {
            Some(hex) => Some(from_hex(&hex)?),
            None => None,
        };
        Some(Pending {
            len: self.len,
            bytes,
            too_long: self.too_long,
        })
    }
}

impl From<Pending> for CarriedLine {
    fn from(p: Pending) -> Self {
        Self {
            len: p.len,
            hex: p.bytes.as_deref().map(to_hex),
            too_long: p.too_long,
        }
    }
}

/// Session facts gathered so far.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct MetaAcc {
    session_id: Option<String>,
    agent_id: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    custom_title: Option<String>,
    summary: Option<String>,
    first_prompt: Option<String>,
    model: Option<String>,
    started: Option<TimestampMs>,
    sidechain: Option<bool>,
}

impl MetaAcc {
    /// Folds in one record; returns whether anything changed. The session id, agent id, cwd,
    /// start time and sidechain flag are the first seen; branch, model and titles the latest.
    fn absorb(&mut self, rec: &ClaudeRecord) -> bool {
        /// Sets an empty slot; returns whether it changed.
        fn first<T: Clone>(slot: &mut Option<T>, v: Option<&T>) -> bool {
            match (slot.is_none(), v) {
                (true, Some(v)) => {
                    *slot = Some(v.clone());
                    true
                }
                _ => false,
            }
        }
        /// Replaces the slot with a new, different value; returns whether it changed.
        fn latest<T: Clone + PartialEq>(slot: &mut Option<T>, v: Option<&T>) -> bool {
            match v {
                Some(v) if slot.as_ref() != Some(v) => {
                    *slot = Some(v.clone());
                    true
                }
                _ => false,
            }
        }
        let f = &rec.facts;
        let mut changed = first(&mut self.session_id, f.session_id.as_ref());
        changed |= first(&mut self.agent_id, f.agent_id.as_ref());
        changed |= first(&mut self.cwd, f.cwd.as_ref());
        changed |= first(&mut self.started, f.timestamp.as_ref());
        changed |= first(&mut self.sidechain, f.is_sidechain.as_ref());
        changed |= latest(&mut self.branch, f.branch.as_ref());
        changed |= latest(&mut self.model, f.model.as_ref());
        changed |= latest(&mut self.custom_title, f.custom_title.as_ref());
        changed |= latest(&mut self.summary, f.summary.as_ref());
        if self.first_prompt.is_none() {
            self.first_prompt = rec.items.iter().find_map(|item| match item {
                TranscriptItem::UserPrompt { text, .. } => Some(title(text, MAX_TITLE_CHARS)),
                _ => None,
            });
            changed |= self.first_prompt.is_some();
        }
        changed
    }

    fn to_meta(&self, path: &Path) -> SessionMeta {
        let in_subagents =
            path.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("subagents"));
        let is_subagent = in_subagents || self.sidechain == Some(true);
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        // Sub-agent records carry their parent's session id, so they are named by agent id.
        let native_id = if is_subagent {
            self.agent_id.clone().or(stem)
        } else {
            self.session_id.clone().or(stem)
        };
        SessionMeta {
            native_id: native_id.unwrap_or_default(),
            cwd: self.cwd.clone(),
            branch: self.branch.clone(),
            title: self
                .custom_title
                .clone()
                .or_else(|| self.summary.clone())
                .or_else(|| self.first_prompt.clone()),
            model: self.model.clone(),
            started: self.started,
            is_subagent,
        }
    }
}
