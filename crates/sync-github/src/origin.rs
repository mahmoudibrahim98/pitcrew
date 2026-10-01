//! A same-origin check for server-supplied URLs.
//!
//! The only server-supplied URL this crate ever follows is a `Link: rel="next"` pagination
//! header (see [`crate::client::GithubClient::list`]), and the `Authorization` header is attached
//! to *every* request the client sends, including that one. A hostile server — or a proxy sitting
//! in front of a GitHub Enterprise Server instance — could answer with a `Link` pointing anywhere
//! and collect the token. [`is_trusted_next_url`] is the gate: a `next` URL is followed only when
//! its scheme, host and port match `api_base`'s, and its path stays under `api_base`'s own path.
//!
//! This crate has no URL-parsing dependency (see `transport.rs`'s module doc for the same
//! reasoning about an HTTP client), so this is a minimal, purpose-built parse: just enough to
//! compare two absolute URLs' origins and a path prefix, not a general URL parser.

/// `url`'s scheme, host, optional port, and path. Query strings and fragments are dropped (this
/// check never needs them), and IPv6 literal hosts are not specially handled (GitHub's API is
/// never addressed by one).
struct Origin<'a> {
    scheme: &'a str,
    host: &'a str,
    port: Option<&'a str>,
    path: &'a str,
}

fn parse(url: &str) -> Option<Origin<'_>> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, after) = rest.split_at(authority_end);
    // Drop userinfo (`user:pass@host`) if present; GitHub never sends one, but strip it rather
    // than mis-parse it as part of the host.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    let path = match after.find(['?', '#']) {
        Some(i) => &after[..i],
        None => after,
    };
    Some(Origin {
        scheme,
        host,
        port,
        path: if path.is_empty() { "/" } else { path },
    })
}

fn effective_port(scheme: &str, port: Option<&str>) -> String {
    if let Some(p) = port {
        return p.to_string();
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" => "443".to_string(),
        "http" => "80".to_string(),
        _ => String::new(),
    }
}

/// Decodes a `%2e`/`%2E` percent-escape to a literal `.`, case-insensitively, leaving every other
/// byte — encoded or not — untouched. **Not** a general percent-decoder: decoding `%2f` to `/`
/// here, for instance, would let an attacker smuggle an extra path separator past the segment
/// splitter below. This closes only the one encoding `.`/`..` could be smuggled through, so
/// `%2e%2e` (or a mix like `.%2e`) normalises exactly like a literal `..` would.
///
/// Safe to slice `segment` at the byte offsets this finds: `%2e`/`%2E` are three ASCII bytes, so
/// both ends of a match always fall on a UTF-8 character boundary, whatever non-ASCII bytes
/// surround them.
fn decode_dot_escapes(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = String::with_capacity(segment.len());
    let mut start = 0usize;
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        if bytes[i] == b'%' && bytes[i + 1] == b'2' && bytes[i + 2].eq_ignore_ascii_case(&b'e') {
            out.push_str(&segment[start..i]);
            out.push('.');
            i += 3;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&segment[start..]);
    out
}

/// Splits `path` on `/` and resolves `.` (dropped) and `..` (pops the previous segment)
/// segments, the way a browser or an HTTP server would before routing a request — after
/// [`decode_dot_escapes`] folds a percent-encoded `.` back to a literal one, so `%2e%2e` can't
/// walk past this check un-normalised. Returns `None` if a `..` has no segment left to pop —
/// escaping past the root — which this treats as never trusted rather than silently clamping it
/// to the root.
fn normalize_segments(path: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw_segment in path.split('/') {
        let segment = decode_dot_escapes(raw_segment);
        match segment.as_str() {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            _ => out.push(segment),
        }
    }
    Some(out)
}

/// Whether `path` is `base`, or a path segment under it, **after** resolving `.`/`..` segments in
/// both: `/api/v3` is under itself and under `/api/v3/repos/x`'s base `/api/v3`, but `/api/v3evil`
/// is not under `/api/v3`, and neither is `/api/v3/repos/../../evil` (it normalises to `/evil`).
/// The comparison is segment-aware, not a bare string prefix, so a path can't be smuggled past the
/// check by appending characters right after the base or by walking back out of it with `..`.
fn path_is_under(path: &str, base: &str) -> bool {
    let (Some(path_segs), Some(base_segs)) = (normalize_segments(path), normalize_segments(base))
    else {
        return false;
    };
    path_segs.len() >= base_segs.len() && path_segs[..base_segs.len()] == base_segs[..]
}

