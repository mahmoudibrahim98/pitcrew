//! Activity blocks: bursts of work, each with what changed and receipts.
//!
//! The block types are wire types, so they live in `pitcrew_protocol::recap`; this module
//! re-exports them and keeps the engine's own settings.

use serde::{Deserialize, Serialize};

pub use pitcrew_protocol::recap::{Block, BlockKey, Counts, Fact, FactKind, FileTouch};

/// How events are grouped, and the caps that bound every block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// A pause longer than this, in milliseconds, ends a block. Default: 20 minutes.
    pub gap_ms: i64,
    /// Most blocks open at once. Past this, the one idle longest is closed. Default: 4096.
    pub max_open: usize,
    /// Most distinct files listed per block. Default: 20.
    pub max_files: usize,
    /// Most facts per block. Default: 24.
    pub max_facts: usize,
    /// Most task links per block. Default: 8.
    pub max_tasks: usize,
    /// Most distinct authors listed per block. Default: 8.
    pub max_actors: usize,
    /// Most receipts per fact, per file and per count. Default: 8; at least 2.
    pub max_receipts: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap_ms: 20 * 60 * 1000,
            max_open: 4096,
            max_files: 20,
            max_facts: 24,
            max_tasks: 8,
            max_actors: 8,
            max_receipts: 8,
        }
    }
}

impl Config {
    /// The same config with every field in a usable range.
    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            gap_ms: self.gap_ms.max(0),
            max_open: self.max_open.max(1),
            max_files: self.max_files.max(1),
            max_facts: self.max_facts.max(1),
            max_tasks: self.max_tasks.max(1),
            max_actors: self.max_actors.max(1),
            max_receipts: self.max_receipts.max(2),
        }
    }
}
