//! # pitcrew-interfaces
//!
//! The seams that let streams work in parallel.
//! - [`runtime::Runtime`]: runs terminals (tmux or a PTY supervisor). Implemented by stream B and
//!   used by the runner (stream D).
//! - [`source::SourceAdapter`]: reads one agent CLI's transcripts incrementally. Implemented by
//!   stream A and used by the runner, the scan and import.
//! - [`fake`]: in-memory implementations of both, so dependants can test without the real thing.
//!
//! The traits are synchronous and runtime-agnostic. Implementations may use threads or an async
//! runtime internally, and callers wrap them as they need.
//!
//! **Change process:** owned by stream 0; see `docs/build/contracts.md`.

#![forbid(unsafe_code)]

pub mod fake;
pub mod runtime;
pub mod source;
