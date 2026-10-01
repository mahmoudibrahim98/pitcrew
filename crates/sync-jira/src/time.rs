//! Jira timestamps, and the account-zone-aware cursor built from them.
//!
//! Jira prints `updated`/`created` as `yyyy-MM-dd'T'HH:mm:ss.SSSZ` with a **numeric** zone offset
//! (`+0000`, `-0500`, …). That offset is the **site's** configured default time zone — Atlassian
//! documents this explicitly for both the v2 and v3 REST APIs — which is *not necessarily* the
//! time zone of the account making the request. JQL's own bare datetime literals
//! (`updated >= "2026-01-02 03:04"`), by contrast, are interpreted in the **searching account's**
//! own profile zone (read from `/myself`; see [`resolve_account_zone`]). An earlier version of
//! this module assumed the two were the same and just truncated the wire text; when the site zone
//! sits east of the account zone, that silently drops a trailing slice of the minute the cursor
//! landed on, and updates in that gap are never synced. [`account_minute`] is the fix: it parses
//! `updated` into an absolute instant and re-renders it in the account's own zone before handing
//! it to [`crate::jql::incremental_query`].

use jiff::Timestamp;
use jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use serde::{Deserialize, Serialize};

/// An upstream timestamp, kept as Jira's own text (with the *site's* offset, not the account's).
/// Still useful as-is for same-call ordering and for snapshot equality checks: every timestamp a
/// single Jira instance returns carries the same site offset, so lexicographic comparison between
/// two of them is still chronological order — it is only the *cursor* (crossing into a different
/// zone) that needs real conversion, which is what [`account_minute`] is for.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JiraTimestamp(pub String);

/// `strptime`'s format for Jira's shape: `%z` parses (and only parses) a bare numeric offset —
/// `[+-]HHMM`, no colon — which is exactly what both REST versions send.
const WIRE_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.f%z";

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

    /// Parses this text into an absolute instant, or `None` if it is not Jira's documented shape.
    #[must_use]
    pub fn to_instant(&self) -> Option<Timestamp> {
        Timestamp::strptime(WIRE_FORMAT, &self.0).ok()
    }

    /// Whether the text parses as a well-formed instant. Callers use this to reject a surprising
    /// value (treating the whole item as malformed) rather than silently mis-order it or build a
    /// bad cursor from it.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.to_instant().is_some()
    }
}

/// Looks up `name` (an IANA zone, e.g. `"America/New_York"`, as `/myself` reports it) in the
/// bundled zone database. If `name` is absent or unrecognised, falls back to a fixed UTC-12
/// offset — the most western offset in use — and logs the fallback once per process (never on
/// every call: a persistently-unresolvable zone would otherwise log on every single sync).
///
/// UTC-12 is safe specifically *because* it is extremal: for any real IANA zone Z and any
/// instant, Z's civil (wall-clock) rendering of that instant is never earlier than UTC-12's.
/// So a cursor built in UTC-12 is always at least as early as one built in the account's real
/// zone would have been — it can only cause redundant re-fetching of already-synced items
/// (harmless: diffing against the stored snapshot makes that a no-op), never a skip.
#[must_use]
pub fn resolve_account_zone(name: Option<&str>) -> TimeZone {
    if let Some(name) = name
        && let Ok(tz) = TimeZone::get(name)
    {
        return tz;
    }
    log_fallback_once(name);
    fallback_zone()
}

fn fallback_zone() -> TimeZone {
    Offset::constant(-12).to_time_zone()
}

static FALLBACK_LOGGED: std::sync::Once = std::sync::Once::new();

fn log_fallback_once(name: Option<&str>) {
    FALLBACK_LOGGED.call_once(|| {
        tracing::warn!(
            zone = ?name,
            "could not resolve the Jira account's time zone; falling back to a fixed UTC-12 \
             offset for the sync cursor (safe, but makes the cursor less precise)"
        );
    });
}

