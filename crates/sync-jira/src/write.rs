//! Jira, write side: the REST requests for an approved outward write (api-v1.md, "Outward
//! writes"), for Jira Cloud (REST v3, descriptions and comments in the Atlassian Document Format)
//! and Data Center (REST v2, plain text), and what their answers mean.
//!
//! Like `pitcrew_sync_github::write`, this module never decides *whether* to write: the hub calls
//! [`send`] only for a write a person approved. Each write is one request, sent once:
//!
//! - [`IssueWrite::Create`]: `POST {api}/issue`;
//! - [`IssueWrite::Comment`]: `POST {api}/issue/{key}/comment`;
//! - [`IssueWrite::Edit`]: `PUT {api}/issue/{key}` with only the fields it sets (summary,
//!   description, labels, the epic);
//! - [`IssueWrite::Transition`]: a read of `GET {api}/issue/{key}/transitions`, then `POST` of the
//!   first transition into the wanted status category (Done to close, To Do to reopen). No such
//!   transition means nothing is sent.
//!
//! Issue keys are checked before they reach a URL (`^[A-Z][A-Z0-9_]*-[1-9][0-9]*$`, as the read
//! side checks them). Answers are untrusted: messages are capped and stripped of hidden
//! characters, and a created issue's key must have that shape too.

use crate::auth::JiraAuth;
use crate::jql::ProjectRef;
use crate::state::StatusCategory;
use crate::wire::looks_like_issue_key;
use pitcrew_sync_github::bounds::{cap_chars, strip_hidden};
use pitcrew_sync_github::transport::{Method, Request, Response, Transport};
use serde_json::{Value, json};

/// The longest refusal message kept from an answer, in characters.
pub const MAX_MESSAGE_CHARS: usize = 300;
/// The issue type an issue is created as.
pub const ISSUE_TYPE: &str = "Task";

/// Which REST version, and so which text format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// Jira Cloud: REST v3, Atlassian Document Format; the epic is `parent`.
    Cloud,
    /// Jira Data Center: REST v2, plain text; the epic is the configured epic link field.
    DataCenter,
}

/// Where and as whom to write.
#[derive(Clone, Debug)]
pub struct WriteConfig {
    /// The site's root, e.g. `https://jira.example.com`.
    pub site: String,
    /// Cloud or Data Center.
    pub flavor: Flavor,
    /// The credential.
    pub auth: JiraAuth,
    /// Data Center's epic link field (`customfield_10008`); without it, no epic is written there.
    pub epic_link_field: Option<String>,
}

impl WriteConfig {
    fn api(&self) -> String {
        let version = match self.flavor {
            Flavor::Cloud => 3,
            Flavor::DataCenter => 2,
        };
        format!("{}/rest/api/{version}", self.site.trim_end_matches('/'))
    }

    /// Text as this flavor wants it: an ADF document (Cloud) or the text itself (Data Center).
    fn text(&self, text: &str) -> Value {
        match self.flavor {
            Flavor::DataCenter => Value::String(text.to_owned()),
            Flavor::Cloud => adf(text),
        }
    }

    /// The epic as a field, or `None` when this site cannot take one.
    fn epic_field(&self, epic: &str) -> Option<(String, Value)> {
        match self.flavor {
            Flavor::Cloud => Some(("parent".to_owned(), json!({ "key": epic }))),
            Flavor::DataCenter => self
                .epic_link_field
                .clone()
                .map(|field| (field, Value::String(epic.to_owned()))),
        }
    }
}

/// `text` as an Atlassian Document Format document: one paragraph per non-empty line.
#[must_use]
pub fn adf(text: &str) -> Value {
    let content: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| json!({"type": "paragraph", "content": [{"type": "text", "text": line}]}))
        .collect();
    json!({"type": "doc", "version": 1, "content": content})
}

/// The fields an edit sets; `None` leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueEdit {
    /// New summary.
    pub summary: Option<String>,
    /// New description.
    pub description: Option<String>,
    /// The whole new label list.
    pub labels: Option<Vec<String>>,
    /// The epic's key.
    pub epic: Option<String>,
}

