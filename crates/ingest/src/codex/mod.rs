//! Codex CLI rollouts: `<home>/sessions/YYYY/MM/DD/rollout-<time>-<uuid>.jsonl`, plus
//! `<home>/archived_sessions/`, where `home` is `~/.codex` or a `CODEX_HOME`.
//!
//! # File edits
//!
//! An `apply_patch` call gives its `FileEdit`s from the call record, one per file, **whether or
//! not the patch applied**. Each `FileEdit` has the same `offset` as its `ToolUse`, whose
//! `call_id` leads to the `ToolResult`; that result's `is_error` says whether the patch applied.
//! Consumers that only want applied edits must check it. (The Claude adapter emits edits only
//! from a successful result; a later change may defer Codex edits the same way.)
//!
//! `FileEdit.path` is the path written in the patch, usually relative to the call's `workdir`
//! or the session's cwd; it is not resolved. For a moved file it is the new path. Diffs keep
//! Codex's hunk headers as written (`@@` or `@@ <context>`, without line ranges), so update diffs
//! are not strict unified diffs; added files get `@@ -0,0 +1,N @@`.

mod parse;

pub use crate::jsonl::ReadReport;
pub use parse::{CodexRecord, RecordFacts, parse_line};

use crate::bound::MAX_TITLE_CHARS;
use crate::jsonl::{self, Format, Skips, set_first, set_latest};
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

/// Folders below `sessions/` (`YYYY/MM/DD`).
const MAX_DEPTH: usize = 3;

/// Reads Codex CLI rollouts. Read-only: files are only ever opened for reading.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    /// A new adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The Codex home on this machine: `CODEX_HOME` if set, else `~/.codex`.
    #[must_use]
    pub fn default_home() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir));
        }
        crate::user_home().map(|home| home.join(".codex"))
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

impl Format for CodexAdapter {
    type Record = CodexRecord;
    type State = ReadState;

    fn parse(line: &[u8], offset: u64) -> Result<CodexRecord, SkipReason> {
        parse_line(line, offset)
    }

    fn absorb(state: &mut ReadState, rec: CodexRecord, out: &mut Vec<TranscriptItem>) -> bool {
        let changed = state.meta.absorb(&rec);
        out.extend(rec.items);
        changed
    }

    fn meta(state: &ReadState, path: &Path) -> SessionMeta {
        state.meta.to_meta(path)
    }
}

impl SourceAdapter for CodexAdapter {
    fn engine(&self) -> Engine {
        Engine::Codex
    }

    fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        let mut out = Vec::new();
        for dir in ["sessions", "archived_sessions"] {
            let dir = home.join(dir);
            if fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
                continue;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            walk(entries, MAX_DEPTH, &mut out);
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
        let path = &transcript.path;
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut back = Backward::new(&mut file, len);
        let end = back.align(before.unwrap_or(len))?;

        // Item-bearing records, newest first, until `limit` items; then one more look back to
        // learn whether anything older exists. Records are whole, so a page can pass `limit`.
        let mut records: Vec<(u64, Vec<TranscriptItem>)> = Vec::new();
        let mut kept = 0usize;
        let mut pos = end;
        let mut skips = Skips::default();
        let mut at_start = false;
        while kept < limit {
            let Some(rec) = prev_items(&mut back, &mut pos, path, &mut skips)? else {
                at_start = true;
                break;
            };
            kept += rec.1.len();
            records.push(rec);
        }
        if !at_start {
            at_start = prev_items(&mut back, &mut pos, path, &mut skips)?.is_none();
        }
        skips.finish(path);

        let from = records
            .last()
            .map_or(if at_start { 0 } else { end }, |rec| rec.0);
        let items = records.into_iter().rev().flat_map(|rec| rec.1).collect();
        Ok(TranscriptPage {
            items,
            from,
            to: end,
            at_start,
        })
    }
}