/// Renders `instant` as the `"YYYY-MM-DD HH:MM"` text JQL's date-time literals expect, in `zone`
/// (the searching account's own zone — see the module docs), floored to the minute.
///
/// If that floored civil minute is ambiguous in `zone` — a DST fold, where the same wall-clock
/// minute occurs twice — this steps the *civil* (wall-clock) value back by the width of the fold
/// before rendering, landing on a minute that occurs only once and is strictly before the entire
/// folded hour. This has to be civil arithmetic, not instant arithmetic: stepping the *instant*
/// back by the fold's width instead would simply walk from the fold's later occurrence to its
/// earlier occurrence of the exact same ambiguous civil minute (both are "01:30", say) — still
/// ambiguous text to hand to JQL. Walking the wall-clock number itself back by an hour instead
/// lands on "00:30", which happens only once, well before either candidate instant for "01:30".
/// JQL's own bare literal would otherwise force Jira to silently pick one of the two ambiguous
/// readings (undocumented which), and if it picked the *later* one, re-querying `>= cursor` next
/// time would skip everything in between. Stepping back trades a small amount of redundant
/// re-fetching (harmless: idempotent via snapshot diffing) for the guarantee that nothing in the
/// fold is ever skipped. The same handling also covers a `Gap` (spring-forward) defensively,
/// though a floored minute derived from a real instant — as this one always is — cannot actually
/// land in one in practice.
#[must_use]
pub fn account_minute(instant: Timestamp, zone: &TimeZone) -> String {
    let floored = floor_to_minute(zone.to_datetime(instant));
    let safe = match zone.to_ambiguous_timestamp(floored).offset() {
        AmbiguousOffset::Unambiguous { .. } => floored,
        AmbiguousOffset::Fold { before, after } | AmbiguousOffset::Gap { before, after } => {
            let shift = before.duration_since(after).abs();
            match floored.checked_sub(shift) {
                Ok(earlier) => floor_to_minute(earlier),
                // Underflow is only reachable within a fold/gap at the very start of the civil
                // calendar, which no real zone transition is anywhere near; fall back to the
                // unshifted minute rather than failing the whole sync over it.
                Err(_) => floored,
            }
        }
    };
    safe.strftime("%Y-%m-%d %H:%M").to_string()
}

