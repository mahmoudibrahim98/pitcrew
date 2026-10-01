//! An independent model of how a WHATWG URL parser (what `reqwest`, `ureq` and browsers use)
//! reads an absolute URL with a special scheme (`http`, `https`, `ws`, `wss`, `ftp`): the scheme,
//! whether there is userinfo, the host, the effective port and the path segments with `.` and
//! `..` resolved. It follows the URL Standard's state machine, written from the standard rather
//! than from the `url` crate, so a target can check code that uses `url` against something that
//! is not `url`.
//!
//! Only what the checks need: no IDNA (a host that decodes to anything but ASCII gives `None`),
//! IPv4 and IPv6 hosts kept as written (lower-cased), and no query or fragment parsing.

/// What a WHATWG parser makes of a URL, as far as where a request goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Lower-case.
    pub scheme: String,
    /// Whether the authority held an `@`.
    pub userinfo: bool,
    /// Lower-case, percent-decoded.
    pub host: String,
    /// The port, or the scheme's default.
    pub port: u16,
    /// The path's segments, `.` and `..` resolved; percent-escapes kept as written. An empty path
    /// is one empty segment, as the standard serialises it (`/`).
    pub segments: Vec<String>,
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

/// `..`, also as `.%2e`, `%2e.` or `%2e%2e` in any case.
fn is_double_dot(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        ".." | ".%2e" | "%2e." | "%2e%2e"
    )
}

