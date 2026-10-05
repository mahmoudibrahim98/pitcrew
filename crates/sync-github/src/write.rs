//! GitHub, write side: the REST requests for an approved outward write (api-v1.md, "Outward
//! writes"), and what their answers mean.
//!
//! This module never decides *whether* to write: the hub calls [`send`] only for a write a person
//! approved (`pitcrewd`'s `integrations/writes.rs`, gated by `pitcrew_hub_work`'s
//! `SyncCommands::start_write`). It builds the requests of an [`IssueWrite`] ([`requests`]) and
//! sends each once, in order, stopping at the first refusal: no retry, no redirect. The sync
//! itself ([`crate::sync::sync`]) still only ever sends `GET`s.
//!
//! - [`IssueWrite::Create`]: `POST /repos/{owner}/{repo}/issues`;
//! - [`IssueWrite::Comment`]: `POST /repos/{owner}/{repo}/issues/{n}/comments`;
//! - [`IssueWrite::Edit`]: `PATCH /repos/{owner}/{repo}/issues/{n}` with only the fields it sets
//!   (title, body, milestone, state and its reason), then labels as a change, never the whole
//!   list: `POST …/issues/{n}/labels` with those to add, and `DELETE …/issues/{n}/labels/{name}`
//!   for each to remove. Labels it does not name are kept.
//!
//! Two reads serve a write, never the sync: [`read_issue`] (`GET …/issues/{n}`), which the hub
//! compares with what the write expects just before an edit, and [`find_earlier`], which looks
//! for an earlier attempt at a create or a comment before it is sent again.
//!
//! Answers are untrusted: a refusal's message is capped and stripped of hidden characters, a
//! created issue's number must be a positive integer, and its `html_url` is kept only when it is
//! on GitHub's own web host (the rules the read side uses).

use crate::bounds::{cap_chars, strip_hidden};
use crate::change::{expected_web_origin, trusted_html_url};
use crate::client::{API_VERSION, DEFAULT_API_BASE};
use crate::state::CloseReason;
use crate::sync::RepoRef;
use crate::time::GithubTimestamp;
use crate::transport::{AuthToken, Method, Request, Response, Transport};
use serde::Serialize;
use serde_json::Value;

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

/// The fields an edit sets; `None` (or an empty list) leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueEdit {
    /// New title.
    pub title: Option<String>,
    /// New body.
    pub body: Option<String>,
    /// Labels to add; the issue's other labels are kept.
    pub add_labels: Vec<String>,
    /// Labels to remove, as GitHub spells them; the issue's other labels are kept.
    pub remove_labels: Vec<String>,
    /// The milestone's number.
    pub milestone: Option<u64>,
    /// Close or reopen.
    pub state: Option<StateChange>,
}

impl IssueEdit {
    /// Whether it sets a field of the issue itself (the `PATCH`).
    fn patches(&self) -> bool {
        self.title.is_some()
            || self.body.is_some()
            || self.milestone.is_some()
            || self.state.is_some()
    }

    /// Whether it changes nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.patches() && self.add_labels.is_empty() && self.remove_labels.is_empty()
    }
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
    milestone: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_reason: Option<&'static str>,
}

#[derive(Serialize)]
struct LabelsBody<'a> {
    labels: &'a [String],
}

fn json_body<T: Serialize>(value: &T) -> Vec<u8> {
    // Serializing these plain structs of strings and numbers cannot fail.
    serde_json::to_vec(value).unwrap_or_default()
}

