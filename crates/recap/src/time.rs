//! Calendar days from timestamps, without a time-zone database: the caller gives a fixed offset.

use pitcrew_protocol::model::{Date, TimestampMs};

const DAY_MS: i64 = 86_400_000;
/// 9999-12-31T23:59:59.999Z. Timestamps are untrusted; later ones are treated as this.
const MAX_MS: i64 = 253_402_300_799_999;
/// The widest real UTC offset is 14 hours.
const MAX_OFFSET_MINUTES: i32 = 14 * 60;

/// The calendar date of `at` at a fixed UTC offset, in minutes (e.g. `120` for UTC+2). Offsets are
/// clamped to ±14 hours and dates to the years 1970–9999, so any input gives a well-formed date.
#[must_use]
pub fn date_of(at: TimestampMs, utc_offset_minutes: i32) -> Date {
    let offset = utc_offset_minutes.clamp(-MAX_OFFSET_MINUTES, MAX_OFFSET_MINUTES);
    let local = at
        .saturating_add(i64::from(offset) * 60_000)
        .clamp(0, MAX_MS);
    let (y, m, d) = civil_from_days(local.div_euclid(DAY_MS));
    Date(format!("{y:04}-{m:02}-{d:02}"))
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day). This is Howard Hinnant's
/// `civil_from_days`; inputs here are always in 0..=2_932_896.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(date_of(0, 0).0, "1970-01-01");
        assert_eq!(date_of(1_790_668_800_000, 0).0, "2026-09-29");
        assert_eq!(date_of(1_790_755_200_000, 0).0, "2026-09-30");
        assert_eq!(date_of(951_782_400_000, 0).0, "2000-02-29");
        assert_eq!(date_of(1_709_164_800_000, 0).0, "2024-02-29");
        // 23:30 UTC on the 29th is the 30th at UTC+1.
        assert_eq!(date_of(1_790_724_600_000, 60).0, "2026-09-30");
        assert_eq!(date_of(1_790_724_600_000, -60).0, "2026-09-29");
    }

    #[test]
    fn extremes_are_well_formed() {
        for at in [i64::MIN, -1, i64::MAX, MAX_MS, MAX_MS + 1] {
            for off in [i32::MIN, 0, i32::MAX] {
                assert!(date_of(at, off).is_well_formed(), "{at} {off}");
            }
        }
        assert_eq!(date_of(i64::MAX, 0).0, "9999-12-31");
        assert_eq!(date_of(i64::MIN, 0).0, "1970-01-01");
    }
}
