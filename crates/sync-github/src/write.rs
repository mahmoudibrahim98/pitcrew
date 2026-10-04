//! GitHub, write side: the REST requests for an approved outward write (api-v1.md, "Outward
//! writes"), and what their answers mean.
//!
//! This module never decides *whether* to write: the hub calls [`send`] only for a write a person
//! approved (`pitcrewd`'s `integrations/writes.rs`, gated by `pitcrew_hub_work`'s
//! `SyncCommands::start_write`). It builds exactly one request from an [`IssueWrite`] and sends it
//! once: no retry, no redirect, no second request. The sync itself ([`crate::sync::sync`]) still
//! only ever sends `GET`s.
//!
//! - [`IssueWrite::Create`]: `POST /repos/{owner}/{repo}/issues`;
//! - [`IssueWrite::Comment`]: `POST /repos/{owner}/{repo}/issues/{n}/comments`;
//! - [`IssueWrite::Edit`]: `PATCH /repos/{owner}/{repo}/issues/{n}` with only the fields it sets
//!   (title, body, labels, milestone, state and its reason).
//!
//! Answers are untrusted: a refusal's message is capped and stripped of hidden characters, a
//! created issue's number must be a positive integer, and its `html_url` is kept only when it is
//! on GitHub's own web host (the rules the read side uses).

use crate::bounds::{cap_chars, strip_hidden};
use crate::change::{expected_web_origin, trusted_html_url};
use crate::client::{API_VERSION, DEFAULT_API_BASE};
use crate::state::CloseReason;
use crate::sync::RepoRef;
use crate::transport::{AuthToken, Method, Request, Response, Transport};
use serde::Serialize;

/// The longest refusal message kept from an answer, in characters.
pub const MAX_MESSAGE_CHARS: usize = 300;

/// Where and as whom to write.
#[derive(Clone, Debug)]
pub struct WriteConfig {
    /// The API root; `None` is `https://api.github.com`.
    pub api_base: Option<String>,
    /// The credential.
    pub token: AuthToken,
}

impl WriteConfig {
    fn api(&self) -> &str {
        self.api_base
            .as_deref()
            .unwrap_or(DEFAULT_API_BASE)
            .trim_end_matches('/')
    }
}

/// Open or close an issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateChange {
    /// Close it, for this reason.
    Close(CloseReason),
    /// Reopen it.
    Reopen,
}

/// The fields an edit sets; `None` leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueEdit {
    /// New title.
    pub title: Option<String>,
    /// New body.
    pub body: Option<String>,
    /// The whole new label list.
    pub labels: Option<Vec<String>>,
    /// The milestone's number.
    pub milestone: Option<u64>,
    /// Close or reopen.
    pub state: Option<StateChange>,
}

/// One write to one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueWrite {
    /// Create an issue.
    Create {
        /// The repository.
        repo: RepoRef,
        /// Title.
        title: String,
        /// Body.
        body: String,
        /// Labels.
        labels: Vec<String>,
        /// The milestone's number.
        milestone: Option<u64>,
    },
    /// Comment on an issue.
    Comment {
        /// The repository.
        repo: RepoRef,
        /// The issue's number.
        number: u64,
        /// The comment.
        body: String,
    },
    /// Change an issue.
    Edit {
        /// The repository.
        repo: RepoRef,
        /// The issue's number.
        number: u64,
        /// What to set.
        edit: IssueEdit,
    },
}

/// What upstream says it wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// The created issue's number (`Create` only).
    pub number: Option<u64>,
    /// A link to what was written: the issue, or the comment. Kept only when trusted.
    pub url: Option<String>,
}

/// Why a write did not go through. Never holds the credential.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// GitHub answered with an error status.
    #[error("GitHub refused it ({status}): {message}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// GitHub's message, capped and stripped of hidden characters.
        message: String,
    },
    /// The request could not be sent, or its answer read.
    #[error("{0}")]
    Unreachable(String),
    /// GitHub accepted it, but its answer does not say what it created.
    #[error("GitHub's answer could not be read: {0}")]
    Malformed(String),
}

impl WriteError {
    /// The HTTP status, when GitHub answered.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Refused { status, .. } => Some(*status),
            Self::Unreachable(_) | Self::Malformed(_) => None,
        }
    }
}

#[derive(Serialize)]
struct CreateBody<'a> {
    title: &'a str,
    body: &'a str,
    labels: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    milestone: Option<u64>,
}

#[derive(Serialize)]
struct CommentBody<'a> {
    body: &'a str,
}

#[derive(Serialize, Default)]
struct EditBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    milestone: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_reason: Option<&'static str>,
}

fn json_body<T: Serialize>(value: &T) -> Vec<u8> {
    // Serializing these plain structs of strings and numbers cannot fail.
    serde_json::to_vec(value).unwrap_or_default()
}

