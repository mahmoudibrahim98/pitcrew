//! Jira timestamps.
//!
//! Jira prints `updated`/`created` as `yyyy-MM-dd'T'HH:mm:ss.SSSZ` with a **numeric** zone offset
//! (`+0000`, `-0500`, …) — Jira Cloud and Data Center both render it in the requesting account's
//! own time zone, not necessarily UTC. Like `pitcrew_sync_github::GithubTimestamp`, this keeps the
//! text as-is rather than parsing it into an epoch (which would need a date/time dependency this
//! crate does not have): within one account the offset is constant for most of the year, so RFC
//! 3339-shaped text still compares lexicographically close enough to chronological order for a
//! cursor — the one place that matters is the minute-granularity JQL comparison in
//! [`JiraTimestamp::to_jql_minute`], not a precise ordering of two instants.
//!
//! JQL's own datetime literals are coarser than this: `updated >= "2026-01-02 03:04"`, to the
//! minute, space-separated, no seconds and no zone (interpreted in the account's own zone — see
//! `/rest/api/3/myself`). Because that comparison is inclusive and minute-grained, re-querying
//! with the cursor set to the last-seen minute naturally re-fetches that whole minute again on
//! the next call — that repetition *is* the brief's "small overlap window", not something this
//! type computes separately.

use serde::{Deserialize, Serialize};

/// An upstream timestamp, kept as Jira's own text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JiraTimestamp(pub String);

impl JiraTimestamp {
    /// Wraps a raw value without checking it.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The text, as Jira sent it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the text has Jira's normal shape: `YYYY-MM-DDTHH:MM:SS.sss±HHMM`. Comparisons and
    /// [`to_jql_minute`](Self::to_jql_minute) only give the right answer for values of this shape,
    /// so callers use this to reject a surprising value (treating the whole item as malformed)
    /// rather than silently mis-order it or build a bad JQL literal from it.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        parse(&self.0).is_some()
    }

    /// The `"YYYY-MM-DD HH:MM"` text JQL's date-time literals expect: the date and hour:minute,
    /// space-separated, with the seconds, fractional seconds and zone offset dropped (JQL compares
    /// at minute granularity, in the account's own zone, which is exactly the precision Jira
    /// already rendered this text in). `None` if the text is not well-formed.
    #[must_use]
    pub fn to_jql_minute(&self) -> Option<String> {
        let p = parse(&self.0)?;
        Some(format!("{} {}", p.date, p.hour_minute))
    }
}

struct Parts<'a> {
    date: &'a str,
    hour_minute: &'a str,
}

/// A minimal, sequential parse of Jira's timestamp shape — not a general date/time parser, just
/// enough to validate the shape and slice out the date and hour:minute this crate needs. Written
/// by scanning forward rather than indexing fixed byte offsets, because (unlike GitHub's always-
/// three-digit-millisecond, always-`Z` shape) Jira's millisecond field length is not pinned down
/// here, and the zone is a signed numeric offset rather than a fixed literal.
fn parse(s: &str) -> Option<Parts<'_>> {
    let b = s.as_bytes();
    let digits = |from: usize, n: usize| {
        b.get(from..from + n)
            .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
    };
    // "YYYY-MM-DD"
    if !(digits(0, 4)
        && b.get(4) == Some(&b'-')
        && digits(5, 2)
        && b.get(7) == Some(&b'-')
        && digits(8, 2))
    {
        return None;
    }
    if b.get(10) != Some(&b'T') {
        return None;
    }
    // "HH:MM:SS"
    if !(digits(11, 2)
        && b.get(13) == Some(&b':')
        && digits(14, 2)
        && b.get(16) == Some(&b':')
        && digits(17, 2))
    {
        return None;
    }
    let mut i = 19;
    // Optional ".sss" (one or more digits).
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        let mut end = start;
        while b.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start {
            return None; // a lone "." with no digits
        }
        i = end;
    }
    // Zone: "+HHMM", "-HHMM", or "Z".
    match b.get(i) {
        Some(b'Z') if i + 1 == b.len() => {}
        Some(b'+' | b'-') if digits(i + 1, 4) && i + 5 == b.len() => {}
        _ => return None,
    }
    Some(Parts {
        date: &s[0..10],
        hour_minute: &s[11..16],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_timestamps_are_accepted() {
        assert!(JiraTimestamp::new("2026-01-02T03:04:05.000+0000").is_well_formed());
        assert!(JiraTimestamp::new("2026-01-02T03:04:05.123-0500").is_well_formed());
        assert!(JiraTimestamp::new("2026-01-02T03:04:05Z").is_well_formed());
        assert!(JiraTimestamp::new("2026-01-02T03:04:05.000Z").is_well_formed());
    }

    #[test]
    fn malformed_timestamps_are_rejected() {
        assert!(!JiraTimestamp::new("").is_well_formed());
        assert!(!JiraTimestamp::new("2026-01-02 03:04:05.000+0000").is_well_formed());
        assert!(!JiraTimestamp::new("2026-01-02T03:04:05.+0000").is_well_formed());
        assert!(!JiraTimestamp::new("2026-01-02T03:04:05.000+5").is_well_formed());
        assert!(!JiraTimestamp::new("not-a-timestamp").is_well_formed());
        assert!(!JiraTimestamp::new("2026-01-02T03:04:05.000+0000trailing").is_well_formed());
    }

    #[test]
    fn to_jql_minute_drops_seconds_millis_and_zone() {
        assert_eq!(
            JiraTimestamp::new("2026-01-02T03:04:05.123-0500").to_jql_minute(),
            Some("2026-01-02 03:04".to_string())
        );
    }

    #[test]
    fn to_jql_minute_is_none_for_malformed_input() {
        assert_eq!(JiraTimestamp::new("garbage").to_jql_minute(), None);
    }

    #[test]
    fn lexicographic_order_matches_chronological_order_within_one_zone() {
        let a = JiraTimestamp::new("2026-01-02T03:04:05.000-0500");
        let b = JiraTimestamp::new("2026-01-02T03:04:06.000-0500");
        let c = JiraTimestamp::new("2026-01-03T00:00:00.000-0500");
        assert!(a < b);
        assert!(b < c);
    }
}
