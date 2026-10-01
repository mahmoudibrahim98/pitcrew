//! A same-origin, same-path check for server-supplied URLs.
//!
//! The only server-supplied URL this crate ever follows is a `Link: rel="next"` pagination
//! header (see [`crate::client::GithubClient::list`]), and the `Authorization` header is attached
//! to *every* request the client sends, including that one. A hostile server — or a proxy sitting
//! in front of a GitHub Enterprise Server instance — could answer with a `Link` pointing anywhere
//! and collect the token, or walk the request to an unintended path. [`is_trusted_next_url`] is
//! the gate: a `next` URL is followed only when it has the *exact* same scheme, host, port and
//! path as the request whose response carried it, differing only in its query — exactly what
//! GitHub's own pagination ever does.
//!
//! This is implemented with the `url` crate — a real WHATWG URL parser, the same kind of parser a
//! real HTTP transport (`reqwest`, `ureq`, a browser) uses to decide where a request actually
//! goes. A hand-rolled parser was tried first (round 1) and closed the straightforward `..`/`%2e`
//! cases, but round 2's review (R8, R9) found it still disagreed with WHATWG on two points a real
//! transport does not: for a "special" scheme like `https`, a backslash (`\`) is *also* a path
//! separator, not just `/`; and userinfo (`user:pass@host`) can sit before the real host, so a
//! URL that merely *contains* the trusted hostname can still resolve somewhere else entirely —
//! `https://attacker.example\@api.github.com/x` resolves (WHATWG) to host `attacker.example`,
//! path `/@api.github.com/x`, not to `api.github.com` at all. Parsing with the same kind of parser
//! a real transport uses, then comparing the *parsed* origin and path, closes both: this check can
//! never disagree with how the URL would actually be dispatched, because it uses the same rules.

use url::Url;

/// Whether `raw` — the *undecoded* `next` URL text, exactly as the `Link` header sent it —
/// contains a byte this check refuses outright, regardless of what parsing it produces:
///
/// - `\`: a path separator for "special" schemes (`http`, `https`, ...) under WHATWG, the same as
///   `/`. The exact-origin-and-path comparison below already catches anything this could be used
///   for (the parsed host or path would no longer match), but refusing it outright is a second,
///   independent line of defense that does not depend on getting every detail of URL parsing
///   exactly right.
/// - `@`: introduces userinfo, which can make a URL's *host* something other than what it looks
///   like at a glance (`https://attacker.example\@api.github.com/...` — see the module doc).
///   Checked again below via the parsed URL's own `username`/`password`, but refused here too,
///   on the raw text, as the same kind of second line of defense.
/// - control characters, a space, or a non-ASCII byte: never legitimately part of a GitHub
///   pagination URL, and a common source of parser-disagreement bugs in general.
fn has_forbidden_raw_bytes(raw: &str) -> bool {
    raw.bytes()
        .any(|b| matches!(b, b'\\' | b'@') || b.is_ascii_control() || b == b' ' || !b.is_ascii())
}

/// The effective (default-filled-in) port for a parsed URL, so `https://host/x` and
/// `https://host:443/x` compare equal.
fn effective_port(url: &Url) -> Option<u16> {
    url.port_or_known_default()
}

