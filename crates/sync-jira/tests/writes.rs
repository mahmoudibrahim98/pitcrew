//! Outward writes against recorded fixtures (`tests/fixtures/writes.fixture`), Jira Cloud: each
//! approved write is exactly one request with exactly its fields (a transition reads the
//! workflow's transitions first; labels change by `update`, never as a whole list), the issue is
//! read as Jira has it now, and a refusal is reported, never retried.

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_github::transport::Method;
use pitcrew_sync_jira::write::{
    Flavor, IssueEdit, IssueWrite, WriteConfig, WriteError, read_issue, send,
};
use pitcrew_sync_jira::{JiraAuth, ProjectRef, StatusCategory};
use serde_json::{Value, json};

fn transport() -> ReplayTransport {
    ReplayTransport::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/writes.fixture"),
    )
    .expect("fixture")
}

fn config() -> WriteConfig {
    WriteConfig {
        site: "https://jira.example.com".into(),
        flavor: Flavor::Cloud,
        auth: JiraAuth::Basic {
            email: "sam@example.com".into(),
            api_token: "synthetic-jira-token-not-real".into(),
        },
        epic_link_field: None,
    }
}

fn text(t: &str) -> Value {
    json!({"type": "doc", "version": 1, "content": [
        {"type": "paragraph", "content": [{"type": "text", "text": t}]}
    ]})
}

#[tokio::test]
async fn each_write_is_one_request_with_exactly_its_fields_and_labels_as_a_change() {
    let transport = transport();
    let config = config();
    let created = send(
        &transport,
        &config,
        &IssueWrite::Create {
            project: ProjectRef::new("DEMO").expect("project"),
            summary: "Write the release notes".into(),
            description: "For v1.".into(),
            labels: vec![],
            epic: None,
        },
    )
    .await
    .expect("created");
    assert_eq!(created.key.as_deref(), Some("DEMO-13"));
    let now = read_issue(&transport, &config, "DEMO-13")
        .await
        .expect("read");
    assert_eq!(now.summary, "Write the release notes");
    assert_eq!(now.labels, vec!["needs-triage".to_string()]);
    assert_eq!(now.category, StatusCategory::New);
    assert_eq!(now.epic.as_deref(), Some("DEMO-5"));
    assert_eq!(
        (now.description.as_str(), now.description_lossless),
        ("For v1.\n", true)
    );
    send(
        &transport,
        &config,
        &IssueWrite::Comment {
            key: "DEMO-13".into(),
            body: "Drafted in PitCrew.".into(),
        },
    )
    .await
    .expect("commented");
    send(
        &transport,
        &config,
        &IssueWrite::Edit {
            key: "DEMO-13".into(),
            edit: IssueEdit {
                summary: Some("Write the v1 release notes".into()),
                add_labels: vec!["docs".into()],
                remove_labels: vec!["needs-triage".into()],
                ..IssueEdit::default()
            },
        },
    )
    .await
    .expect("edited");
    send(
        &transport,
        &config,
        &IssueWrite::Transition {
            key: "DEMO-13".into(),
            to: StatusCategory::Done,
        },
    )
    .await
    .expect("closed");
    let refused = send(
        &transport,
        &config,
        &IssueWrite::Comment {
            key: "DEMO-1".into(),
            body: "x".into(),
        },
    )
    .await
    .expect_err("refused");
    assert_eq!(
        refused,
        WriteError::Refused {
            status: 403,
            message: "You do not have the permission to comment on this issue.".into()
        }
    );

    let api = "https://jira.example.com/rest/api/3";
    let shown: Vec<(Method, String, Option<Value>)> = transport
        .requests_sent()
        .iter()
        .map(|r| {
            (
                r.method,
                r.url.clone(),
                (!r.body.is_empty()).then(|| serde_json::from_slice(&r.body).expect("JSON")),
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            (
                Method::Post,
                format!("{api}/issue"),
                Some(json!({"fields": {"summary": "Write the release notes",
                    "description": text("For v1."), "labels": [],
                    "project": {"key": "DEMO"}, "issuetype": {"name": "Task"}}}))
            ),
            (
                Method::Get,
                format!(
                    "{api}/issue/DEMO-13?fields=summary%2Cdescription%2Cstatus%2Clabels%2Cparent%2Cissuetype%2Cupdated"
                ),
                None
            ),
            (
                Method::Post,
                format!("{api}/issue/DEMO-13/comment"),
                Some(json!({"body": text("Drafted in PitCrew.")}))
            ),
            (
                Method::Put,
                format!("{api}/issue/DEMO-13"),
                Some(json!({"fields": {"summary": "Write the v1 release notes"},
                    "update": {"labels": [{"add": "docs"}, {"remove": "needs-triage"}]}}))
            ),
            (
                Method::Get,
                format!("{api}/issue/DEMO-13/transitions"),
                None
            ),
            (
                Method::Post,
                format!("{api}/issue/DEMO-13/transitions"),
                Some(json!({"transition": {"id": "31"}}))
            ),
            (
                Method::Post,
                format!("{api}/issue/DEMO-1/comment"),
                Some(json!({"body": text("x")}))
            ),
        ]
    );
    assert_eq!(
        transport.remaining(),
        0,
        "every recorded answer was used once"
    );
}