/// A request with the API's headers; `body` is JSON when given. The credential goes only in
/// `Authorization`.
fn build(config: &WriteConfig, method: Method, url: String, body: Option<Vec<u8>>) -> Request {
    let mut headers = vec![
        (
            "Accept".to_string(),
            "application/vnd.github+json".to_string(),
        ),
        ("X-GitHub-Api-Version".to_string(), API_VERSION.to_string()),
    ];
    if body.is_some() {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    headers.push(("Authorization".to_string(), config.token.header_value()));
    Request {
        method,
        url,
        headers,
        body: body.unwrap_or_default(),
    }
}

/// `{issue}/labels/{name}`, the name percent-encoded as one path segment.
fn label_url(issue: &str, name: &str) -> String {
    let mut url = match url::Url::parse(&format!("{issue}/labels")) {
        Ok(url) => url,
        // The API root is the integration's own setting, checked when it was added; a URL that
        // does not parse is refused by the transport, not sent.
        Err(_) => return format!("{issue}/labels/"),
    };
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.push(name);
    }
    url.to_string()
}

/// The requests `write` is sent as, in order: one for a create or a comment; for an edit, the
/// `PATCH` (when it sets a field of the issue), the `POST` of the labels to add, and one `DELETE`
/// per label to remove.
#[must_use]
pub fn requests(config: &WriteConfig, write: &IssueWrite) -> Vec<Request> {
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
            let issue = format!("{api}/repos/{}/issues/{number}", repo.as_str());
            let mut out = Vec::new();
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
            if edit.patches() {
                out.push(build(
                    config,
                    Method::Patch,
                    issue.clone(),
                    Some(json_body(&EditBody {
                        title: edit.title.as_deref(),
                        body: edit.body.as_deref(),
                        milestone: edit.milestone,
                        state,
                        state_reason,
                    })),
                ));
            }
            if !edit.add_labels.is_empty() {
                out.push(build(
                    config,
                    Method::Post,
                    format!("{issue}/labels"),
                    Some(json_body(&LabelsBody {
                        labels: &edit.add_labels,
                    })),
                ));
            }
            for name in &edit.remove_labels {
                out.push(build(config, Method::Delete, label_url(&issue, name), None));
            }
            return out;
        }
    };
    vec![build(config, method, url, Some(body))]
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

async fn exchange<T: Transport>(transport: &T, request: Request) -> Result<Response, WriteError> {
    transport
        .send(request)
        .await
        .map_err(|e| WriteError::Unreachable(e.to_string()))
}

/// Sends `write` through `transport`: each of its [`requests`] once, in order, stopping at the
/// first refusal. A label to remove that the issue no longer has (`404`) is already gone.
///
/// # Errors
///
/// See [`read_response`]; [`WriteError::Unreachable`] when the transport fails.
pub async fn send<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    write: &IssueWrite,
) -> Result<Written, WriteError> {
    let mut written = Written::default();
    for request in requests(config, write) {
        let method = request.method;
        let response = exchange(transport, request).await?;
        if method == Method::Delete && response.status == 404 {
            continue;
        }
        let part = read_response(config, write, &response)?;
        written.number = written.number.or(part.number);
        written.url = written.url.or(part.url);
    }
    Ok(written)
}

/// An issue as GitHub has it now, exactly as it answered: what an edit is checked against just
/// before it is sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueNow {
    /// Its title.
    pub title: String,
    /// Its body (empty when it has none).
    pub body: String,
    /// Its labels' names.
    pub labels: Vec<String>,
    /// Its milestone's number.
    pub milestone: Option<u64>,
    /// Whether it is open.
    pub open: bool,
    /// Its link, when trusted.
    pub url: Option<String>,
}

fn read_request(config: &WriteConfig, url: String) -> Request {
    build(config, Method::Get, url, None)
}

fn read_json(response: &Response) -> Result<Value, WriteError> {
    if !(200..300).contains(&response.status) {
        return Err(WriteError::Refused {
            status: response.status,
            message: refusal_message(&response.body),
        });
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| WriteError::Malformed("the answer is not JSON".into()))
}