/// Whether `next` is safe to follow as this client's next pagination request, given
/// `previous_request_url` — the exact URL of the request whose response carried this `next` link
/// (either the first URL this `list()` call built itself, which is never server-supplied, or an
/// earlier `next` that already passed this same check).
///
/// The rule is deliberately strict and simple, per GitHub's own pagination contract (it only ever
/// changes the query string of the same path): `next` is trusted only when it has the *exact*
/// same scheme, host, port and path as `previous_request_url`. Merely staying "under" the API
/// base path (this crate's round-1 rule) is no longer enough — GitHub's own pagination never
/// produces a different path, so there is no legitimate reason to ever follow one, and the
/// narrower the check, the smaller the attack surface in front of the bearer token this request
/// carries.
///
/// Also rejects, independent of the comparison above (see [`has_forbidden_raw_bytes`] for the
/// first three): `next` with embedded userinfo, or a fragment. Malformed input (either side fails
/// to parse as an absolute URL) is untrusted: this returns `false`, never panics.
#[must_use]
pub(crate) fn is_trusted_next_url(next: &str, previous_request_url: &str) -> bool {
    if has_forbidden_raw_bytes(next) {
        return false;
    }
    let (Some(n), Some(p)) = (Url::parse(next).ok(), Url::parse(previous_request_url).ok()) else {
        return false;
    };
    if !n.username().is_empty() || n.password().is_some() || n.fragment().is_some() {
        return false;
    }
    n.scheme() == p.scheme()
        && n.host_str() == p.host_str()
        && effective_port(&n) == effective_port(&p)
        && n.path() == p.path()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://api.github.com/repos/example-org/demo-repo/issues?page=1";
    const ENTERPRISE_BASE: &str =
        "https://ghe.example.com/api/v3/repos/example-org/demo-repo/issues?page=1";

    #[test]
    fn same_scheme_host_port_and_path_with_a_different_query_is_trusted() {
        assert!(is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn a_different_host_is_never_trusted() {
        assert!(!is_trusted_next_url(
            "https://attacker.example/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn a_host_merely_containing_the_real_one_is_never_trusted() {
        assert!(!is_trusted_next_url(
            "https://api.github.com.attacker.example/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
        assert!(!is_trusted_next_url(
            "https://attacker-api.github.com/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn a_different_scheme_is_never_trusted() {
        assert!(!is_trusted_next_url(
            "http://api.github.com/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn http_vs_https_is_never_trusted_either_direction() {
        let http_base = "http://api.github.com/repos/example-org/demo-repo/issues?page=1";
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2",
            http_base
        ));
    }

    #[test]
    fn a_different_port_is_never_trusted() {
        assert!(!is_trusted_next_url(
            "https://api.github.com:8443/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn an_explicit_default_port_still_matches() {
        assert!(is_trusted_next_url(
            "https://api.github.com:443/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn a_different_path_is_never_trusted() {
        // GitHub's own pagination never changes the path — only round 1's looser "under the API
        // base" rule would have accepted this.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/repos/example-org/demo-repo/milestones?page=2",
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
    fn malformed_urls_are_never_trusted() {
        assert!(!is_trusted_next_url("not a url", BASE));
        assert!(!is_trusted_next_url("", BASE));
        assert!(!is_trusted_next_url("https:///no-host", BASE));
    }

    // --- Round 2 review items R8 and R9: regressions Stream Q's fuzzer found. ---

    #[test]
    fn r8_a_backslash_before_an_at_sign_resolves_to_the_attacker_host_and_is_rejected() {
        // WHATWG: `\` is a path separator for a special scheme, so this is parsed as host
        // `attacker.example`, path `/@api.github.com/repos/...` — not `api.github.com` at all.
        // Exactly Stream Q's r8-backslash-before-at regression.
        assert!(!is_trusted_next_url(
            "https://attacker.example\\@api.github.com/repos/example-org/demo-repo/milestones?page=2",
            BASE
        ));
    }

    #[test]
    fn r9_plain_dot_dot_segments_leaving_the_api_base_path_are_rejected() {
        // Stream Q's r9-plain-dot-segments regression.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/../../admin?page=2",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn r9_percent_encoded_dot_dot_segments_are_rejected() {
        // WHATWG's "double-dot path segment" is explicitly defined to also match the
        // percent-encoded spellings (`%2e.`, `.%2e`, `%2e%2e`), case-insensitively — this crate's
        // round-1 hand-rolled `decode_dot_escapes` existed only to approximate that one rule.
        // Stream Q's r9-encoded-dot-segments regression.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/%2e%2e/%2e%2e/admin?page=2",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn r9_backslash_dot_segments_are_rejected() {
        // `\..\..\` — backslash acting as the path separator, each segment still a double-dot.
        // Stream Q's r9-backslash-dot-segments regression.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/..\\..\\admin?page=2",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn mixed_case_percent_encoded_dot_dot_is_rejected() {
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/%2E%2e/%2E%2e/admin?page=2",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn an_encoded_backslash_is_not_a_separator_but_still_changes_the_path_and_is_rejected() {
        // `%5C` is *not* specially recognised as a separator by WHATWG (unlike `%2e%2e` for dot
        // segments) — it just becomes part of one long, literal path segment. That segment is
        // still not the previous request's path, so the exact-path check rejects it on its own
        // merits, with no special-casing needed for the encoded form.
        assert!(!is_trusted_next_url(
            "https://ghe.example.com/api/v3/..%5C..%5Cadmin?page=2",
            ENTERPRISE_BASE
        ));
    }

    #[test]
    fn a_literal_backslash_anywhere_in_the_url_is_rejected_outright() {
        // Defense in depth (`has_forbidden_raw_bytes`): a `\` is refused on sight, independent of
        // where it sits or what the exact-match comparison alone would have concluded.
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2\\",
            BASE
        ));
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos\\/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn a_userinfo_form_is_rejected() {
        assert!(!is_trusted_next_url(
            "https://user:pass@api.github.com/repos/example-org/demo-repo/issues?page=2",
            BASE
        ));
    }

    #[test]
    fn a_bare_at_sign_anywhere_is_rejected_outright() {
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2&q=@evil",
            BASE
        ));
    }

    #[test]
    fn a_fragment_is_rejected() {
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2#frag",
            BASE
        ));
    }

    #[test]
    fn control_characters_and_spaces_are_rejected() {
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2\u{0}",
            BASE
        ));
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2 ",
            BASE
        ));
    }

    #[test]
    fn non_ascii_bytes_are_rejected() {
        assert!(!is_trusted_next_url(
            "https://api.github.com/repos/example-org/demo-repo/issues?page=2&q=café",
            BASE
        ));
    }
}
