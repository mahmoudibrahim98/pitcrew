//! Checks the fuzz targets share. A failed check panics, and libFuzzer reports the panic as a
//! crash together with the input that caused it.

pub mod shell;

use pitcrew_protocol::transcript::TranscriptItem;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Whether to relax the checks for findings already reported and listed in
/// `docs/security/threat-model.md` §8 (`PITCREW_FUZZ_SKIP_KNOWN=1`), so a run can look past them.
/// Off by default: a known finding still fails the target until it is fixed.
#[must_use]
pub fn skip_known() -> bool {
    static SKIP: OnceLock<bool> = OnceLock::new();
    *SKIP.get_or_init(|| std::env::var_os("PITCREW_FUZZ_SKIP_KNOWN").is_some_and(|v| v == "1"))
}

/// An empty folder `name` in this process's scratch folder (see [`scratch_path`]), emptied first
/// if it exists.
pub fn fresh_dir(name: &str) -> PathBuf {
    let dir = scratch_path(name);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("cannot empty {}: {e}", dir.display()),
    }
    std::fs::create_dir_all(&dir).expect("create a scratch folder");
    dir
}

// Caps copied from `crates/ingest/src/bound.rs`, where they are crate-private. `truncate_chars`
// keeps `max` characters and adds one `…`, hence the `+ 1`s.
const MAX_TEXT_CHARS: usize = 100_000;
const MAX_TARGET_CHARS: usize = 200;
/// Codex appends ` (+N more)` to a cut target when a patch touches several files.
const TARGET_SUFFIX_CHARS: usize = 16;
const MAX_TOOL_CHARS: usize = 100;
const MAX_ID_CHARS: usize = 256;
const SUMMARY_CHARS: usize = 240;
const MAX_INPUT_JSON_BYTES: usize = 16 * 1024;
const MAX_DIFF_BYTES: usize = 64 * 1024;
const MAX_PATH_CHARS: usize = 4096;
const MAX_PLAN_ITEMS: usize = 200;
const MAX_PLAN_TEXT_CHARS: usize = 1000;
const MAX_QUESTION_CHARS: usize = 4000;
const MAX_OPTIONS: usize = 50;
const MAX_OPTION_CHARS: usize = 200;
/// Session facts: ids, model and branch are dropped over this many bytes, a cwd over 4 KiB.
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_PATH_BYTES: usize = 4096;
/// Titles and summaries are cut to this many characters (plus `…`).
pub const MAX_TITLE_CHARS: usize = 120;

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// A diff holds at most its cap, plus its `---`/`+++` header (two cut paths of up to four bytes
/// per character) and the truncation marker.
const MAX_DIFF_LEN: usize = MAX_DIFF_BYTES + 2 * 4 * (MAX_PATH_CHARS + 1) + 64;

/// Checks the caps every adapter promises, and that the item survives the wire.
pub fn check_item(item: &TranscriptItem) {
    match item {
        TranscriptItem::UserPrompt { text, .. } | TranscriptItem::AssistantText { text, .. } => {
            assert!(chars(text) <= MAX_TEXT_CHARS + 1, "text over its cap");
        }
        TranscriptItem::ToolUse {
            call_id,
            tool,
            target,
            input,
            ..
        } => {
            assert!(chars(call_id) <= MAX_ID_CHARS + 1, "call id over its cap");
            assert!(chars(tool) <= MAX_TOOL_CHARS + 1, "tool name over its cap");
            assert!(
                chars(target) <= MAX_TARGET_CHARS + 1 + TARGET_SUFFIX_CHARS,
                "target over its cap"
            );
            if let Some(input) = input {
                assert!(!input.is_null(), "a null input must be None");
                let len = serde_json::to_string(input).map_or(0, |s| s.len());
                assert!(
                    len <= MAX_INPUT_JSON_BYTES,
                    "tool input over its cap: {len}"
                );
            }
        }
        TranscriptItem::ToolResult {
            call_id, summary, ..
        } => {
            assert!(chars(call_id) <= MAX_ID_CHARS + 1, "call id over its cap");
            assert!(chars(summary) <= SUMMARY_CHARS + 1, "summary over its cap");
        }
        TranscriptItem::FileEdit { path, diff, .. } => {
            assert!(chars(path) <= MAX_PATH_CHARS + 1, "path over its cap");
            if let Some(diff) = diff {
                assert!(
                    diff.len() <= MAX_DIFF_LEN,
                    "diff over its cap: {}",
                    diff.len()
                );
            }
        }
        TranscriptItem::PlanUpdated { items, .. } => {
            assert!(items.len() <= MAX_PLAN_ITEMS, "too many plan items");
            for p in items {
                assert!(
                    chars(&p.text) <= MAX_PLAN_TEXT_CHARS + 1,
                    "plan text over its cap"
                );
            }
        }
        TranscriptItem::Question { text, options, .. } => {
            assert!(
                chars(text) <= MAX_QUESTION_CHARS + 1,
                "question over its cap"
            );
            assert!(options.len() <= MAX_OPTIONS, "too many options");
            for o in options {
                assert!(chars(o) <= MAX_OPTION_CHARS + 1, "option over its cap");
            }
        }
        _ => {}
    }
    roundtrip(item);
}

/// [`check_item`] for each item; with `offset`, every item must carry it.
pub fn check_items(items: &[TranscriptItem], offset: Option<u64>) {
    for item in items {
        if let Some(offset) = offset {
            assert_eq!(item.offset(), offset, "an item carries its record's offset");
        }
        check_item(item);
    }
}

/// Serializing a decoded value and decoding it again gives the same value.
pub fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
    let json = serde_json::to_string(value).expect("a decoded value serializes");
    let back: T = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("a value's own JSON does not decode: {e}\n{json}"));
    assert_eq!(&back, value, "the round trip changed the value\n{json}");
}

/// A file path in a private scratch folder for this process: under `PITCREW_FUZZ_TMP` if set,
/// else `/dev/shm` where it exists, else the temporary folder.
pub fn scratch_path(name: &str) -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let shm = Path::new("/dev/shm");
        let base = if let Some(dir) = std::env::var_os("PITCREW_FUZZ_TMP") {
            PathBuf::from(dir)
        } else if shm.is_dir() {
            shm.to_path_buf()
        } else {
            std::env::temp_dir()
        };
        let dir = base.join(format!("pitcrew-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the scratch folder");
        dir
    })
    .join(name)
}