/// Reads issue `number` of `repo` as GitHub has it now (`GET …/issues/{n}`): one request.
///
/// # Errors
///
/// [`WriteError::Refused`] for any status but 2xx; [`WriteError::Unreachable`] when the transport
/// fails; [`WriteError::Malformed`] when the answer has no title or state.
pub async fn read_issue<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    repo: &RepoRef,
    number: u64,
) -> Result<IssueNow, WriteError> {
    let url = format!("{}/repos/{}/issues/{number}", config.api(), repo.as_str());
    let answer = read_json(&exchange(transport, read_request(config, url)).await?)?;
    let title = answer
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| WriteError::Malformed("no title".into()))?;
    let state = answer
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| WriteError::Malformed("no state".into()))?;
    let web = expected_web_origin(config.api_base.as_deref());
    Ok(IssueNow {
        title: title.to_owned(),
        body: answer
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        labels: answer
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| l.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect(),
        milestone: answer
            .get("milestone")
            .and_then(|m| m.get("number"))
            .and_then(Value::as_u64),
        open: state != "closed",
        url: answer
            .get("html_url")
            .and_then(Value::as_str)
            .and_then(|u| trusted_html_url(u, &web))
            .map(|u| u.as_str().to_owned()),
    })
}

/// Whether `created_at` is a GitHub timestamp no earlier than `since` (both `YYYY-MM-DDTHH:MM:SSZ`,
/// which then order as text).
fn created_since(item: &Value, since: &GithubTimestamp) -> bool {
    item.get("created_at")
        .and_then(Value::as_str)
        .map(GithubTimestamp::new)
        .is_some_and(|at| at.is_well_formed() && at.as_str() >= since.as_str())
}

/// Whether `item` (an issue or a comment from a listing) has exactly the text `write` sends: an
/// issue, not a pull request, with its title and body; a comment with its text.
fn same_text(write: &IssueWrite, item: &Value) -> bool {
    match write {
        IssueWrite::Create { title, body, .. } => {
            item.get("pull_request").is_none()
                && item.get("title").and_then(Value::as_str) == Some(title.as_str())
                && item.get("body").and_then(Value::as_str).unwrap_or_default() == body
        }
        IssueWrite::Comment { body, .. } => {
            item.get("body").and_then(Value::as_str) == Some(body.as_str())
        }
        IssueWrite::Edit { .. } => false,
    }
}

