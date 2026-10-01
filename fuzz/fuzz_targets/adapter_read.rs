//! `ClaudeAdapter` / `CodexAdapter` on arbitrary file bytes, through the `SourceAdapter` trait.
//!
//! Input: four control bytes (engine, split point, page size, page end), then the file.
//!
//! Checks, besides "no panic" and the item caps:
//! - reading the file in two parts (as if the agent was still writing it) gives the same items
//!   as reading it at once;
//! - paging backwards from the end to the start gives the same items as reading forwards, and
//!   every page makes progress;
//! - a page never ends after the offset it was asked to end at.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::{check_items, scratch_path};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_ingest::codex::CodexAdapter;
use pitcrew_interfaces::source::{Cursor, SourceAdapter, TranscriptItem, TranscriptRef};
use pitcrew_protocol::model::Engine;
use std::fs;
use std::io::Write as _;

fuzz_target!(|input: &[u8]| {
    let Some((&[engine, split, limit, before], data)) = input.split_first_chunk::<4>() else {
        return;
    };
    let (adapter, engine): (&dyn SourceAdapter, Engine) = if engine & 1 == 0 {
        (&ClaudeAdapter, Engine::Claude)
    } else {
        (&CodexAdapter, Engine::Codex)
    };
    let path = scratch_path("transcript.jsonl");
    let len = data.len() as u64;
    let transcript = TranscriptRef {
        engine,
        path: path.clone(),
        inner_id: None,
        size: len,
        modified: 0,
    };

    // The whole file at once.
    fs::write(&path, data).expect("write the transcript");
    let whole = adapter
        .read_from(&transcript, &Cursor::default())
        .expect("a first read of a readable file never fails");
    check_items(&whole.items, None);
    check_offsets(&whole.items, 0, whole.cursor.offset);
    assert!(whole.cursor.offset <= len, "the cursor is past the end");
    assert!(whole.meta.is_some(), "a first read reports session facts");

    // The same file in two parts.
    let cut = data.len() * usize::from(split) / 255;
    fs::write(&path, &data[..cut]).expect("write the first part");
    let first = adapter
        .read_from(&transcript, &Cursor::default())
        .expect("a first read never fails");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(&data[cut..]))
        .expect("append the rest");
    let second = adapter
        .read_from(&transcript, &first.cursor)
        .expect("a read from a returned cursor never fails");
    let mut parts = first.items;
    parts.extend(second.items);
    assert_eq!(parts, whole.items, "two reads differ from one read");
    assert_eq!(second.cursor.offset, whole.cursor.offset);

    // Pages from the end back to the start.
    let limit = 1 + usize::from(limit % 16);
    let mut pages = Vec::new();
    let mut end: Option<u64> = None;
    loop {
        let page = adapter
            .read_page(&transcript, end, limit)
            .expect("paging a readable file never fails");
        assert!(page.from <= page.to && page.to <= len, "bad page bounds");
        assert!(
            end.is_none_or(|e| page.to <= e),
            "the page ends after `before`"
        );
        check_offsets(&page.items, page.from, page.to);
        let at_start = page.at_start;
        let from = page.from;
        if !at_start {
            assert!(
                from < page.to,
                "a page that is not at the start makes no progress"
            );
        }
        pages.push(page.items);
        if at_start {
            break;
        }
        end = Some(from);
    }
    let paged: Vec<TranscriptItem> = pages.into_iter().rev().flatten().collect();
    assert_eq!(
        paged, whole.items,
        "paging backwards differs from reading forwards"
    );

    // One page ending at an arbitrary offset.
    let before = len * u64::from(before) / 255;
    let page = adapter
        .read_page(&transcript, Some(before), limit)
        .expect("paging never fails");
    assert!(page.to <= before && page.from <= page.to);
    check_offsets(&page.items, page.from, page.to);
});

/// Item offsets are in `[from, to)` and never go backwards.
fn check_offsets(items: &[TranscriptItem], from: u64, to: u64) {
    let mut last = from;
    for item in items {
        let offset = item.offset();
        assert!(
            offset >= last && offset < to,
            "offset {offset} out of order or range"
        );
        last = offset;
    }
}
