//! Bounded terminal history addressed by absolute byte offsets.

use std::collections::VecDeque;

use pitcrew_interfaces::runtime::OutputChunk;

/// Default retained history: 2 MiB per terminal.
pub const DEFAULT_CAPACITY: usize = 2 * 1024 * 1024;

/// A byte ring whose offsets never change when old bytes are evicted.
#[derive(Debug, Clone)]
pub struct ReplayBuffer {
    data: VecDeque<u8>,
    capacity: usize,
    end: u64,
}

impl Default for ReplayBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl ReplayBuffer {
    /// Retain at most `capacity` bytes. Zero tracks offsets without retaining data.
    pub fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::new(),
            capacity,
            end: 0,
        }
    }

    /// Resume absolute numbering at `offset`, with the default capacity.
    pub fn starting_at(offset: u64) -> Self {
        Self::with_capacity_at(DEFAULT_CAPACITY, offset)
    }

    /// Resume numbering with an explicit history capacity.
    pub fn with_capacity_at(capacity: usize, offset: u64) -> Self {
        Self {
            end: offset,
            ..Self::new(capacity)
        }
    }

    /// Append output, retaining only the newest `capacity` bytes.
    ///
    /// At `u64::MAX`, numbering saturates and unaddressable bytes are discarded.
    /// Retained bytes keep their original offsets; further appends are no-ops.
    pub fn append(&mut self, bytes: &[u8]) {
        let next_end = self.end.saturating_add(bytes.len() as u64);
        let bytes = &bytes[..(next_end - self.end) as usize];
        self.end = next_end;
        if bytes.len() >= self.capacity {
            self.data.clear();
            self.data.extend(&bytes[bytes.len() - self.capacity..]);
        } else {
            let drop = bytes.len().saturating_sub(self.capacity - self.data.len());
            self.data.drain(..drop);
            self.data.extend(bytes);
        }
    }

    /// Offset immediately after all output appended so far, including evicted bytes.
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Offset of the oldest byte still retained; `end()` when none is.
    pub fn start(&self) -> u64 {
        self.end - self.data.len() as u64
    }

    /// Read at most `max` bytes, clamping `from` to the retained interval.
    ///
    /// Requests before retained history set `truncated`, including empty reads.
    /// Requests at or beyond `end()` return an empty chunk at `end()`, without
    /// truncation. `end` always describes the entire stream, even for partial reads.
    pub fn read(&self, from: u64, max: usize) -> OutputChunk {
        let start = self.end - self.data.len() as u64;
        let offset = from.clamp(start, self.end);
        let index = (offset - start) as usize;
        let count = max.min(self.data.len() - index);
        OutputChunk {
            offset,
            data: self.data.range(index..index + count).copied().collect(),
            end: self.end,
            truncated: from < start,
        }
    }
}