/// Looks upstream for an earlier attempt at `write`, made since `since` (`YYYY-MM-DDTHH:MM:SSZ`):
/// for a create, an issue (not a pull request) with exactly its title and body, among the 100
/// newest updated since; for a comment, one with exactly its text. One request; an edit has none
/// and finds nothing. Before a create or a comment is sent again, so a first attempt that arrived
/// although its answer was lost is not made twice.
///
/// # Errors
///
/// [`WriteError::Refused`] for any status but 2xx; [`WriteError::Unreachable`] when the transport
/// fails; [`WriteError::Malformed`] for an answer that is not a list, or a `since` that is not a
/// GitHub timestamp.
pub async fn find_earlier<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    write: &IssueWrite,
    since: &GithubTimestamp,
) -> Result<Option<Written>, WriteError> {
    if !since.is_well_formed() {
        return Err(WriteError::Malformed("not a GitHub timestamp".into()));
    }
    let api = config.api();
    let after = since.as_str().replace(':', "%3A");
    let url = match write {
        IssueWrite::Create { repo, .. } => format!(
            "{api}/repos/{}/issues?state=all&sort=created&direction=desc&per_page=100&since={after}",
            repo.as_str()
        ),
        IssueWrite::Comment { repo, number, .. } => format!(
            "{api}/repos/{}/issues/{number}/comments?since={after}&per_page=100",
            repo.as_str()
        ),
        IssueWrite::Edit { .. } => return Ok(None),
    };
    let answer = read_json(&exchange(transport, read_request(config, url)).await?)?;
    let items = answer
        .as_array()
        .ok_or_else(|| WriteError::Malformed("the answer is not a list".into()))?;
    let web = expected_web_origin(config.api_base.as_deref());
    Ok(items
        .iter()
        .find(|item| created_since(item, since) && same_text(write, item))
        .map(|item| Written {
            number: match write {
                IssueWrite::Create { .. } => item
                    .get("number")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0),
                _ => None,
            },
            url: item
                .get("html_url")
                .and_then(Value::as_str)
                .and_then(|u| trusted_html_url(u, &web))
                .map(|u| u.as_str().to_owned()),
        })
        .filter(|found| found.number.is_some() || !matches!(write, IssueWrite::Create { .. })))
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
                serde_json::json!({"title": "Fix the flaky login test", "milestone": 1}),
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

    #[tokio::test]
    async fn labels_are_sent_as_a_change_and_others_are_kept() {
        let issue = "https://api.github.com/repos/example-org/demo-repo/issues/1";
        let transport = ReplayTransport::from_exchanges(vec![
            answer(
                "POST",
                &format!("{issue}/labels"),
                200,
                r#"[{"name":"security"}]"#,
            ),
            answer(
                "DELETE",
                &format!("{issue}/labels/good%20first%20issue"),
                200,
                "[]",
            ),
            answer(
                "DELETE",
                &format!("{issue}/labels/a%2Fb"),
                404,
                r#"{"message":"Label does not exist"}"#,
            ),
        ]);
        let write = IssueWrite::Edit {
            repo: repo(),
            number: 1,
            edit: IssueEdit {
                add_labels: vec!["security".into()],
                remove_labels: vec!["good first issue".into(), "a/b".into()],
                ..IssueEdit::default()
            },
        };
        assert!(!match &write {
            IssueWrite::Edit { edit, .. } => edit.is_empty(),
            _ => true,
        });
        // No PATCH: nothing of the issue itself changes, and no whole label list is ever sent.
        send(&transport, &config(), &write).await.unwrap();
        let sent = transport.requests_sent();
        assert_eq!(
            sent.iter()
                .map(|r| (r.method, r.url.clone()))
                .collect::<Vec<_>>(),
            vec![
                (Method::Post, format!("{issue}/labels")),
                (
                    Method::Delete,
                    format!("{issue}/labels/good%20first%20issue")
                ),
                (Method::Delete, format!("{issue}/labels/a%2Fb")),
            ]
        );
        assert_eq!(body(&sent[0]), serde_json::json!({"labels": ["security"]}));
        assert!(sent[1].body.is_empty() && sent[1].header("content-type").is_none());
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn a_refusal_stops_the_rest_of_an_edit() {
        let issue = "https://api.github.com/repos/example-org/demo-repo/issues/1";
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "PATCH",
            issue,
            403,
            r#"{"message":"Must have admin rights"}"#,
        )]);
        let write = IssueWrite::Edit {
            repo: repo(),
            number: 1,
            edit: IssueEdit {
                title: Some("New".into()),
                add_labels: vec!["security".into()],
                ..IssueEdit::default()
            },
        };
        let err = send(&transport, &config(), &write).await.unwrap_err();
        assert_eq!(err.status(), Some(403));
        assert_eq!(
            transport.requests_sent().len(),
            1,
            "the labels were not sent"
        );
    }

    #[tokio::test]
    async fn an_issue_is_read_as_github_has_it_now() {
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "GET",
            "https://api.github.com/repos/example-org/demo-repo/issues/1",
            200,
            "{\"number\":1,\"title\":\"Ship it \u{200d} now\",\"body\":null,\"state\":\"closed\",\"labels\":[{\"name\":\"bug\"},{\"name\":\"security\"}],\"milestone\":{\"number\":2},\"html_url\":\"https://github.com/example-org/demo-repo/issues/1\"}",
        )]);
        let now = read_issue(&transport, &config(), &repo(), 1).await.unwrap();
        assert_eq!(
            now,
            IssueNow {
                title: "Ship it \u{200d} now".into(),
                body: String::new(),
                labels: vec!["bug".into(), "security".into()],
                milestone: Some(2),
                open: false,
                url: Some("https://github.com/example-org/demo-repo/issues/1".into()),
            },
            "exactly as GitHub sent it, hidden characters included"
        );
        let sent = transport.requests_sent();
        assert_eq!((sent.len(), sent[0].method), (1, Method::Get));
        let missing = ReplayTransport::from_exchanges(vec![answer(
            "GET",
            "https://api.github.com/repos/example-org/demo-repo/issues/2",
            404,
            r#"{"message":"Not Found"}"#,
        )]);
        assert_eq!(
            read_issue(&missing, &config(), &repo(), 2)
                .await
                .unwrap_err()
                .status(),
            Some(404)
        );
    }

    #[tokio::test]
    async fn an_earlier_attempt_is_found_by_its_exact_text_since_it_was_approved() {
        let since = GithubTimestamp::new("2026-03-01T10:00:00Z");
        let list = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=created&direction=desc&per_page=100&since=2026-03-01T10%3A00%3A00Z";
        let issues = r#"[
            {"number":12,"title":"Write the release notes","body":"For v1.","pull_request":{},"created_at":"2026-03-01T10:05:00Z"},
            {"number":11,"title":"Write the release notes","body":"For v1.","created_at":"2026-02-01T10:05:00Z"},
            {"number":10,"title":"Write the release notes","body":"For v2.","created_at":"2026-03-01T10:04:00Z"},
            {"number":9,"title":"Write the release notes","body":"For v1.","created_at":"2026-03-01T10:03:00Z","html_url":"https://github.com/example-org/demo-repo/issues/9"}
        ]"#;
        let transport = ReplayTransport::from_exchanges(vec![
            answer("GET", list, 200, issues),
            answer("GET", list, 200, "[]"),
        ]);
        let create = IssueWrite::Create {
            repo: repo(),
            title: "Write the release notes".into(),
            body: "For v1.".into(),
            labels: vec![],
            milestone: None,
        };
        // Not the pull request, not the one created before, not the one with another body.
        assert_eq!(
            find_earlier(&transport, &config(), &create, &since)
                .await
                .unwrap(),
            Some(Written {
                number: Some(9),
                url: Some("https://github.com/example-org/demo-repo/issues/9".into()),
            })
        );
        assert_eq!(
            find_earlier(&transport, &config(), &create, &since)
                .await
                .unwrap(),
            None
        );
        let comments = "https://api.github.com/repos/example-org/demo-repo/issues/8/comments?since=2026-03-01T10%3A00%3A00Z&per_page=100";
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "GET",
            comments,
            200,
            r#"[{"id":1,"body":"Done.","created_at":"2026-03-01T09:00:00Z"},{"id":2,"body":"Done.","created_at":"2026-03-01T10:01:00Z","html_url":"https://github.com/example-org/demo-repo/issues/8#issuecomment-2"}]"#,
        )]);
        let comment = IssueWrite::Comment {
            repo: repo(),
            number: 8,
            body: "Done.".into(),
        };
        assert_eq!(
            find_earlier(&transport, &config(), &comment, &since)
                .await
                .unwrap(),
            Some(Written {
                number: None,
                url: Some(
                    "https://github.com/example-org/demo-repo/issues/8#issuecomment-2".into()
                ),
            })
        );
        assert!(
            transport
                .requests_sent()
                .iter()
                .all(|r| r.method == Method::Get)
        );
        // An edit has no earlier attempt to look for, and asks nothing.
        let none = ReplayTransport::from_exchanges(vec![]);
        let edit = IssueWrite::Edit {
            repo: repo(),
            number: 1,
            edit: IssueEdit::default(),
        };
        assert_eq!(
            find_earlier(&none, &config(), &edit, &since).await.unwrap(),
            None
        );
        assert!(none.requests_sent().is_empty());
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
            requests(&config, &write)[0].url,
            "https://ghe.example.com/api/v3/repos/example-org/demo-repo/issues/3/comments"
        );
    }
}