/// One write to one Jira site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueWrite {
    /// Create an issue (of type [`ISSUE_TYPE`]).
    Create {
        /// The project.
        project: ProjectRef,
        /// Summary.
        summary: String,
        /// Description.
        description: String,
        /// Labels.
        labels: Vec<String>,
        /// The epic's key.
        epic: Option<String>,
    },
    /// Comment on an issue.
    Comment {
        /// The issue's key.
        key: String,
        /// The comment.
        body: String,
    },
    /// Change an issue.
    Edit {
        /// The issue's key.
        key: String,
        /// What to set.
        edit: IssueEdit,
    },
    /// Move an issue into a status category: [`StatusCategory::Done`] closes it,
    /// [`StatusCategory::New`] reopens it.
    Transition {
        /// The issue's key.
        key: String,
        /// The category to move it into.
        to: StatusCategory,
    },
}

/// What Jira says it wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// The created issue's key (`Create` only).
    pub key: Option<String>,
    /// A link to the issue written to.
    pub url: Option<String>,
}

/// Why a write did not go through. Never holds the credential.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// Jira answered with an error status.
    #[error("Jira refused it ({status}): {message}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// Jira's messages, capped and stripped of hidden characters.
        message: String,
    },
    /// The request could not be sent, or its answer read.
    #[error("{0}")]
    Unreachable(String),
    /// The write cannot be made as asked (a malformed key, no epic field, no transition), or Jira
    /// accepted it without saying what it created. Nothing more was sent.
    #[error("{0}")]
    Unsupported(String),
}

impl WriteError {
    /// The HTTP status, when Jira answered.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Refused { status, .. } => Some(*status),
            Self::Unreachable(_) | Self::Unsupported(_) => None,
        }
    }
}

fn checked_key(key: &str) -> Result<&str, WriteError> {
    if looks_like_issue_key(key) {
        Ok(key)
    } else {
        Err(WriteError::Unsupported(
            "The issue key is not a Jira issue key.".into(),
        ))
    }
}