/// Collects `rollout-*.jsonl` files up to `depth` folders down. Symbolic links are not followed,
/// so the walk stays inside the home; folders that cannot be listed are skipped.
fn walk(entries: fs::ReadDir, depth: usize, out: &mut Vec<TranscriptRef>) {
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if depth > 0
                && let Ok(sub) = fs::read_dir(&path)
            {
                walk(sub, depth - 1, out);
            }
        } else if kind.is_file()
            && entry.file_name().to_string_lossy().starts_with("rollout-")
            && let Ok(meta) = entry.metadata()
            && let Some(t) = jsonl::transcript_ref(Engine::Codex, &path, &meta)
        {
            out.push(t);
        }
    }
}

/// The next older record that yields items, moving `pos` to its start.
fn prev_items<F: io::Read + io::Seek>(
    back: &mut Backward<'_, F>,
    pos: &mut u64,
    path: &Path,
    skips: &mut Skips,
) -> io::Result<Option<(u64, Vec<TranscriptItem>)>> {
    while let Some((offset, rec)) = jsonl::prev_parsed::<CodexAdapter, _>(back, pos, path, skips)? {
        if !rec.items.is_empty() {
            return Ok(Some((offset, rec.items)));
        }
    }
    Ok(None)
}

/// What the cursor carries between reads, besides a partial line.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ReadState {
    #[serde(default)]
    meta: MetaAcc,
}

/// Session facts gathered so far.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct MetaAcc {
    session_id: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    model: Option<String>,
    started: Option<TimestampMs>,
    first_prompt: Option<String>,
    subagent: Option<bool>,
}

impl MetaAcc {
    /// Folds in one record; returns whether anything changed. The session id, cwd, branch, start
    /// time and sub-agent flag are the first seen (from `session_meta`); the model the latest.
    fn absorb(&mut self, rec: &CodexRecord) -> bool {
        let f = &rec.facts;
        let mut changed = set_first(&mut self.session_id, f.session_id.as_ref());
        changed |= set_first(&mut self.cwd, f.cwd.as_ref());
        changed |= set_first(&mut self.started, f.timestamp.as_ref());
        changed |= set_first(&mut self.subagent, f.is_subagent.as_ref());
        changed |= set_first(&mut self.branch, f.branch.as_ref());
        changed |= set_latest(&mut self.model, f.model.as_ref());
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
        let native_id = self.session_id.clone().unwrap_or_else(|| {
            path.file_stem()
                .map(|s| id_from_stem(&s.to_string_lossy()).to_owned())
                .unwrap_or_default()
        });
        SessionMeta {
            native_id,
            cwd: self.cwd.clone(),
            branch: self.branch.clone(),
            // Codex has no title record.
            title: self.first_prompt.clone(),
            model: self.model.clone(),
            started: self.started,
            is_subagent: self.subagent == Some(true),
        }
    }
}

/// The session id at the end of `rollout-<time>-<uuid>`, else the whole stem.
fn id_from_stem(stem: &str) -> &str {
    let tail = stem
        .len()
        .checked_sub(36)
        .and_then(|start| stem.get(start..));
    match tail {
        Some(id)
            if id.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            }) =>
        {
            id
        }
        _ => stem,
    }
}

#[cfg(test)]
mod tests {
    use super::id_from_stem;

    #[test]
    fn session_id_from_the_file_name() {
        assert_eq!(
            id_from_stem("rollout-2026-09-29T08-00-00-7c1e9d2a-0b3f-4e6a-8d5c-1f2e3a4b5c6d"),
            "7c1e9d2a-0b3f-4e6a-8d5c-1f2e3a4b5c6d"
        );
        assert_eq!(id_from_stem("rollout-x"), "rollout-x");
        assert_eq!(
            id_from_stem("§§§§§§§§§§§§§§§§§§§§§§§§§§§§§"),
            "§§§§§§§§§§§§§§§§§§§§§§§§§§§§§"
        );
    }
}
