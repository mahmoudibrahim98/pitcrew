//! Bounds: caps that keep one sync call finite regardless of what the server sends, plus
//! [`strip_hidden`]/[`cap_chars`]/[`cap_labels`], which also keep untrusted text honest about what
//! it displays as.

/// Stop paginating after this many pages in one call. The next sync call resumes through the
/// `since` cursor (issues) or the stored "last seen" bound (pull requests).
pub const MAX_PAGES_PER_CALL: usize = 20;

/// Stop collecting items after this many, across all pages, in one call.
pub const MAX_ITEMS_PER_SYNC: usize = 2_000;

/// Drop a page whose body is larger than this, counting it as malformed rather than parsing a
/// huge document.
pub const MAX_PAGE_BODY_BYTES: usize = 5 * 1024 * 1024;

/// Cap on a title's length, in characters.
pub const MAX_TITLE_CHARS: usize = 512;

/// Cap on a body's length, in characters.
pub const MAX_BODY_CHARS: usize = 64 * 1024;

/// Cap on how many labels one item keeps.
pub const MAX_LABELS: usize = 50;

/// Cap on one label's length, in characters.
pub const MAX_LABEL_CHARS: usize = 100;

/// Base delay for secondary-rate-limit backoff when the server gives no `retry-after`.
pub const SECONDARY_BACKOFF_BASE_SECS: i64 = 2;

/// Never back off longer than this.
pub const SECONDARY_BACKOFF_CAP_SECS: i64 = 300;

/// Stop growing the exponent here (the cap above is reached well before this on its own).
const MAX_BACKOFF_EXPONENT: u32 = 8;

/// Exponential backoff with a cap: `min(cap, base * 2^attempts)`.
#[must_use]
pub fn backoff_secs(attempts: u32) -> i64 {
    let exponent = attempts.min(MAX_BACKOFF_EXPONENT);
    let secs = SECONDARY_BACKOFF_BASE_SECS.saturating_mul(1i64 << exponent);
    secs.min(SECONDARY_BACKOFF_CAP_SECS)
}

/// Caps on one `list` call: how many pages to follow, and how many items to collect, before
/// treating the walk as truncated rather than complete. Production code always uses
/// [`Limits::default`] (this module's [`MAX_PAGES_PER_CALL`]/[`MAX_ITEMS_PER_SYNC`]); tests
/// override it to exercise cap-triggered truncation and resume behaviour without multi-thousand-
/// item fixtures.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_pages: usize,
    pub max_items: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pages: MAX_PAGES_PER_CALL,
            max_items: MAX_ITEMS_PER_SYNC,
        }
    }
}

/// Characters that change text direction or are invisible. Dropped from any upstream text this
/// crate stores or re-emits (round 2 review item R10), so a crafted title, body, label or name
/// cannot make a change — or anything downstream reading it, such as a summary or a UI list —
/// display differently from what it actually says. The same character set
/// `pitcrew_recap::text::clean`'s own `is_hidden` drops, reimplemented here rather than taken as a
/// dependency on that crate: `pitcrew-recap` depends on sync data flowing *up* to it, not the
/// other way around, and this check is small enough that duplicating it is cheaper than a new
/// cross-stream dependency.
fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Drops [`is_hidden`] characters, keeping everything else untouched. Unlike
/// `pitcrew_recap::text::clean`, this does not also collapse whitespace/control characters to a
/// single space or trim the ends: title/body/label text here is stored close to as-sent (beyond
/// the length caps below), and that further normalisation is display-layer policy, not sync
/// policy — callers that want it (e.g. a summary) already have their own `clean`.
#[must_use]
pub fn strip_hidden(s: &str) -> String {
    s.chars().filter(|c| !is_hidden(*c)).collect()
}

/// Truncates `s` to at most `max` characters (not bytes), on a char boundary, after
/// [`strip_hidden`] removes any hidden/direction-changing characters.
#[must_use]
pub fn cap_chars(s: &str, max: usize) -> String {
    strip_hidden(s).chars().take(max).collect()
}

/// Caps a list of labels: at most [`MAX_LABELS`] entries, each at most [`MAX_LABEL_CHARS`] long
/// (and, via [`cap_chars`], hidden-character-stripped).
#[must_use]
pub fn cap_labels(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .take(MAX_LABELS)
        .map(|l| cap_chars(l, MAX_LABEL_CHARS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_secs(0), 2);
        assert_eq!(backoff_secs(1), 4);
        assert_eq!(backoff_secs(2), 8);
        assert_eq!(backoff_secs(20), SECONDARY_BACKOFF_CAP_SECS);
    }

    #[test]
    fn cap_chars_truncates_on_char_boundaries() {
        assert_eq!(cap_chars("hello", 10), "hello");
        assert_eq!(cap_chars("hello", 3), "hel");
        assert_eq!(cap_chars("héllo", 2), "hé");
        assert_eq!(cap_chars("", 0), "");
    }

    #[test]
    fn strip_hidden_drops_bidi_overrides_and_zero_width_characters() {
        assert_eq!(strip_hidden("ab\u{202E}cd"), "abcd");
        assert_eq!(strip_hidden("a\u{200B}b\u{FEFF}c"), "abc");
        // Ordinary control characters (a newline, say) and whitespace are left untouched — only
        // the hidden/direction-changing set is removed here; any further normalisation is a
        // display-layer concern.
        assert_eq!(strip_hidden("a\nb\tc"), "a\nb\tc");
    }

    #[test]
    fn cap_chars_strips_hidden_characters_before_truncating() {
        assert_eq!(cap_chars("ab\u{202E}cd", 10), "abcd");
        // A right-to-left override made the title *look* like "gnikcah rof loot" at a glance, but
        // stripped it reads as what it actually says.
        assert_eq!(cap_chars("safe\u{202E}loot rof gnikcah", 4), "safe");
    }

    #[test]
    fn cap_labels_bounds_count_and_length() {
        let labels: Vec<String> = (0..100).map(|i| format!("label-{i}")).collect();
        let capped = cap_labels(&labels);
        assert_eq!(capped.len(), MAX_LABELS);
        let long = vec!["x".repeat(500)];
        assert_eq!(cap_labels(&long)[0].len(), MAX_LABEL_CHARS);
    }
}
