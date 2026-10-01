//! A same-origin, under-the-API-base check for server-supplied URLs.
//!
//! The only server-supplied URL this crate ever follows is a `Link: rel="next"` pagination
//! header (see [`crate::client::GithubClient::list`]), and the `Authorization` header is attached
//! to *every* request the client sends, including that one. A hostile server — or a proxy sitting
//! in front of a GitHub Enterprise Server instance — could answer with a `Link` pointing anywhere
//! and collect the token, or walk the request to an unintended path. [`trusted_next_url`] is the
//! gate: a `next` URL is followed only when it has the same scheme, host and effective port as
//! the configured API base, with a path that stays under the API base's own path.
//!
//! **Round 3 review, blocking item B-1:** an earlier version of this check (round 2) compared
//! `next` against the *exact* path of the request that returned it, reasoning that GitHub's own
//! pagination never changes the path. That reasoning was wrong: GitHub rewrites the path in the
//! very first `next` link for several list endpoints. `GET /repos/{owner}/{repo}/pulls` answers
//! with `Link: <https://api.github.com/repositories/724712/pulls?...&page=2>; rel="next"` — a
//! numeric-id alias path, not the `owner/repo` one the request used — and issues and milestones do
//! the same. The exact-path rule rejected this real, legitimate link outright: pull requests never
//! advanced past page 1, and issues/milestones were capped at one page's worth every sync. The
//! check is restored to comparing against the configured **API base**, not the previous request,
//! with "under its path" rather than "identical path" — this crate's original (round 1) model,
//! now implemented with a real URL parser instead of a hand-rolled one (see below) so it cannot
//! disagree with how the URL would actually be dispatched.
//!
//! This is implemented with the `url` crate — a real WHATWG URL parser, the same kind of parser a
//! real HTTP transport (`reqwest`, `ureq`, a browser) uses to decide where a request actually
//! goes. A hand-rolled parser was tried first (round 1) and closed the straightforward `..`/`%2e`
//! cases, but round 2's review (R8, R9) found it still disagreed with WHATWG on two points a real
//! transport does not: for a "special" scheme like `https`, a backslash (`\`) is *also* a path
//! separator, not just `/`; and userinfo (`user:pass@host`) can sit before the real host, so a URL
//! that merely *contains* the trusted hostname can still resolve somewhere else entirely —
//! `https://attacker.example\@api.github.com/x` resolves (WHATWG) to host `attacker.example`, path
//! `/@api.github.com/x`, not to `api.github.com` at all. Parsing with the same kind of parser a
//! real transport uses, then comparing the *parsed* origin and path, closes both — and, since
//! WHATWG path parsing already resolves `.`/`..`/percent-encoded-dot segments before `Url::path()`
//! is ever read, this crate no longer needs its own dot-segment walker to compare paths safely.

use url::Url;

/// Whether `raw` — the *undecoded* `next` URL text, exactly as the `Link` header sent it —
/// contains a byte this check refuses outright, regardless of what parsing it produces:
///
/// - `\`: a path separator for "special" schemes (`http`, `https`, ...) under WHATWG, the same as
///   `/`. The parsed-origin comparison below already catches anything this could be used for (the
///   parsed host or path would no longer match), but refusing it outright is a second, independent
///   line of defense that does not depend on getting every detail of URL parsing exactly right.
/// - `@`: introduces userinfo, which can make a URL's *host* something other than what it looks
///   like at a glance (`https://attacker.example\@api.github.com/...` — see the module doc).
///   Checked again below via the parsed URL's own `username`/`password`, but refused here too, on
///   the raw text, as the same kind of second line of defense.
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

