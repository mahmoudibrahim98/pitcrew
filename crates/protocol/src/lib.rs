//! # pitcrew-protocol
//!
//! The contract every PitCrew stream codes against. It has six parts:
//! - [`model`]: the domain model (workspace → project → workstream → task → subtask; members who
//!   are people or agents; sessions; asks; receipts).
//! - [`events`]: the append-only event log. Every change is an authored event, and pages, recaps
//!   and the Inbox are all projections of it.
//! - [`runner`]: the protocol between a hub and the runner on each machine.
//! - [`api`]: host info and the frames of the desktop's delta stream.
//! - [`transcript`]: transcript items and pages, as the Agent console receives them.
//! - [`recap`]: activity blocks, summaries with receipts and day paragraphs, as the API serves
//!   them.
//!
//! Besides the six, [`text`] holds the one set of hidden characters every crate drops from
//! untrusted text.
//!
//! **Change process.** This crate belongs to stream 0. Other streams propose changes in a
//! `s/0/contract-…` pull request. Breaking changes bump [`version::PROTOCOL_VERSION`].
//! See `docs/build/contracts.md`.

#![forbid(unsafe_code)]

pub mod api;
pub mod events;
pub mod ids;
pub mod model;
pub mod recap;
pub mod runner;
pub mod text;
pub mod transcript;
pub mod version;

pub use ids::*;
pub use version::{PROTOCOL_MIN, PROTOCOL_VERSION, is_compatible};
