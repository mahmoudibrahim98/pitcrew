//! [`probe`]: one read of each repository with the credential, for "test this connection". It
//! changes nothing, keeps no state and never writes: `GET /repos/{owner}/{repo}` only.
//!
//! What it reports is for a person to read: whether each repository is readable, and whether the
//! credential can do more than read (push or admin rights, or a classic token's broad scopes), so
//! the person can choose a fine-grained, read-only token instead (G.5: "repo-scoped where
//! possible").

use crate::bounds::{SECONDARY_BACKOFF_CAP_SECS, backoff_secs};
use crate::client::{GithubClient, body_mentions_rate_limit};
use crate::sync::{RateLimited, RepoRef, SyncConfig};
use crate::transport::Transport;
use serde::Deserialize;

/// What one check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Readable. `can_write` when the credential could also change it.
    Readable {
        /// Push, maintain or admin rights.
        can_write: bool,
    },
    /// Upstream says it does not exist, or the credential may not see it (they look the same).
    NotFound,
    /// Upstream refused the credential itself (401).
    Refused,
    /// Upstream refused this read (403 that is not a rate limit).
    Forbidden,
    /// Not reached: a rate limit (see [`ProbeReport::rate_limited`]) or a refused credential
    /// stopped the probe first.
    NotChecked,
    /// The request failed, or upstream answered something else. Never holds the credential.
    Failed(String),
}

/// One repository's check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoCheck {
    /// The repository.
    pub repo: RepoRef,
    /// What the check found.
    pub outcome: CheckOutcome,
}

/// What [`probe`] found.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ProbeReport {
    /// One check per repository, in order.
    pub repos: Vec<RepoCheck>,
    /// The credential's scopes when upstream reported them (`X-OAuth-Scopes`: classic tokens and
    /// OAuth apps such as `gh`; fine-grained tokens report none).
    pub token_scopes: Option<Vec<String>>,
    /// Set when a rate limit stopped the probe.
    pub rate_limited: Option<RateLimited>,
}

impl ProbeReport {
    /// Whether every repository is readable.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.rate_limited.is_none()
            && self
                .repos
                .iter()
                .all(|c| matches!(c.outcome, CheckOutcome::Readable { .. }))
    }

    /// Whether upstream refused the credential.
    #[must_use]
    pub fn refused(&self) -> bool {
        self.repos
            .iter()
            .any(|c| c.outcome == CheckOutcome::Refused)
    }

    /// Scopes that let a classic token write: `repo`, `public_repo`, or any `write:`/`admin:`
    /// scope, and `workflow` and `delete_repo`.
    #[must_use]
    pub fn broad_scopes(&self) -> Vec<String> {
        self.token_scopes
            .iter()
            .flatten()
            .filter(|scope| {
                matches!(
                    scope.as_str(),
                    "repo" | "public_repo" | "workflow" | "delete_repo"
                ) || scope.starts_with("write:")
                    || scope.starts_with("admin:")
            })
            .cloned()
            .collect()
    }
}

#[derive(Deserialize, Default)]
struct WirePermissions {
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    maintain: bool,
    #[serde(default)]
    push: bool,
}

#[derive(Deserialize)]
struct WireRepo {
    #[serde(default)]
    permissions: Option<WirePermissions>,
}

