//! Fixture tests for the Jira Data Center path: a first full sync spread across two `startAt`
//! pages, a plain-text (not ADF) description passing through unchanged, then an incremental call
//! that changes nothing (idempotence, with the overlap minute re-including the last item).

mod support;

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_jira::deployment::JiraDataCenter;
use pitcrew_sync_jira::{SyncConfig, SyncState, UpstreamChange};
use support::*;

fn config() -> SyncConfig {
    SyncConfig {
        projects: vec![pitcrew_sync_jira::ProjectRef::new("DEMO").expect("valid")],
        auth: dc_auth(),
        api_base: DC_API_BASE.to_string(),
        site_base: SITE_BASE.to_string(),
        epic_link_field: None,
        now_unix: 2_000_000_000,
    }
}

#[tokio::test]
async fn first_sync_paginates_by_start_at_then_idempotent() {
    let jql1 = jql_for("DEMO", None);
    let page1 = pitcrew_sync_jira::PageState::DataCenter { start_at: 0 };
    let url1 = dc_search_url(&jql1, &page1);
    let page2 = pitcrew_sync_jira::PageState::DataCenter { start_at: 2 };
    let url2 = dc_search_url(&jql1, &page2);

    let transport1 = ReplayTransport::from_exchanges(vec![
        myself_exchange(DC_API_BASE, "America/New_York"),
        ok(
            &url1,
            dc_page(
                vec![
                    issue_with_description(
                        "DEMO-1",
                        "Login bug",
                        "new",
                        "2026-01-01T00:01:00.000-0500",
                        plain_description("Plain text body for v2."),
                    ),
                    issue_json(
                        "DEMO-2",
                        "Crash on startup",
                        "indeterminate",
                        "2026-01-01T00:02:00.000-0500",
                    ),
                ],
                0,
                3,
            ),
        ),
        ok(
            &url2,
            dc_page(
                vec![issue_json(
                    "DEMO-3",
                    "Docs typo",
                    "new",
                    "2026-01-01T00:03:00.000-0500",
                )],
                2,
                3,
            ),
        ),
    ]);

    let outcome1 =
        pitcrew_sync_jira::sync::sync(SyncState::new(), &transport1, &JiraDataCenter, &config())
            .await;
    assert!(outcome1.errors.is_empty(), "{:?}", outcome1.errors);
    assert!(outcome1.rate_limited.is_none());
    assert_eq!(outcome1.malformed_skipped, 0);
    assert_eq!(
        transport1.remaining(),
        0,
        "both startAt pages should be used"
    );
    assert_eq!(outcome1.state.timezone.as_deref(), Some("America/New_York"));

    let created = outcome1
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::IssueCreated { source, body, .. } => {
                Some((source.key.as_str(), body.as_str()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(created.len(), 3, "{:#?}", outcome1.changes);
    let (_, demo1_body) = created
        .iter()
        .find(|(k, _)| *k == "DEMO-1")
        .expect("DEMO-1");
    assert_eq!(
        *demo1_body, "Plain text body for v2.",
        "a Data Center plain-text description passes through unchanged, never through ADF conversion"
    );

    let project = outcome1.state.projects.get("DEMO").expect("project state");
    assert_eq!(project.cursor.as_deref(), Some("2026-01-01T05:03:00Z"));

    // --- Second call: only the overlap minute's own (unchanged) item comes back. Idempotence. ---
    let jql2 = jql_for(
        "DEMO",
        query_cursor_text(project.cursor.as_deref(), "America/New_York").as_deref(),
    );
    let page = pitcrew_sync_jira::PageState::DataCenter { start_at: 0 };
    let url = dc_search_url(&jql2, &page);
    let transport2 = ReplayTransport::from_exchanges(vec![
        myself_exchange(DC_API_BASE, "America/New_York"),
        ok(
            &url,
            dc_page(
                vec![issue_json(
                    "DEMO-3",
                    "Docs typo",
                    "new",
                    "2026-01-01T00:03:00.000-0500",
                )],
                0,
                1,
            ),
        ),
    ]);

    let state_before = outcome1.state;
    let outcome2 = pitcrew_sync_jira::sync::sync(
        state_before.clone(),
        &transport2,
        &JiraDataCenter,
        &config(),
    )
    .await;
    assert!(outcome2.changes.is_empty(), "{:#?}", outcome2.changes);
    assert!(outcome2.errors.is_empty());
    assert_eq!(
        transport2.remaining(),
        0,
        "/myself is re-requested, and the single search page, should both be used"
    );
    assert_eq!(
        outcome2.state, state_before,
        "a no-op sync must not perturb the state"
    );
}
