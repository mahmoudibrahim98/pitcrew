//! Fixture test for a 429 honouring `Retry-After`, at the `sync` entry point: the rate limit must
//! stop the whole call (nothing later in `config.projects` is contacted) and must never sleep —
//! `sync` returns a `RateLimited { until }` instead.

mod support;

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_jira::deployment::JiraCloud;
use pitcrew_sync_jira::{SyncConfig, SyncState};
use support::*;

fn config(projects: Vec<&str>) -> SyncConfig {
    SyncConfig {
        projects: projects
            .into_iter()
            .map(|p| pitcrew_sync_jira::ProjectRef::new(p).expect("valid"))
            .collect(),
        auth: cloud_auth(),
        api_base: CLOUD_API_BASE.to_string(),
        site_base: SITE_BASE.to_string(),
        epic_link_field: None,
        now_unix: 2_000_000_000,
    }
}

#[tokio::test]
async fn a_429_with_retry_after_stops_the_sync_without_sleeping() {
    let jql = jql_for("DEMO", None);
    let page = pitcrew_sync_jira::PageState::Cloud {
        next_page_token: None,
    };
    let url = cloud_search_url(&jql, &page);

    let transport = ReplayTransport::from_exchanges(vec![
        myself_exchange(CLOUD_API_BASE, "UTC"),
        exchange(
            &url,
            429,
            vec![("Retry-After", "45")],
            serde_json::json!({}),
        ),
    ]);

    let outcome = pitcrew_sync_jira::sync::sync(
        SyncState::new(),
        &transport,
        &JiraCloud,
        &config(vec!["DEMO", "OTHER"]),
    )
    .await;

    let rl = outcome.rate_limited.expect("rate limited");
    assert_eq!(rl.until, 2_000_000_045);
    assert!(outcome.changes.is_empty());
    assert!(
        outcome.errors.is_empty(),
        "a rate limit is not an error: {:?}",
        outcome.errors
    );
    assert_eq!(
        transport.remaining(),
        0,
        "only myself and the one (rate-limited) search should have been requested"
    );
    // "OTHER" was never reached: its project state is untouched (still absent).
    assert!(!outcome.state.projects.contains_key("OTHER"));
}