/// Reads each repository of `config` once, in order, with `config`'s token. Stops at a refused
/// credential or a rate limit: the rest are [`CheckOutcome::NotChecked`]. Uses `config.now_unix`
/// to turn a `retry-after` into [`RateLimited::until`], as `sync` does.
pub async fn probe<T: Transport>(transport: &T, config: &SyncConfig) -> ProbeReport {
    let mut client = GithubClient::new(transport, config.token.clone());
    if let Some(base) = &config.api_base {
        client = client.with_api_base(base.clone());
    }
    let mut report = ProbeReport::default();
    let mut stopped = false;
    for repo in &config.repos {
        if stopped {
            report.repos.push(RepoCheck {
                repo: repo.clone(),
                outcome: CheckOutcome::NotChecked,
            });
            continue;
        }
        let url = format!("{}/repos/{}", client.api_base(), repo.as_str());
        let outcome = match client.get(&url, None, None).await {
            Err(e) => CheckOutcome::Failed(e.to_string()),
            Ok(raw) => {
                if report.token_scopes.is_none() {
                    report.token_scopes = raw.oauth_scopes.as_deref().map(|scopes| {
                        scopes
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect()
                    });
                }
                match raw.status {
                    200 => {
                        let permissions = serde_json::from_slice::<WireRepo>(&raw.body)
                            .ok()
                            .and_then(|r| r.permissions)
                            .unwrap_or_default();
                        CheckOutcome::Readable {
                            can_write: permissions.admin
                                || permissions.maintain
                                || permissions.push,
                        }
                    }
                    401 => {
                        stopped = true;
                        CheckOutcome::Refused
                    }
                    404 => CheckOutcome::NotFound,
                    403 | 429 => {
                        if raw.ratelimit_remaining == Some(0)
                            && let Some(reset) = raw.ratelimit_reset
                        {
                            report.rate_limited = Some(RateLimited {
                                until: reset,
                                secondary: false,
                            });
                            stopped = true;
                            CheckOutcome::NotChecked
                        } else if raw.retry_after.is_some() || body_mentions_rate_limit(&raw.body) {
                            let wait = raw.retry_after.map_or_else(
                                || backoff_secs(1),
                                |s| s.clamp(0, SECONDARY_BACKOFF_CAP_SECS),
                            );
                            report.rate_limited = Some(RateLimited {
                                until: config.now_unix.saturating_add(wait),
                                secondary: true,
                            });
                            stopped = true;
                            CheckOutcome::NotChecked
                        } else {
                            CheckOutcome::Forbidden
                        }
                    }
                    status => CheckOutcome::Failed(format!("unexpected status {status}")),
                }
            }
        };
        report.repos.push(RepoCheck {
            repo: repo.clone(),
            outcome,
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{RecordedExchange, ReplayTransport};
    use crate::time::GithubTimestamp;
    use crate::transport::AuthToken;

    const TOKEN: &str = "synthetic-probe-credential";

    fn config(repos: &[&str]) -> SyncConfig {
        SyncConfig {
            repos: repos.iter().map(|r| RepoRef::new(*r).unwrap()).collect(),
            token: AuthToken::new(TOKEN),
            now_unix: 1_000,
            now: GithubTimestamp::new("2026-01-01T00:00:00Z"),
            api_base: None,
        }
    }

    fn exchange(repo: &str, status: u16, headers: &[(&str, &str)], body: &str) -> RecordedExchange {
        RecordedExchange {
            method: "GET".into(),
            url: format!("https://api.github.com/repos/{repo}"),
            request_headers: vec![],
            status,
            response_headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[tokio::test]
    async fn readable_repositories_and_what_the_credential_could_do() {
        let transport = ReplayTransport::from_exchanges(vec![
            exchange(
                "example-org/demo-repo",
                200,
                &[("X-OAuth-Scopes", "repo, read:org")],
                r#"{"full_name":"example-org/demo-repo","permissions":{"admin":false,"push":true,"pull":true}}"#,
            ),
            exchange(
                "example-org/docs",
                200,
                &[],
                r#"{"full_name":"example-org/docs","permissions":{"pull":true}}"#,
            ),
            exchange("example-org/hidden", 404, &[], r#"{"message":"Not Found"}"#),
        ]);
        let report = probe(
            &transport,
            &config(&[
                "example-org/demo-repo",
                "example-org/docs",
                "example-org/hidden",
            ]),
        )
        .await;
        assert_eq!(
            report
                .repos
                .iter()
                .map(|c| c.outcome.clone())
                .collect::<Vec<_>>(),
            vec![
                CheckOutcome::Readable { can_write: true },
                CheckOutcome::Readable { can_write: false },
                CheckOutcome::NotFound,
            ]
        );
        assert!(!report.ok());
        assert_eq!(report.broad_scopes(), vec!["repo".to_string()]);
        // Only reads, each with the token as a bearer header and GitHub's API version.
        for request in transport.requests_sent() {
            assert_eq!(request.method, crate::transport::Method::Get);
            assert_eq!(
                request.header("authorization"),
                Some(format!("Bearer {TOKEN}").as_str())
            );
            assert!(request.header("x-github-api-version").is_some());
            assert!(!format!("{request:?}").contains(TOKEN));
        }
    }

    #[tokio::test]
    async fn a_refused_credential_stops_the_probe() {
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            "example-org/demo-repo",
            401,
            &[],
            r#"{"message":"Bad credentials"}"#,
        )]);
        let report = probe(
            &transport,
            &config(&["example-org/demo-repo", "example-org/docs"]),
        )
        .await;
        assert!(report.refused());
        assert_eq!(report.repos[1].outcome, CheckOutcome::NotChecked);
        assert_eq!(transport.remaining(), 0);
        assert_eq!(transport.requests_sent().len(), 1);
    }

    #[tokio::test]
    async fn a_rate_limit_stops_the_probe_with_its_reset_time() {
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            "example-org/demo-repo",
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "5000"),
            ],
            r#"{"message":"API rate limit exceeded"}"#,
        )]);
        let report = probe(&transport, &config(&["example-org/demo-repo"])).await;
        assert_eq!(
            report.rate_limited,
            Some(RateLimited {
                until: 5000,
                secondary: false
            })
        );
        assert!(!report.ok());

        let transport = ReplayTransport::from_exchanges(vec![exchange(
            "example-org/demo-repo",
            403,
            &[("retry-after", "999999999")],
            r#"{"message":"You have exceeded a secondary rate limit"}"#,
        )]);
        let report = probe(&transport, &config(&["example-org/demo-repo"])).await;
        assert_eq!(
            report.rate_limited,
            Some(RateLimited {
                until: 1_000 + SECONDARY_BACKOFF_CAP_SECS,
                secondary: true
            })
        );
    }

    #[tokio::test]
    async fn a_plain_forbidden_is_not_a_rate_limit() {
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            "example-org/demo-repo",
            403,
            &[],
            r#"{"message":"Resource not accessible by personal access token"}"#,
        )]);
        let report = probe(&transport, &config(&["example-org/demo-repo"])).await;
        assert_eq!(report.repos[0].outcome, CheckOutcome::Forbidden);
        assert_eq!(report.rate_limited, None);
    }

    #[tokio::test]
    async fn a_transport_failure_is_reported_without_the_token() {
        let transport = ReplayTransport::from_exchanges(vec![]);
        let report = probe(&transport, &config(&["example-org/demo-repo"])).await;
        match &report.repos[0].outcome {
            CheckOutcome::Failed(message) => assert!(!message.contains(TOKEN)),
            other => panic!("unexpected {other:?}"),
        }
    }
}
