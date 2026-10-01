//! # pitcrew-recap
//!
//! Recap engine: activity blocks, summaries with receipts, and Where-it-stands proposals.
//!
//! **Owned by stream F.** The work packages are in `docs/build/streams/F.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! ## Blocks
//!
//! [`blocks`] turns events in log order into [`Block`]s: bursts of one session's (or one
//! workstream's) work with no pause longer than a gap. Each block has its links, counts, the files
//! touched and notable [`Fact`]s, and every fact carries receipts. [`BlockBuilder`] does the same
//! incrementally and reports which blocks changed or closed; fed in any batches it gives the same
//! blocks as [`blocks`]. A [`Directory`] seeds what is known before the first event (sessions,
//! tasks, names) and is kept current from the events.
//!
//! ## Summaries
//!
//! Rules turn a block into a [`Draft`] ([`draft_line`]) and a workstream's day into another
//! ([`draft_paragraph`], [`days`]). A draft is clauses with receipts; a [`Summarizer`] turns it into
//! a [`Summary`] whose every clause is a [`Span`] with receipts. [`RuleSummarizer`] is the
//! default and needs no model; [`FakeSummarizer`] stands in for a model in tests.
//!
//! ## Where it stands
//!
//! [`standing`] reads a workstream's recent blocks into its state, tasks, checks, open asks and
//! next step, each a clause with receipts. [`propose_workstream`] and [`propose_project`] write
//! them as [`BriefProposal`]s, whose [`BriefProposal::body`] is the `BriefProposed` event the
//! caller appends. Pinned briefs only get proposals; unpinned ones may be applied automatically
//! when the [`BriefPolicy`] allows it. [`propose_paused`] asks "paused?" about a quiet workstream.
//!
//! Everything here is pure and deterministic: no clock, no I/O, no model calls. Internal maps hash
//! with a random seed each, but no output follows their order, so the same input always gives
//! byte-identical output. All event text is untrusted: it is cleaned and capped before it is kept,
//! and nothing panics on any input.

#![forbid(unsafe_code)]

mod block;
mod brief;
mod build;
mod checks;
mod day;
mod directory;
mod draft;
mod hash;
mod summary;
mod text;
mod time;

pub use block::{Block, BlockKey, Config, Counts, Fact, FactKind, FileTouch};
pub use brief::{
    BriefPolicy, BriefProposal, Disposition, NextStep, Standing, Urgency, draft_brief,
    draft_rollup, propose_paused, propose_project, propose_workstream, standing,
};
pub use build::{BlockBuilder, BlockChanges, blocks};
pub use checks::{Check, classify, mentions_divergence};
pub use day::{Day, DayRecap, block_line, day_recaps, days};
pub use directory::Directory;
pub use draft::{draft_line, draft_paragraph};
pub use summary::{
    Clause, Draft, DraftKind, FakeSummarizer, RuleSummarizer, Sentence, Span, Summarizer, Summary,
    SummaryError, verify,
};
pub use text::clean;
pub use time::date_of;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
