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

/// Characters that change text direction or are invisible. Dropped from any upstream text this
/// crate stores or re-emits (round 2 review item R10), so a crafted title, body, label or name
/// cannot make a change — or anything downstream reading it, such as a summary or a UI list —
/// display differently from what it actually says, and so a run of Unicode tag characters cannot
/// smuggle text invisible to a person but readable by another LLM into a title, body or label (a
/// prompt-injection vector into whatever later reads these fields — round 3 review item S-6).
///
/// This is meant to be the *same* character set `pitcrew_recap::text::clean`'s own `is_hidden`
/// drops, reimplemented here rather than taken as a dependency on that crate (`pitcrew-recap`
/// depends on sync data flowing *up* to it, not the other way around, and this check is small
/// enough that duplicating it is cheaper than a new cross-stream dependency) — **when this set
/// changes, `pitcrew_recap::text::is_hidden` needs the identical change**, or the two diverge on
/// exactly the kind of input this exists to catch; that crate is owned by stream F, outside this
/// stream's path ownership. Round 3's addition here (tag characters, soft hyphen, the Mongolian
/// vowel separator, and the two line/paragraph separators) could not be mirrored there in that
/// same change, but `s/F/recap-hardening` (R33) has since made `pitcrew_recap::text::is_hidden`
/// match this set exactly — the two sets agree again as of this round.
fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'            // soft hyphen
            | '\u{061C}'      // Arabic letter mark
            | '\u{180E}'      // Mongolian vowel separator
            | '\u{200B}'..='\u{200F}' // zero-width space/joiners, LTR/RTL marks
            | '\u{2028}'..='\u{2029}' // line/paragraph separator
            | '\u{202A}'..='\u{202E}' // bidi embedding/override
            | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
            | '\u{2066}'..='\u{2069}' // bidi isolates
            | '\u{FEFF}'      // byte-order mark / zero-width no-break space
            | '\u{E0000}'..='\u{E007F}' // Unicode tag characters
    )
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