/// Whether `next` names the same scheme, host and port as `api_base`, with a path under
/// `api_base`'s own path. Malformed input (either side fails to parse as an absolute URL) is
/// untrusted: this returns `false`, never panics.
#[must_use]
pub(crate) fn is_trusted_next_url(next: &str, api_base: &str) -> bool {
    let (Some(n), Some(b)) = (parse(next), parse(api_base)) else {
        return false;
    };
    n.scheme.eq_ignore_ascii_case(b.scheme)
        && n.host.eq_ignore_ascii_case(b.host)
        && effective_port(n.scheme, n.port) == effective_port(b.scheme, b.port)
        && path_is_under(n.path, b.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://api.github.com";
    const ENTERPRISE_BASE: &str = "https://ghe.example.com/api/v3";

    #[test]
    fn same_host_and_scheme_is_trusted() {
        assert!(is_trusted_next_url(
            "https://api.github.com/repos/o/r/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn a_different_host_is_never_trusted() {
        assert!(!is_trusted_next_url("https://attacker.example/x", BASE));
    }

    #[test]
    fn a_host_merely_containing_the_real_one_is_never_trusted() {
        assert!(!is_trusted_next_url(
            "https://api.github.com.attacker.example/x",
            BASE
        ));
        assert!(!is_trusted_next_url(
            "https://attacker-api.github.com/x",
            BASE
        ));
    }

    #[test]
    fn a_different_scheme_is_never_trusted() {
        assert!(!is_trusted_next_url("http://api.github.com/x", BASE));
    }

    #[test]
    fn a_different_port_is_never_trusted() {
        assert!(!is_trusted_next_url("https://api.github.com:8443/x", BASE));
    }

    #[test]
    fn an_explicit_default_port_still_matches() {
        assert!(is_trusted_next_url("https://api.github.com:443/x", BASE));
    }

    #[test]
    fn ghes_paths_must_stay_under_the_api_base_path() {
        assert!(is_trusted_next_url(
            "https://ghe.example.com/api/v3/repos/o/r/issues?page=2",
            ENTERPRISE_BASE
        ));
        assert!(is_trusted_next_url(
            "https://ghe.example.com/api/v3",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn a_path_merely_prefixed_by_the_base_is_not_under_it() {
        // `/api/v3evil/...` starts with the string "/api/v3" but is not a path segment under it.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3evil/repos",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn a_sibling_path_outside_the_api_base_is_untrusted() {
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/other",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn dot_dot_segments_cannot_escape_the_api_base_path() {
        // String-prefix matching alone would accept this (it starts with "/api/v3/"), but it
        // normalises to "/api/evil" — a sibling of the base, not something under it.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/repos/../../evil",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn dot_dot_escaping_past_the_root_is_untrusted() {
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/../evil",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn dot_segments_normalise_away_without_affecting_trust() {
        assert!(is_trusted_next_url(
            "https://ghe.example.com/api/v3/./repos/o/r/issues?page=2",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn a_dot_dot_that_stays_under_the_base_is_still_trusted() {
        // Resolves to "/api/v3/repos/o/r/issues", genuinely under the base.
        assert!(is_trusted_next_url(
            "https://ghe.example.com/api/v3/repos/o/x/../r/issues",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn percent_encoded_dot_dot_cannot_escape_the_api_base_path() {
        // Lowercase %2e.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/repos/%2e%2e/%2e%2e/evil",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn mixed_case_percent_encoded_dot_dot_cannot_escape_the_api_base_path() {
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/repos/%2E%2e/%2E%2e/evil",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn a_literal_dot_plus_an_encoded_dot_is_still_dot_dot() {
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/repos/.%2e/.%2e/evil",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn a_double_slash_collapses_to_an_empty_segment_without_affecting_trust() {
        // "//" produces an empty segment between the slashes, which normalises away (like a
        // single "/") rather than being smuggled through as something else.
        assert!(is_trusted_next_url(
            "https://ghe.example.com/api/v3//repos/o/r/issues?page=2",
            ENTERPRISE_BASE
        ));
        assert!(!is_trusted_next_url(
            "https://ghe.example.com//other",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn malformed_urls_are_never_trusted() {
        assert!(!is_trusted_next_url("not a url", BASE));
        assert!(!is_trusted_next_url("", BASE));
        assert!(!is_trusted_next_url("https:///no-host", BASE));
    }
}
