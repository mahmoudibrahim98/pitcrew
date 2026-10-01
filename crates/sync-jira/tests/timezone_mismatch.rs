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
        Some("2026-01-01T18:04:00Z"),
        "the stored cursor is the real instant (2026-01-02T03:04+0900 converted to UTC), not a \
         truncation of the site's own +0900 wire text"
    );

    // --- Second call: the (margin-adjusted, zone-rendered) cursor re-includes DEMO-1, one minute
    // before a genuinely new DEMO-2. Nothing may be skipped (DEMO-2 must be seen) and nothing may
    // be duplicated (DEMO-1, unchanged, must not be reported again).
    let jql2 = jql_for(
        "DEMO",
        query_cursor_text(project.cursor.as_deref(), "America/New_York").as_deref(),
    );
    let url2 = cloud_search_url(&jql2, &page);
    let transport2 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "America/New_York"),
        ok(
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
        ),
    ]);

    let outcome2 =
        pitcrew_sync_jira::sync::sync(outcome1.state, &transport2, &JiraCloud, &config()).await;
    assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
    assert_eq!(
        transport2.remaining(),
        0,
        "/myself (re-requested) and the search should both be used"
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
        "DEMO-1 (unchanged, re-included only by the overlap window) must not be reported again, \
         and DEMO-2 must not be skipped: {:#?}",
        outcome2.changes
    );

    let project2 = outcome2.state.projects.get("DEMO").expect("project state");
    assert_eq!(project2.cursor.as_deref(), Some("2026-01-01T18:05:00Z"));
}

#[tokio::test]
async fn the_account_zone_moving_east_between_syncs_skips_nothing() {
    // Round 2 review item B1: the account's Jira profile zone changes from Europe/Brussels to
    // Asia/Tokyo (east, ahead) *between* two sync calls. A cached zone combined with a cursor
    // stored pre-rendered in the *old* zone's local time would render the second call's query
    // about the zone gap too late, silently dropping real updates in between. Storing the cursor
    // as an instant and re-reading the zone every call (this crate's actual fix) must not.
    let page = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let jql1 = jql_for("DEMO", None);
    let url1 = cloud_search_url(&jql1, &page);
    let transport1 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "Europe/Brussels"),
        ok(
            &url1,
            cloud_page(
                vec![issue_json(
                    "DEMO-1",
                    "Filed before the move",
                    "new",
                    "2026-01-01T10:00:00.000+0100", // Brussels, January: UTC+1
                )],
                None,
            ),
        ),
    ]);
    let outcome1 =
        pitcrew_sync_jira::sync::sync(SyncState::new(), &transport1, &JiraCloud, &config()).await;
    assert!(outcome1.errors.is_empty(), "{:?}", outcome1.errors);
    let project = outcome1.state.projects.get("DEMO").expect("project state");
    assert_eq!(project.cursor.as_deref(), Some("2026-01-01T09:00:00Z"));

    // The account's profile zone is now Asia/Tokyo (UTC+9) — moved 8 hours east. A new issue
    // lands 30 minutes (real time) after DEMO-1.
    let jql2 = jql_for(
        "DEMO",
        query_cursor_text(project.cursor.as_deref(), "Asia/Tokyo").as_deref(),
    );
    let url2 = cloud_search_url(&jql2, &page);
    let transport2 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "Asia/Tokyo"),
        ok(
            &url2,
            cloud_page(
                vec![issue_json(
                    "DEMO-2",
                    "Filed after the move",
                    "new",
                    "2026-01-01T19:30:00.000+0900", // = 2026-01-01T10:30:00Z
                )],
                None,
            ),
        ),
    ]);
    let outcome2 =
        pitcrew_sync_jira::sync::sync(outcome1.state, &transport2, &JiraCloud, &config()).await;
    assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
    assert_eq!(transport2.remaining(), 0);
    let created: Vec<&str> = outcome2
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::IssueCreated { source, .. } => Some(source.key.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(created, vec!["DEMO-2"], "{:#?}", outcome2.changes);
}

