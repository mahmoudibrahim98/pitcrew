//! GitHub timestamps.
//!
//! GitHub prints every timestamp this crate reads (`updated_at`, `created_at`, `merged_at`) as
//! RFC 3339 UTC, always to the second, always `Z`-suffixed: `2026-01-02T03:04:05Z`. Rather than
//! parse these into an epoch (which would need a date/time dependency this crate does not have),
//! [`GithubTimestamp`] keeps the text as-is: normalised like this, RFC 3339 strings compare
//! lexicographically in the same order as the instants they name, which is all a `since` cursor
//! or a "what changed most recently" comparison needs.

use serde::{Deserialize, Serialize};

/// An upstream timestamp, kept as GitHub's own RFC 3339 text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GithubTimestamp(pub String);

impl GithubTimestamp {
    /// Wraps a raw value without checking it.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The text, as GitHub sent it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the text has GitHub's normal shape: `YYYY-MM-DDTHH:MM:SSZ`. Comparisons only give
    /// the right answer across values of the same shape, so callers can use this to reject a
    /// surprising value rather than silently mis-order it.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let b = self.0.as_bytes();
        let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
        b.len() == 20
            && (0..4).all(digit)
            && b[4] == b'-'
            && (5..7).all(digit)
            && b[7] == b'-'
            && (8..10).all(digit)
            && b[10] == b'T'
            && (11..13).all(digit)
            && b[13] == b':'
            && (14..16).all(digit)
            && b[16] == b':'
            && (17..19).all(digit)
            && b[19] == b'Z'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_timestamps_are_accepted() {
        assert!(GithubTimestamp::new("2026-01-02T03:04:05Z").is_well_formed());
        assert!(!GithubTimestamp::new("2026-01-02T03:04:05.000Z").is_well_formed());
        assert!(!GithubTimestamp::new("2026-01-02 03:04:05Z").is_well_formed());
        assert!(!GithubTimestamp::new("").is_well_formed());
    }

    #[test]
    fn lexicographic_order_matches_chronological_order() {
        let a = GithubTimestamp::new("2026-01-02T03:04:05Z");
        let b = GithubTimestamp::new("2026-01-02T03:04:06Z");
        let c = GithubTimestamp::new("2026-01-03T00:00:00Z");
        assert!(a < b);
        assert!(b < c);
    }
}