fn headers(config: &WriteConfig, body: bool) -> Vec<(String, String)> {
    let mut out = vec![("Accept".to_string(), "application/json".to_string())];
    if body {
        out.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    out.push(("Authorization".to_string(), config.auth.header_value()));
    out
}

fn with_body(config: &WriteConfig, method: Method, url: String, body: &Value) -> Request {
    Request {
        method,
        url,
        headers: headers(config, true),
        // Serializing a `Value` cannot fail.
        body: serde_json::to_vec(body).unwrap_or_default(),
    }
}

/// The fields object of a create or an edit.
fn fields(
    config: &WriteConfig,
    summary: Option<&str>,
    description: Option<&str>,
    labels: Option<&[String]>,
    epic: Option<&str>,
) -> Result<serde_json::Map<String, Value>, WriteError> {
    let mut out = serde_json::Map::new();
    if let Some(summary) = summary {
        out.insert("summary".into(), Value::String(summary.to_owned()));
    }
    if let Some(description) = description {
        out.insert("description".into(), config.text(description));
    }
    if let Some(labels) = labels {
        out.insert("labels".into(), json!(labels));
    }
    if let Some(epic) = epic {
        let epic = checked_key(epic)?;
        let (name, value) = config.epic_field(epic).ok_or_else(|| {
            WriteError::Unsupported(
                "This Jira Data Center site has no epic link field set, so no epic is written."
                    .into(),
            )
        })?;
        out.insert(name, value);
    }
    Ok(out)
}

/// The request that makes `write` (for a transition, the `POST` of transition `transition`).
///
/// # Errors
///
/// [`WriteError::Unsupported`] for a malformed key, or an epic a Data Center site without an epic
/// link field cannot take.
pub fn request(
    config: &WriteConfig,
    write: &IssueWrite,
    transition: Option<&str>,
) -> Result<Request, WriteError> {
    let api = config.api();
    Ok(match write {
        IssueWrite::Create {
            project,
            summary,
            description,
            labels,
            epic,
        } => {
            let mut f = fields(
                config,
                Some(summary),
                Some(description),
                Some(labels),
                epic.as_deref(),
            )?;
            f.insert("project".into(), json!({ "key": project.as_str() }));
            f.insert("issuetype".into(), json!({ "name": ISSUE_TYPE }));
            with_body(
                config,
                Method::Post,
                format!("{api}/issue"),
                &json!({ "fields": f }),
            )
        }
        IssueWrite::Comment { key, body } => with_body(
            config,
            Method::Post,
            format!("{api}/issue/{}/comment", checked_key(key)?),
            &json!({ "body": config.text(body) }),
        ),
        IssueWrite::Edit { key, edit } => {
            let f = fields(
                config,
                edit.summary.as_deref(),
                edit.description.as_deref(),
                edit.labels.as_deref(),
                edit.epic.as_deref(),
            )?;
            with_body(
                config,
                Method::Put,
                format!("{api}/issue/{}", checked_key(key)?),
                &json!({ "fields": f }),
            )
        }
        IssueWrite::Transition { key, .. } => {
            let id = transition
                .ok_or_else(|| WriteError::Unsupported("No transition was chosen.".into()))?;
            with_body(
                config,
                Method::Post,
                format!("{api}/issue/{}/transitions", checked_key(key)?),
                &json!({ "transition": { "id": id } }),
            )
        }
    })
}

/// Jira's error messages, made safe to show.
fn refusal_message(body: &[u8]) -> String {
    let value: Value = serde_json::from_slice(body).unwrap_or_default();
    let mut parts: Vec<String> = value
        .get("errorMessages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect();
    if let Some(errors) = value.get("errors").and_then(Value::as_object) {
        for (field, message) in errors {
            if let Some(message) = message.as_str() {
                parts.push(format!("{field}: {message}"));
            }
        }
    }
    let text = if parts.is_empty() {
        "no message".to_owned()
    } else {
        parts.join("; ")
    };
    let clean: String = strip_hidden(&text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cap_chars(clean.trim(), MAX_MESSAGE_CHARS)
}

fn refused(response: &Response) -> Option<WriteError> {
    (!(200..300).contains(&response.status)).then(|| WriteError::Refused {
        status: response.status,
        message: refusal_message(&response.body),
    })
}

async fn exchange<T: Transport>(transport: &T, request: Request) -> Result<Response, WriteError> {
    transport
        .send(request)
        .await
        .map_err(|e| WriteError::Unreachable(e.to_string()))
}

/// The first transition of `key` into `to`, read from Jira.
async fn transition_to<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    key: &str,
    to: StatusCategory,
) -> Result<String, WriteError> {
    let url = format!("{}/issue/{}/transitions", config.api(), checked_key(key)?);
    let response = exchange(
        transport,
        Request {
            method: Method::Get,
            url,
            headers: headers(config, false),
            body: Vec::new(),
        },
    )
    .await?;
    if let Some(e) = refused(&response) {
        return Err(e);
    }
    let value: Value = serde_json::from_slice(&response.body).unwrap_or_default();
    value
        .get("transitions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|t| {
            t.pointer("/to/statusCategory/key")
                .and_then(Value::as_str)
                .is_some_and(|k| StatusCategory::from_key(k) == to)
        })
        .and_then(|t| t.get("id").and_then(Value::as_str))
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
        .ok_or_else(|| {
            let wanted = match to {
                StatusCategory::Done => "Done",
                StatusCategory::New => "To Do",
                StatusCategory::Indeterminate => "In Progress",
            };
            WriteError::Unsupported(format!(
                "{key}'s workflow offers no transition into {wanted}; nothing was sent."
            ))
        })
}

/// Sends `write` once through `transport` and reads the answer. A transition first reads which
/// transitions the issue offers.
///
/// # Errors
///
/// [`WriteError::Refused`] for any status but 2xx; [`WriteError::Unreachable`] when the transport
/// fails; [`WriteError::Unsupported`] when the write cannot be made (see [`request`]), no
/// transition fits, or a create's answer has no well-formed key.
pub async fn send<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    write: &IssueWrite,
) -> Result<Written, WriteError> {
    let transition = match write {
        IssueWrite::Transition { key, to } => {
            Some(transition_to(transport, config, key, *to).await?)
        }
        _ => None,
    };
    let request = request(config, write, transition.as_deref())?;
    let response = exchange(transport, request).await?;
    if let Some(e) = refused(&response) {
        return Err(e);
    }
    let site = config.site.trim_end_matches('/');
    let browse = |key: &str| format!("{site}/browse/{key}");
    match write {
        IssueWrite::Create { .. } => {
            let value: Value = serde_json::from_slice(&response.body).unwrap_or_default();
            let key = value
                .get("key")
                .and_then(Value::as_str)
                .filter(|k| looks_like_issue_key(k))
                .ok_or_else(|| {
                    WriteError::Unsupported(
                        "Jira created the issue but did not say its key.".into(),
                    )
                })?;
            Ok(Written {
                key: Some(key.to_owned()),
                url: Some(browse(key)),
            })
        }
        IssueWrite::Comment { key, .. }
        | IssueWrite::Edit { key, .. }
        | IssueWrite::Transition { key, .. } => Ok(Written {
            key: None,
            url: Some(browse(key)),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};

    const API: &str = "https://jira.example.com/rest/api/3";

    fn cloud() -> WriteConfig {
        WriteConfig {
            site: "https://jira.example.com".into(),
            flavor: Flavor::Cloud,
            auth: JiraAuth::Basic {
                email: "sam@example.com".into(),
                api_token: "synthetic-jira-token".into(),
            },
            epic_link_field: None,
        }
    }

    fn data_center(epic_link_field: Option<&str>) -> WriteConfig {
        WriteConfig {
            site: "https://jira.example.com".into(),
            flavor: Flavor::DataCenter,
            auth: JiraAuth::Bearer {
                token: "synthetic-jira-pat".into(),
            },
            epic_link_field: epic_link_field.map(str::to_owned),
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

    fn body(request: &Request) -> Value {
        serde_json::from_slice(&request.body).unwrap()
    }

    #[tokio::test]
    async fn a_cloud_create_sends_adf_and_the_parent_epic_once() {
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "POST",
            &format!("{API}/issue"),
            201,
            r#"{"id":"10013","key":"DEMO-13","self":"https://jira.example.com/rest/api/3/issue/10013"}"#,
        )]);
        let write = IssueWrite::Create {
            project: ProjectRef::new("DEMO").unwrap(),
            summary: "Write the release notes".into(),
            description: "For v1.\n\nAll of it.".into(),
            labels: vec!["docs".into()],
            epic: Some("DEMO-5".into()),
        };
        let written = send(&transport, &cloud(), &write).await.unwrap();
        assert_eq!(written.key.as_deref(), Some("DEMO-13"));
        assert_eq!(
            written.url.as_deref(),
            Some("https://jira.example.com/browse/DEMO-13")
        );
        let sent = transport.requests_sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, Method::Post);
        assert_eq!(
            body(&sent[0]),
            json!({"fields": {
                "summary": "Write the release notes",
                "description": {"type": "doc", "version": 1, "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "For v1."}]},
                    {"type": "paragraph", "content": [{"type": "text", "text": "All of it."}]}
                ]},
                "labels": ["docs"],
                "parent": {"key": "DEMO-5"},
                "project": {"key": "DEMO"},
                "issuetype": {"name": "Task"}
            }})
        );
        assert!(!format!("{:?}", sent[0]).contains("synthetic-jira-token"));
    }

    #[tokio::test]
    async fn data_center_writes_plain_text_and_its_epic_link_field() {
        let url = "https://jira.example.com/rest/api/2/issue/DEMO-1";
        let transport = ReplayTransport::from_exchanges(vec![answer("PUT", url, 204, "")]);
        let write = IssueWrite::Edit {
            key: "DEMO-1".into(),
            edit: IssueEdit {
                description: Some("Plain.".into()),
                epic: Some("DEMO-5".into()),
                ..IssueEdit::default()
            },
        };
        send(&transport, &data_center(Some("customfield_10008")), &write)
            .await
            .unwrap();
        assert_eq!(
            body(&transport.requests_sent()[0]),
            json!({"fields": {"description": "Plain.", "customfield_10008": "DEMO-5"}})
        );
        // Without the field, nothing is sent at all.
        let none = ReplayTransport::from_exchanges(vec![]);
        assert!(matches!(
            send(&none, &data_center(None), &write).await,
            Err(WriteError::Unsupported(_))
        ));
        assert!(none.requests_sent().is_empty());
    }

    #[tokio::test]
    async fn a_close_reads_the_transitions_then_posts_the_first_into_done() {
        let transitions = format!("{API}/issue/DEMO-1/transitions");
        let transport = ReplayTransport::from_exchanges(vec![
            answer(
                "GET",
                &transitions,
                200,
                r#"{"transitions":[{"id":"11","name":"Start","to":{"statusCategory":{"key":"indeterminate"}}},{"id":"31","name":"Done","to":{"statusCategory":{"key":"done"}}}]}"#,
            ),
            answer("POST", &transitions, 204, ""),
        ]);
        let write = IssueWrite::Transition {
            key: "DEMO-1".into(),
            to: StatusCategory::Done,
        };
        send(&transport, &cloud(), &write).await.unwrap();
        let sent = transport.requests_sent();
        assert_eq!(
            sent.iter().map(|r| r.method).collect::<Vec<_>>(),
            vec![Method::Get, Method::Post]
        );
        assert_eq!(body(&sent[1]), json!({"transition": {"id": "31"}}));
    }

    #[tokio::test]
    async fn no_fitting_transition_sends_nothing() {
        let transitions = format!("{API}/issue/DEMO-1/transitions");
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "GET",
            &transitions,
            200,
            r#"{"transitions":[{"id":"31","name":"Done","to":{"statusCategory":{"key":"done"}}}]}"#,
        )]);
        let reopen = IssueWrite::Transition {
            key: "DEMO-1".into(),
            to: StatusCategory::New,
        };
        let err = send(&transport, &cloud(), &reopen).await.unwrap_err();
        assert!(err.to_string().contains("To Do"), "{err}");
        let sent = transport.requests_sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, Method::Get);
    }

    #[tokio::test]
    async fn refusals_are_safe_to_show_and_malformed_keys_never_reach_a_url() {
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "POST",
            &format!("{API}/issue/DEMO-1/comment"),
            400,
            "{\"errorMessages\":[\"Comment\u{200b} body is required\"],\"errors\":{\"body\":\"empty\"}}",
        )]);
        let comment = IssueWrite::Comment {
            key: "DEMO-1".into(),
            body: "Done.".into(),
        };
        let err = send(&transport, &cloud(), &comment).await.unwrap_err();
        assert_eq!(
            err,
            WriteError::Refused {
                status: 400,
                message: "Comment body is required; body: empty".into()
            }
        );
        let hostile = IssueWrite::Comment {
            key: "DEMO-1/../../secure/Logout.jspa".into(),
            body: "x".into(),
        };
        let none = ReplayTransport::from_exchanges(vec![]);
        assert!(matches!(
            send(&none, &cloud(), &hostile).await,
            Err(WriteError::Unsupported(_))
        ));
        assert!(none.requests_sent().is_empty());
    }

    #[test]
    fn adf_has_one_paragraph_per_line() {
        assert_eq!(
            adf("a\n\n b"),
            json!({"type": "doc", "version": 1, "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "a"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": " b"}]}
            ]})
        );
        assert_eq!(adf(""), json!({"type": "doc", "version": 1, "content": []}));
    }
}
