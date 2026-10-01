//! Fixture tests for the main Jira Cloud sync path: a first full sync, then an incremental sync
//! that exercises pagination, a status-category move to `done`, a reopen (`done` leaving its
//! category), an epic rename, a re-parent, and the overlap minute re-including an unchanged item
//! without duplicating it — then a third call that changes nothing (idempotence). All synthetic:
//! the `DEMO` project on `jira.example.com`, no real accounts.

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

fn find(
    changes: &[UpstreamChange],
    matches: impl Fn(&UpstreamChange) -> bool,
) -> Vec<&UpstreamChange> {
    changes.iter().filter(|c| matches(c)).collect()
}

#[tokio::test]
async fn first_sync_then_incremental_then_idempotent() {
    // --- Call 1: first full sync, one page. ---
    let page = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let jql1 = jql_for("DEMO", None);
    let url1 = cloud_search_url(&jql1, &page);
    let transport1 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "UTC"),
        ok(
            &url1,
            cloud_page(
                vec![
                    issue_json("DEMO-1", "Login bug", "new", "2026-01-01T00:01:00.000+0000"),
                    with_resolution(
                        issue_json(
                            "DEMO-2",
                            "Crash on startup",
                            "done",
                            "2026-01-01T00:02:00.000+0000",
                        ),
                        "Done",
                    ),
                    epic_json("DEMO-10", "Big Epic", "new", "2026-01-01T00:03:00.000+0000"),
                ],
                None,
            ),
        ),
    ]);

    let outcome1 =
        pitcrew_sync_jira::sync::sync(SyncState::new(), &transport1, &JiraCloud, &config()).await;
    assert!(outcome1.errors.is_empty(), "{:?}", outcome1.errors);
    assert!(outcome1.rate_limited.is_none());
    assert_eq!(outcome1.malformed_skipped, 0);
    assert_eq!(
        transport1.remaining(),
        0,
        "every recorded exchange should have been used"
    );
    assert_eq!(outcome1.state.timezone.as_deref(), Some("UTC"));

    assert_eq!(
        find(&outcome1.changes, |c| matches!(
            c,
            UpstreamChange::IssueCreated { .. }
        ))
        .len(),
        2
    );
    assert_eq!(
        find(&outcome1.changes, |c| matches!(
            c,
            UpstreamChange::EpicCreated { .. }
        ))
        .len(),
        1
    );
    let done = find(&outcome1.changes, |c| {
        matches!(c, UpstreamChange::IssueDone { .. })
    });
    assert_eq!(done.len(), 1, "{:#?}", outcome1.changes);
    assert_eq!(done[0].source().key, "DEMO-2");

    let project = outcome1.state.projects.get("DEMO").expect("project state");
    assert_eq!(project.cursor.as_deref(), Some("2026-01-01T00:03:00Z"));
    assert_eq!(project.issue_snapshots.len(), 2);
    assert_eq!(project.epic_snapshots.len(), 1);

    // --- Call 2: incremental, two pages, several kinds of change at once. ---
    let jql2 = jql_for(
        "DEMO",
        query_cursor_text(project.cursor.as_deref(), "UTC").as_deref(),
    );
    let page1 = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let url2a = cloud_search_url(&jql2, &page1);
    let page2 = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: Some("page2tok".to_string()),
    };
    let url2b = cloud_search_url(&jql2, &page2);

    let transport2 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "UTC"),
        ok(
            &url2a,
            cloud_page(
                vec![
                    // The overlap minute re-including DEMO-10 unchanged: must produce no change.
                    epic_json("DEMO-10", "Big Epic", "new", "2026-01-01T00:03:00.000+0000"),
                    // Retitled AND re-parented in the same call.
                    with_parent(
                        issue_json(
                            "DEMO-1",
                            "Fix flaky login test",
                            "new",
                            "2026-01-01T00:04:00.000+0000",
                        ),
                        "DEMO-10",
                    ),
                ],
                Some("page2tok"),
            ),
        ),
        ok(
            &url2b,
            cloud_page(
                vec![
                    // A reopen: category leaves `done`.
                    issue_json(
                        "DEMO-2",
                        "Crash on startup",
                        "new",
                        "2026-01-01T00:05:00.000+0000",
                    ),
                    // An epic rename.
                    epic_json(
                        "DEMO-10",
                        "Platform Epic",
                        "new",
                        "2026-01-01T00:07:00.000+0000",
                    ),
                ],
                None,
            ),
        ),
    ]);

    let outcome2 =
        pitcrew_sync_jira::sync::sync(outcome1.state, &transport2, &JiraCloud, &config()).await;
    assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
    assert!(outcome2.rate_limited.is_none());
    assert_eq!(outcome2.malformed_skipped, 0);
    assert_eq!(
        transport2.remaining(),
        0,
        "both pages, and the (re-requested) /myself, should all be used"
    );

    assert_eq!(
        find(&outcome2.changes, |c| matches!(
            c,
            UpstreamChange::IssueRetitled { .. }
        ))
        .len(),
        1
    );
    assert_eq!(
        find(&outcome2.changes, |c| matches!(
            c,
            UpstreamChange::IssueReparented { .. }
        ))
        .len(),
        1
    );
    assert_eq!(
        find(&outcome2.changes, |c| matches!(
            c,
            UpstreamChange::EpicRenamed { .. }
        ))
        .len(),
        1
    );
    let reopened = find(&outcome2.changes, |c| {
        matches!(c, UpstreamChange::IssueReopened { .. })
    });
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].source().key, "DEMO-2");
    // DEMO-10's first (unchanged, overlap-minute) appearance must not have produced an
    // EpicRenamed or EpicCreated alongside the real rename later in the same call.
    assert_eq!(
        find(&outcome2.changes, |c| matches!(
            c,
            UpstreamChange::EpicCreated { .. }
        ))
        .len(),
        0
    );

    let project2 = outcome2.state.projects.get("DEMO").expect("project state");
    assert_eq!(project2.cursor.as_deref(), Some("2026-01-01T00:07:00Z"));

    // --- Call 3: nothing changed upstream (just the overlap minute again). Idempotence. ---
    let jql3 = jql_for(
        "DEMO",
        query_cursor_text(project2.cursor.as_deref(), "UTC").as_deref(),
    );
    let page3 = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let url3 = cloud_search_url(&jql3, &page3);
    let transport3 = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "UTC"),
        ok(
            &url3,
            cloud_page(
                vec![epic_json(
                    "DEMO-10",
                    "Platform Epic",
                    "new",
                    "2026-01-01T00:07:00.000+0000",
                )],
                None,
            ),
        ),
    ]);

    let state_before = outcome2.state;
    let outcome3 =
        pitcrew_sync_jira::sync::sync(state_before.clone(), &transport3, &JiraCloud, &config())
            .await;
    assert!(outcome3.changes.is_empty(), "{:#?}", outcome3.changes);
    assert!(outcome3.errors.is_empty());
    assert_eq!(outcome3.malformed_skipped, 0);
    assert_eq!(
        outcome3.state, state_before,
        "a no-op sync must not perturb the state"
    );
}
