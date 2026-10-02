//! What the JSONL adapters share: the incremental read with a carried partial line, capped skip
//! reporting, discovery helpers, and the backward record walk that pages use.

use crate::lines::{self, Backward, Line, OwnedLine, Pending, SkipReason, SkippedLine};
use crate::open::open_transcript;
use crate::text::{from_hex, to_hex};
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SessionMeta, SourceError, TranscriptItem, TranscriptRef,
};
use pitcrew_protocol::model::{Engine, TimestampMs};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// A read reports at most this many skipped lines; the rest are only counted.
pub const MAX_REPORTED_SKIPS: usize = 100;

/// One incremental read, with what the trait's [`ParseChunk`] leaves out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    /// The chunk the trait returns.
    pub chunk: ParseChunk,
    /// The first [`MAX_REPORTED_SKIPS`] lines skipped because they were too long, not UTF-8 or
    /// not a JSON object.
    pub skipped: Vec<SkippedLine>,
    /// How many lines were skipped in all, including those not listed in `skipped`.
    pub skipped_total: u64,
    /// Bytes this read took from the file.
    pub bytes_read: u64,
}

/// One transcript format: how a line becomes a record, and how records fold into items and
/// session facts.
pub(crate) trait Format {
    /// What one line gives.
    type Record;
    /// What the cursor carries between reads, besides the partial line.
    type State: Default + Serialize + DeserializeOwned;

    /// Parses one line (without its newline) at byte `offset`.
    fn parse(line: &[u8], offset: u64) -> Result<Self::Record, SkipReason>;

    /// Folds `rec` into `state`, appending its items to `out`. Returns whether the session facts
    /// changed.
    fn absorb(state: &mut Self::State, rec: Self::Record, out: &mut Vec<TranscriptItem>) -> bool;

    /// The session facts gathered so far.
    fn meta(state: &Self::State, path: &Path) -> SessionMeta;
}

/// The cursor's state: the format's own, plus the carried partial line.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored<S> {
    #[serde(flatten)]
    state: S,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<CarriedLine>,
}

/// Reads complete lines from `cursor` to the end of the file at `path`, if it is a regular file
/// (see [`crate::open`]).
pub(crate) fn read<F: Format>(path: &Path, cursor: &Cursor) -> Result<ReadReport, SourceError> {
    let mut file = open_transcript(path)?;
    let len = file.metadata()?.len();
    if cursor.offset > len {
        return Err(SourceError::Unreadable {
            path: path.to_path_buf(),
            reason: format!(
                "file is {len} bytes but the cursor is at {}; it was truncated or replaced",
                cursor.offset
            ),
        });
    }

    let first_read = cursor.state.is_none();
    let mut stored: Stored<F::State> = cursor
        .state
        .clone()
        .and_then(|s| serde_json::from_value(s).ok())
        .unwrap_or_default();
    // A carried line that no longer fits the file (or whose length overflows) is dropped and
    // read again from the cursor.
    let resume = stored
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
    let mut skips = Skips::default();
    let mut meta_changed = first_read;
    let fwd =
        lines::read_forward(
            &mut file,
            cursor.offset,
            resume,
            |offset, line| match parse_framed::<F>(offset, line) {
                Ok(rec) => meta_changed |= F::absorb(&mut stored.state, rec, &mut items),
                Err(skip) => skips.note(path, skip),
            },
        )?;
    let (skipped, skipped_total) = skips.finish(path);

    stored.pending = fwd.pending.map(CarriedLine::from);
    let meta = meta_changed.then(|| F::meta(&stored.state, path));
    let state = serde_json::to_value(&stored).map_err(|e| SourceError::Unreadable {
        path: path.to_path_buf(),
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
        skipped_total,
        bytes_read: fwd.bytes_read,
    })
}

pub(crate) fn parse_framed<F: Format>(
    offset: u64,
    line: Line<'_>,
) -> Result<F::Record, SkippedLine> {
    match line {
        Line::TooLong(len) => Err(SkippedLine {
            offset,
            len,
            reason: SkipReason::TooLong,
        }),
        Line::Data(bytes) => F::parse(bytes, offset).map_err(|reason| SkippedLine {
            offset,
            len: bytes.len() as u64,
            reason,
        }),
    }
}

