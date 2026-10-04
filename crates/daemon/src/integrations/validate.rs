//! `POST /v1/integrations`'s checks (api-v1.md, "Integrations"): a [`NewIntegration`] becomes the
//! settings the hub keeps, trimmed and normalised, or a `400` naming what is wrong. Conflicts with
//! other connections (`409`) are checked by the caller, which knows them.

use pitcrew_hub_work::links::is_github_repo;
use pitcrew_protocol::integrations::{
    CredentialSource, DEFAULT_INTERVAL_MINUTES, INTERVAL_MINUTES, IntegrationSettings,
    JiraDeployment, MAX_SCOPES, NewIntegration,
};
use pitcrew_sync_jira::ProjectRef;
use std::collections::HashSet;

/// The longest name, in characters.
const NAME_CHARS: usize = 80;
/// The longest URL, in bytes.
const URL_BYTES: usize = 2048;
/// The longest e-mail, in characters.
const EMAIL_CHARS: usize = 254;
/// github.com's API root, which `api_base` leaves out.
const GITHUB_API: &str = "https://api.github.com";

/// A checked [`NewIntegration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub name: String,
    pub settings: IntegrationSettings,
    pub credential: CredentialSource,
    pub interval_minutes: u32,
}

/// An `https://` root with no user name, password, query or fragment, at most [`URL_BYTES`],
/// without its trailing `/`.
fn https_root(value: &str, field: &str, path_allowed: bool) -> Result<String, String> {
    let bad = || {
        format!(
            "{field} must be an https:// URL of at most {URL_BYTES} bytes, with no user name, \
             password, query or fragment{}.",
            if path_allowed { "" } else { " and no path" }
        )
    };
    let value = value.trim();
    if value.len() > URL_BYTES || value.chars().any(char::is_control) {
        return Err(bad());
    }
    let url = url::Url::parse(value).map_err(|_| bad())?;
    if url.scheme() != "https"
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (!path_allowed && url.path() != "/")
    {
        return Err(bad());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn distinct(
    values: &[String],
    field: &str,
    valid: impl Fn(&str) -> bool,
    rule: &str,
) -> Result<Vec<String>, String> {
    if values.is_empty() || values.len() > MAX_SCOPES {
        return Err(format!("{field} needs 1 to {MAX_SCOPES} entries."));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(values.len());
    for (i, value) in values.iter().enumerate() {
        let value = value.trim();
        if !valid(value) {
            return Err(format!("{field}[{i}] must be {rule}."));
        }
        if !seen.insert(value.to_ascii_lowercase()) {
            return Err(format!("{field}[{i}] repeats an entry."));
        }
        out.push(value.to_owned());
    }
    Ok(out)
}

/// Checks `new`.
///
/// # Errors
/// What is wrong, for a `400`.
pub fn check(new: NewIntegration) -> Result<Checked, String> {
    let name = new.name.trim();
    if name.is_empty() || name.chars().count() > NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!(
            "name must be 1 to {NAME_CHARS} characters after trimming, without control characters."
        ));
    }
    let interval_minutes = new.interval_minutes.unwrap_or(DEFAULT_INTERVAL_MINUTES);
    if !INTERVAL_MINUTES.contains(&interval_minutes) {
        return Err(format!(
            "interval_minutes must be {} to {}.",
            INTERVAL_MINUTES.start(),
            INTERVAL_MINUTES.end()
        ));
    }
    let settings = match new.settings {
        IntegrationSettings::Github { repos, api_base } => IntegrationSettings::Github {
            repos: distinct(&repos, "repos", is_github_repo, "owner/repo")?,
            // github.com's own API root is the default: kept as none, so its web host and the
            // host `gh` is asked about stay `github.com`.
            api_base: api_base
                .map(|base| https_root(&base, "api_base", true))
                .transpose()?
                .filter(|base| !base.eq_ignore_ascii_case(GITHUB_API)),
        },
        IntegrationSettings::Jira {
            deployment,
            site,
            projects,
            email,
            epic_link_field,
        } => {
            if new.credential == CredentialSource::GhCli {
                return Err("credential gh_cli is for GitHub only; Jira needs stored.".into());
            }
            let email = match (deployment, email) {
                (JiraDeployment::Cloud, Some(email)) => {
                    let email = email.trim().to_owned();
                    if email.chars().count() > EMAIL_CHARS
                        || !email.contains('@')
                        || email.starts_with('@')
                        || email.ends_with('@')
                        || email.chars().any(|c| c.is_whitespace() || c.is_control())
                    {
                        return Err(format!(
                            "email must be an e-mail address of at most {EMAIL_CHARS} characters."
                        ));
                    }
                    Some(email)
                }
                (JiraDeployment::Cloud, None) => {
                    return Err("email is required for Jira Cloud.".into());
                }
                (JiraDeployment::DataCenter, Some(_)) => {
                    return Err(
                        "email is for Jira Cloud only; Data Center uses a personal access token."
                            .into(),
                    );
                }
                (JiraDeployment::DataCenter, None) => None,
            };
            let epic_link_field = match epic_link_field {
                Some(field) => {
                    let field = field.trim().to_owned();
                    let digits = field.strip_prefix("customfield_").unwrap_or_default();
                    if digits.is_empty()
                        || digits.len() > 18
                        || !digits.bytes().all(|b| b.is_ascii_digit())
                    {
                        return Err("epic_link_field must be customfield_<digits>.".into());
                    }
                    Some(field)
                }
                None => None,
            };
            IntegrationSettings::Jira {
                deployment,
                site: https_root(&site, "site", true)?,
                projects: distinct(
                    &projects,
                    "projects",
                    |p| ProjectRef::new(p).is_ok(),
                    "a Jira project key (2 to 10 uppercase letters or digits, a letter first)",
                )?,
                email,
                epic_link_field,
            }
        }
    };
    Ok(Checked {
        name: name.to_owned(),
        settings,
        credential: new.credential,
        interval_minutes,
    })
}

/// The scopes `settings` syncs, for telling two connections apart: `github:<api base>:<owner/repo>`
/// or `jira:<site>:<KEY>`, lower-cased.
#[must_use]
pub fn scope_keys(settings: &IntegrationSettings) -> Vec<String> {
    match settings {
        IntegrationSettings::Github { repos, api_base } => {
            let base = api_base.as_deref().unwrap_or("https://api.github.com");
            repos
                .iter()
                .map(|r| format!("github:{base}:{r}").to_ascii_lowercase())
                .collect()
        }
        IntegrationSettings::Jira { site, projects, .. } => projects
            .iter()
            .map(|p| format!("jira:{site}:{p}").to_ascii_lowercase())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(repos: &[&str], api_base: Option<&str>) -> NewIntegration {
        NewIntegration {
            name: " Demo ".into(),
            settings: IntegrationSettings::Github {
                repos: repos.iter().map(|r| (*r).to_string()).collect(),
                api_base: api_base.map(str::to_string),
            },
            credential: CredentialSource::GhCli,
            interval_minutes: None,
        }
    }

    fn jira(deployment: JiraDeployment, email: Option<&str>) -> NewIntegration {
        NewIntegration {
            name: "Jira".into(),
            settings: IntegrationSettings::Jira {
                deployment,
                site: "https://jira.example.com/".into(),
                projects: vec!["DEMO".into()],
                email: email.map(str::to_string),
                epic_link_field: None,
            },
            credential: CredentialSource::Stored,
            interval_minutes: Some(30),
        }
    }

    #[test]
    fn a_good_connection_is_trimmed_and_normalised() {
        let checked = check(github(
            &["example-org/demo-repo"],
            Some("https://ghe.example.com/api/v3/"),
        ))
        .unwrap();
        assert_eq!(checked.name, "Demo");
        assert_eq!(checked.interval_minutes, DEFAULT_INTERVAL_MINUTES);
        assert_eq!(
            checked.settings,
            IntegrationSettings::Github {
                repos: vec!["example-org/demo-repo".into()],
                api_base: Some("https://ghe.example.com/api/v3".into()),
            }
        );
        let checked = check(jira(JiraDeployment::Cloud, Some("sam@example.com"))).unwrap();
        assert!(matches!(
            checked.settings,
            IntegrationSettings::Jira { ref site, .. } if site == "https://jira.example.com"
        ));
        check(jira(JiraDeployment::DataCenter, None)).unwrap();
    }

    #[test]
    fn malformed_connections_are_refused() {
        for bad in [
            github(&[], None),
            github(&["example-org"], None),
            github(&["example-org/demo-repo", "Example-Org/Demo-Repo"], None),
            github(&["../etc/passwd"], None),
            github(
                &["example-org/demo-repo"],
                Some("http://ghe.example.com/api/v3"),
            ),
            github(
                &["example-org/demo-repo"],
                Some("https://u:p@ghe.example.com/api/v3"),
            ),
            github(
                &["example-org/demo-repo"],
                Some("https://ghe.example.com/api/v3?x=1"),
            ),
            jira(JiraDeployment::Cloud, None),
            jira(JiraDeployment::Cloud, Some("no-at-sign")),
            jira(JiraDeployment::DataCenter, Some("sam@example.com")),
            NewIntegration {
                credential: CredentialSource::GhCli,
                ..jira(JiraDeployment::DataCenter, None)
            },
            NewIntegration {
                name: "  ".into(),
                ..github(&["example-org/demo-repo"], None)
            },
            NewIntegration {
                interval_minutes: Some(1),
                ..github(&["example-org/demo-repo"], None)
            },
            NewIntegration {
                settings: IntegrationSettings::Jira {
                    deployment: JiraDeployment::DataCenter,
                    site: "https://jira.example.com".into(),
                    projects: vec!["DEMO\" OR 1=1".into()],
                    email: None,
                    epic_link_field: None,
                },
                ..jira(JiraDeployment::DataCenter, None)
            },
        ] {
            assert!(check(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn scope_keys_tell_connections_apart() {
        let a = check(github(&["example-org/demo-repo"], None)).unwrap();
        assert_eq!(
            scope_keys(&a.settings),
            vec!["github:https://api.github.com:example-org/demo-repo".to_string()]
        );
    }

    #[test]
    fn githubs_own_api_root_is_the_default() {
        for base in ["https://api.github.com", "https://API.github.com/"] {
            let checked = check(github(&["example-org/demo-repo"], Some(base))).unwrap();
            assert!(
                matches!(
                    checked.settings,
                    IntegrationSettings::Github { api_base: None, .. }
                ),
                "{base}"
            );
        }
    }
}
