//! Top-level orchestration: [`sync`] turns a [`SyncState`] and a [`Transport`] into a new state
//! and the [`UpstreamChange`]s found — the same contract as `pitcrew_sync_github::sync::sync`:
//! pure beyond the `Transport` it is given, the caller persists the returned state, and this
//! function never touches the event log.

use crate::auth::JiraAuth;
use crate::change::{UpstreamChange, diff_epic, diff_issue};
use crate::client::{JiraClient, Limits, Outcome};
use crate::deployment::Deployment;
use crate::jql::{InvalidProjectRef, ProjectRef, incremental_query};
use crate::state::SyncState;
use crate::time::JiraTimestamp;
use pitcrew_sync_github::transport::Transport;

pub use crate::client::RateLimited;

/// The fields every search asks for, fetched in full on both deployments regardless of
/// `epic_link_field` — "fetch only the fields you use" (see the brief).
const BASE_FIELDS: &[&str] = &[
    "summary",
    "description",
    "status",
    "resolution",
    "labels",
    "assignee",
    "parent",
    "issuetype",
    "updated",
];

/// What to sync, and the inputs that keep `sync` a pure function of its arguments: it reads no
/// clock itself.
#[derive(Debug)]
pub struct SyncConfig {
    /// Projects to sync, in order. Each gets its own JQL query (`project in ("<key>") AND
    /// updated >= …`, the shape the brief asks for — a one-element `in (...)`) and its own
    /// cursor/snapshots in [`SyncState`], rather than one query spanning every project: a project
    /// added later starts its own first full sync without disturbing the others' cursors, and the
    /// per-project resource/error reporting (see [`SyncIssue::project`]) stays precise. This
    /// mirrors `pitcrew_sync_github::SyncConfig::repos`'s per-repository shape.
    pub projects: Vec<ProjectRef>,
    /// Credentials: `JiraAuth::Basic` for Cloud, `JiraAuth::Bearer` for Data Center.
    pub auth: JiraAuth,
    /// The full REST API root, e.g. `https://jira.example.com/rest/api/3` (Cloud) or
    /// `https://jira.example.com/rest/api/2` (Data Center).
    pub api_base: String,
    /// The site root used to build browsable `ExternalRef` urls, e.g.
    /// `https://jira.example.com`. Jira's REST responses carry no browsable URL field (unlike
    /// GitHub's `html_url`), so this is supplied separately from `api_base`.
    pub site_base: String,
    /// The custom field id holding the epic link on a classic (non-next-gen) Data Center project,
    /// e.g. `customfield_10008`. Modern Jira Cloud reports this through `fields.parent` instead,
    /// which needs no configuration; see [`crate::wire::WireFields::epic_key`].
    pub epic_link_field: Option<String>,
    /// Wall-clock time as Unix seconds, used only to turn a `Retry-After` duration or an
    /// exponential backoff into an absolute `RateLimited.until`.
    pub now_unix: i64,
}

/// Which resource a non-fatal problem came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// `/myself` (the account time zone).
    Myself,
    /// A project's issue search.
    Search,
}

/// A non-fatal problem hit while syncing one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncIssue {
    /// The project key, or empty for a problem not specific to one project (`/myself`).
    pub project: String,
    /// Which resource.
    pub resource: Resource,
    /// A short, non-sensitive description (never includes the credential).
    pub message: String,
}

/// Everything one `sync` call produced.
#[derive(Debug)]
pub struct SyncOutcome {
    /// The new state; the caller persists this for next time.
    pub state: SyncState,
    /// What changed upstream, across every project reached before any rate limit.
    pub changes: Vec<UpstreamChange>,
    /// Set when a rate limit stopped the sync before every project was reached. Nothing later in
    /// `config.projects` (in iteration order) was contacted.
    pub rate_limited: Option<RateLimited>,
    /// Non-fatal problems. A project that hits one still tries to persist whatever it already
    /// collected this call.
    pub errors: Vec<SyncIssue>,
    /// How many items across the whole call failed to parse, or carried a malformed timestamp,
    /// and were skipped.
    pub malformed_skipped: u32,
}

