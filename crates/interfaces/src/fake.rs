//! In-memory fakes of the interfaces, for tests in dependent streams.

use crate::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use crate::source::{
    Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptItem, TranscriptPage, TranscriptRef,
};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::model::Engine;
use pitcrew_protocol::runner::Key;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct FakeTerminal {
    info: Option<TerminalInfo>,
    output: Vec<u8>,
    keys: Vec<Key>,
}

/// A runtime that records writes and keys, and echoes typed bytes into its output.
#[derive(Debug, Default)]
pub struct FakeRuntime {
    terminals: Mutex<BTreeMap<TerminalId, FakeTerminal>>,
}

impl FakeRuntime {
    /// Keys sent to a terminal so far.
    #[must_use]
    pub fn keys(&self, id: TerminalId) -> Vec<Key> {
        self.terminals
            .lock()
            .map(|t| t.get(&id).map(|x| x.keys.clone()).unwrap_or_default())
            .unwrap_or_default()
    }

    fn with<T>(
        &self,
        id: TerminalId,
        f: impl FnOnce(&mut FakeTerminal) -> T,
    ) -> Result<T, RuntimeError> {
        let mut map = self
            .terminals
            .lock()
            .map_err(|_| RuntimeError::Unavailable("poisoned".into()))?;
        map.get_mut(&id).map(f).ok_or(RuntimeError::NotFound(id))
    }
}

impl Runtime for FakeRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Pty
    }

    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        let info = TerminalInfo {
            id: TerminalId::new(),
            name: spec.name.clone(),
            pid: None,
            alive: true,
            native_target: None,
        };
        let mut map = self
            .terminals
            .lock()
            .map_err(|_| RuntimeError::Unavailable("poisoned".into()))?;
        map.insert(
            info.id,
            FakeTerminal {
                info: Some(info.clone()),
                ..FakeTerminal::default()
            },
        );
        Ok(info)
    }

    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.with(id, |t| t.output.extend_from_slice(bytes))
    }

    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        self.with(id, |t| t.keys.extend_from_slice(keys))
    }

    fn resize(&self, id: TerminalId, _cols: u16, _rows: u16) -> Result<(), RuntimeError> {
        self.with(id, |_| ())
    }

    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        self.with(id, |t| Screen {
            rows: String::from_utf8_lossy(&t.output)
                .lines()
                .map(|l| l.trim_end().to_owned())
                .collect(),
            cols: 80,
            cursor_row: 0,
            cursor_col: 0,
        })
    }

    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        self.with(id, |t| {
            let end = t.output.len() as u64;
            let start = usize::try_from(from.min(end))
                .unwrap_or(usize::MAX)
                .min(t.output.len());
            let stop = start.saturating_add(max).min(t.output.len());
            OutputChunk {
                offset: start as u64,
                data: t.output[start..stop].to_vec(),
                end,
                truncated: false,
            }
        })
    }

    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.with(id, |t| t.info.clone())?
            .ok_or(RuntimeError::NotFound(id))
    }

    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        let map = self
            .terminals
            .lock()
            .map_err(|_| RuntimeError::Unavailable("poisoned".into()))?;
        Ok(map.values().filter_map(|t| t.info.clone()).collect())
    }

    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.with(id, |t| {
            if let Some(info) = t.info.as_mut() {
                info.alive = false;
            }
        })
    }
}

/// A source adapter that serves a fixed list of transcripts and items, one item per read.
#[derive(Debug)]
pub struct FakeSource {
    engine: Engine,
    transcripts: Vec<TranscriptRef>,
    items: Vec<TranscriptItem>,
}

impl FakeSource {
    /// A fake for `engine` with the given transcripts and items.
    #[must_use]
    pub fn new(
        engine: Engine,
        transcripts: Vec<TranscriptRef>,
        items: Vec<TranscriptItem>,
    ) -> Self {
        Self {
            engine,
            transcripts,
            items,
        }
    }
}