#[tokio::test]
async fn the_account_zone_moving_west_between_syncs_skips_nothing() {
    // The reverse direction: Asia/Tokyo (UTC+9) to Europe/Brussels (UTC+1), 8 hours west.
    let page = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let jql1 = jql_for("DEMO", None);
    let url1 = cloud_search_url(&jql1, &page);
    let transport1 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "Asia/Tokyo"),
        ok(
            &url1,
            cloud_page(
                vec![issue_json(
                    "DEMO-1",
                    "Filed before the move",
                    "new",
                    "2026-01-01T19:00:00.000+0900", // = 2026-01-01T10:00:00Z
                )],
                None,
            ),
        ),
    ]);
    let outcome1 =
        pitcrew_sync_jira::sync::sync(SyncState::new(), &transport1, &JiraCloud, &config()).await;
    assert!(outcome1.errors.is_empty(), "{:?}", outcome1.errors);
    let project = outcome1.state.projects.get("DEMO").expect("project state");
    assert_eq!(project.cursor.as_deref(), Some("2026-01-01T10:00:00Z"));

    let jql2 = jql_for(
        "DEMO",
        query_cursor_text(project.cursor.as_deref(), "Europe/Brussels").as_deref(),
    );
    let url2 = cloud_search_url(&jql2, &page);
    let transport2 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "Europe/Brussels"),
        ok(
            &url2,
            cloud_page(
                vec![issue_json(
                    "DEMO-2",
                    "Filed after the move",
                    "new",
                    "2026-01-01T11:30:00.000+0100", // = 2026-01-01T10:30:00Z
                )],
                None,
            ),
        ),
    ]);
    let outcome2 =
        pitcrew_sync_jira::sync::sync(outcome1.state, &transport2, &JiraCloud, &config()).await;
    assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
    assert_eq!(transport2.remaining(), 0);
    let created: Vec<&str> = outcome2
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::IssueCreated { source, .. } => Some(source.key.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(created, vec!["DEMO-2"], "{:#?}", outcome2.changes);
}

#[tokio::test]
async fn an_old_pre_rendered_string_cursor_is_treated_as_no_cursor() {
    // Before this crate stored the cursor as an instant, it stored it pre-rendered as a bare
    // "YYYY-MM-DD HH:MM" local-time string with no zone attached. Such a value can no longer be
    // safely reinterpreted (the zone it was rendered in is not recoverable) — `sync` must treat
    // it exactly like no cursor at all, not guess, and not error out the whole state.
    let mut state = SyncState::new();
    state.timezone = Some("UTC".to_string());
    state.projects.insert(
        "DEMO".to_string(),
        pitcrew_sync_jira::ProjectState {
            cursor: Some("2026-01-01 00:03".to_string()), // the old format
            ..Default::default()
        },
    );

    // A first-sync-shaped query (no `AND updated >=` clause) is exactly what must be sent.
    let jql = jql_for("DEMO", None);
    let page = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let url = cloud_search_url(&jql, &page);
    let transport = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "UTC"),
        ok(
            &url,
            cloud_page(
                vec![issue_json(
                    "DEMO-9",
                    "Seen for the first time",
                    "new",
                    "2026-02-01T00:00:00.000+0000",
                )],
                None,
            ),
        ),
    ]);
    let outcome = pitcrew_sync_jira::sync::sync(state, &transport, &JiraCloud, &config()).await;
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(
        transport.remaining(),
        0,
        "the query must have had no cursor clause at all"
    );
    assert_eq!(
        outcome
            .changes
            .iter()
            .filter(|c| matches!(c, UpstreamChange::IssueCreated { .. }))
            .count(),
        1
    );
    let project = outcome.state.projects.get("DEMO").expect("project state");
    assert_eq!(
        project.cursor.as_deref(),
        Some("2026-02-01T00:00:00Z"),
        "the old string is fully replaced by a real instant once this project syncs again"
    );
}
