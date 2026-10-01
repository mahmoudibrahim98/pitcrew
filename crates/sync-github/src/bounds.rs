//! Bounds: caps that keep one sync call finite regardless of what the server sends.

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

/// Truncates `s` to at most `max` characters (not bytes), on a char boundary.
#[must_use]
pub fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Caps a list of labels: at most [`MAX_LABELS`] entries, each at most [`MAX_LABEL_CHARS`] long.
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
    fn cap_labels_bounds_count_and_length() {
        let labels: Vec<String> = (0..100).map(|i| format!("label-{i}")).collect();
        let capped = cap_labels(&labels);
        assert_eq!(capped.len(), MAX_LABELS);
        let long = vec!["x".repeat(500)];
        assert_eq!(cap_labels(&long)[0].len(), MAX_LABEL_CHARS);
    }
}
