//! Dates in UTC, for the read verbs: shown as `YYYY-MM-DD HH:MM`, and `--since` read as a day.
//! Civil dates from days since 1970-01-01 and back are Howard Hinnant's algorithms.

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

/// `(year, month, day)` of the `days`-th day since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` of a time in milliseconds since the epoch, in UTC.
#[must_use]
pub fn day(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(DAY_MS));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DD HH:MM` of a time in milliseconds since the epoch, in UTC.
#[must_use]
pub fn when(ms: i64) -> String {
    let minutes = ms.rem_euclid(DAY_MS) / 60_000;
    format!("{} {:02}:{:02}", day(ms), minutes / 60, minutes % 60)
}

/// The start of a day, in milliseconds: `today` (in UTC, at `now`) or `YYYY-MM-DD`.
///
/// # Errors
/// Anything else, as a sentence.
pub fn since(text: &str, now: i64) -> Result<i64, String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("today") {
        return Ok(now.div_euclid(DAY_MS) * DAY_MS);
    }
    let bad = || format!("{text:?} is not a day: use today or YYYY-MM-DD");
    let parts: Vec<&str> = text.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return Err(bad());
    };
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return Err(bad());
    }
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return Err(bad());
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    let days = days_from_civil(y, m, d);
    // 2026-02-31 is no day: it would come back as March.
    if civil_from_days(days) != (y, m, d) {
        return Err(bad());
    }
    Ok(days * DAY_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_times_in_utc() {
        assert_eq!(day(0), "1970-01-01");
        // 2026-09-30 10:00 UTC.
        assert_eq!(when(1_790_762_400_000), "2026-09-30 10:00");
        assert_eq!(since("2026-09-30", 0), Ok(1_790_726_400_000));
        assert_eq!(since(" today ", 1_790_762_400_000), Ok(1_790_726_400_000));
        for bad in ["yesterday", "2026-9-30", "2026-02-31", "2026-13-01", ""] {
            assert!(since(bad, 0).is_err(), "{bad:?}");
        }
        for ms in [0, 951_782_400_000, 1_790_762_400_000, 4_102_444_800_000] {
            let d = day(ms);
            assert_eq!(day(since(&d, 0).unwrap()), d);
        }
    }
}
