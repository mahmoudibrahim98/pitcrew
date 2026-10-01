//! Fixture test for review round 1, blocking item 1, at the full `sync()` level (not just
//! `account_minute`'s own unit tests in `time.rs`): the Jira *site* here is configured for Tokyo
//! (`+0900`), while the searching *account* (from `/myself`) is `America/New_York`. The two
//! differ by 14 hours in January, including a date change — exactly the case an earlier version
//! of this crate got wrong by truncating the wire text instead of converting it. This proves nothing
//! is skipped and nothing is duplicated across two calls when the zones disagree.

mod support;

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_jira::deployment::JiraCloud;
use pitcrew_sync_jira::{SyncConfig, SyncState, UpstreamChange};
use support::*;

fn config() -> SyncConfig {
    SyncConfig {
        projects: vec![pitcrew_sync_jira::ProjectRef::new("DEMO").expect("valid")],
        auth: cloud_auth(),
        api_base: CLOUD_API_BASE.to_string(),
        site_base: SITE_BASE.to_string(),
        epic_link_field: None,
        now_unix: 2_000_000_000,
    }
}

#[tokio::test]
async fn a_site_zone_ahead_of_the_account_zone_skips_nothing_and_duplicates_nothing() {
    // DEMO-1 at 2026-01-02T03:04:00+0900 (Tokyo) is 2026-01-01T18:04:00Z, which is
    // 2026-01-01T13:04:00-05:00 in America/New York (no DST in January) — a different *day* in
    // the account's zone. The old (wrong) behaviour would have sliced the wire text as-is and
    // produced a cursor of "2026-01-02 03:04", in the wrong zone entirely.
    let page = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let jql1 = jql_for("DEMO", None);
    let url1 = cloud_search_url(&jql1, &page);
    let transport1 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "America/New_York"),
        ok(
            &url1,
            cloud_page(
                vec![issue_json(
                    "DEMO-1",
                    "Filed from the Tokyo office",
                    "new",
                    "2026-01-02T03:04:00.000+0900",
                )],
                None,
            ),
        ),
    ]);

    let outcome1 =
        pitcrew_sync_jira::sync::sync(SyncState::new(), &transport1, &JiraCloud, &config()).await;
    assert!(outcome1.errors.is_empty(), "{:?}", outcome1.errors);
    assert_eq!(transport1.remaining(), 0);
    assert_eq!(
        outcome1
            .changes
            .iter()
            .filter(|c| matches!(c, UpstreamChange::IssueCreated { .. }))
            .count(),
        1
    );

    let project = outcome1.state.projects.get("DEMO").expect("project state");
    assert_eq!(
        project.cursor.as_deref(),
        Some("2026-01-01 13:04"),
        "the cursor must be converted into the account's zone, not a truncation of the site's own \
         +0900 wire text (which would wrongly read \"2026-01-02 03:04\")"
    );

    // --- Second call: the cursor re-includes DEMO-1 at the exact boundary (a real Jira server
    // would, since `>=` is inclusive and DEMO-1's account-local minute is exactly the cursor), one
    // minute before a genuinely new DEMO-2. Nothing may be skipped (DEMO-2 must be seen) and
    // nothing may be duplicated (DEMO-1, unchanged, must not be reported again).
    let jql2 = jql_for("DEMO", Some("2026-01-01 13:04"));
    let url2 = cloud_search_url(&jql2, &page);
    let transport2 = ReplayTransport::from_exchanges(vec![ok(
        &url2,
        cloud_page(
            vec![
                issue_json(
                    "DEMO-1",
                    "Filed from the Tokyo office",
                    "new",
                    "2026-01-02T03:04:00.000+0900",
                ),
                issue_json(
                    "DEMO-2",
                    "A second issue, one minute later",
                    "new",
                    "2026-01-02T03:05:00.000+0900",
                ),
            ],
            None,
        ),
    )]);

    let outcome2 =
        pitcrew_sync_jira::sync::sync(outcome1.state, &transport2, &JiraCloud, &config()).await;
    assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
    assert_eq!(
        transport2.remaining(),
        0,
        "/myself must not be re-requested on the second call"
    );

    let created: Vec<&str> = outcome2
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::IssueCreated { source, .. } => Some(source.key.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        created,
        vec!["DEMO-2"],
        "DEMO-1 (unchanged, re-included only by the overlap boundary) must not be reported again, \
         and DEMO-2 must not be skipped: {:#?}",
        outcome2.changes
    );

    let project2 = outcome2.state.projects.get("DEMO").expect("project state");
    assert_eq!(project2.cursor.as_deref(), Some("2026-01-01 13:05"));
}