fn fields_for(config: &SyncConfig) -> Vec<&str> {
    let mut fields = BASE_FIELDS.to_vec();
    if let Some(custom) = &config.epic_link_field {
        fields.push(custom.as_str());
    }
    fields
}

/// Syncs every project in `config.projects`, in order, stopping as soon as any is rate-limited
/// (a Jira token's rate limit applies to the whole site, so there is no point trying the next
/// project). Projects already synced before that point keep their results.
pub async fn sync<T: Transport, D: Deployment>(
    state: SyncState,
    transport: &T,
    deployment: &D,
    config: &SyncConfig,
) -> SyncOutcome {
    sync_with_limits(state, transport, deployment, config, Limits::default()).await
}

/// `sync`'s actual implementation, parameterised over the page/item caps — see
/// `pitcrew_sync_github::sync`'s identical split for why.
async fn sync_with_limits<T: Transport, D: Deployment>(
    mut state: SyncState,
    transport: &T,
    deployment: &D,
    config: &SyncConfig,
    limits: Limits,
) -> SyncOutcome {
    let mut changes = Vec::new();
    let mut errors = Vec::new();
    let mut malformed_skipped = 0u32;
    let mut rate_limited = None;

    let client = JiraClient::new(transport, config.auth.clone(), config.api_base.clone());

    // Read once and cached — re-reading it on every call would be one more request for a value
    // that essentially never changes (see `SyncState::timezone`).
    if state.timezone.is_none() {
        match client.myself().await {
            Ok(tz) => state.timezone = tz,
            Err(e) => errors.push(SyncIssue {
                project: String::new(),
                resource: Resource::Myself,
                message: e.to_string(),
            }),
        }
    }

    let fields = fields_for(config);

    for project in &config.projects {
        let key = project.as_str().to_string();
        let mut project_state = state.projects.remove(&key).unwrap_or_default();
        let mut attempts = project_state.secondary_backoff_attempts;

        let cursor = project_state.cursor.as_ref().map(JiraTimestamp::as_str);
        let jql = incremental_query(project, cursor);

        let result = client
            .search(
                deployment,
                &jql,
                &fields,
                config.now_unix,
                &mut attempts,
                limits,
            )
            .await;
        project_state.secondary_backoff_attempts = attempts;

        match result {
            Err(e) => errors.push(SyncIssue {
                project: key.clone(),
                resource: Resource::Search,
                message: e.to_string(),
            }),
            Ok(Outcome::RateLimited(rl)) => {
                state.projects.insert(key, project_state);
                rate_limited = Some(rl);
                break;
            }
            Ok(Outcome::Ok(search_result)) => {
                malformed_skipped += search_result.malformed_skipped;
                for issue in &search_result.items {
                    if issue.is_epic() {
                        let previous = project_state.epic_snapshots.get(&issue.key);
                        match diff_epic(&config.site_base, issue, previous) {
                            Some((mut found, snapshot)) => {
                                changes.append(&mut found);
                                project_state
                                    .epic_snapshots
                                    .insert(issue.key.clone(), snapshot);
                            }
                            None => malformed_skipped += 1,
                        }
                    } else {
                        let previous = project_state.issue_snapshots.get(&issue.key);
                        match diff_issue(
                            &config.site_base,
                            issue,
                            previous,
                            config.epic_link_field.as_deref(),
                        ) {
                            Some((mut found, snapshot)) => {
                                changes.append(&mut found);
                                project_state
                                    .issue_snapshots
                                    .insert(issue.key.clone(), snapshot);
                            }
                            None => malformed_skipped += 1,
                        }
                    }
                }
                // Advance the cursor to the newest minute actually seen. This is unconditional
                // (not gated on "every page was fetched"), the same way sync-github's *issues*
                // cursor is: it is always safe to advance to the newest item actually processed,
                // because the walk is ascending with a server-side `updated >=` filter — anything
                // older than what this call saw is guaranteed either already synced, or still
                // `>=` the *old* cursor and so still matched by this same JQL next time. A cap
                // cutting the walk short just means next call repeats the same query from the
                // (now slightly advanced) cursor instead of resuming mid-walk; nothing is skipped.
                if let Some(minute) = search_result
                    .max_updated
                    .as_ref()
                    .and_then(JiraTimestamp::to_jql_minute)
                {
                    let candidate = JiraTimestamp::new(minute);
                    if project_state.cursor.as_ref().is_none_or(|c| candidate > *c) {
                        project_state.cursor = Some(candidate);
                    }
                }
            }
        }
        state.projects.insert(key, project_state);
    }

    SyncOutcome {
        state,
        changes,
        rate_limited,
        errors,
        malformed_skipped,
    }
}

