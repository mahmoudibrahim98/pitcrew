//! Claude Code transcripts: `<home>/projects/<encoded cwd>/<session>.jsonl`, where `home` is
//! `~/.claude` or a `CLAUDE_CONFIG_DIR`. Sub-agent transcripts live in
//! `<session>/subagents/*.jsonl` next to their parent, or (in older versions) as top-level files
//! whose records say `isSidechain: true`.

mod parse;

pub use crate::jsonl::ReadReport;
pub use parse::{ClaudeRecord, RecordFacts, parse_line};

use crate::bound::MAX_TITLE_CHARS;
use crate::jsonl::{self, Format, Skips, read_dir_or_empty, set_first, set_latest};
use crate::lines::{Backward, SkipReason};
use crate::text::title;
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SessionMeta, SourceAdapter, SourceError, TranscriptItem, TranscriptPage,
    TranscriptRef,
};
use pitcrew_protocol::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// Reads Claude Code transcripts. Read-only: files are only ever opened for reading.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeAdapter;

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
        crate::user_home().map(|home| home.join(".claude"))
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
        jsonl::read::<Self>(&transcript.path, cursor)
    }
}

impl Format for ClaudeAdapter {
    type Record = ClaudeRecord;
    type State = ReadState;

    fn parse(line: &[u8], offset: u64) -> Result<ClaudeRecord, SkipReason> {
        parse_line(line, offset)
    }

    fn absorb(state: &mut ReadState, rec: ClaudeRecord, out: &mut Vec<TranscriptItem>) -> bool {
        let changed = state.meta.absorb(&rec);
        push_record(&mut state.last, rec.items, rec.soft_turn_end, out);
        changed
    }

    fn meta(state: &ReadState, path: &Path) -> SessionMeta {
        state.meta.to_meta(path)
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
        let mut skips = Skips::default();
        while kept < limit {
            let Some(rec) = prev_record(&mut back, &mut pos, &transcript.path, &mut skips)? else {
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
            prev_record(&mut back, &mut pos, &transcript.path, &mut skips)?
        };
        skips.finish(&transcript.path);
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

fn transcript_ref(path: &Path) -> Option<TranscriptRef> {
    let meta = fs::metadata(path).ok()?;
    jsonl::transcript_ref(Engine::Claude, path, &meta)
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
    skips: &mut Skips,
) -> io::Result<Option<PageRecord>> {
    while let Some((offset, rec)) = jsonl::prev_parsed::<ClaudeAdapter, _>(back, pos, path, skips)?
    {
        if !rec.items.is_empty() {
            return Ok(Some(PageRecord {
                offset,
                items: rec.items,
                soft: rec.soft_turn_end,
            }));
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

/// What the cursor carries between reads, besides a partial line.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ReadState {
    #[serde(default)]
    meta: MetaAcc,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last: Option<LastKind>,
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
        let f = &rec.facts;
        let mut changed = set_first(&mut self.session_id, f.session_id.as_ref());
        changed |= set_first(&mut self.agent_id, f.agent_id.as_ref());
        changed |= set_first(&mut self.cwd, f.cwd.as_ref());
        changed |= set_first(&mut self.started, f.timestamp.as_ref());
        changed |= set_first(&mut self.sidechain, f.is_sidechain.as_ref());
        changed |= set_latest(&mut self.branch, f.branch.as_ref());
        changed |= set_latest(&mut self.model, f.model.as_ref());
        changed |= set_latest(&mut self.custom_title, f.custom_title.as_ref());
        changed |= set_latest(&mut self.summary, f.summary.as_ref());
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
