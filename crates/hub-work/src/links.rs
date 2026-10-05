//! A workstream's links upstream (`Workstream::external`, api-v1.md "Linking a workstream
//! upstream"): their checks, and what each GitHub or Jira link names ([`LinkScope`]), which is
//! what a tracker sync routes issues by. A link whose key names no scope is kept as a plain link;
//! no sync acts on it.

use crate::error::{Result, WorkError};
use pitcrew_protocol::model::{ExternalRef, ExternalSystem};
use pitcrew_protocol::text::is_hidden;
use std::collections::HashSet;

/// The most links a workstream has.
pub const MAX_LINKS: usize = pitcrew_protocol::integrations::MAX_LINKS;
fn contains_hidden(text: &str) -> bool {
    text.chars().any(is_hidden)
}

/// The longest link key, in characters.
pub const MAX_KEY_CHARS: usize = 300;
/// The longest link URL, in bytes.
pub const MAX_URL_BYTES: usize = 2048;

/// What a GitHub or Jira link names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LinkScope {
    /// A whole GitHub repository, `owner/repo`.
    GithubRepo {
        /// `owner/repo`.
        repo: String,
    },
    /// One milestone of a repository, `owner/repo#milestone:<n>`.
    GithubMilestone {
        /// `owner/repo`.
        repo: String,
        /// The milestone's number.
        number: u64,
    },
    /// A whole Jira project, `DEMO`.
    JiraProject {
        /// The project key.
        project: String,
    },
    /// One Jira epic, `DEMO-5`.
    JiraEpic {
        /// The project key.
        project: String,
        /// The epic's issue key.
        key: String,
    },
}

impl LinkScope {
    /// The repository or Jira project the scope is in.
    #[must_use]
    pub fn container(&self) -> &str {
        match self {
            Self::GithubRepo { repo } | Self::GithubMilestone { repo, .. } => repo,
            Self::JiraProject { project } | Self::JiraEpic { project, .. } => project,
        }
    }
}

/// An owner or repository name: GitHub's characters, never `.` or `..` alone.
fn github_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Whether `repo` is `owner/repo` with GitHub's characters.
#[must_use]
pub fn is_github_repo(repo: &str) -> bool {
    repo.split_once('/')
        .is_some_and(|(owner, name)| github_name(owner) && github_name(name))
}

