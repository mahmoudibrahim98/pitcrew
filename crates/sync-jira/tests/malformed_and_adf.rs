//! Fixture tests for bounds: a malformed issue is skipped (and counted) rather than failing the
//! whole page, and a huge ADF description is capped rather than stored in full — end to end
//! through `sync`, not just the `adf` module's own unit tests.

mod support;

use pitcrew_sync_jira::deployment::JiraCloud;
use pitcrew_sync_jira::{SyncConfig, SyncState, UpstreamChange};
use pitcrew_sync_github::fixture::ReplayTransport;
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
async fn a_malformed_issue_is_skipped_and_a_huge_adf_description_is_capped() {
    let jql = jql_for("DEMO", None);
    let page = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let url = cloud_search_url(&jql, &page);

    let huge_text = "x".repeat(pitcrew_sync_jira::bounds::MAX_BODY_CHARS + 5_000);
    let huge_adf = adf_description(&huge_text);

    let issues = serde_json::json!([
        issue_json("DEMO-1", "Normal issue", "new", "2026-01-01T00:00:00.000+0000"),
        "this element is not an issue object at all",
        { "key": "DEMO-2", "fields": { "summary": "missing required fields entirely" } },
        issue_with_description(
            "DEMO-3",
            "Huge description",
            "new",
            "2026-01-01T00:01:00.000+0000",
            huge_adf,
        ),
    ]);

    let transport = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "UTC"),
        ok(&url, cloud_page(issues.as_array().expect("array").clone(), None)),
    ]);

    let outcome = pitcrew_sync_jira::sync::sync(SyncState::new(), &transport, &JiraCloud, &config()).await;

    assert!(outcome.rate_limited.is_none());
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(
        outcome.malformed_skipped, 2,
        "the bare string and the issue missing required fields both fail to parse"
    );

    let created = outcome
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::IssueCreated { source, body, .. } => Some((source.key.as_str(), body.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(created.len(), 2, "the two malformed elements must not stop the valid ones");

    let (_, normal_body) = created.iter().find(|(k, _)| *k == "DEMO-1").expect("DEMO-1");
    assert_eq!(*normal_body, "");

    let (_, huge_body) = created.iter().find(|(k, _)| *k == "DEMO-3").expect("DEMO-3");
    assert_eq!(
        huge_body.chars().count(),
        pitcrew_sync_jira::bounds::MAX_BODY_CHARS,
        "the huge ADF description must be capped, not stored in full"
    );
}