/// Validates a batch of raw project keys, for callers building a [`SyncConfig`] from
/// configuration. Not used by `sync` itself (which only ever sees already-valid [`ProjectRef`]s);
/// a convenience for callers.
pub fn validate_projects(
    keys: impl IntoIterator<Item = impl Into<String>>,
) -> Result<Vec<ProjectRef>, InvalidProjectRef> {
    keys.into_iter().map(ProjectRef::new).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::JiraCloud;
    use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};

    fn config(projects: Vec<&str>) -> SyncConfig {
        SyncConfig {
            projects: projects
                .into_iter()
                .map(|p| ProjectRef::new(p).expect("valid"))
                .collect(),
            auth: JiraAuth::Bearer {
                token: "pat-test-not-real".to_string(),
            },
            api_base: "https://jira.example.com/rest/api/3".to_string(),
            site_base: "https://jira.example.com".to_string(),
            epic_link_field: None,
            now_unix: 2_000_000_000,
        }
    }

    fn myself_exchange() -> RecordedExchange {
        RecordedExchange {
            method: "GET".to_string(),
            url: "https://jira.example.com/rest/api/3/myself".to_string(),
            request_headers: vec![],
            status: 200,
            response_headers: vec![],
            body: br#"{"timeZone":"UTC"}"#.to_vec(),
        }
    }

    fn empty_search(url: &str) -> RecordedExchange {
        RecordedExchange {
            method: "GET".to_string(),
            url: url.to_string(),
            request_headers: vec![],
            status: 200,
            response_headers: vec![],
            body: br#"{"issues":[]}"#.to_vec(),
        }
    }

    #[tokio::test]
    async fn myself_is_read_once_and_then_cached() {
        let search_url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20ORDER%20BY%20updated%20ASC&maxResults=100&fields=summary%2Cdescription%2Cstatus%2Cresolution%2Clabels%2Cassignee%2Cparent%2Cissuetype%2Cupdated";
        let transport =
            ReplayTransport::from_exchanges(vec![myself_exchange(), empty_search(search_url)]);
        let outcome = sync(
            SyncState::new(),
            &transport,
            &JiraCloud,
            &config(vec!["DEMO"]),
        )
        .await;
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.state.timezone.as_deref(), Some("UTC"));
        assert_eq!(transport.remaining(), 0);

        // Second call: no /myself fixture provided, so if the client tried to re-read it, the
        // transport would error on an unmatched request.
        let transport2 = ReplayTransport::from_exchanges(vec![empty_search(search_url)]);
        let outcome2 = sync(
            outcome.state,
            &transport2,
            &JiraCloud,
            &config(vec!["DEMO"]),
        )
        .await;
        assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
        assert_eq!(transport2.remaining(), 0);
    }

    #[test]
    fn validate_projects_rejects_an_invalid_key() {
        assert!(validate_projects(["DEMO", "bad key"]).is_err());
        assert!(validate_projects(["DEMO", "OTHER"]).is_ok());
    }
}
