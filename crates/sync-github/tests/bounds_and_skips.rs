//! Fixture tests for bounds: a malformed item is skipped (and counted) rather than failing the
//! whole page, and an oversized body is capped rather than stored in full.
//!
//! The oversized body needs a response many times [`MAX_BODY_CHARS`] long; rather than check in a
//! multi-hundred-kilobyte static fixture file, this builds that one response in code (still going
//! through the same [`ReplayTransport`] every other test uses) and keeps everything else recorded
//! the normal way.

use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};
use pitcrew_sync_github::{
    AuthToken, GithubTimestamp, RepoRef, SyncConfig, SyncState, UpstreamChange,
};

const MILESTONES_URL: &str = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";
const ISSUES_URL: &str = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100";
const PULLS_URL: &str = "https://api.github.com/repos/example-org/demo-repo/pulls?state=all&sort=updated&direction=desc&per_page=100";

fn empty_list(url: &str) -> RecordedExchange {
    RecordedExchange {
        method: "GET".to_string(),
        url: url.to_string(),
        request_headers: vec![],
        status: 200,
        response_headers: vec![],
        body: b"[]".to_vec(),
    }
}

#[tokio::test]
async fn a_malformed_item_is_skipped_and_an_oversized_body_is_capped() {
    let long_body = "x".repeat(pitcrew_sync_github::bounds::MAX_BODY_CHARS + 5_000);
    let issues = serde_json::json!([
        {
            "number": 1,
            "title": "Normal issue",
            "body": "Normal body.",
            "state": "open",
            "updated_at": "2026-01-01T00:00:00Z",
            "html_url": "https://github.com/example-org/demo-repo/issues/1",
        },
        "this element is not an issue object at all",
        {
            "number": 2,
            "title": "Oversized body issue",
            "body": long_body,
            "state": "open",
            "updated_at": "2026-01-01T00:01:00Z",
            "html_url": "https://github.com/example-org/demo-repo/issues/2",
        },
    ]);
    let exchanges = vec![
        empty_list(MILESTONES_URL),
        RecordedExchange {
            method: "GET".to_string(),
            url: ISSUES_URL.to_string(),
            request_headers: vec![],
            status: 200,
            response_headers: vec![],
            body: issues.to_string().into_bytes(),
        },
        empty_list(PULLS_URL),
    ];
    let transport = ReplayTransport::from_exchanges(exchanges);

    let config = SyncConfig {
        repos: vec![RepoRef::new("example-org/demo-repo").expect("valid repo")],
        token: AuthToken::new("ghp_test_token_not_real"),
        now_unix: 2_000_000_000,
        now: GithubTimestamp::new("2026-01-01T00:00:00Z"),
        api_base: None,
    };
    let outcome = pitcrew_sync_github::sync::sync(SyncState::new(), &transport, &config).await;

    assert!(outcome.rate_limited.is_none());
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(
        outcome.malformed_skipped, 1,
        "the bare string element does not parse as an issue"
    );

    let opened: Vec<(&str, &str)> = outcome
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::IssueOpened { source, body, .. } => {
                Some((source.key.as_str(), body.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        opened.len(),
        2,
        "the malformed element must not stop the valid ones from being read"
    );

    let (_, normal_body) = opened
        .iter()
        .find(|(k, _)| k.ends_with('1'))
        .expect("issue 1");
    assert_eq!(*normal_body, "Normal body.");

    let (_, capped_body) = opened
        .iter()
        .find(|(k, _)| k.ends_with('2'))
        .expect("issue 2");
    assert_eq!(
        capped_body.chars().count(),
        pitcrew_sync_github::bounds::MAX_BODY_CHARS,
        "the oversized body must be capped, not stored in full"
    );
}
