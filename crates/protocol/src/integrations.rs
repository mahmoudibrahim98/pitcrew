//! Integrations: GitHub and Jira connections, read-only (api-v1.md, "Integrations").
//!
//! A person connects repositories or Jira projects to the workspace and links workstreams to
//! upstream scopes (`Workstream::external`); the hub's sync then keeps tasks in step with upstream.
//! Nothing here ever carries a credential: [`CredentialInfo`] only says where one comes from and
//! whether one is kept.

use crate::ids::{IntegrationId, MemberId, WorkstreamId};
use crate::model::{ExternalRef, TimestampMs};
use serde::{Deserialize, Serialize};

/// The longest secret `PUT /v1/integrations/{id}/credential` accepts, in characters.
pub const MAX_SECRET_CHARS: usize = 4096;
/// The most repositories or Jira projects one integration syncs.
pub const MAX_SCOPES: usize = 50;
/// The most links one workstream has (`Workstream::external`).
pub const MAX_LINKS: usize = 16;
/// The shortest and longest sync interval, in minutes.
pub const INTERVAL_MINUTES: std::ops::RangeInclusive<u32> = 5..=1440;
/// The sync interval when `NewIntegration::interval_minutes` is absent.
pub const DEFAULT_INTERVAL_MINUTES: u32 = 15;

/// Which Jira a connection talks to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum JiraDeployment {
    /// Jira Cloud: REST v3, an account e-mail and an API token.
    Cloud,
    /// Jira Data Center: REST v2, a personal access token.
    DataCenter,
}

/// What an integration syncs. On the wire, tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationSettings {
    /// GitHub repositories.
    Github {
        /// `owner/repo`, distinct.
        repos: Vec<String>,
        /// A GitHub Enterprise Server API root; absent means `https://api.github.com`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        api_base: Option<String>,
    },
    /// Jira projects on one site.
    Jira {
        /// Cloud or Data Center.
        deployment: JiraDeployment,
        /// The site's root, e.g. `https://jira.example.com`.
        site: String,
        /// Project keys, distinct.
        projects: Vec<String>,
        /// The account e-mail (Cloud only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        email: Option<String>,
        /// Data Center's epic link field, e.g. `customfield_10008`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        epic_link_field: Option<String>,
    },
}

/// Where an integration's credential comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// `gh auth token` on the hub's machine, read at each sync and never kept. GitHub only.
    GhCli,
    /// A secret handed to the hub once (`PUT /v1/integrations/{id}/credential`) and kept private.
    Stored,
}

/// What a route may say about a credential: never the credential itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CredentialInfo {
    /// Where it comes from.
    pub source: CredentialSource,
    /// Whether a secret is kept (always `false` for `gh_cli`).
    pub stored: bool,
}

/// `POST /v1/integrations`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewIntegration {
    /// A name for people, 1–80 characters.
    pub name: String,
    /// What to sync.
    pub settings: IntegrationSettings,
    /// Where the credential comes from.
    pub credential: CredentialSource,
    /// Minutes between syncs, 5–1440; 15 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub interval_minutes: Option<u32>,
}

/// `PUT /v1/integrations/{id}/credential`. Its `Debug` never shows the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewCredential {
    /// The secret: a token, an API token or a personal access token.
    pub secret: String,
}

impl std::fmt::Debug for NewCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NewCredential { secret: *** }")
    }
}

/// A problem the last sync met.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SyncProblem {
    /// A repository, a Jira project, or `""` for the whole integration.
    pub scope: String,
    /// For people to read. Never holds a credential.
    pub message: String,
}

/// What the last sync did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SyncCounts {
    /// Upstream changes read.
    pub changes: u32,
    /// Changes the hub applied (tasks created, updated or moved; workstreams shipped; notes).
    pub applied: u32,
    /// Conflicts raised as asks.
    pub conflicts: u32,
    /// Changes outside every linked scope, or about issues closed before they were first seen.
    pub skipped: u32,
    /// Upstream items or fields that were malformed and skipped.
    pub malformed: u32,
}

/// Where an integration's sync stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SyncStatus {
    /// A sync is under way.
    pub running: bool,
    /// When the last sync started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_attempt_at: Option<TimestampMs>,
    /// When the last sync that read every scope and applied what it found without a problem
    /// ended: the "last sync" people see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_success_at: Option<TimestampMs>,
    /// When the next sync is due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub next_at: Option<TimestampMs>,
    /// Upstream asked the hub to wait until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub rate_limited_until: Option<TimestampMs>,
    /// The last sync's problems.
    #[serde(default)]
    pub problems: Vec<SyncProblem>,
    /// What the last sync did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_run: Option<SyncCounts>,
}

/// A workstream link an integration syncs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IntegrationLink {
    /// The workstream.
    pub workstream: WorkstreamId,
    /// The upstream scope it links: a repository, a milestone, a Jira project or an epic.
    pub scope: ExternalRef,
    /// The upstream title, once a sync has seen it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
}

/// A connection to GitHub or Jira, as the routes return it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Integration {
    /// Id.
    pub id: IntegrationId,
    /// Name.
    pub name: String,
    /// What it syncs.
    pub settings: IntegrationSettings,
    /// Where its credential comes from.
    pub credential: CredentialInfo,
    /// Minutes between syncs.
    pub interval_minutes: u32,
    /// The person who added it; conflicts are asked of them.
    pub added_by: MemberId,
    /// When.
    pub added_at: TimestampMs,
    /// Its sync.
    pub status: SyncStatus,
    /// The workstream links it syncs.
    #[serde(default)]
    pub links: Vec<IntegrationLink>,
}

/// One check of `POST /v1/integrations/{id}/test`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScopeCheck {
    /// A repository, a Jira project, or `""` for the credential.
    pub scope: String,
    /// It passed.
    pub ok: bool,
    /// For people to read.
    pub message: String,
}

/// `POST /v1/integrations/{id}/test`: one read of upstream with the credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IntegrationCheck {
    /// Every check passed.
    pub ok: bool,
    /// When.
    pub at: TimestampMs,
    /// The checks.
    pub checks: Vec<ScopeCheck>,
    /// The credential can do more than read.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_tagged_by_kind() {
        let github = IntegrationSettings::Github {
            repos: vec!["example-org/demo-repo".into()],
            api_base: None,
        };
        assert_eq!(
            serde_json::to_value(&github).unwrap(),
            serde_json::json!({"kind": "github", "repos": ["example-org/demo-repo"]})
        );
        let jira: IntegrationSettings = serde_json::from_value(serde_json::json!({
            "kind": "jira", "deployment": "data_center", "site": "https://jira.example.com",
            "projects": ["DEMO"]
        }))
        .unwrap();
        assert!(matches!(
            jira,
            IntegrationSettings::Jira {
                deployment: JiraDeployment::DataCenter,
                ..
            }
        ));
    }

    #[test]
    fn a_new_credential_never_shows_its_secret() {
        let credential = NewCredential {
            secret: "synthetic-secret-value".into(),
        };
        assert!(!format!("{credential:?}").contains("synthetic-secret-value"));
    }
}
