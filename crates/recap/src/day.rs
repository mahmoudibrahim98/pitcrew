//! Days: blocks grouped per workstream per calendar day, and a paragraph for each.

use crate::block::Block;
use crate::directory::Directory;
use crate::draft::{draft_line, draft_paragraph};
use crate::summary::{RuleSummarizer, Summarizer, Summary, SummaryError, verify};
use crate::time::date_of;
use pitcrew_protocol::ids::{EventId, WorkstreamId};
use pitcrew_protocol::model::Date;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One workstream's blocks on one day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Day<'a> {
    /// The workstream; `None` for blocks not linked to one.
    pub workstream: Option<WorkstreamId>,
    /// The day a block started on, at the chosen UTC offset.
    pub date: Date,
    /// The blocks, in order.
    pub blocks: Vec<&'a Block>,
}

/// Groups blocks per workstream per day. Days come in date order, and within a day, workstreams
/// in id order (unlinked blocks first).
#[must_use]
pub fn days(blocks: &[Block], utc_offset_minutes: i32) -> Vec<Day<'_>> {
    let mut groups: BTreeMap<(Date, Option<WorkstreamId>), Vec<&Block>> = BTreeMap::new();
    for block in blocks {
        groups
            .entry((date_of(block.start, utc_offset_minutes), block.workstream))
            .or_default()
            .push(block);
    }
    groups
        .into_iter()
        .map(|((date, workstream), mut blocks)| {
            blocks.sort_by_key(|b| (b.start, b.id));
            Day {
                workstream,
                date,
                blocks,
            }
        })
        .collect()
}

/// The paragraph for one workstream's day, with the blocks it covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRecap {
    /// The workstream; `None` for blocks not linked to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workstream: Option<WorkstreamId>,
    /// The day.
    pub date: Date,
    /// Ids of the blocks covered, in order.
    pub blocks: Vec<EventId>,
    /// The paragraph.
    pub summary: Summary,
}

/// A paragraph per workstream per day, written by `summarizer` and checked with [`verify`].
///
/// # Errors
///
/// Returns the summarizer's error, or the problem [`verify`] found in its output.
pub fn day_recaps(
    blocks: &[Block],
    directory: &Directory,
    utc_offset_minutes: i32,
    summarizer: &dyn Summarizer,
) -> Result<Vec<DayRecap>, SummaryError> {
    days(blocks, utc_offset_minutes)
        .into_iter()
        .map(|day| {
            let draft = draft_paragraph(&day.blocks, directory);
            let summary = summarizer.summarize(&draft)?;
            verify(&summary, &draft)?;
            Ok(DayRecap {
                workstream: day.workstream,
                date: day.date,
                blocks: day.blocks.iter().map(|b| b.id).collect(),
                summary,
            })
        })
        .collect()
}

/// A block's one-line summary, by the rules.
#[must_use]
pub fn block_line(block: &Block, directory: &Directory) -> Summary {
    RuleSummarizer.render(&draft_line(block, directory))
}
