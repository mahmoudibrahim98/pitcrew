//! Small text helpers shared by the parsers.

/// Cuts `s` to at most `max` characters, adding `…` when it cut something.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_owned(),
        Some((i, _)) => {
            let mut out = String::with_capacity(i + 3);
            out.push_str(&s[..i]);
            out.push('…');
            out
        }
    }
}

/// The first line of `s`, trimmed and cut to `max` characters.
pub(crate) fn first_line(s: &str, max: usize) -> String {
    truncate_chars(s.trim().lines().next().unwrap_or("").trim(), max)
}

/// `s` as a one-line title: whitespace collapsed, cut to `max` characters.
pub(crate) fn title(s: &str, max: usize) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if out.chars().count() > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    truncate_chars(&out, max)
}

/// Up to `lines` non-empty lines of `s`, cut to `max` characters in total.
pub(crate) fn summary(s: &str, lines: usize, max: usize) -> String {
    let joined = s
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .take(lines)
        .collect::<Vec<_>>()
        .join("\n");
    truncate_chars(&joined, max)
}

/// Lower-case hex, for carrying raw bytes in a JSON cursor.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

/// The inverse of [`to_hex`]; `None` if `s` is not valid hex.
pub(crate) fn from_hex(s: &str) -> Option<Vec<u8>> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    b.chunks_exact(2)
        .map(|p| Some((nibble(p[0])? << 4) | nibble(p[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate_chars("§§§§", 2), "§§…");
        assert_eq!(truncate_chars("ab", 2), "ab");
        assert_eq!(first_line("  hello\nworld", 10), "hello");
        assert_eq!(summary("a\n\n b \nc\nd", 3, 100), "a\n b\nc");
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 0xff, 0x10, 0xc2];
        assert_eq!(from_hex(&to_hex(&bytes)).as_deref(), Some(&bytes[..]));
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex("abc"), None);
    }
}
