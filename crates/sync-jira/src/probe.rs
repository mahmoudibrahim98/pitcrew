//! [`probe`]: one read of the account and each project with the credential, for "test this
//! connection". It changes nothing, keeps no state and never writes: `GET /myself`, then
//! `GET /project/{key}` for each project.
//!
//! [`CheckOutcome`] is `pitcrew_sync_github`'s own, so the hub reads both probes the same way.

use crate::auth::JiraAuth;
use crate::bounds::SECONDARY_BACKOFF_CAP_SECS;
use crate::client::RateLimited;
use crate::jql::ProjectRef;
use crate::sync::SyncConfig;
pub use pitcrew_sync_github::probe::CheckOutcome;
use pitcrew_sync_github::transport::{Method, Request, Response, Transport};

/// One project's check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectCheck {
    /// The project.
    pub project: ProjectRef,
    /// What the check found. `Readable { can_write: false }` when readable: Jira's project read
    /// says nothing about the credential's write rights.
    pub outcome: CheckOutcome,
}

/// What [`probe`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeReport {
    /// The credential itself (`GET /myself`).
    pub account: CheckOutcome,
    /// One check per project, in order.
    pub projects: Vec<ProjectCheck>,
    /// Set when a rate limit stopped the probe.
    pub rate_limited: Option<RateLimited>,
}

impl ProbeReport {
    /// Whether the credential works and every project is readable.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.rate_limited.is_none()
            && matches!(self.account, CheckOutcome::Readable { .. })
            && self
                .projects
                .iter()
                .all(|c| matches!(c.outcome, CheckOutcome::Readable { .. }))
    }
}

fn request(url: String, auth: &JiraAuth) -> Request {
    Request {
        method: Method::Get,
        url,
        headers: vec![
            ("Accept".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), auth.header_value()),
        ],
        body: Vec::new(),
    }
}

fn retry_after(response: &Response, now_unix: i64) -> RateLimited {
    let wait = response
        .header("retry-after")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(SECONDARY_BACKOFF_CAP_SECS)
        .clamp(0, SECONDARY_BACKOFF_CAP_SECS);
    RateLimited {
        until: now_unix.saturating_add(wait),
    }
}

/// Reads the account, then each project of `config`, with `config.auth`. Stops at a refused
/// credential or a rate limit: what is left is [`CheckOutcome::NotChecked`].
pub async fn probe<T: Transport>(transport: &T, config: &SyncConfig) -> ProbeReport {
    let mut report = ProbeReport {
        account: CheckOutcome::NotChecked,
        projects: Vec::new(),
        rate_limited: None,
    };
    let mut stopped = false;
    match transport
        .send(request(format!("{}/myself", config.api_base), &config.auth))
        .await
    {
        Err(e) => report.account = CheckOutcome::Failed(e.to_string()),
        Ok(response) => {
            report.account = match response.status {
                200 => CheckOutcome::Readable { can_write: false },
                401 => CheckOutcome::Refused,
                403 => CheckOutcome::Forbidden,
                429 => {
                    report.rate_limited = Some(retry_after(&response, config.now_unix));
                    CheckOutcome::NotChecked
                }
                status => CheckOutcome::Failed(format!("unexpected status {status}")),
            };
            stopped = !matches!(report.account, CheckOutcome::Readable { .. });
        }
    }
    for project in &config.projects {
        if stopped {
            report.projects.push(ProjectCheck {
                project: project.clone(),
                outcome: CheckOutcome::NotChecked,
            });
            continue;
        }
        let url = format!("{}/project/{}", config.api_base, project.as_str());
        let outcome = match transport.send(request(url, &config.auth)).await {
            Err(e) => CheckOutcome::Failed(e.to_string()),
            Ok(response) => match response.status {
                200 => CheckOutcome::Readable { can_write: false },
                401 => {
                    stopped = true;
                    CheckOutcome::Refused
                }
                403 => CheckOutcome::Forbidden,
                404 => CheckOutcome::NotFound,
                429 => {
                    report.rate_limited = Some(retry_after(&response, config.now_unix));
                    stopped = true;
                    CheckOutcome::NotChecked
                }
                status => CheckOutcome::Failed(format!("unexpected status {status}")),
            },
        };
        report.projects.push(ProjectCheck {
            project: project.clone(),
            outcome,
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};

    const SECRET: &str = "synthetic-jira-credential";

    fn config(projects: &[&str]) -> SyncConfig {
        SyncConfig {
            projects: projects
                .iter()
                .map(|p| ProjectRef::new(*p).unwrap())
                .collect(),
            auth: JiraAuth::Basic {
                email: "sam@example.com".into(),
                api_token: SECRET.into(),
            },
            api_base: "https://jira.example.com/rest/api/3".into(),
            site_base: "https://jira.example.com".into(),
            epic_link_field: None,
            now_unix: 1_000,
        }
    }

    fn exchange(path: &str, status: u16, headers: &[(&str, &str)]) -> RecordedExchange {
        RecordedExchange {
            method: "GET".into(),
            url: format!("https://jira.example.com/rest/api/3{path}"),
            request_headers: vec![],
            status,
            response_headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: b"{}".to_vec(),
        }
    }

    #[tokio::test]
    async fn the_account_and_each_project_are_read() {
        let transport = ReplayTransport::from_exchanges(vec![
            exchange("/myself", 200, &[]),
            exchange("/project/DEMO", 200, &[]),
            exchange("/project/OPS", 404, &[]),
        ]);
        let report = probe(&transport, &config(&["DEMO", "OPS"])).await;
        assert_eq!(report.account, CheckOutcome::Readable { can_write: false });
        assert_eq!(
            report
                .projects
                .iter()
                .map(|c| c.outcome.clone())
                .collect::<Vec<_>>(),
            vec![
                CheckOutcome::Readable { can_write: false },
                CheckOutcome::NotFound
            ]
        );
        assert!(!report.ok());
        for request in transport.requests_sent() {
            assert_eq!(request.method, Method::Get);
            assert!(!format!("{request:?}").contains(SECRET));
        }
    }

    #[tokio::test]
    async fn a_refused_credential_checks_no_project() {
        let transport = ReplayTransport::from_exchanges(vec![exchange("/myself", 401, &[])]);
        let report = probe(&transport, &config(&["DEMO"])).await;
        assert_eq!(report.account, CheckOutcome::Refused);
        assert_eq!(report.projects[0].outcome, CheckOutcome::NotChecked);
        assert_eq!(transport.requests_sent().len(), 1);
    }

    #[tokio::test]
    async fn a_rate_limit_is_capped_and_stops_the_probe() {
        let transport = ReplayTransport::from_exchanges(vec![
            exchange("/myself", 200, &[]),
            exchange("/project/DEMO", 429, &[("Retry-After", "99999999999")]),
        ]);
        let report = probe(&transport, &config(&["DEMO", "OPS"])).await;
        assert_eq!(
            report.rate_limited,
            Some(RateLimited {
                until: 1_000 + SECONDARY_BACKOFF_CAP_SECS
            })
        );
        assert_eq!(report.projects[1].outcome, CheckOutcome::NotChecked);
    }
}
