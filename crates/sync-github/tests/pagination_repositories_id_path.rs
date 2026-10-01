//! Fixture test for round 3 review item B-1: GitHub rewrites the path in the very first `next`
//! link it sends for several list endpoints, to a numeric-id "/repositories/<id>/..." alias
//! rather than the "owner/repo" path the request itself used — observed on pull requests, issues
//! and milestones alike. An earlier version of the pagination trust check (round 2) compared
//! `next` against the *exact* path of the request that returned it, which rejected this real,
//! legitimate link outright and silently capped every one of these listings at one page. This
//! exercises all three endpoints with that real shape and asserts every page is actually followed.

use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};
use pitcrew_sync_github::{
    AuthToken, GithubTimestamp, RepoRef, SyncConfig, SyncState, UpstreamChange,
};

const ISSUES_PAGE1: &str = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100";
const ISSUES_PAGE2: &str = "https://api.github.com/repositories/724712/issues?state=all&sort=updated&direction=asc&per_page=100&page=2";
const PULLS_PAGE1: &str = "https://api.github.com/repos/example-org/demo-repo/pulls?state=all&sort=updated&direction=desc&per_page=100";
const PULLS_PAGE2: &str = "https://api.github.com/repositories/724712/pulls?state=all&sort=updated&direction=desc&per_page=100&page=2";
const MILESTONES_PAGE1: &str = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";
const MILESTONES_PAGE2: &str = "https://api.github.com/repositories/724712/milestones?state=all&sort=due_on&direction=asc&per_page=100&page=2";

fn config() -> SyncConfig {
    SyncConfig {
        repos: vec![RepoRef::new("example-org/demo-repo").expect("valid repo")],
        token: AuthToken::new("ghp_test_token_not_real"),
        now_unix: 2_000_000_000,
        now: GithubTimestamp::new("2026-01-01T00:00:00Z"),
        api_base: None,
    }
}

fn paged(url: &str, next: Option<&str>, body: &[u8]) -> RecordedExchange {
    let mut response_headers = vec![];
    if let Some(next) = next {
        response_headers.push(("Link".to_string(), format!("<{next}>; rel=\"next\"")));
    }
    RecordedExchange {
        method: "GET".to_string(),
        url: url.to_string(),
        request_headers: vec![],
        status: 200,
        response_headers,
        body: body.to_vec(),
    }
}

#[tokio::test]
async fn pulls_issues_and_milestones_all_follow_a_next_link_rewritten_to_repositories_id() {
    let issues_p1 = paged(
        ISSUES_PAGE1,
        Some(ISSUES_PAGE2),
        br#"[{"number":1,"title":"From page 1","state":"open","updated_at":"2026-01-01T00:01:00Z","html_url":"https://github.com/example-org/demo-repo/issues/1"}]"#,
    );
    let issues_p2 = paged(
        ISSUES_PAGE2,
        None,
        br#"[{"number":2,"title":"From page 2","state":"open","updated_at":"2026-01-01T00:02:00Z","html_url":"https://github.com/example-org/demo-repo/issues/2"}]"#,
    );
    let pulls_p1 = paged(
        PULLS_PAGE1,
        Some(PULLS_PAGE2),
        br#"[{"number":10,"title":"PR from page 1","state":"open","updated_at":"2026-01-01T00:03:00Z","html_url":"https://github.com/example-org/demo-repo/pull/10"}]"#,
    );
    let pulls_p2 = paged(
        PULLS_PAGE2,
        None,
        br#"[{"number":11,"title":"PR from page 2","state":"open","updated_at":"2026-01-01T00:04:00Z","html_url":"https://github.com/example-org/demo-repo/pull/11"}]"#,
    );
    let milestones_p1 = paged(
        MILESTONES_PAGE1,
        Some(MILESTONES_PAGE2),
        br#"[{"number":20,"title":"Milestone from page 1","state":"open","html_url":"https://github.com/example-org/demo-repo/milestone/20"}]"#,
    );
    let milestones_p2 = paged(
        MILESTONES_PAGE2,
        None,
        br#"[{"number":21,"title":"Milestone from page 2","state":"open","html_url":"https://github.com/example-org/demo-repo/milestone/21"}]"#,
    );

    let transport = ReplayTransport::from_exchanges(vec![
        milestones_p1,
        milestones_p2,
        issues_p1,
        issues_p2,
        pulls_p1,
        pulls_p2,
    ]);

    let outcome = pitcrew_sync_github::sync::sync(SyncState::new(), &transport, &config()).await;

    assert!(outcome.rate_limited.is_none(), "{:?}", outcome.rate_limited);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(outcome.malformed_skipped, 0);
    assert_eq!(
        transport.remaining(),
        0,
        "every page, including the repositories/<id>-rewritten ones, should have been requested"
    );

    let issue_numbers: Vec<u64> = outcome
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::IssueOpened { source, .. } => {
                Some(source.key.rsplit('#').next()?.parse().ok()?)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        issue_numbers,
        vec![1, 2],
        "both the page-1 and the page-2 (repositories/<id>-rewritten) issue must be seen: {:#?}",
        outcome.changes
    );

    let pull_numbers: Vec<u64> = outcome
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::PullRequestOpened { source, .. } => {
                Some(source.key.rsplit('#').next()?.parse().ok()?)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        pull_numbers,
        vec![10, 11],
        "pull requests must not be stuck on page 1 forever: {:#?}",
        outcome.changes
    );

    let milestone_titles: Vec<&str> = outcome
        .changes
        .iter()
        .filter_map(|c| match c {
            UpstreamChange::MilestoneCreated { title, .. } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        milestone_titles,
        vec!["Milestone from page 1", "Milestone from page 2"],
        "{:#?}",
        outcome.changes
    );
}