/// Whether `path` is `base`, or a path segment under it, comparing WHATWG-normalised path
/// segments strictly — an empty segment counts, so `/api//v3/x` and `//api/v3/x` are never under
/// `/api/v3` (round 3 finding R31: the previous version dropped empty segments before comparing,
/// so a server, or a proxy, that merges repeated slashes could send a `next` link an actual
/// WHATWG-driven transport would also treat as under the base, while a stricter proxy in front of
/// it might route the raw, unmerged path somewhere else entirely — the same kind of escape R9
/// closed for `.`/`..` segments). Not a bare string prefix either, so a path can't be smuggled
/// past the check by appending characters right after the base (`/api/v3evil` is not under
/// `/api/v3`).
///
/// `base`'s own single trailing slash, if it has one, is dropped first: a configured API base of
/// `.../api/v3/` means "this directory and everything under it", the same as `.../api/v3` — not
/// one more empty segment every path must also carry right after `v3`. This is the only place
/// empty segments are ever ignored; an empty segment anywhere else in `path` still has to match
/// `base` segment-for-segment. `Url::path()` has already resolved `.`/`..` (including the
/// percent-encoded and backslash-separated spellings — see the module doc) by the time this ever
/// sees it, so no further dot-segment walking is needed here.
fn path_is_under(path: &str, base: &str) -> bool {
    let path_segs: Vec<&str> = path.split('/').collect();
    let base_segs: Vec<&str> = base.strip_suffix('/').unwrap_or(base).split('/').collect();
    path_segs.len() >= base_segs.len() && path_segs[..base_segs.len()] == base_segs[..]
}

