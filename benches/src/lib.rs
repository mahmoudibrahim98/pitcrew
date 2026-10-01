//! # pitcrew-benches
//!
//! Benchmarks for the performance budgets in `docs/build/streams/P.md`. **Owned by stream P.**
//!
//! - `benches/*.rs`: criterion benchmarks, run with `cargo bench -p pitcrew-benches`.
//! - [`inputs`]: synthetic transcripts, control-mode output and events. Nothing real.
//! - [`probe`]: measures the delta stream from an append to the frame that carries it.
//! - [`metrics`]: which benchmark measures which budget.
//! - [`report`]: reads criterion's results, writes the JSON summary and compares it with
//!   `benches/baseline.json`. The `pitcrew-bench-report` binary and `benches/run.sh` drive it.

#![forbid(unsafe_code)]

pub mod inputs;
pub mod metrics;
pub mod probe;
pub mod report;

use serde::{Deserialize, Serialize};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

/// The environment variable that picks the [`Mode`].
pub const MODE_VAR: &str = "PITCREW_BENCH_MODE";

/// How much to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// For CI: 20 MiB transcripts only, few samples.
    Quick,
    /// For a local run: 20 and 200 MiB transcripts, more samples.
    Full,
}

impl Mode {
    /// `PITCREW_BENCH_MODE=quick` or `full`; full when unset or unknown.
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var(MODE_VAR)
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or(Self::Full)
    }

    /// `quick` or `full`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "quick" => Some(Self::Quick),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// The name used in files and on the command line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Full => "full",
        }
    }

    /// Transcript sizes to generate, in MiB.
    #[must_use]
    pub fn transcript_sizes(self) -> &'static [u64] {
        match self {
            Self::Quick => &[20],
            Self::Full => &[20, 200],
        }
    }
}
