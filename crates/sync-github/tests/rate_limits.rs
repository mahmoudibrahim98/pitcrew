//! Fixture tests for rate-limit handling: the primary hourly quota (exhausted, with a reset
//! time) and a secondary (abuse-detection) limit honouring `retry-after`. Neither sleeps; both
//! return a `RateLimited` outcome instead, and no other resource is called afterwards.

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_github::{AuthToken, GithubTimestamp, RepoRef, SyncConfig, SyncState};

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
    // sleeping itself.
    let url = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";
    let fixture_text =
        format!("GET {url} HTTP/1.1\n\nHTTP/1.1 403\nX-RateLimit-Remaining: 5\n\n{{}}\n");

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
