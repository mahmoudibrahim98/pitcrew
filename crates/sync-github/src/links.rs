//! Linked issues: GitHub's "closing keywords" in a pull request body (`Closes #12`,
//! `Fixes owner/repo#34`, `Resolved: #5, #6`), parsed without a regex dependency.
//!
//! GitHub recognises `close`, `closes`, `closed`, `fix`, `fixes`, `fixed`, `resolve`, `resolves`
//! and `resolved`, each followed by one or more `#n` or `owner/repo#n` references, optionally
//! joined by commas or "and". This is a best-effort, whitespace-token scan: good enough to find
//! the references GitHub itself would link, not a full implementation of GitHub's markdown
//! parser.

use pitcrew_protocol::model::{ExternalRef, ExternalSystem};

const CLOSING_KEYWORDS: &[&str] = &[
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// Trims common sentence punctuation from both ends, keeping the characters a reference needs
/// (`#`, `/`, `-`, `_`, `.`).
fn trim_punct(s: &str) -> &str {
    s.trim_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ':' | ';' | '!' | '?' | '(' | ')' | '"' | '\''
        )
    })
}

fn is_closing_keyword(token: &str) -> bool {
    let word = trim_punct(token).to_ascii_lowercase();
    CLOSING_KEYWORDS.contains(&word.as_str())
}

/// Blanks out fenced code blocks (```` ```...``` ```` or `~~~...~~~`) and blockquoted lines
/// (starting with `>`, after optional leading whitespace), replacing each with an equal number of
/// blank lines so byte offsets in error messages elsewhere stay meaningful. A keyword GitHub
/// itself would not treat as a closing reference — because it only appears in quoted or code text
/// — must not be treated as one here either.
fn strip_quoted_and_code(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed_start = line.trim_start();
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && !trimmed_start.starts_with('>') {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Parses `#n` (using `default_repo`) or `owner/repo#n` from one trimmed token.
fn parse_ref(token: &str, default_repo: &str) -> Option<ExternalRef> {
    let token = trim_punct(token);
    if let Some(rest) = token.strip_prefix('#') {
        let n: u64 = rest.parse().ok()?;
        return Some(issue_ref(default_repo, n));
    }
    let hash = token.find('#')?;
    let (repo, rest) = token.split_at(hash);
    let n: u64 = rest[1..].parse().ok()?;
    if !repo.is_empty() && repo.matches('/').count() == 1 {
        return Some(issue_ref(repo, n));
    }
    None
}

fn issue_ref(owner_repo: &str, number: u64) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#{number}"),
        url: Some(format!("https://github.com/{owner_repo}/issues/{number}")),
    }
}

/// Finds every issue a pull request body closes. `default_repo` (`owner/repo`) is used for bare
/// `#n` references. Order is the order keywords appear in the body; duplicates are removed.
#[must_use]
pub fn linked_issues(body: &str, default_repo: &str) -> Vec<ExternalRef> {
    let prose = strip_quoted_and_code(body);
    let tokens: Vec<&str> = prose.split_whitespace().collect();
    let mut refs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, token) in tokens.iter().enumerate() {
        if !is_closing_keyword(token) {
            continue;
        }
        let mut j = i + 1;
        while let Some(&word) = tokens.get(j) {
            let trimmed = trim_punct(word);
            if trimmed.eq_ignore_ascii_case("and") {
                j += 1;
                continue;
            }
            let Some(r) = parse_ref(word, default_repo) else {
                break;
            };
            if seen.insert(r.key.clone()) {
                refs.push(r);
            }
            j += 1;
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const REPO: &str = "example-org/demo-repo";

    #[test]
    fn a_single_bare_reference() {
        let refs = linked_issues("Closes #12", REPO);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].key, "example-org/demo-repo#12");
    }

    #[test]
    fn a_cross_repo_reference() {
        let refs = linked_issues("Fixes other-org/other-repo#45.", REPO);
        assert_eq!(refs[0].key, "other-org/other-repo#45");
    }

    #[test]
    fn several_references_joined_by_and_and_commas() {
        let refs = linked_issues("This closed #1, #2 and #3 for good.", REPO);
        let keys: Vec<_> = refs.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "example-org/demo-repo#1",
                "example-org/demo-repo#2",
                "example-org/demo-repo#3"
            ]
        );
    }

    #[test]
    fn duplicates_are_removed() {
        let refs = linked_issues("Fixes #1. Also resolves #1 again.", REPO);
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn plain_text_has_no_references() {
        assert!(linked_issues("Just a description, no keywords here.", REPO).is_empty());
        assert!(linked_issues("See issue #12 for context.", REPO).is_empty());
    }

    #[test]
    fn a_keyword_with_no_following_reference_is_ignored() {
        assert!(linked_issues("This is closed.", REPO).is_empty());
    }

    #[test]
    fn a_keyword_inside_a_fenced_code_block_is_ignored() {
        let body = "Here is an example:\n```\nCloses #99\n```\nFixes #1";
        let refs = linked_issues(body, REPO);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].key, "example-org/demo-repo#1");
    }

    #[test]
    fn a_keyword_inside_a_tilde_fenced_code_block_is_ignored() {
        let body = "~~~\nCloses #99\n~~~\nFixes #1";
        let refs = linked_issues(body, REPO);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].key, "example-org/demo-repo#1");
    }

    #[test]
    fn a_keyword_inside_a_blockquote_is_ignored() {
        let body = "> Someone once said: Closes #99\nFixes #1";
        let refs = linked_issues(body, REPO);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].key, "example-org/demo-repo#1");
    }

    #[test]
    fn an_unclosed_fence_blanks_out_the_rest_of_the_body() {
        // Malformed markdown (a fence never closed) is treated conservatively: once inside a
        // fence, nothing after it is scanned, rather than guessing where it "should" have ended.
        let body = "```\nCloses #1";
        assert!(linked_issues(body, REPO).is_empty());
    }

    proptest! {
        /// Never panics on arbitrary text, and every reference it does return is well-shaped.
        #[test]
        fn never_panics_on_arbitrary_text(body in ".{0,500}") {
            let refs = linked_issues(&body, REPO);
            for r in refs {
                prop_assert!(r.key.contains('#'));
            }
        }
    }
}