/// The one request `write` is sent as. The credential goes only in `Authorization`.
#[must_use]
pub fn request(config: &WriteConfig, write: &IssueWrite) -> Request {
    let api = config.api();
    let (method, url, body) = match write {
        IssueWrite::Create {
            repo,
            title,
            body,
            labels,
            milestone,
        } => (
            Method::Post,
            format!("{api}/repos/{}/issues", repo.as_str()),
            json_body(&CreateBody {
                title,
                body,
                labels,
                milestone: *milestone,
            }),
        ),
        IssueWrite::Comment { repo, number, body } => (
            Method::Post,
            format!("{api}/repos/{}/issues/{number}/comments", repo.as_str()),
            json_body(&CommentBody { body }),
        ),
        IssueWrite::Edit { repo, number, edit } => {
            let (state, state_reason) = match edit.state {
                None => (None, None),
                Some(StateChange::Reopen) => (Some("open"), None),
                Some(StateChange::Close(CloseReason::Completed)) => {
                    (Some("closed"), Some("completed"))
                }
                Some(StateChange::Close(CloseReason::NotPlanned)) => {
                    (Some("closed"), Some("not_planned"))
                }
            };
            (
                Method::Patch,
                format!("{api}/repos/{}/issues/{number}", repo.as_str()),
                json_body(&EditBody {
                    title: edit.title.as_deref(),
                    body: edit.body.as_deref(),
                    labels: edit.labels.as_deref(),
                    milestone: edit.milestone,
                    state,
                    state_reason,
                }),
            )
        }
    };
    Request {
        method,
        url,
        headers: vec![
            (
                "Accept".to_string(),
                "application/vnd.github+json".to_string(),
            ),
            ("X-GitHub-Api-Version".to_string(), API_VERSION.to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), config.token.header_value()),
        ],
        body,
    }
}

/// GitHub's error message, made safe to show: its JSON `message` (else a fixed text), stripped of
/// hidden characters and capped.
fn refusal_message(body: &[u8]) -> String {
    let message = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "no message".to_owned());
    let clean: String = strip_hidden(&message)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cap_chars(clean.trim(), MAX_MESSAGE_CHARS)
}

/// What `response` says about `write`.
///
/// # Errors
///
/// [`WriteError::Refused`] for any status but 2xx; [`WriteError::Malformed`] for a created issue
/// whose answer has no positive `number`.
pub fn read_response(
    config: &WriteConfig,
    write: &IssueWrite,
    response: &Response,
) -> Result<Written, WriteError> {
    if !(200..300).contains(&response.status) {
        return Err(WriteError::Refused {
            status: response.status,
            message: refusal_message(&response.body),
        });
    }
    let answer: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
    let web = expected_web_origin(config.api_base.as_deref());
    let url = answer
        .get("html_url")
        .and_then(|u| u.as_str())
        .and_then(|u| trusted_html_url(u, &web))
        .map(|u| u.as_str().to_owned());
    match write {
        IssueWrite::Create { .. } => {
            let number = answer
                .get("number")
                .and_then(serde_json::Value::as_u64)
                .filter(|n| *n > 0)
                .ok_or_else(|| WriteError::Malformed("no issue number".into()))?;
            Ok(Written {
                number: Some(number),
                url,
            })
        }
        IssueWrite::Comment { .. } | IssueWrite::Edit { .. } => Ok(Written { number: None, url }),
    }
}

