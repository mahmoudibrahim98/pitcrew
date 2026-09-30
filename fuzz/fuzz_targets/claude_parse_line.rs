//! `pitcrew_ingest::claude::parse_line` on one arbitrary line: no panic, every payload capped,
//! every item carries the line's offset and survives the wire.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::{MAX_ID_BYTES, MAX_PATH_BYTES, MAX_TITLE_CHARS, check_items};
use pitcrew_ingest::claude::parse_line;
use pitcrew_protocol::transcript::TranscriptItem;

const OFFSET: u64 = 4242;

fuzz_target!(|line: &[u8]| {
    let Ok(rec) = parse_line(line, OFFSET) else {
        return;
    };
    check_items(&rec.items, Some(OFFSET));
    if rec.soft_turn_end {
        assert!(
            matches!(&rec.items[..], [TranscriptItem::TurnEnded { .. }]),
            "a turn-duration record holds one TurnEnded"
        );
    }
    let f = &rec.facts;
    for id in [&f.session_id, &f.agent_id, &f.branch, &f.model] {
        assert!(id.as_ref().is_none_or(|s| s.len() <= MAX_ID_BYTES));
    }
    assert!(f.cwd.as_ref().is_none_or(|s| s.len() <= MAX_PATH_BYTES));
    for title in [&f.custom_title, &f.summary] {
        assert!(
            title
                .as_ref()
                .is_none_or(|s| s.chars().count() <= MAX_TITLE_CHARS + 1)
        );
    }
});
