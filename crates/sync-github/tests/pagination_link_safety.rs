//! Fixture test for review item 1: pagination must never follow a `Link: rel="next"` outside the
//! configured API base — doing so would send the `Authorization` header to whatever host answered
//! it. A fixture's first (trusted) page points `Link: rel="next"` at `https://attacker.example`;
//! the transport must never be asked for it, the trusted page's own items must still come through
//! normally, and the caller must see a `SyncIssue` about it (not just a silent log line).

use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};
use pitcrew_sync_github::{
    AuthToken, GithubTimestamp, RepoRef, Resource, SyncConfig, SyncState, UpstreamChange,
};

const MILESTONES_URL: &str = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";
const ISSUES_URL: &str = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100";
const PULLS_URL: &str = "https://api.github.com/repos/example-org/demo-repo/pulls?state=all&sort=updated&direction=desc&per_page=100";
const ATTACKER_URL: &str = "https://attacker.example/steal-the-token";

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
async fn an_untrusted_link_header_is_never_followed_and_is_reported() {
    let issues_page1 = RecordedExchange {
        method: "GET".to_string(),
        url: ISSUES_URL.to_string(),
        request_headers: vec![],
        status: 200,
        response_headers: vec![(
            "Link".to_string(),
            format!("<{ATTACKER_URL}>; rel=\"next\""),
        )],
        body: br#"[{
            "number": 1,
            "title": "Trusted issue",
            "body": "From the real page.",
            "state": "open",
            "updated_at": "2026-01-01T00:00:00Z",
            "html_url": "https://github.com/example-org/demo-repo/issues/1"
        }]"#
        .to_vec(),
    };

    let exchanges = vec![
        empty_list(MILESTONES_URL),
        issues_page1,
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

    assert!(outcome.rate_limited.is_none(), "{:?}", outcome.rate_limited);

    // The attacker host was never requested: only the three legitimate exchanges were consumed,
    // and none of the requests the transport actually saw name it.
    assert_eq!(
        transport.remaining(),
        0,
        "no fixture for the attacker host was consumed (none was provided)"
    );
    for request in transport.requests_sent() {
        assert!(
            !request.url.contains("attacker"),
            "a request was sent to the untrusted Link target: {}",
            request.url
        );
    }

    // The trusted page's own item was still read normally: an attacker's malformed pagination
    // response must not take down the data that WAS legitimately returned.
    let opened: Vec<_> = outcome
        .changes
        .iter()
        .filter(|c| matches!(c, UpstreamChange::IssueOpened { .. }))
        .collect();
    assert_eq!(opened.len(), 1, "{:#?}", outcome.changes);

    // And it is surfaced as a visible SyncIssue, not just a debug log: a server (or a proxy in
    // front of it) handing back a pagination link to an unexpected host is worth a person's
    // attention.
    assert_eq!(outcome.errors.len(), 1, "{:#?}", outcome.errors);
    let issue = &outcome.errors[0];
    assert_eq!(issue.resource, Resource::Issues);
    assert!(issue.message.contains(ATTACKER_URL), "{}", issue.message);
}
