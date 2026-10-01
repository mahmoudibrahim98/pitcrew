//! Fixture tests for rate-limit handling: the primary hourly quota (exhausted, with a reset
//! time) and a secondary (abuse-detection) limit honouring `retry-after`. Neither sleeps; both
//! return a `RateLimited` outcome instead, and no other resource is called afterwards.

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_github::{AuthToken, GithubTimestamp, RepoRef, Resource, SyncConfig, SyncState};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn config(now_unix: i64) -> SyncConfig {
    SyncConfig {
        repos: vec![RepoRef::new("example-org/demo-repo").expect("valid repo")],
        token: AuthToken::new("ghp_test_token_not_real"),
        now_unix,
        now: GithubTimestamp::new("2026-01-01T00:00:00Z"),
        api_base: None,
    }
}

#[tokio::test]
async fn primary_rate_limit_exhaustion_is_reported_with_the_reset_time() {
    let transport =
        ReplayTransport::load(fixture_path("rate_limit_primary.fixture")).expect("load");
    let outcome =
        pitcrew_sync_github::sync::sync(SyncState::new(), &transport, &config(2_000_000_000)).await;

    let rl = outcome.rate_limited.expect("rate limited");
    assert_eq!(rl.until, 2_000_003_600);
    assert!(!rl.secondary);
    assert!(outcome.changes.is_empty());
    assert!(outcome.errors.is_empty(), "a rate limit is not an error");
    // Only the one request (milestones) was ever sent; issues and pulls were never attempted.
    assert_eq!(transport.requests_sent().len(), 1);
}

#[tokio::test]
async fn secondary_rate_limit_honours_retry_after() {
    let transport =
        ReplayTransport::load(fixture_path("rate_limit_secondary.fixture")).expect("load");
    let now_unix = 2_000_000_000;
    let outcome =
        pitcrew_sync_github::sync::sync(SyncState::new(), &transport, &config(now_unix)).await;

    let rl = outcome.rate_limited.expect("rate limited");
    assert_eq!(rl.until, now_unix + 30);
    assert!(rl.secondary);
    assert!(outcome.changes.is_empty());
    assert_eq!(transport.requests_sent().len(), 1);
}

#[tokio::test]
async fn secondary_backoff_without_retry_after_grows_and_persists_in_state() {
    // Three consecutive secondary-limited responses, none with `retry-after`: the client must
    // back off exponentially and remember the attempt count in `SyncState` between calls, never
    // sleeping itself. The body names the secondary limit explicitly (as GitHub's own abuse-
    // detection responses do): without that signal (or a `retry-after`), a 403 is no longer
    // treated as a rate limit at all (review item 4) — see
    // `a_plain_403_with_no_rate_limit_signal_is_a_sync_issue`, below.
    let url = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";
    let body = r#"{"message":"You have exceeded a secondary rate limit. Please wait a few minutes before you try again."}"#;
    let fixture_text =
        format!("GET {url} HTTP/1.1\n\nHTTP/1.1 403\nX-RateLimit-Remaining: 5\n\n{body}\n");

    let mut state = SyncState::new();
    let mut deadlines = Vec::new();
    for attempt in 0..3 {
        let transport = ReplayTransport::from_text(&fixture_text).expect("parse");
        let outcome =
            pitcrew_sync_github::sync::sync(state, &transport, &config(2_000_000_000)).await;
        let rl = outcome
            .rate_limited
            .unwrap_or_else(|| panic!("attempt {attempt}: expected RateLimited"));
        assert!(rl.secondary);
        deadlines.push(rl.until);
        state = outcome.state;
    }
    // Strictly increasing: base * 2^0, base * 2^1, base * 2^2.
    assert!(deadlines[0] < deadlines[1], "{deadlines:?}");
    assert!(deadlines[1] < deadlines[2], "{deadlines:?}");
    assert_eq!(
        state
            .repos
            .get("example-org/demo-repo")
            .expect("repo")
            .secondary_backoff_attempts,
        3
    );
}

#[tokio::test]
async fn a_plain_403_with_no_rate_limit_signal_is_a_sync_issue_not_a_rate_limit() {
    // No `X-RateLimit-Remaining: 0` + reset, no `Retry-After`, and a body that names neither a
    // rate limit nor abuse detection: a revoked token or a missing scope looks exactly like this,
    // and must be visible to the caller rather than retried forever as if it would ever clear.
    // Unlike a rate limit (which stops the whole sync), a plain resource error does not: issues
    // and pull requests are still attempted, so both get a normal empty response here too.
    use pitcrew_sync_github::fixture::RecordedExchange;

    let milestones_url = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";
    let issues_url = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100";
    let pulls_url = "https://api.github.com/repos/example-org/demo-repo/pulls?state=all&sort=updated&direction=desc&per_page=100";

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

    let milestones_403 = RecordedExchange {
        method: "GET".to_string(),
        url: milestones_url.to_string(),
        request_headers: vec![],
        status: 403,
        response_headers: vec![],
        body: br#"{"message":"Bad credentials"}"#.to_vec(),
    };

    let transport = ReplayTransport::from_exchanges(vec![
        milestones_403,
        empty_list(issues_url),
        empty_list(pulls_url),
    ]);

    let outcome =
        pitcrew_sync_github::sync::sync(SyncState::new(), &transport, &config(2_000_000_000)).await;

    assert!(
        outcome.rate_limited.is_none(),
        "a bare 403 with no rate-limit signal must not be treated as a rate limit: {:?}",
        outcome.rate_limited
    );
    assert_eq!(outcome.errors.len(), 1, "{:#?}", outcome.errors);
    assert_eq!(outcome.errors[0].resource, Resource::Milestones);
    assert!(
        outcome.errors[0].message.contains("403"),
        "{}",
        outcome.errors[0].message
    );
}
