//! Caps on what an adapter copies out of a transcript, and the helpers that apply them.

use crate::text::{title, truncate_chars};
use pitcrew_interfaces::source::{PlanItem, PlanStatus};
use serde_json::{Value, json};

/// Longest prompt or assistant text kept, in characters.
pub(crate) const MAX_TEXT_CHARS: usize = 100_000;
/// Longest tool target, in characters.
pub(crate) const MAX_TARGET_CHARS: usize = 200;
/// Tool result summaries: this many lines, this many characters.
pub(crate) const SUMMARY_LINES: usize = 3;
pub(crate) const SUMMARY_CHARS: usize = 240;
/// Tool inputs: strings are cut to this many characters...
pub(crate) const MAX_INPUT_STRING_CHARS: usize = 1024;
/// ...and an input still larger than this (as JSON) is replaced by a preview.
pub(crate) const MAX_INPUT_JSON_BYTES: usize = 16 * 1024;
/// Longest diff kept for one file, in bytes.
pub(crate) const MAX_DIFF_BYTES: usize = 64 * 1024;
/// Ids, model and branch longer than this (in bytes) are dropped as implausible.
pub(crate) const MAX_ID_BYTES: usize = 256;
/// A cwd or file path longer than this (in bytes) is dropped or cut.
pub(crate) const MAX_PATH_BYTES: usize = 4096;
/// Longest title, in characters.
pub(crate) const MAX_TITLE_CHARS: usize = 120;
/// Tool names, in characters.
pub(crate) const MAX_TOOL_CHARS: usize = 100;
/// Plan lines: at most this many, each cut to this many characters.
pub(crate) const MAX_PLAN_ITEMS: usize = 200;
pub(crate) const MAX_PLAN_TEXT_CHARS: usize = 1000;

/// A trimmed, non-empty value no longer than `max` bytes; longer values are dropped, not cut,
/// because a cut id or path would name something else.
pub(crate) fn bounded(s: Option<&str>, max: usize) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= max)
        .map(str::to_owned)
}

/// A title-like value: whitespace collapsed, cut to [`MAX_TITLE_CHARS`].
pub(crate) fn title_value(s: Option<&str>) -> Option<String> {
    s.map(|s| title(s, MAX_TITLE_CHARS))
        .filter(|s| !s.is_empty())
}

/// A call id, cut the same way on both the call and the result so they still pair.
pub(crate) fn call_id(s: &str) -> String {
    truncate_chars(s, MAX_ID_BYTES)
}

/// A plan line; unknown statuses count as pending.
pub(crate) fn plan_item(text: &str, status: Option<&str>) -> PlanItem {
    PlanItem {
        text: truncate_chars(text, MAX_PLAN_TEXT_CHARS),
        status: match status {
            Some("in_progress") => PlanStatus::InProgress,
            Some("completed") => PlanStatus::Completed,
            _ => PlanStatus::Pending,
        },
    }
}

/// A tool input with long strings cut; an input still too large becomes a preview.
pub(crate) fn bounded_input(input: &Value) -> Value {
    fn shorten(v: &Value, depth: usize) -> Value {
        match v {
            Value::String(s) => Value::String(truncate_chars(s, MAX_INPUT_STRING_CHARS)),
            _ if depth > 16 => Value::String("…".into()),
            Value::Array(a) => {
                Value::Array(a.iter().take(200).map(|x| shorten(x, depth + 1)).collect())
            }
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), shorten(x, depth + 1)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    let short = shorten(input, 0);
    let json = serde_json::to_string(&short).unwrap_or_default();
    if json.len() <= MAX_INPUT_JSON_BYTES {
        short
    } else {
        json!({ "truncated": true, "preview": truncate_chars(&json, MAX_INPUT_STRING_CHARS) })
    }
}

/// A diff that is already text, cut at a line boundary to [`MAX_DIFF_BYTES`].
pub(crate) fn capped_diff(diff: &str) -> String {
    if diff.len() <= MAX_DIFF_BYTES {
        return diff.to_owned();
    }
    let mut end = 0;
    for line in diff.split_inclusive('\n') {
        if end + line.len() > MAX_DIFF_BYTES {
            break;
        }
        end += line.len();
    }
    let mut out = diff[..end].to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("… (diff truncated)\n");
    out
}

/// A unified diff capped at `cap` bytes of body.
#[derive(Debug)]
pub(crate) struct Diff {
    text: String,
    cap: usize,
    full: bool,
}

impl Diff {
    /// A diff from `old` to `new` (paths, or `/dev/null`).
    pub(crate) fn new(old: &str, new: &str, cap: usize) -> Self {
        Self {
            text: format!("--- {old}\n+++ {new}\n"),
            cap,
            full: false,
        }
    }

    pub(crate) fn push(&mut self, line: &str) {
        if self.full {
            return;
        }
        if self.text.len() + line.len() + 1 > self.cap {
            self.text.push_str("… (diff truncated)\n");
            self.full = true;
            return;
        }
        self.text.push_str(line);
        self.text.push('\n');
    }

    pub(crate) fn finish(self) -> String {
        self.text
    }
}
