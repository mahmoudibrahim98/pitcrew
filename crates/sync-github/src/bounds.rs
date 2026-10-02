//! Bounds: caps that keep one sync call finite regardless of what the server sends, plus
//! [`strip_hidden`]/[`cap_chars`]/[`cap_labels`], which also keep untrusted text honest about what
//! it displays as.

// Characters that change text direction or are invisible: `pitcrew_protocol::text::is_hidden`,
// the one set the recap engine and the CLI drop too. Dropped from any upstream text this crate
// stores or re-emits (round 2 review item R10), so a crafted title, body, label or name cannot
// make a change — or anything downstream reading it, such as a summary or a UI list — display
// differently from what it actually says, and so a run of Unicode tag characters cannot smuggle
// text invisible to a person but readable by another LLM into a title, body or label (a
// prompt-injection vector into whatever later reads these fields — round 3 review item S-6).
use pitcrew_protocol::text::is_hidden;

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

/// Cap on an untrusted URL's length when it is quoted inside a `SyncIssue` message or a log line
/// (e.g. a rejected `Link: rel="next"`) — these are diagnostic text for a person to read, not a
/// place to dump an attacker-sized string.
pub const MAX_REPORTED_URL_CHARS: usize = 300;

/// Cap, in bytes, on an `html_url` kept verbatim in an `ExternalRef` (round 3 review item O30,
/// R10's residual: the rest of R10 — scheme, host, hidden characters — was fixed, but a trusted
/// scheme and host never meant a *reasonable* length either). Matches the console's own display
/// cap for a link shown to a person, so a kept URL this crate accepts is never longer than the UI
/// is willing to show in full.
pub const MAX_KEPT_URL_BYTES: usize = 2_048;

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

/// Whether `s` contains any [`is_hidden`] character. For text that gets *stripped* (titles,
/// bodies, labels, names — see [`strip_hidden`]), silently dropping the character is the right
/// call. For text where silently editing it would be unsafe — a URL, where dropping a character
/// changes what it points to without that being obvious — round 3 review item S-5 instead refuses
/// the whole value outright when this is `true`, rather than storing a silently-modified one.
#[must_use]
pub fn contains_hidden(s: &str) -> bool {
    s.chars().any(is_hidden)
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
    fn contains_hidden_detects_but_does_not_modify() {
        assert!(contains_hidden("ab\u{202E}cd"));
        assert!(!contains_hidden("plain text"));
        assert!(!contains_hidden(""));
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
    fn strip_hidden_drops_round_3s_additions() {
        // Round 3 review item S-6.
        assert_eq!(strip_hidden("a\u{00AD}b"), "ab", "soft hyphen");
        assert_eq!(
            strip_hidden("a\u{180E}b"),
            "ab",
            "Mongolian vowel separator"
        );
        assert_eq!(
            strip_hidden("a\u{2028}b\u{2029}c"),
            "abc",
            "line/paragraph separator"
        );
        // Unicode tag characters: invisible to a person, but a channel some LLMs have been shown
        // to read as smuggled instruction text — the prompt-injection concern this set exists for.
        let tagged = format!("hello{}{}", '\u{E0068}', '\u{E0069}');
        assert_eq!(strip_hidden(&tagged), "hello");
    }

    /// The hidden set, written out: `pitcrew_protocol::text::is_hidden`, which the recap engine and
    /// the CLI drop too. Pinned in each of them: change them together.
    const HIDDEN: &[(u32, u32)] = &[
        (0x00AD, 0x00AD),
        (0x034F, 0x034F),
        (0x061C, 0x061C),
        (0x115F, 0x1160),
        (0x180E, 0x180E),
        (0x200B, 0x200F),
        (0x2028, 0x2029),
        (0x202A, 0x202E),
        (0x2060, 0x2064),
        (0x2066, 0x2069),
        (0x3164, 0x3164),
        (0xFE00, 0xFE0F),
        (0xFEFF, 0xFEFF),
        (0xFFA0, 0xFFA0),
        (0xFFF9, 0xFFFB),
        (0xE0000, 0xE007F),
        (0xE0100, 0xE01EF),
    ];

    #[test]
    fn the_hidden_set_is_pinned() {
        let mut buf = [0u8; 4];
        for c in (0..=0x10_FFFFu32).filter_map(char::from_u32) {
            let listed = HIDDEN
                .iter()
                .any(|&(lo, hi)| (lo..=hi).contains(&u32::from(c)));
            let one = c.encode_utf8(&mut buf);
            assert_eq!(contains_hidden(one), listed, "U+{:04X}", u32::from(c));
            if listed {
                assert_eq!(
                    strip_hidden(&format!("a{c}b")),
                    "ab",
                    "U+{:04X}",
                    u32::from(c)
                );
            }
        }
    }

    #[test]
    fn strip_hidden_drops_the_shared_sets_additions() {
        // What the shared set added to this crate's own: the combining grapheme joiner, the
        // Hangul fillers, variation selectors (both blocks) and the interlinear annotation marks.
        for c in [
            '\u{034F}',
            '\u{115F}',
            '\u{1160}',
            '\u{3164}',
            '\u{FFA0}',
            '\u{FE00}',
            '\u{FE0F}',
            '\u{E0100}',
            '\u{E01EF}',
            '\u{FFF9}',
            '\u{FFFA}',
            '\u{FFFB}',
        ] {
            let title = format!("Fix{c} login");
            assert_eq!(strip_hidden(&title), "Fix login", "U+{:04X}", u32::from(c));
            assert_eq!(cap_chars(&title, 9), "Fix login", "U+{:04X}", u32::from(c));
            assert!(contains_hidden(&title), "U+{:04X}", u32::from(c));
        }
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
