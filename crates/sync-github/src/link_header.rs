//! `Link` header parsing (RFC 8288, as GitHub sends it), for pagination.
//!
//! GitHub sends something like:
//! `<https://api.github.com/repos/o/r/issues?page=2>; rel="next", <...>; rel="last"`

/// Parses a `Link` header value into `(rel, url)` pairs.
fn parse(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in value.split(',') {
        let Some((url_part, params)) = part.trim().split_once(';') else {
            continue;
        };
        let url = url_part
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        if url.is_empty() {
            continue;
        }
        for param in params.split(';') {
            let param = param.trim();
            if let Some(rel) = param.strip_prefix("rel=") {
                out.push((rel.trim_matches('"').to_string(), url.to_string()));
            }
        }
    }
    out
}

/// The `next` page's URL, if the header names one.
#[must_use]
pub fn next_link(value: &str) -> Option<String> {
    parse(value)
        .into_iter()
        .find(|(rel, _)| rel == "next")
        .map(|(_, url)| url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_next_among_several_rels() {
        let header = concat!(
            "<https://api.github.com/repos/example-org/demo-repo/issues?page=2>; rel=\"next\", ",
            "<https://api.github.com/repos/example-org/demo-repo/issues?page=5>; rel=\"last\""
        );
        assert_eq!(
            next_link(header).as_deref(),
            Some("https://api.github.com/repos/example-org/demo-repo/issues?page=2")
        );
    }

    #[test]
    fn no_next_gives_none() {
        let header =
            "<https://api.github.com/repos/example-org/demo-repo/issues?page=1>; rel=\"first\"";
        assert_eq!(next_link(header), None);
    }

    #[test]
    fn empty_header_gives_none() {
        assert_eq!(next_link(""), None);
    }
}
