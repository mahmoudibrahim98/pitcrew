//! `read_from` with an arbitrary (corrupt, stale or hostile) cursor. Cursors are stored by the
//! runner between reads, so a damaged state file or a replaced transcript must not crash it.
//!
//! Input: `[engine, offset lo, offset hi, state length lo, state length hi]`, the cursor state
//! (JSON, if it parses), then the file.
//!
//! Checks: no panic; an error only when the cursor is past the end of the file; otherwise the
//! returned cursor is within the file, and reading again from it finds nothing new.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::{check_items, scratch_path};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_ingest::codex::CodexAdapter;
use pitcrew_interfaces::source::{Cursor, SourceAdapter, TranscriptRef};
use pitcrew_protocol::model::Engine;
use std::fs;

fuzz_target!(|input: &[u8]| {
    let Some((&[engine, o0, o1, s0, s1], rest)) = input.split_first_chunk::<5>() else {
        return;
    };
    let state_len = usize::from(u16::from_le_bytes([s0, s1])).min(rest.len());
    let (state, data) = rest.split_at(state_len);
    let (adapter, engine): (&dyn SourceAdapter, Engine) = if engine & 1 == 0 {
        (&ClaudeAdapter, Engine::Claude)
    } else {
        (&CodexAdapter, Engine::Codex)
    };
    let len = data.len() as u64;
    // 0..=len+1, so both "inside the file" and "past the end" are reached.
    let offset = u64::from(u16::from_le_bytes([o0, o1])) % (len + 2);
    let cursor = Cursor {
        offset,
        state: serde_json::from_slice(state).ok(),
    };

    let path = scratch_path("cursor.jsonl");
    fs::write(&path, data).expect("write the transcript");
    let transcript = TranscriptRef {
        engine,
        path,
        inner_id: None,
        size: len,
        modified: 0,
    };
    let chunk = match adapter.read_from(&transcript, &cursor) {
        Ok(chunk) => chunk,
        Err(e) => {
            assert!(
                offset > len,
                "read_from failed with the cursor inside the file: {e}"
            );
            return;
        }
    };
    assert!(offset <= len, "a cursor past the end was accepted");
    assert!(
        chunk.cursor.offset >= offset && chunk.cursor.offset <= len,
        "the new cursor moved backwards or past the end"
    );
    check_items(&chunk.items, None);

    let again = adapter
        .read_from(&transcript, &chunk.cursor)
        .expect("a read from a returned cursor never fails");
    assert!(again.items.is_empty(), "a second read found items again");
    assert_eq!(again.cursor.offset, chunk.cursor.offset);
});
