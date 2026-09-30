//! `pitcrew_ingest::codex::parse_line` on one arbitrary line: no panic, every payload capped
//! (including `apply_patch` diffs), every item carries the line's offset and survives the wire.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::{MAX_ID_BYTES, MAX_PATH_BYTES, check_items};
use pitcrew_ingest::codex::parse_line;
use pitcrew_protocol::transcript::TranscriptItem;

const OFFSET: u64 = 4242;
/// `MAX_PATCH_FILES` in `crates/ingest/src/codex/parse.rs`.
const MAX_PATCH_FILES: usize = 100;

fuzz_target!(|line: &[u8]| {
    let Ok(rec) = parse_line(line, OFFSET) else {
        return;
    };
    check_items(&rec.items, Some(OFFSET));
    let edits = rec
        .items
        .iter()
        .filter(|i| matches!(i, TranscriptItem::FileEdit { .. }))
        .count();
    assert!(edits <= MAX_PATCH_FILES, "{edits} file edits from one call");
    let f = &rec.facts;
    for id in [&f.session_id, &f.branch, &f.model] {
        assert!(id.as_ref().is_none_or(|s| s.len() <= MAX_ID_BYTES));
    }
    assert!(f.cwd.as_ref().is_none_or(|s| s.len() <= MAX_PATH_BYTES));
});
