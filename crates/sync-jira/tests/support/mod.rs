//! Shared test support: URLs built the same way production code builds them (through
//! [`Deployment::build_search_request`], not hand-typed and percent-encoded by a human), plus
//! small JSON builders for synthetic issues and epics. `DEMO`/`jira.example.com`/fake accounts
//! throughout — nothing here is a real host, token or person.

#![allow(dead_code)]

use pitcrew_sync_github::fixture::RecordedExchange;
use pitcrew_sync_jira::jql::{ProjectRef, incremental_query};
use pitcrew_sync_jira::{Deployment, JiraAuth, JiraCloud, JiraDataCenter, PageState};
use serde_json::Value;

/// Builds the exact JQL `sync` itself would build for project `DEMO` at this cursor — reusing the
/// production function rather than a hand-typed string, so a test URL can never silently drift
/// from what the real query shape becomes.
pub fn jql_for(project: &str, cursor: Option<&str>) -> String {
    let project = ProjectRef::new(project).expect("valid project key");
    incremental_query(&project, cursor)
}

pub const CLOUD_API_BASE: &str = "https://jira.example.com/rest/api/3";
pub const DC_API_BASE: &str = "https://jira.example.com/rest/api/2";
pub const SITE_BASE: &str = "https://jira.example.com";

pub const FIELDS: &[&str] = &[
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

pub fn cloud_auth() -> JiraAuth {
    JiraAuth::Basic {
        email: "demo@jira.example.com".to_string(),
        api_token: "demo-api-token-not-real".to_string(),
    }
}

pub fn dc_auth() -> JiraAuth {
    JiraAuth::Bearer {
        token: "pat-demo-not-real".to_string(),
    }
}

pub fn cloud_search_url(jql: &str, page: &PageState) -> String {
    JiraCloud
        .build_search_request(CLOUD_API_BASE, &cloud_auth(), jql, FIELDS, page)
        .url
}

pub fn dc_search_url(jql: &str, page: &PageState) -> String {
    JiraDataCenter
        .build_search_request(DC_API_BASE, &dc_auth(), jql, FIELDS, page)
        .url
}

pub fn myself_exchange(api_base: &str, time_zone: &str) -> RecordedExchange {
    RecordedExchange {
        method: "GET".to_string(),
        url: format!("{api_base}/myself"),
        request_headers: vec![],
        status: 200,
        response_headers: vec![],
        body: format!(r#"{{"timeZone":"{time_zone}"}}"#).into_bytes(),
    }
}

pub fn ok(url: &str, body: Value) -> RecordedExchange {
    exchange(url, 200, vec![], body)
}

pub fn exchange(url: &str, status: u16, headers: Vec<(&str, &str)>, body: Value) -> RecordedExchange {
    RecordedExchange {
        method: "GET".to_string(),
        url: url.to_string(),
        request_headers: vec![],
        status,
        response_headers: headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.to_string().into_bytes(),
    }
}

pub fn cloud_page(issues: Vec<Value>, next_page_token: Option<&str>) -> Value {
    let mut v = serde_json::json!({ "issues": issues });
    if let Some(token) = next_page_token {
        v["nextPageToken"] = Value::String(token.to_string());
    }
    v
}

pub fn dc_page(issues: Vec<Value>, start_at: u64, total: u64) -> Value {
    serde_json::json!({ "startAt": start_at, "total": total, "issues": issues })
}

/// A regular (non-epic) issue, in the shape both deployments return.
pub fn issue_json(key: &str, summary: &str, category: &str, updated: &str) -> Value {
    serde_json::json!({
        "id": key,
        "key": key,
        "fields": {
            "summary": summary,
            "status": {"name": "Status", "statusCategory": {"key": category}},
            "issuetype": {"name": "Story"},
            "updated": updated,
        }
    })
}

/// An issue with a description (plain text, as Data Center — and a hand-built ADF object —
/// Cloud — both send it).
pub fn issue_with_description(key: &str, summary: &str, category: &str, updated: &str, description: Value) -> Value {
    let mut v = issue_json(key, summary, category, updated);
    v["fields"]["description"] = description;
    v
}

pub fn plain_description(text: &str) -> Value {
    Value::String(text.to_string())
}

pub fn adf_description(text: &str) -> Value {
    serde_json::json!({
        "type": "doc", "version": 1,
        "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}],
    })
}

/// An epic (an issue whose `issuetype.name` is `"Epic"`).
pub fn epic_json(key: &str, summary: &str, category: &str, updated: &str) -> Value {
    let mut v = issue_json(key, summary, category, updated);
    v["fields"]["issuetype"] = serde_json::json!({"name": "Epic"});
    v
}

pub fn with_parent(mut issue: Value, parent_key: &str) -> Value {
    issue["fields"]["parent"] = serde_json::json!({"key": parent_key});
    issue
}

pub fn with_resolution(mut issue: Value, name: &str) -> Value {
    issue["fields"]["resolution"] = serde_json::json!({"name": name});
    issue
}
