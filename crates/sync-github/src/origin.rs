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

/// Whether `path` is `base`, or a path segment under it. `/api/v3` is under itself and under
/// `/api/v3/repos/x`'s base `/api/v3`, but `/api/v3evil` is not under `/api/v3`: the comparison is
/// segment-aware, not a bare string prefix, so a path can't be smuggled past the check by
/// appending characters right after the base.
fn path_is_under(path: &str, base: &str) -> bool {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        return true;
    }
    path == base || path.starts_with(&format!("{base}/"))
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
    fn malformed_urls_are_never_trusted() {
        assert!(!is_trusted_next_url("not a url", BASE));
        assert!(!is_trusted_next_url("", BASE));
        assert!(!is_trusted_next_url("https:///no-host", BASE));
    }
}