impl SourceAdapter for FakeSource {
    fn engine(&self) -> Engine {
        self.engine
    }

    fn discover(&self, _home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        Ok(self.transcripts.clone())
    }

    fn read_from(&self, _t: &TranscriptRef, cursor: &Cursor) -> Result<ParseChunk, SourceError> {
        let index = usize::try_from(cursor.offset).unwrap_or(usize::MAX);
        let items: Vec<TranscriptItem> = self.items.get(index).cloned().into_iter().collect();
        let advanced = u64::try_from(items.len()).unwrap_or(0);
        Ok(ParseChunk {
            cursor: Cursor {
                offset: cursor.offset + advanced,
                state: None,
            },
            meta: None,
            items,
        })
    }

    /// Pages by item offset: returns up to `limit` items whose offset is below `before`.
    fn read_page(
        &self,
        _t: &TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, SourceError> {
        let end = before.map_or(self.items.len(), |b| {
            self.items.partition_point(|i| i.offset() < b)
        });
        let start = end.saturating_sub(limit);
        let items = self.items[start..end].to_vec();
        Ok(TranscriptPage {
            from: items
                .first()
                .map_or(before.unwrap_or(0), TranscriptItem::offset),
            to: self
                .items
                .get(end)
                .map_or_else(|| before.unwrap_or(u64::MAX), TranscriptItem::offset),
            at_start: start == 0,
            items,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_runtime_records_and_replays() {
        let rt = FakeRuntime::default();
        let t = rt
            .start(&StartSpec {
                program: "claude".into(),
                args: vec![],
                cwd: "/tmp".into(),
                env: vec![],
                name: "writer".into(),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        rt.write(t.id, b"hello\nworld").unwrap();
        rt.send_keys(t.id, &[Key::Enter]).unwrap();
        assert_eq!(rt.keys(t.id), vec![Key::Enter]);
        let chunk = rt.read_output(t.id, 6, 100).unwrap();
        assert_eq!(chunk.data, b"world");
        assert_eq!(chunk.end, 11);
        assert_eq!(rt.screen(t.id).unwrap().rows, vec!["hello", "world"]);
        rt.kill(t.id).unwrap();
        assert!(!rt.info(t.id).unwrap().alive);
    }

    #[test]
    fn fake_source_reads_incrementally() {
        let src = FakeSource::new(
            Engine::Claude,
            vec![],
            vec![
                TranscriptItem::TurnEnded { at: 1, offset: 0 },
                TranscriptItem::TurnEnded { at: 2, offset: 10 },
            ],
        );
        let t = TranscriptRef {
            engine: Engine::Claude,
            path: "x.jsonl".into(),
            inner_id: None,
            size: 0,
            modified: 0,
        };
        let first = src.read_from(&t, &Cursor::default()).unwrap();
        assert_eq!(first.items.len(), 1);
        let second = src.read_from(&t, &first.cursor).unwrap();
        assert_eq!(second.items.len(), 1);
        let third = src.read_from(&t, &second.cursor).unwrap();
        assert!(third.items.is_empty());
    }

    #[test]
    fn fake_source_pages_from_the_tail() {
        let items: Vec<TranscriptItem> = (0..5)
            .map(|i| TranscriptItem::TurnEnded {
                at: i,
                offset: u64::try_from(i).unwrap() * 100,
            })
            .collect();
        let src = FakeSource::new(Engine::Claude, vec![], items);
        let t = TranscriptRef {
            engine: Engine::Claude,
            path: "x.jsonl".into(),
            inner_id: None,
            size: 0,
            modified: 0,
        };
        let newest = src.read_page(&t, None, 2).unwrap();
        assert_eq!(newest.from, 300);
        assert!(!newest.at_start);
        let older = src.read_page(&t, Some(newest.from), 2).unwrap();
        assert_eq!((older.from, older.to), (100, 300));
        let oldest = src.read_page(&t, Some(older.from), 2).unwrap();
        assert_eq!(oldest.items.len(), 1);
        assert!(oldest.at_start);
    }
}
