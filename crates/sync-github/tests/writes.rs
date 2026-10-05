//! Outward writes against recorded fixtures (`tests/fixtures/writes.fixture`): each approved write
//! is exactly its requests, each carrying exactly its fields (labels as a change, never the whole
//! list), and a refusal is reported, never retried.

use pitcrew_sync_github::fixture::ReplayTransport;
use pitcrew_sync_github::write::{IssueEdit, IssueWrite, StateChange, WriteConfig, WriteError};
use pitcrew_sync_github::{AuthToken, CloseReason, Method, RepoRef};
use serde_json::{Value, json};

fn transport() -> ReplayTransport {
    ReplayTransport::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/writes.fixture"),
    )
    .expect("fixture")
}

fn config() -> WriteConfig {
    WriteConfig {
        api_base: None,
        token: AuthToken::new("synthetic-write-token-not-real"),
    }
}

fn repo() -> RepoRef {
    RepoRef::new("example-org/demo-repo").expect("repo")
}

#[tokio::test]
async fn each_write_sends_exactly_its_fields_and_labels_as_a_change() {
    let transport = transport();
    let config = config();
    let created = pitcrew_sync_github::write::send(
        &transport,
        &config,
        &IssueWrite::Create {
            repo: repo(),
            title: "Write the release notes".into(),
            body: "For v1.".into(),
            labels: vec!["docs".into()],
            milestone: Some(2),
        },
    )
    .await
    .expect("created");
    assert_eq!(created.number, Some(8));
    let commented = pitcrew_sync_github::write::send(
        &transport,
        &config,
        &IssueWrite::Comment {
            repo: repo(),
            number: 8,
            body: "Drafted in PitCrew.".into(),
        },
    )
    .await
    .expect("commented");
    assert_eq!(
        commented.url.as_deref(),
        Some("https://github.com/example-org/demo-repo/issues/8#issuecomment-901")
    );
    pitcrew_sync_github::write::send(
        &transport,
        &config,
        &IssueWrite::Edit {
            repo: repo(),
            number: 8,
            edit: IssueEdit {
                state: Some(StateChange::Close(CloseReason::Completed)),
                ..IssueEdit::default()
            },
        },
    )
    .await
    .expect("closed");
    let refused = pitcrew_sync_github::write::send(
        &transport,
        &config,
        &IssueWrite::Edit {
            repo: repo(),
            number: 1,
            edit: IssueEdit {
                milestone: Some(99),
                ..IssueEdit::default()
            },
        },
    )
    .await
    .expect_err("refused");
    assert_eq!(
        refused,
        WriteError::Refused {
            status: 422,
            message: "Validation Failed".into()
        }
    );
    // Read before an edit, then labels as a change: one added, one removed, the rest kept.
    let now = pitcrew_sync_github::write::read_issue(&transport, &config, &repo(), 8)
        .await
        .expect("read");
    assert_eq!(
        now.labels,
        vec!["docs".to_string(), "needs triage".to_string()]
    );
    pitcrew_sync_github::write::send(
        &transport,
        &config,
        &IssueWrite::Edit {
            repo: repo(),
            number: 8,
            edit: IssueEdit {
                add_labels: vec!["release".into()],
                remove_labels: vec!["needs triage".into()],
                ..IssueEdit::default()
            },
        },
    )
    .await
    .expect("relabelled");

    let sent = transport.requests_sent();
    let shown: Vec<(Method, String, Value)> = sent
        .iter()
        .map(|r| {
            (
                r.method,
                r.url.clone(),
                if r.body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&r.body).expect("JSON body")
                },
            )
        })
        .collect();
    let api = "https://api.github.com/repos/example-org/demo-repo";
    assert_eq!(
        shown,
        vec![
            (
                Method::Post,
                format!("{api}/issues"),
                json!({"title": "Write the release notes", "body": "For v1.", "labels": ["docs"],
                    "milestone": 2})
            ),
            (
                Method::Post,
                format!("{api}/issues/8/comments"),
                json!({"body": "Drafted in PitCrew."})
            ),
            (
                Method::Patch,
                format!("{api}/issues/8"),
                json!({"state": "closed", "state_reason": "completed"})
            ),
            (
                Method::Patch,
                format!("{api}/issues/1"),
                json!({"milestone": 99})
            ),
            (Method::Get, format!("{api}/issues/8"), Value::Null),
            (
                Method::Post,
                format!("{api}/issues/8/labels"),
                json!({"labels": ["release"]})
            ),
            (
                Method::Delete,
                format!("{api}/issues/8/labels/needs%20triage"),
                Value::Null
            ),
        ]
    );
    assert_eq!(
        transport.remaining(),
        0,
        "every recorded answer was used once"
    );
    for request in &sent {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer synthetic-write-token-not-real")
        );
        assert!(!request.url.contains("synthetic-write-token-not-real"));
    }
}