/// Whether `key` is a Jira project key: an uppercase letter, then uppercase letters, digits or
/// `_`, at most 255 characters.
#[must_use]
pub fn is_jira_project(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        && key.len() <= 255
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// What `link` names, for `github` and `jira` links with a well-formed key; `None` for anything
/// else (other systems, or a key of another shape).
#[must_use]
pub fn scope_of(link: &ExternalRef) -> Option<LinkScope> {
    match link.system {
        ExternalSystem::Github => match link.key.split_once("#milestone:") {
            Some((repo, number)) => {
                let digits = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
                let number = number.parse::<u64>().ok().filter(|_| digits)?;
                is_github_repo(repo).then(|| LinkScope::GithubMilestone {
                    repo: repo.to_owned(),
                    number,
                })
            }
            None => is_github_repo(&link.key).then(|| LinkScope::GithubRepo {
                repo: link.key.clone(),
            }),
        },
        ExternalSystem::Jira => match link.key.rsplit_once('-') {
            Some((project, number)) => {
                let digits = !number.is_empty()
                    && number.len() <= 18
                    && number.bytes().all(|b| b.is_ascii_digit());
                (digits && is_jira_project(project)).then(|| LinkScope::JiraEpic {
                    project: project.to_owned(),
                    key: link.key.clone(),
                })
            }
            None => is_jira_project(&link.key).then(|| LinkScope::JiraProject {
                project: link.key.clone(),
            }),
        },
        _ => None,
    }
}

/// Whether `url` is a link worth keeping: `https://`, at most [`MAX_URL_BYTES`], no user name or
/// password, no hidden characters.
#[must_use]
pub fn is_safe_url(url: &str) -> bool {
    if url.len() > MAX_URL_BYTES || contains_hidden(url) || url.chars().any(char::is_control) {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty() && !authority.contains('@') && !authority.contains('\\')
}

/// Checks a workstream's new links (api-v1.md, "Linking a workstream upstream").
///
/// # Errors
///
/// `invalid`, naming the first link that breaks a rule.
pub fn check_links(links: &[ExternalRef]) -> Result<()> {
    if links.len() > MAX_LINKS {
        return Err(WorkError::invalid(format!(
            "A workstream has at most {MAX_LINKS} links."
        )));
    }
    let mut seen = HashSet::with_capacity(links.len());
    for (i, link) in links.iter().enumerate() {
        let key = &link.key;
        if key.is_empty()
            || key.len() > MAX_KEY_CHARS * 4
            || key.chars().count() > MAX_KEY_CHARS
            || key.chars().any(char::is_control)
            || contains_hidden(key)
        {
            return Err(WorkError::invalid(format!(
                "external[{i}].key must be 1 to {MAX_KEY_CHARS} characters, without control or \
                 hidden characters."
            )));
        }
        if let Some(url) = &link.url
            && !is_safe_url(url)
        {
            return Err(WorkError::invalid(format!(
                "external[{i}].url must be an https:// URL of at most {MAX_URL_BYTES} bytes, \
                 without a user name or password."
            )));
        }
        if !seen.insert((link.system, key.as_str())) {
            return Err(WorkError::invalid(format!(
                "external[{i}] repeats a link already in the list."
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(system: ExternalSystem, key: &str) -> ExternalRef {
        ExternalRef {
            system,
            key: key.into(),
            url: None,
        }
    }

    #[test]
    fn github_and_jira_keys_name_their_scope() {
        assert_eq!(
            scope_of(&link(ExternalSystem::Github, "example-org/demo-repo")),
            Some(LinkScope::GithubRepo {
                repo: "example-org/demo-repo".into()
            })
        );
        assert_eq!(
            scope_of(&link(
                ExternalSystem::Github,
                "example-org/demo-repo#milestone:3"
            )),
            Some(LinkScope::GithubMilestone {
                repo: "example-org/demo-repo".into(),
                number: 3
            })
        );
        assert_eq!(
            scope_of(&link(ExternalSystem::Jira, "DEMO")),
            Some(LinkScope::JiraProject {
                project: "DEMO".into()
            })
        );
        assert_eq!(
            scope_of(&link(ExternalSystem::Jira, "DEMO-5")),
            Some(LinkScope::JiraEpic {
                project: "DEMO".into(),
                key: "DEMO-5".into()
            })
        );
        for (system, key) in [
            (ExternalSystem::Github, "example-org"),
            (ExternalSystem::Github, "../demo-repo"),
            (ExternalSystem::Github, "example-org/.."),
            (ExternalSystem::Github, "example-org/demo repo"),
            (ExternalSystem::Github, "example-org/demo-repo#milestone:"),
            (ExternalSystem::Github, "example-org/demo-repo#milestone:+1"),
            (ExternalSystem::Github, "example-org/demo-repo#3"),
            (ExternalSystem::Jira, "demo"),
            (ExternalSystem::Jira, "DEMO-"),
            (ExternalSystem::Jira, "DEMO-1a"),
            (ExternalSystem::Jira, "DEMO\" OR 1=1"),
            (ExternalSystem::Linear, "ENG-1"),
        ] {
            assert_eq!(scope_of(&link(system, key)), None, "{key}");
        }
    }

    #[test]
    fn links_are_checked() {
        let good = vec![
            ExternalRef {
                system: ExternalSystem::Github,
                key: "example-org/demo-repo#milestone:1".into(),
                url: Some("https://github.com/example-org/demo-repo/milestone/1".into()),
            },
            link(ExternalSystem::Jira, "DEMO-5"),
            link(ExternalSystem::Linear, "anything at all"),
        ];
        check_links(&good).unwrap();
        check_links(&[]).unwrap();

        let twice = vec![
            link(ExternalSystem::Jira, "DEMO"),
            link(ExternalSystem::Jira, "DEMO"),
        ];
        assert!(check_links(&twice).is_err());
        assert!(check_links(&vec![link(ExternalSystem::Jira, "DEMO"); MAX_LINKS + 1]).is_err());
        // A key of another shape is kept, as a plain link: no sync acts on it.
        check_links(&[link(ExternalSystem::Github, "demo-lab/paper milestone 2")]).unwrap();
        assert!(check_links(&[link(ExternalSystem::Linear, "")]).is_err());
        assert!(check_links(&[link(ExternalSystem::Linear, "a\u{202e}b")]).is_err());
        for url in [
            "http://github.com/example-org/demo-repo",
            "https://user:pass@github.com/example-org/demo-repo",
            "https://",
            "javascript:alert(1)",
        ] {
            let with_url = ExternalRef {
                system: ExternalSystem::Github,
                key: "example-org/demo-repo".into(),
                url: Some(url.into()),
            };
            assert!(check_links(&[with_url]).is_err(), "{url}");
        }
    }
}
