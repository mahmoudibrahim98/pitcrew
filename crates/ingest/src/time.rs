//! RFC 3339 timestamps to Unix milliseconds, without a calendar dependency.

use pitcrew_protocol::model::TimestampMs;

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`. Returns `None` for anything else.
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<TimestampMs> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year = digits(&b[0..4])?;
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let minute = digits(&b[14..16])?;
    let second = digits(&b[17..19])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    // Allow a leap second rather than rejecting the record.
    if second > 60 {
        return None;
    }

    let mut i = 19;
    let mut millis = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if i - start < 3 {
                millis = millis * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        if i == start {
            return None;
        }
        for _ in (i - start)..3 {
            millis *= 10;
        }
    }

    let offset_secs = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let h = digits(&b[i + 1..i + 3])?;
            let m = digits(&b[i + 4..i + 6])?;
            let secs = h * 3600 + m * 60;
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    Some(secs * 1000 + millis)
}

fn digits(b: &[u8]) -> Option<i64> {
    b.iter().try_fold(0i64, |acc, c| {
        c.is_ascii_digit().then(|| acc * 10 + i64::from(c - b'0'))
    })
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::parse_rfc3339_ms;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2026-09-30T08:00:00.000Z"),
            Some(1_790_755_200_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-09-30T08:00:09.5Z"),
            Some(1_790_755_209_500)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-09-30T10:00:00.123456+02:00"),
            Some(1_790_755_200_123)
        );
        assert_eq!(
            parse_rfc3339_ms("2000-02-29T00:00:00Z"),
            Some(951_782_400_000)
        );
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "",
            "2026-09-30",
            "2026-13-01T00:00:00Z",
            "2026-09-30T08:00:00",
            "x".repeat(40).as_str(),
        ] {
            assert_eq!(parse_rfc3339_ms(s), None, "{s}");
        }
    }
}
