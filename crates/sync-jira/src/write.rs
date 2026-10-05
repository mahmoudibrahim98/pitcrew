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
//!   description, the epic) and labels as a change (`update.labels`, `add` and `remove`), never
//!   the whole list, so labels it does not name are kept;
//! - [`IssueWrite::Transition`]: a read of `GET {api}/issue/{key}/transitions`, then `POST` of the
//!   first transition into the wanted status category (Done to close, To Do to reopen). No such
//!   transition means nothing is sent.
//!
//! Two reads serve a write, never the sync: [`read_issue`] (`GET {api}/issue/{key}`), which the
//! hub compares with what the write expects just before an edit or a transition, and
//! [`find_earlier`], which looks for an earlier attempt at a create or a comment before it is sent
//! again.
//!
//! Issue keys are checked before they reach a URL (`^[A-Z][A-Z0-9_]*-[1-9][0-9]*$`, as the read
//! side checks them). Answers are untrusted: messages are capped and stripped of hidden
//! characters, and a created issue's key must have that shape too.

use crate::auth::JiraAuth;
use crate::change::{description_is_lossless, description_text};
use crate::deployment::percent_encode;
use crate::jql::ProjectRef;
use crate::state::StatusCategory;
use crate::time::JiraTimestamp;
use crate::wire::{WireFields, looks_like_issue_key};
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

/// The fields an edit sets; `None` (or an empty list) leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueEdit {
    /// New summary.
    pub summary: Option<String>,
    /// New description.
    pub description: Option<String>,
    /// Labels to add; the issue's other labels are kept.
    pub add_labels: Vec<String>,
    /// Labels to remove, as Jira spells them; the issue's other labels are kept.
    pub remove_labels: Vec<String>,
    /// The epic's key.
    pub epic: Option<String>,
}

impl IssueEdit {
    /// Whether it changes nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.description.is_none()
            && self.epic.is_none()
            && self.add_labels.is_empty()
            && self.remove_labels.is_empty()
    }
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
                None,
                edit.epic.as_deref(),
            )?;
            let mut body = serde_json::Map::new();
            if !f.is_empty() {
                body.insert("fields".into(), Value::Object(f));
            }
            let labels: Vec<Value> = edit
                .add_labels
                .iter()
                .map(|l| json!({ "add": l }))
                .chain(edit.remove_labels.iter().map(|l| json!({ "remove": l })))
                .collect();
            if !labels.is_empty() {
                body.insert("update".into(), json!({ "labels": labels }));
            }
            with_body(
                config,
                Method::Put,
                format!("{api}/issue/{}", checked_key(key)?),
                &Value::Object(body),
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

fn get(config: &WriteConfig, url: String) -> Request {
    Request {
        method: Method::Get,
        url,
        headers: headers(config, false),
        body: Vec::new(),
    }
}

async fn read_json<T: Transport>(transport: &T, request: Request) -> Result<Value, WriteError> {
    let response = exchange(transport, request).await?;
    if let Some(e) = refused(&response) {
        return Err(e);
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| WriteError::Unsupported("Jira's answer is not JSON.".into()))
}

/// An issue as Jira has it now: what an edit or a transition is checked against just before it is
/// sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueNow {
    /// Its summary, exactly as Jira sent it.
    pub summary: String,
    /// Its description as text, as the sync reads it.
    pub description: String,
    /// Whether `description` is the whole description (see
    /// [`crate::change::description_is_lossless`]).
    pub description_lossless: bool,
    /// Its labels, as Jira spells them.
    pub labels: Vec<String>,
    /// Its status category.
    pub category: StatusCategory,
    /// Its epic's key (Cloud's `parent`, else Data Center's epic link field).
    pub epic: Option<String>,
}

/// Reads issue `key` as Jira has it now (`GET {api}/issue/{key}?fields=…`): one request.
///
/// # Errors
///
/// [`WriteError::Unsupported`] for a malformed key (nothing is sent) or an answer without the
/// issue's fields; [`WriteError::Refused`] for any status but 2xx; [`WriteError::Unreachable`]
/// when the transport fails.
pub async fn read_issue<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    key: &str,
) -> Result<IssueNow, WriteError> {
    let mut wanted = "summary,description,status,labels,parent,issuetype,updated".to_owned();
    if let (Flavor::DataCenter, Some(field)) = (config.flavor, &config.epic_link_field) {
        wanted.push(',');
        wanted.push_str(field);
    }
    let url = format!(
        "{}/issue/{}?fields={}",
        config.api(),
        checked_key(key)?,
        percent_encode(&wanted)
    );
    let answer = read_json(transport, get(config, url)).await?;
    let fields: WireFields = answer
        .get("fields")
        .cloned()
        .and_then(|f| serde_json::from_value(f).ok())
        .ok_or_else(|| WriteError::Unsupported("Jira's answer has no issue fields.".into()))?;
    let description = description_text(&fields.description);
    let epic_link = match config.flavor {
        Flavor::Cloud => None,
        Flavor::DataCenter => config.epic_link_field.as_deref(),
    };
    Ok(IssueNow {
        description_lossless: description_is_lossless(fields.description.as_ref(), &description),
        description,
        epic: fields.epic_key(epic_link),
        category: StatusCategory::from_key(&fields.status.status_category.key),
        labels: fields.labels,
        summary: fields.summary,
    })
}