/// `.`, also as `%2e` in any case.
fn is_single_dot(segment: &str) -> bool {
    matches!(segment.to_ascii_lowercase().as_str(), "." | "%2e")
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(&h), Some(&l)) = (b.get(i + 1), b.get(i + 2))
            && let (Some(h), Some(l)) = (hex(h), hex(l))
        {
            out.push(u8::try_from(h * 16 + l).unwrap_or(0));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// The URL Standard's forbidden domain code points, for ASCII.
fn forbidden_in_domain(c: u8) -> bool {
    c <= 0x20
        || c == 0x7F
        || matches!(
            c,
            b'#' | b'/'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b'\\'
                | b']'
                | b'^'
                | b'|'
                | b'%'
        )
}

/// Parses `input` as an absolute URL with a special scheme, or `None` where the standard fails
/// (or where this model does not follow it: another scheme, a host that is not ASCII once
/// decoded).
#[must_use]
pub fn parse(input: &str) -> Option<Parsed> {
    // Leading and trailing C0 controls and spaces go; tabs and newlines go everywhere.
    let trimmed = input.trim_matches(|c: char| c <= ' ');
    let text: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();

    let colon = text.find(':')?;
    let scheme = &text[..colon];
    let mut chars = scheme.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let default = default_port(&scheme)?;

    // Any number of slashes, of either kind, before the authority.
    let rest = text[colon + 1..].trim_start_matches(['/', '\\']);
    let end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
    let (authority, after) = rest.split_at(end);

    // The host starts after the last `@`.
    let (userinfo, host_port) = match authority.rfind('@') {
        Some(at) => (true, &authority[at + 1..]),
        None => (false, authority),
    };

    // The port starts at the first `:` outside brackets.
    let mut inside = false;
    let mut split = None;
    for (i, c) in host_port.char_indices() {
        match c {
            '[' => inside = true,
            ']' => inside = false,
            ':' if !inside => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (host, port) = match split {
        Some(i) => (&host_port[..i], Some(&host_port[i + 1..])),
        None => (host_port, None),
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        None | Some("") => default,
        Some(p) => {
            if !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let trimmed = p.trim_start_matches('0');
            if trimmed.len() > 5 {
                return None;
            }
            let n: u32 = if trimmed.is_empty() {
                0
            } else {
                trimmed.parse().ok()?
            };
            u16::try_from(n).ok()?
        }
    };

    let host = if host.starts_with('[') {
        if !host.ends_with(']') {
            return None;
        }
        host.to_ascii_lowercase()
    } else {
        let decoded = String::from_utf8(percent_decode(host)).ok()?;
        if !decoded.is_ascii() || decoded.is_empty() {
            return None;
        }
        if decoded.bytes().any(forbidden_in_domain) {
            return None;
        }
        decoded.to_ascii_lowercase()
    };

    // The path runs to the query or the fragment; `\` separates segments like `/`.
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    let path = &after[..path_end];
    let path = path
        .strip_prefix('/')
        .or_else(|| path.strip_prefix('\\'))
        .unwrap_or(path);
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let mut segments: Vec<String> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        if is_double_dot(part) {
            segments.pop();
            if last {
                segments.push(String::new());
            }
        } else if is_single_dot(part) {
            if last {
                segments.push(String::new());
            }
        } else {
            segments.push((*part).to_owned());
        }
    }
    if segments.is_empty() {
        segments.push(String::new());
    }

    Some(Parsed {
        scheme,
        userinfo,
        host,
        port,
        segments,
    })
}

impl Parsed {
    /// The segments a base URL's path puts every URL under it below: its own, without the
    /// trailing empty one a final `/` makes.
    #[must_use]
    pub fn base_segments(&self) -> &[String] {
        match self.segments.split_last() {
            Some((last, rest)) if last.is_empty() => rest,
            _ => &self.segments,
        }
    }

    /// Whether `self` goes to `base`'s origin, at or below its path, segment by segment (an
    /// empty segment counts: `/api//v3` is not under `/api/v3`).
    #[must_use]
    pub fn is_under(&self, base: &Parsed) -> bool {
        self.same_origin(base) && self.segments.starts_with(base.base_segments())
    }

    /// The same scheme, host and port.
    #[must_use]
    pub fn same_origin(&self, other: &Parsed) -> bool {
        self.scheme == other.scheme && self.host == other.host && self.port == other.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(url: &str) -> Vec<String> {
        parse(url).expect(url).segments
    }

    #[test]
    fn reads_like_the_standard() {
        let p = parse("HTTPS://Api.GitHub.com:443/a/b?x#y").unwrap();
        assert_eq!(
            (p.scheme.as_str(), p.host.as_str(), p.port),
            ("https", "api.github.com", 443)
        );
        assert_eq!(p.segments, ["a", "b"]);
        let p = parse("https://attacker.example\\@api.github.com/x").unwrap();
        assert_eq!(p.host, "attacker.example");
        assert!(!p.userinfo);
        let p = parse("https://a@b@host/x").unwrap();
        assert!(p.userinfo);
        assert_eq!(p.host, "host");
        assert_eq!(segs("https://h/api/v3/%2e%2E/x"), ["api", "x"]);
        assert_eq!(segs("https://h/api/v3/..\\..\\x"), ["x"]);
        assert_eq!(segs("https://h/a/.."), [""]);
        assert_eq!(segs("https://h"), [""]);
        assert_eq!(segs("https:h/a"), ["a"]);
        assert_eq!(segs("https:///\\\\h/a//b"), ["a", "", "b"]);
        assert_eq!(parse("https://h:08443/").unwrap().port, 8443);
        assert!(parse("https://h:65536/").is_none());
        assert!(parse("https://h:1a/").is_none());
        assert!(parse("https://%41pi.example/").is_some_and(|p| p.host == "api.example"));
        assert!(parse("https://a%2fb/").is_none());
        assert!(parse("mailto:x@y").is_none());
        assert!(parse("https://@/x").is_none());
    }

    #[test]
    fn under_counts_empty_segments() {
        let base = parse("https://h/api/v3/").unwrap();
        assert!(parse("https://h/api/v3/x").unwrap().is_under(&base));
        assert!(parse("https://h/api/v3").unwrap().is_under(&base));
        assert!(!parse("https://h/api//v3/x").unwrap().is_under(&base));
        assert!(!parse("https://h//api/v3/x").unwrap().is_under(&base));
        assert!(!parse("https://h:444/api/v3/x").unwrap().is_under(&base));
    }
}