/// The next older line that parses, moving `pos` to its start. Lines that do not parse are noted
/// in `skips`.
pub(crate) fn prev_parsed<F: Format, R: io::Read + io::Seek>(
    back: &mut Backward<'_, R>,
    pos: &mut u64,
    path: &Path,
    skips: &mut Skips,
) -> io::Result<Option<(u64, F::Record)>> {
    while let Some((start, line)) = back.prev_line(*pos)? {
        *pos = start;
        let parsed = match &line {
            OwnedLine::Data(bytes) => parse_framed::<F>(start, Line::Data(bytes)),
            OwnedLine::TooLong(len) => parse_framed::<F>(start, Line::TooLong(*len)),
        };
        match parsed {
            Ok(rec) => return Ok(Some((start, rec))),
            Err(skip) => skips.note(path, skip),
        }
    }
    Ok(None)
}

/// Skipped lines for one call: the first [`MAX_REPORTED_SKIPS`] kept, all counted, one warning
/// at the end and details at debug level.
#[derive(Debug, Default)]
pub(crate) struct Skips {
    kept: Vec<SkippedLine>,
    total: u64,
    first: Option<u64>,
}

impl Skips {
    pub(crate) fn note(&mut self, path: &Path, skip: SkippedLine) {
        self.total += 1;
        self.first.get_or_insert(skip.offset);
        tracing::debug!(path = %path.display(), offset = skip.offset, len = skip.len, reason = ?skip.reason, "skipped transcript line");
        if self.kept.len() < MAX_REPORTED_SKIPS {
            self.kept.push(skip);
        }
    }

    /// Logs the summary; returns the kept lines and the total.
    pub(crate) fn finish(self, path: &Path) -> (Vec<SkippedLine>, u64) {
        if let Some(first) = self.first {
            tracing::warn!(path = %path.display(), count = self.total, first_offset = first, "skipped transcript lines");
        }
        (self.kept, self.total)
    }
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

/// Sets an empty slot; returns whether it changed.
pub(crate) fn set_first<T: Clone>(slot: &mut Option<T>, v: Option<&T>) -> bool {
    match (slot.is_none(), v) {
        (true, Some(v)) => {
            *slot = Some(v.clone());
            true
        }
        _ => false,
    }
}

/// Replaces the slot with a new, different value; returns whether it changed.
pub(crate) fn set_latest<T: Clone + PartialEq>(slot: &mut Option<T>, v: Option<&T>) -> bool {
    match v {
        Some(v) if slot.as_ref() != Some(v) => {
            *slot = Some(v.clone());
            true
        }
        _ => false,
    }
}

/// The entries of `dir`, or `None` if it does not exist.
pub(crate) fn read_dir_or_empty(dir: &Path) -> io::Result<Option<Vec<PathBuf>>> {
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

/// A reference to a `.jsonl` file with the given metadata; `None` for anything else.
pub(crate) fn transcript_ref(
    engine: Engine,
    path: &Path,
    meta: &fs::Metadata,
) -> Option<TranscriptRef> {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") || !meta.is_file() {
        return None;
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| {
            TimestampMs::try_from(d.as_millis()).unwrap_or(TimestampMs::MAX)
        });
    Some(TranscriptRef {
        engine,
        path: path.to_path_buf(),
        inner_id: None,
        size: meta.len(),
        modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_keep_the_first_entries_and_count_all() {
        let mut skips = Skips::default();
        for i in 0..(MAX_REPORTED_SKIPS as u64 + 50) {
            skips.note(
                Path::new("x"),
                SkippedLine {
                    offset: i,
                    len: 1,
                    reason: SkipReason::InvalidUtf8,
                },
            );
        }
        let (kept, total) = skips.finish(Path::new("x"));
        assert_eq!(kept.len(), MAX_REPORTED_SKIPS);
        assert_eq!(kept[0].offset, 0);
        assert_eq!(total, MAX_REPORTED_SKIPS as u64 + 50);
    }
}