/// Whether Jira's `created` is no earlier than `since_unix` (seconds).
fn created_since(item: &Value, since_unix: i64) -> bool {
    item.get("created")
        .and_then(Value::as_str)
        .and_then(|at| JiraTimestamp::new(at).to_instant())
        .is_some_and(|at| at.as_second() >= since_unix)
}

/// Text as Jira holds what this module wrote: `config.text(text)`, or nothing for an empty
/// description Jira may keep as `null`.
fn holds_text(config: &WriteConfig, value: Option<&Value>, text: &str) -> bool {
    match value {
        None | Some(Value::Null) => text.trim().is_empty(),
        Some(value) => *value == config.text(text),
    }
}

/// Looks upstream for an earlier attempt at `write`, made since `since_unix` (seconds): for a
/// create, an issue the credential's own account reported in its project with exactly its summary
/// and description (a search, newest first); for a comment, one on the issue with exactly its
/// text (the newest 100). One request; an edit or a transition has none and finds nothing. Before
/// a create or a comment is sent again, so a first attempt that arrived although its answer was
/// lost is not made twice.
///
/// # Errors
///
/// [`WriteError::Unsupported`] for a malformed key (nothing is sent) or an answer that is not
/// JSON; [`WriteError::Refused`] for any status but 2xx; [`WriteError::Unreachable`] when the
/// transport fails.
pub async fn find_earlier<T: Transport>(
    transport: &T,
    config: &WriteConfig,
    write: &IssueWrite,
    since_unix: i64,
) -> Result<Option<Written>, WriteError> {
    let api = config.api();
    let site = config.site.trim_end_matches('/');
    match write {
        IssueWrite::Create {
            project,
            summary,
            description,
            ..
        } => {
            let jql = format!(
                "project = \"{}\" AND reporter = currentUser() ORDER BY created DESC",
                project.as_str()
            );
            let (path, paging) = match config.flavor {
                Flavor::Cloud => ("/search/jql", ""),
                Flavor::DataCenter => ("/search", "&startAt=0"),
            };
            let url = format!(
                "{api}{path}?jql={}{paging}&maxResults=50&fields={}",
                percent_encode(&jql),
                percent_encode("summary,description,created")
            );
            let answer = read_json(transport, get(config, url)).await?;
            Ok(answer
                .get("issues")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|issue| {
                    let f = issue.get("fields");
                    f.is_some_and(|f| {
                        created_since(f, since_unix)
                            && f.get("summary").and_then(Value::as_str) == Some(summary.as_str())
                            && holds_text(config, f.get("description"), description)
                    })
                })
                .and_then(|issue| issue.get("key").and_then(Value::as_str))
                .filter(|key| looks_like_issue_key(key))
                .map(|key| Written {
                    key: Some(key.to_owned()),
                    url: Some(format!("{site}/browse/{key}")),
                }))
        }
        IssueWrite::Comment { key, body } => {
            let key = checked_key(key)?;
            let url = format!("{api}/issue/{key}/comment?orderBy=-created&maxResults=100");
            let answer = read_json(transport, get(config, url)).await?;
            Ok(answer
                .get("comments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|c| created_since(c, since_unix) && holds_text(config, c.get("body"), body))
                .then(|| Written {
                    key: None,
                    url: Some(format!("{site}/browse/{key}")),
                }))
        }
        IssueWrite::Edit { .. } | IssueWrite::Transition { .. } => Ok(None),
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

    #[tokio::test]
    async fn an_edit_of_labels_alone_sends_only_the_change() {
        let url = "https://jira.example.com/rest/api/2/issue/DEMO-1";
        let transport = ReplayTransport::from_exchanges(vec![answer("PUT", url, 204, "")]);
        let write = IssueWrite::Edit {
            key: "DEMO-1".into(),
            edit: IssueEdit {
                remove_labels: vec!["billing".into()],
                ..IssueEdit::default()
            },
        };
        send(&transport, &data_center(None), &write).await.unwrap();
        assert_eq!(
            body(&transport.requests_sent()[0]),
            json!({"update": {"labels": [{"remove": "billing"}]}}),
            "no fields, and never the whole list"
        );
        assert!(IssueEdit::default().is_empty());
    }

    #[tokio::test]
    async fn an_issue_is_read_with_its_epic_link_field_on_data_center() {
        let url = "https://jira.example.com/rest/api/2/issue/DEMO-1?fields=summary%2Cdescription%2Cstatus%2Clabels%2Cparent%2Cissuetype%2Cupdated%2Ccustomfield_10008";
        let transport = ReplayTransport::from_exchanges(vec![answer(
            "GET",
            url,
            200,
            r#"{"key":"DEMO-1","fields":{"summary":"S","description":"Plain *wiki* text.","status":{"statusCategory":{"key":"done"}},"labels":["a"],"issuetype":{"name":"Task"},"updated":"2026-01-01T00:00:00.000+0000","customfield_10008":"DEMO-5"}}"#,
        )]);
        let now = read_issue(
            &transport,
            &data_center(Some("customfield_10008")),
            "DEMO-1",
        )
        .await
        .unwrap();
        assert_eq!(
            now,
            IssueNow {
                summary: "S".into(),
                description: "Plain *wiki* text.".into(),
                description_lossless: true,
                labels: vec!["a".into()],
                category: StatusCategory::Done,
                epic: Some("DEMO-5".into()),
            }
        );
        // A malformed key reads nothing.
        let none = ReplayTransport::from_exchanges(vec![]);
        assert!(matches!(
            read_issue(&none, &cloud(), "DEMO-1/../x").await,
            Err(WriteError::Unsupported(_))
        ));
        assert!(none.requests_sent().is_empty());
    }

    #[tokio::test]
    async fn an_earlier_attempt_is_found_by_its_exact_text_since_it_was_approved() {
        // 2026-03-01T10:00:00Z
        let since = 1_772_359_200;
        let search = format!(
            "{API}/search/jql?jql=project%20%3D%20%22DEMO%22%20AND%20reporter%20%3D%20currentUser%28%29%20ORDER%20BY%20created%20DESC&maxResults=50&fields=summary%2Cdescription%2Ccreated"
        );
        let doc = |t: &str| adf(t).to_string();
        let issues = format!(
            r#"{{"issues":[
              {{"key":"DEMO-15","fields":{{"summary":"Notes","description":{},"created":"2026-03-01T09:00:00.000+0000"}}}},
              {{"key":"DEMO-14","fields":{{"summary":"Notes","description":{},"created":"2026-03-01T10:05:00.000+0000"}}}},
              {{"key":"DEMO-13","fields":{{"summary":"Notes","description":{},"created":"2026-03-01T11:01:00.000+0100"}}}}
            ]}}"#,
            doc("For v1."),
            doc("For v2."),
            doc("For v1.")
        );
        let transport = ReplayTransport::from_exchanges(vec![answer("GET", &search, 200, &issues)]);
        let create = IssueWrite::Create {
            project: ProjectRef::new("DEMO").unwrap(),
            summary: "Notes".into(),
            description: "For v1.".into(),
            labels: vec![],
            epic: None,
        };
        // Not the one made before, not the one with another description.
        assert_eq!(
            find_earlier(&transport, &cloud(), &create, since)
                .await
                .unwrap(),
            Some(Written {
                key: Some("DEMO-13".into()),
                url: Some("https://jira.example.com/browse/DEMO-13".into()),
            })
        );
        let comments = format!("{API}/issue/DEMO-1/comment?orderBy=-created&maxResults=100");
        let listed = format!(
            r#"{{"comments":[{{"id":"1","body":{},"created":"2026-03-01T09:59:00.000+0000"}}]}}"#,
            doc("Done.")
        );
        let transport = ReplayTransport::from_exchanges(vec![
            answer("GET", &comments, 200, &listed),
            answer("GET", &comments, 200, &listed.replace("09:59", "10:01")),
        ]);
        let comment = IssueWrite::Comment {
            key: "DEMO-1".into(),
            body: "Done.".into(),
        };
        assert_eq!(
            find_earlier(&transport, &cloud(), &comment, since)
                .await
                .unwrap(),
            None,
            "a comment made before the approval is not this one"
        );
        assert!(
            find_earlier(&transport, &cloud(), &comment, since)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            transport
                .requests_sent()
                .iter()
                .all(|r| r.method == Method::Get)
        );
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