/// Whether `next` is safe to follow as this client's next pagination request, given the
/// configured `api_base`. On success, returns the *parsed* URL — the caller sends that (`.as_str()`),
/// not the original `next` text, so the request actually made can never diverge from what this
/// check approved (round 3 review item S-2).
///
/// `next` is trusted when it has the same scheme, host and effective port as `api_base`, with a
/// path that stays under `api_base`'s own path (see [`path_is_under`]) — not merely within the
/// exact path of whichever request returned it: see the module doc for why that stricter rule
/// (round 2) broke real GitHub pagination.
///
/// Also rejects, independent of the comparison above (see [`has_forbidden_raw_bytes`] for the
/// first three): `next` with embedded userinfo, or a fragment. Malformed input (either side fails
/// to parse as an absolute URL) is untrusted: this returns `None`, never panics.
///
/// `pub` (rather than `pub(crate)`) and `#[doc(hidden)]` only so stream Q's fuzz harness can call
/// this directly instead of only reaching it indirectly through `sync::sync` — **not public API**:
/// it may change shape or disappear without notice, and no caller outside this crate's own fuzz
/// targets should depend on it.
#[doc(hidden)]
#[must_use]
pub fn trusted_next_url(next: &str, api_base: &str) -> Option<Url> {
    if has_forbidden_raw_bytes(next) {
        return None;
    }
    let n = Url::parse(next).ok()?;
    let b = Url::parse(api_base).ok()?;
    if !n.username().is_empty() || n.password().is_some() || n.fragment().is_some() {
        return None;
    }
    let same_origin = n.scheme() == b.scheme()
        && n.host_str() == b.host_str()
        && effective_port(&n) == effective_port(&b);
    (same_origin && path_is_under(n.path(), b.path())).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The configured API base for GitHub.com — no path of its own, so *any* path on the same
    /// host is "under" it.
    const BASE: &str = "https://api.github.com";
    /// A GitHub Enterprise Server API base, which does have a path prefix every request (and every
    /// trusted `next`) must stay under.
    const ENTERPRISE_BASE: &str = "https://ghe.example.com/api/v3";

    #[test]
    fn same_host_and_scheme_is_trusted() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_some()
        );
    }

    // --- Round 3 review item B-1: GitHub rewrites the path in several list endpoints' very first
    // `next` link, to a numeric-id "/repositories/<id>/..." alias rather than the "owner/repo" path
    // the request used. This must be trusted on GitHub.com (no API base path to stay under) and on
    // GHES as long as it stays under the configured API base path. ---

    #[test]
    fn b1_a_pull_requests_next_link_rewritten_to_the_repositories_id_path_is_trusted() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repositories/724712/pulls?state=all&page=2",
                BASE
            )
            .is_some()
        );
    }

    #[test]
    fn b1_an_issues_next_link_rewritten_to_the_repositories_id_path_is_trusted() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repositories/724712/issues?state=all&page=2",
                BASE
            )
            .is_some()
        );
    }

    #[test]
    fn b1_a_milestones_next_link_rewritten_to_the_repositories_id_path_is_trusted() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repositories/724712/milestones?state=all&page=2",
                BASE
            )
            .is_some()
        );
    }

    #[test]
    fn b1_the_same_rewrite_is_trusted_on_ghes_as_long_as_it_stays_under_the_api_base_path() {
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/repositories/724712/pulls?state=all&page=2",
                ENTERPRISE_BASE
            )
            .is_some()
        );
    }

    #[test]
    fn b1_a_repositories_id_path_that_drops_the_ghes_api_base_prefix_is_untrusted() {
        // If a GHES deployment ever rewrote to a path with no "/api/v3" prefix at all, that would
        // no longer be under the configured API base, and must still be rejected.
        assert!(
            trusted_next_url(
                "https://ghe.example.com/repositories/724712/pulls?state=all&page=2",
                ENTERPRISE_BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_different_host_is_never_trusted() {
        assert!(
            trusted_next_url(
                "https://attacker.example/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_host_merely_containing_the_real_one_is_never_trusted() {
        assert!(
            trusted_next_url(
                "https://api.github.com.attacker.example/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_none()
        );
        assert!(
            trusted_next_url(
                "https://attacker-api.github.com/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_different_scheme_is_never_trusted() {
        assert!(
            trusted_next_url(
                "http://api.github.com/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_different_port_is_never_trusted() {
        assert!(
            trusted_next_url(
                "https://api.github.com:8443/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn an_explicit_default_port_still_matches() {
        assert!(
            trusted_next_url(
                "https://api.github.com:443/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_some()
        );
    }

    #[test]
    fn ghes_paths_must_stay_under_the_api_base_path() {
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/repos/example-org/demo-repo/issues?page=2",
                ENTERPRISE_BASE
            )
            .is_some()
        );
        assert!(trusted_next_url(ENTERPRISE_BASE, ENTERPRISE_BASE).is_some());
    }

    #[test]
    fn a_path_merely_prefixed_by_the_base_is_not_under_it() {
        // "/api/v3evil/..." starts with the string "/api/v3" but is not a path segment under it.
        assert!(
            trusted_next_url("https://ghe.example.com/api/v3evil/repos", ENTERPRISE_BASE).is_none()
        );
    }

    #[test]
    fn a_sibling_path_outside_the_api_base_is_untrusted() {
        assert!(trusted_next_url("https://ghe.example.com/other", ENTERPRISE_BASE).is_none());
    }

    #[test]
    fn malformed_urls_are_never_trusted() {
        assert!(trusted_next_url("not a url", BASE).is_none());
        assert!(trusted_next_url("", BASE).is_none());
        assert!(trusted_next_url("https:///no-host", BASE).is_none());
    }

    // --- Round 2 review items R8 and R9: regressions Stream Q's fuzzer found. Still rejected
    // under the "under the API base" rule: all of these try to leave the base path (or the host)
    // entirely, which no amount of loosening "exact path" to "under base path" permits. ---

    #[test]
    fn r8_a_backslash_before_an_at_sign_resolves_to_the_attacker_host_and_is_rejected() {
        // WHATWG: `\` is a path separator for a special scheme, so this is parsed as host
        // `attacker.example`, path `/@api.github.com/repos/...` — not `api.github.com` at all.
        // Exactly Stream Q's r8-backslash-before-at regression.
        assert!(
            trusted_next_url(
                "https://attacker.example\\@api.github.com/repos/example-org/demo-repo/milestones?page=2",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn r9_plain_dot_dot_segments_leaving_the_api_base_path_are_rejected() {
        // Stream Q's r9-plain-dot-segments regression.
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/../../admin?page=2",
                ENTERPRISE_BASE
            )
            .is_none()
        );
    }

    #[test]
    fn r9_percent_encoded_dot_dot_segments_are_rejected() {
        // WHATWG's "double-dot path segment" is explicitly defined to also match the
        // percent-encoded spellings (`%2e.`, `.%2e`, `%2e%2e`), case-insensitively — this crate's
        // round-1 hand-rolled `decode_dot_escapes` existed only to approximate that one rule.
        // Stream Q's r9-encoded-dot-segments regression.
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/%2e%2e/%2e%2e/admin?page=2",
                ENTERPRISE_BASE
            )
            .is_none()
        );
    }

    #[test]
    fn r9_backslash_dot_segments_are_rejected() {
        // `\..\..\` — backslash acting as the path separator, each segment still a double-dot.
        // Stream Q's r9-backslash-dot-segments regression.
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/..\\..\\admin?page=2",
                ENTERPRISE_BASE
            )
            .is_none()
        );
    }

    #[test]
    fn r31_an_empty_segment_in_the_middle_of_the_next_links_path_is_not_under_the_api_base() {
        // Stream Q's open-r31-empty-segment-in-next-link regression: a server that merges
        // repeated slashes would treat `/api//v3/...` as `/api/v3/...`, but a stricter proxy in
        // front of it might route the raw, unmerged path somewhere else — the same kind of
        // divergence R9 closed for `.`/`..` segments.
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api//v3/repos/example-org/demo-repo/milestones?page=2",
                ENTERPRISE_BASE
            )
            .is_none()
        );
    }

    #[test]
    fn r31_a_leading_empty_segment_in_the_next_links_path_is_not_under_the_api_base() {
        // Stream Q's open-r31-leading-empty-segment-in-next-link regression.
        assert!(
            trusted_next_url(
                "https://ghe.example.com//api/v3/repos/example-org/demo-repo/milestones?page=2",
                ENTERPRISE_BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_trailing_slash_on_the_configured_api_base_still_trusts_a_legitimate_next_link() {
        // The R31 fix compares path segments strictly (empty ones included), except for exactly
        // one trailing slash on `base` itself, which means "this directory and everything under
        // it" — not one more empty segment every `next` link must also carry.
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/repos/example-org/demo-repo/milestones?page=2",
                "https://ghe.example.com/api/v3/"
            )
            .is_some()
        );
    }

    #[test]
    fn mixed_case_percent_encoded_dot_dot_is_rejected() {
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/%2E%2e/%2E%2e/admin?page=2",
                ENTERPRISE_BASE
            )
            .is_none()
        );
    }

    #[test]
    fn an_encoded_backslash_is_not_a_separator_and_so_is_just_an_extra_segment_under_the_base() {
        // `%5C` is *not* specially recognised as a separator by WHATWG (unlike `%2e%2e` for dot
        // segments) — it just becomes part of one long, literal path segment, "..%5C..%5Cadmin".
        // Under the "stay under the API base" rule (round 3), that is trusted: it is still one
        // extra segment appended *under* "/api/v3", exactly like "/api/v3/repos/..." would be —
        // following it risks a 404 from the real server at worst, never leaving the API's
        // authority. (It would have mattered under round 2's stricter "exact path" rule, which no
        // longer applies — see the module doc for why that rule was wrong.)
        assert!(
            trusted_next_url(
                "https://ghe.example.com/api/v3/..%5C..%5Cadmin?page=2",
                ENTERPRISE_BASE
            )
            .is_some()
        );
    }

    #[test]
    fn a_literal_backslash_anywhere_in_the_url_is_rejected_outright() {
        // Defense in depth (`has_forbidden_raw_bytes`): a `\` is refused on sight, independent of
        // where it sits or what the origin/path comparison alone would have concluded.
        assert!(
            trusted_next_url(
                "https://api.github.com/repos/example-org/demo-repo/issues?page=2\\",
                BASE
            )
            .is_none()
        );
        assert!(
            trusted_next_url(
                "https://api.github.com/repos\\/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_userinfo_form_is_rejected() {
        assert!(
            trusted_next_url(
                "https://user:pass@api.github.com/repos/example-org/demo-repo/issues?page=2",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_bare_at_sign_anywhere_is_rejected_outright() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repos/example-org/demo-repo/issues?page=2&q=@evil",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn a_fragment_is_rejected() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repos/example-org/demo-repo/issues?page=2#frag",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn control_characters_and_spaces_are_rejected() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repos/example-org/demo-repo/issues?page=2\u{0}",
                BASE
            )
            .is_none()
        );
        assert!(
            trusted_next_url(
                "https://api.github.com/repos/example-org/demo-repo/issues?page=2 ",
                BASE
            )
            .is_none()
        );
    }

    #[test]
    fn non_ascii_bytes_are_rejected() {
        assert!(
            trusted_next_url(
                "https://api.github.com/repos/example-org/demo-repo/issues?page=2&q=café",
                BASE
            )
            .is_none()
        );
    }
}