/// Sends `write` once through `transport` and reads the answer.
///
/// # Errors
///
/// See [`read_response`]; [`WriteError::Unreachable`] when the transport fails.
pub async fn send<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    write: &IssueWrite,
) -> Result<Written, WriteError> {
    let response = transport
        .send(request(config, write))
        .await
        .map_err(|e| WriteError::Unreachable(e.to_string()))?;
    read_response(config, write, &response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{RecordedExchange, ReplayTransport};

    fn repo() -> RepoRef {
        RepoRef::new("example-org/demo-repo").unwrap()
    }

    fn config() -> WriteConfig {
        WriteConfig {
            api_base: None,
            token: AuthToken::new("synthetic-write-credential"),
        }
    }

    fn answer(method: &str, url: &str, status: u16, body: &str) -> RecordedExchange {
        RecordedExchange {
            method: method.into(),
            url: url.into(),
            request_headers: vec![],
            status,
            response_headers: vec![],
            body: body.as_bytes().to_vec(),
        }
    }

    fn body(request: &Request) -> serde_json::Value {
        serde_json::from_slice(&request.body).unwrap()
    }

    #[tokio::test]
    async fn a_create_sends_exactly_its_fields_once_and_reads_the_new_number() {
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "POST",
            "https://api.github.com/repos/example-org/demo-repo/issues",
            201,
            r#"{"number":8,"html_url":"https://github.com/example-org/demo-repo/issues/8"}"#,
        )]);
        let write = IssueWrite::Create {
            repo: repo(),
            title: "Write the release notes".into(),
            body: "For v1.".into(),
            labels: vec!["docs".into()],
            milestone: Some(2),
        };
        let written = send(&transport, &config(), &write).await.unwrap();
        assert_eq!(written.number, Some(8));
        assert_eq!(
            written.url.as_deref(),
            Some("https://github.com/example-org/demo-repo/issues/8")
        );
        let sent = transport.requests_sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, Method::Post);
        assert_eq!(
            body(&sent[0]),
            serde_json::json!({"title": "Write the release notes", "body": "For v1.",
                "labels": ["docs"], "milestone": 2})
        );
        assert_eq!(
            sent[0].header("authorization"),
            Some("Bearer synthetic-write-credential")
        );
        assert!(!format!("{:?}", sent[0]).contains("synthetic-write-credential"));
        assert!(!sent[0].url.contains("synthetic-write-credential"));
    }

    #[tokio::test]
    async fn an_edit_sends_only_the_fields_it_sets() {
        let url = "https://api.github.com/repos/example-org/demo-repo/issues/1";
        let transport = ReplayTransport::from_exchanges(vec![
            answer("PATCH", url, 200, r#"{"number":1}"#),
            answer("PATCH", url, 200, r#"{"number":1}"#),
            answer("PATCH", url, 200, r#"{"number":1}"#),
        ]);
        let close = IssueWrite::Edit {
            repo: repo(),
            number: 1,
            edit: IssueEdit {
                state: Some(StateChange::Close(CloseReason::NotPlanned)),
                ..IssueEdit::default()
            },
        };
        let reopen = IssueWrite::Edit {
            repo: repo(),
            number: 1,
            edit: IssueEdit {
                state: Some(StateChange::Reopen),
                ..IssueEdit::default()
            },
        };
        let retitle = IssueWrite::Edit {
            repo: repo(),
            number: 1,
            edit: IssueEdit {
                title: Some("Fix the flaky login test".into()),
                labels: Some(vec![]),
                milestone: Some(1),
                ..IssueEdit::default()
            },
        };
        for write in [&close, &reopen, &retitle] {
            send(&transport, &config(), write).await.unwrap();
        }
        let sent = transport.requests_sent();
        assert_eq!(
            sent.iter().map(body).collect::<Vec<_>>(),
            vec![
                serde_json::json!({"state": "closed", "state_reason": "not_planned"}),
                serde_json::json!({"state": "open"}),
                serde_json::json!({"title": "Fix the flaky login test", "labels": [],
                    "milestone": 1}),
            ]
        );
        assert!(sent.iter().all(|r| r.method == Method::Patch));
    }

    #[tokio::test]
    async fn a_refusal_is_reported_with_a_safe_message_and_never_retried() {
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "POST",
            "https://api.github.com/repos/example-org/demo-repo/issues/1/comments",
            422,
            "{\"message\":\"Validation\u{202e} Failed\\n\"}",
        )]);
        let write = IssueWrite::Comment {
            repo: repo(),
            number: 1,
            body: "Done.".into(),
        };
        let err = send(&transport, &config(), &write).await.unwrap_err();
        assert_eq!(
            err,
            WriteError::Refused {
                status: 422,
                message: "Validation Failed".into()
            }
        );
        assert_eq!(err.status(), Some(422));
        assert_eq!(transport.requests_sent().len(), 1);
        // No fixture: unreachable, and still one request.
        let empty = ReplayTransport::from_exchanges(vec![]);
        assert!(matches!(
            send(&empty, &config(), &write).await,
            Err(WriteError::Unreachable(_))
        ));
        assert_eq!(empty.requests_sent().len(), 1);
    }

    #[tokio::test]
    async fn an_untrusted_link_or_a_missing_number_is_not_kept() {
        let transport = ReplayTransport::from_exchanges(vec![
            answer(
                "POST",
                "https://api.github.com/repos/example-org/demo-repo/issues",
                201,
                r#"{"number":9,"html_url":"https://evil.example.com/x"}"#,
            ),
            answer(
                "POST",
                "https://api.github.com/repos/example-org/demo-repo/issues",
                201,
                r#"{"html_url":"https://github.com/example-org/demo-repo/issues/9"}"#,
            ),
        ]);
        let write = IssueWrite::Create {
            repo: repo(),
            title: "t".into(),
            body: String::new(),
            labels: vec![],
            milestone: None,
        };
        let written = send(&transport, &config(), &write).await.unwrap();
        assert_eq!(
            written,
            Written {
                number: Some(9),
                url: None
            }
        );
        assert!(matches!(
            send(&transport, &config(), &write).await,
            Err(WriteError::Malformed(_))
        ));
    }

    #[test]
    fn an_enterprise_server_is_written_to_at_its_own_root() {
        let config = WriteConfig {
            api_base: Some("https://ghe.example.com/api/v3/".into()),
            token: AuthToken::new("synthetic"),
        };
        let write = IssueWrite::Comment {
            repo: repo(),
            number: 3,
            body: "x".into(),
        };
        assert_eq!(
            request(&config, &write).url,
            "https://ghe.example.com/api/v3/repos/example-org/demo-repo/issues/3/comments"
        );
    }
}