fn floor_to_minute(dt: jiff::civil::DateTime) -> jiff::civil::DateTime {
    dt.with()
        .second(0)
        .millisecond(0)
        .microsecond(0)
        .nanosecond(0)
        .build()
        // Zeroing components already present in a valid `DateTime` cannot itself produce an
        // invalid one; this is defensive, not expected to trigger.
        .unwrap_or(dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> JiraTimestamp {
        JiraTimestamp::new(s)
    }

    #[test]
    fn well_formed_timestamps_are_accepted() {
        assert!(ts("2026-01-02T03:04:05.000+0000").is_well_formed());
        assert!(ts("2026-01-02T03:04:05.123-0500").is_well_formed());
    }

    #[test]
    fn malformed_timestamps_are_rejected() {
        assert!(!ts("").is_well_formed());
        assert!(
            !ts("2026-01-02 03:04:05.000+0000").is_well_formed(),
            "needs a 'T'"
        );
        assert!(!ts("not-a-timestamp").is_well_formed());
        assert!(!ts("2026-01-02T03:04:05.000+0000trailing").is_well_formed());
        assert!(
            !ts("2026-01-02T03:04:05.000").is_well_formed(),
            "no offset at all"
        );
    }

    #[test]
    fn lexicographic_order_matches_chronological_order_within_one_site_offset() {
        let a = ts("2026-01-02T03:04:05.000-0500");
        let b = ts("2026-01-02T03:04:06.000-0500");
        let c = ts("2026-01-03T00:00:00.000-0500");
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn account_minute_converts_across_offsets_not_just_truncates() {
        // The site is UTC+0900 (e.g. Tokyo); the account is America/New_York (UTC-5 in January,
        // no DST). 2026-01-02T03:04:00+0900 is 2026-01-01T13:04:00-0500 — a different *date* in
        // the account's zone. The old (wrong) behaviour, which just sliced the wire text, would
        // have produced "2026-01-02 03:04": a cursor in entirely the wrong zone.
        let instant = ts("2026-01-02T03:04:00.000+0900")
            .to_instant()
            .expect("parses");
        let zone = TimeZone::get("America/New_York").expect("bundled tzdb has this zone");
        assert_eq!(account_minute(instant, &zone), "2026-01-01 13:04");
    }

    #[test]
    fn account_minute_handles_the_reverse_direction_too() {
        // The site is America/New_York (UTC-5 in January); the account is Asia/Tokyo (UTC+9).
        let instant = ts("2026-01-01T13:04:00.000-0500")
            .to_instant()
            .expect("parses");
        let zone = TimeZone::get("Asia/Tokyo").expect("bundled tzdb has this zone");
        assert_eq!(account_minute(instant, &zone), "2026-01-02 03:04");
    }

    #[test]
    fn account_minute_floors_seconds_and_subseconds() {
        let instant = ts("2026-01-02T03:04:59.999+0000")
            .to_instant()
            .expect("parses");
        assert_eq!(account_minute(instant, &TimeZone::UTC), "2026-01-02 03:04");
    }

    #[test]
    fn account_minute_steps_back_across_a_dst_fold() {
        // America/New_York falls back from EDT (-04) to EST (-05) at 2026-11-01T02:00:00-04:00
        // (2:00 AM EDT, read back to 1:00 AM EST), i.e. 2026-11-01T06:00:00Z. The hour 01:00-01:59
        // local time occurs twice: once as EDT (05:00Z-05:59Z) and once as EST (06:00Z-06:59Z).
        // An instant landing in either occurrence must not produce a cursor that a `>=` query
        // could ever interpret as excluding the other (which would skip real updates in between).
        let zone = TimeZone::get("America/New_York").expect("bundled tzdb has this zone");
        // 2026-11-01T06:30:00Z = 2026-11-01T01:30 EST (the *second* occurrence of 01:30 local;
        // the first, as EDT, was an hour earlier at 05:30Z). Either occurrence must behave the
        // same way here: both format as the identical ambiguous civil text "01:30".
        let instant = ts("2026-11-01T06:30:00.000+0000")
            .to_instant()
            .expect("parses");
        let minute = account_minute(instant, &zone);
        // Stepped back by the fold's width (1 hour): must be strictly before the ambiguous
        // 01:30 local minute, so a re-query from this cursor is guaranteed to include it again
        // regardless of which offset Jira's JQL engine would have picked for "01:30".
        assert_eq!(minute, "2026-11-01 00:30");
    }

    #[test]
    fn unknown_zone_name_falls_back_to_utc_minus_twelve() {
        let zone = resolve_account_zone(Some("Not/A_Real_Zone"));
        let instant = ts("2026-01-02T00:00:00.000+0000")
            .to_instant()
            .expect("parses");
        // UTC-12 is 12 hours behind UTC, so 2026-01-02T00:00Z is still 2026-01-01 there.
        assert_eq!(account_minute(instant, &zone), "2026-01-01 12:00");
    }

    #[test]
    fn absent_zone_name_falls_back_to_utc_minus_twelve() {
        let zone = resolve_account_zone(None);
        let instant = ts("2026-01-02T00:00:00.000+0000")
            .to_instant()
            .expect("parses");
        assert_eq!(account_minute(instant, &zone), "2026-01-01 12:00");
    }

    #[test]
    fn a_recognised_zone_name_resolves_to_itself() {
        let zone = resolve_account_zone(Some("UTC"));
        assert_eq!(zone, TimeZone::UTC);
    }
}
