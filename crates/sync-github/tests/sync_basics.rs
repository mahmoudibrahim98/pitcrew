//! Fixture tests for the main sync path: first full sync, then an incremental sync that exercises
//! pagination, a reopened issue, a milestone rename and a conditional 304, then a third call that
//! changes nothing (idempotence).

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_github::{
    AuthToken, CloseReason, GithubTimestamp, RepoRef, SyncConfig, SyncState, UpstreamChange,
};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn config(now_unix: i64, now: &str) -> SyncConfig {
    SyncConfig {
        repos: vec![RepoRef::new("example-org/demo-repo").expect("valid repo")],
        token: AuthToken::new("ghp_test_token_not_real"),
        now_unix,
        now: GithubTimestamp::new(now),
        api_base: None,
    }
}

fn find(
    changes: &[UpstreamChange],
    matches: impl Fn(&UpstreamChange) -> bool,
) -> Vec<&UpstreamChange> {
    changes.iter().filter(|c| matches(c)).collect()
}

#[tokio::test]
async fn first_full_sync_creates_everything_and_closes_what_is_already_closed() {
    let transport =
        ReplayTransport::load(fixture_path("first_sync.fixture")).expect("load fixture");
    let outcome = pitcrew_sync_github::sync::sync(
        SyncState::new(),
        &transport,
        &config(2_000_000_000, "2026-01-01T00:00:00Z"),
    )
    .await;

    assert!(outcome.rate_limited.is_none(), "{:?}", outcome.rate_limited);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(outcome.malformed_skipped, 0);
    assert_eq!(
        transport.remaining(),
        0,
        "every recorded exchange should have been used"
    );

    let opened = find(&outcome.changes, |c| {
        matches!(c, UpstreamChange::IssueOpened { .. })
    });
    assert_eq!(opened.len(), 3, "{:#?}", outcome.changes);

    let closed = find(&outcome.changes, |c| {
        matches!(c, UpstreamChange::IssueClosed { .. })
    });
    let reasons: Vec<CloseReason> = closed
        .iter()
        .map(|c| match c {
            UpstreamChange::IssueClosed { reason, .. } => *reason,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(reasons.len(), 2);
    assert!(reasons.contains(&CloseReason::Completed));
    assert!(reasons.contains(&CloseReason::NotPlanned));

    assert_eq!(
        find(&outcome.changes, |c| matches!(
            c,
            UpstreamChange::MilestoneCreated { .. }
        ))
        .len(),
        1
    );
    assert_eq!(
        find(&outcome.changes, |c| matches!(
            c,
            UpstreamChange::PullRequestOpened { .. }
        ))
        .len(),
        1
    );

    let merged = find(&outcome.changes, |c| {
        matches!(c, UpstreamChange::PullRequestMerged { .. })
    });
    assert_eq!(merged.len(), 1);
    match merged[0] {
        UpstreamChange::PullRequestMerged { closes, .. } => {
            assert_eq!(closes.len(), 1);
            assert_eq!(closes[0].key, "example-org/demo-repo#1");
        }
        _ => unreachable!(),
    }

    // State now has what the next call needs: cursors and ETags.
    let repo = outcome
        .state
        .repos
        .get("example-org/demo-repo")
        .expect("repo state");
    assert_eq!(
        repo.issues.since.as_ref().map(GithubTimestamp::as_str),
        Some("2026-01-01T00:10:00Z")
    );
    assert_eq!(repo.issues.etag.as_deref(), Some("\"issues-etag-1\""));
    assert_eq!(repo.pulls.etag.as_deref(), Some("\"pulls-etag-1\""));
    assert_eq!(repo.milestones.etag.as_deref(), Some("\"ms-etag-1\""));
    assert_eq!(repo.issue_snapshots.len(), 3);
    assert_eq!(repo.pull_snapshots.len(), 1);
    assert_eq!(repo.milestone_snapshots.len(), 1);

    // --- Second call: pagination, a reopen, a milestone rename, and a 304 for pull requests. ---
    let transport2 =
        ReplayTransport::load(fixture_path("second_sync.fixture")).expect("load fixture");
    let outcome2 = pitcrew_sync_github::sync::sync(
        outcome.state,
        &transport2,
        &config(2_000_010_000, "2026-01-01T01:00:00Z"),
    )
    .await;

    assert!(
        outcome2.rate_limited.is_none(),
        "{:?}",
        outcome2.rate_limited
    );
    assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
    assert_eq!(outcome2.malformed_skipped, 0);
    assert_eq!(
        transport2.remaining(),
        0,
        "both issue pages and the conditional pulls request should be used"
    );

    assert_eq!(
        find(&outcome2.changes, |c| matches!(
            c,
            UpstreamChange::IssueReopened { .. }
        ))
        .len(),
        1
    );
    assert_eq!(
        find(&outcome2.changes, |c| matches!(
            c,
            UpstreamChange::MilestoneRenamed { .. }
        ))
        .len(),
        1
    );
    // Pagination: one issue from page 1 (besides the reopen) plus one from page 2.
    let opened2 = find(&outcome2.changes, |c| {
        matches!(c, UpstreamChange::IssueOpened { .. })
    });
    assert_eq!(opened2.len(), 2, "{:#?}", outcome2.changes);

    let repo2 = outcome2
        .state
        .repos
        .get("example-org/demo-repo")
        .expect("repo state");
    assert_eq!(
        repo2.issues.since.as_ref().map(GithubTimestamp::as_str),
        Some("2026-01-01T01:10:00Z")
    );
    assert_eq!(
        repo2.issues.etag.as_deref(),
        Some("\"issues-etag-2\""),
        "page 1's ETag wins; page 2 sent none"
    );
    // A 304 keeps the old ETag.
    assert_eq!(repo2.pulls.etag.as_deref(), Some("\"pulls-etag-1\""));
    assert_eq!(repo2.issue_snapshots.len(), 5);

    // --- Third call: nothing changed upstream. Idempotence. ---
    let transport3 =
        ReplayTransport::load(fixture_path("no_change.fixture")).expect("load fixture");
    let state_before = outcome2.state;
    let outcome3 = pitcrew_sync_github::sync::sync(
        state_before.clone(),
        &transport3,
        &config(2_000_020_000, "2026-01-01T02:00:00Z"),
    )
    .await;
    assert!(outcome3.changes.is_empty(), "{:#?}", outcome3.changes);
    assert!(outcome3.errors.is_empty());
    assert_eq!(outcome3.malformed_skipped, 0);
    assert_eq!(
        outcome3.state, state_before,
        "a no-op sync must not perturb the state"
    );
}
