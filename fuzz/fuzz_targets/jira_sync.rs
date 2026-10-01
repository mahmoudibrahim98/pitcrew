//! `pitcrew_sync_jira::sync::sync` against a fake Jira (Cloud or Data Center) that answers with
//! arbitrary responses, from an arbitrary stored state. A Jira server, or anything in front of
//! it, controls the responses (B10, U6); the stored cursor is read back from disk.
//!
//! Input: a flags byte (bit 0: Data Center; bits 1-2: the epic-link field; bit 3: resume without
//! margin), then sections separated by `0xFF` bytes:
//! 1. a project key candidate (`DEMO` is used when it is refused);
//! 2. the stored cursor text;
//! 3. the `/myself` response, then each search response, as `STATUS\nName: value\n…\n\nBODY`
//!    (a status that does not parse is 200). When they run out, the fake answers an empty page.
//!
//! The sync runs twice: the second time from the state the first returned.
//!
//! Checks, besides "no panic":
//! - **`ProjectRef::new`** accepts exactly `^[A-Z][A-Z0-9]{1,9}$`;
//! - **JQL stays a query**: every search request goes to the API base's search path, and its
//!   `jql` is exactly `project in ("KEY") [AND updated >= "YYYY-MM-DD HH:MM"] ORDER BY updated
//!   ASC, key ASC`, whatever the stored cursor or the server's time zone;
//! - a call makes a bounded number of requests;
//! - **keys are validated**: every reference is to a key matching `^[A-Z][A-Z0-9_]*-[1-9][0-9]*$`,
//!   at `SITE/browse/KEY`;
//! - titles, bodies and labels are within their caps and hold no hidden character, nor do the
//!   assignee and resolution names;
//! - the credential never appears in errors or in the outcome's `Debug`;
//! - the state and changes survive a JSON round trip;
//! - no arithmetic overflows on server numbers (R28: a `Retry-After` near `i64::MAX`; R30: a Data
//!   Center `startAt` near `u64::MAX`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::{is_hidden_char, percent_decode, roundtrip, scripted_response, skip_known};
use pitcrew_protocol::model::ExternalRef;
use pitcrew_sync_github::{Request, Response, Transport, TransportError};
use pitcrew_sync_jira::bounds::{
    MAX_BODY_CHARS, MAX_LABEL_CHARS, MAX_LABELS, MAX_PAGES_PER_CALL, MAX_TITLE_CHARS,
};
use pitcrew_sync_jira::sync::sync;
use pitcrew_sync_jira::{
    Deployment, JiraAuth, JiraCloud, JiraDataCenter, ProjectRef, ProjectState, SyncConfig,
    SyncOutcome, SyncState, UpstreamChange,
};
use std::sync::{Mutex, OnceLock, PoisonError};

const SITE: &str = "https://jira.example.com";
const TOKEN: &str = "jira-fuzz-TOKEN-0123456789abcdef";
const EMAIL: &str = "demo@jira.example.com";
const FIELDS: [Option<&str>; 4] = [None, Some("customfield_10008"), Some("parent"), Some("")];

/// Answers `/myself` and then the search pages from a script, and records the requests.
struct Fake {
    myself: Response,
    pages: Mutex<Vec<Response>>,
    empty: &'static [u8],
    sent: Mutex<Vec<Request>>,
}

impl Transport for Fake {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let is_myself = request.url.ends_with("/myself");
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        if is_myself {
            return Ok(self.myself.clone());
        }
        let mut pages = self.pages.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(if pages.is_empty() {
            Response {
                status: 200,
                headers: Vec::new(),
                body: self.empty.to_vec(),
            }
        } else {
            pages.remove(0)
        })
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
    })
}

fn response(bytes: &[u8]) -> Response {
    let (status, headers, mut body) = scripted_response(bytes);
    if skip_known()
        && let Ok(mut page) = serde_json::from_slice::<serde_json::Value>(&body)
        && page["startAt"].as_u64().is_some_and(|n| n > 1 << 53)
    {
        // Known finding R30: a `startAt` near `u64::MAX` overflows the next offset.
        page["startAt"] = 0.into();
        body = page.to_string().into_bytes();
    }
    Response {
        status,
        headers,
        body,
    }
}

/// `^[A-Z][A-Z0-9]{1,9}$`, written out.
fn project_key_model(key: &str) -> bool {
    let b = key.as_bytes();
    (2..=10).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// `^[A-Z][A-Z0-9_]*-[1-9][0-9]*$`, written out.
fn issue_key_model(key: &str) -> bool {
    let Some((project, number)) = key.split_once('-') else {
        return false;
    };
    let p = project.as_bytes();
    let n = number.as_bytes();
    !p.is_empty()
        && p[0].is_ascii_uppercase()
        && p[1..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
        && !n.is_empty()
        && (b'1'..=b'9').contains(&n[0])
        && n[1..].iter().all(u8::is_ascii_digit)
}

/// `-?YYYY-MM-DD HH:MM` (years as jiff prints them).
fn cursor_model(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    let Some((date, time)) = text.split_once(' ') else {
        return false;
    };
    let date: Vec<&str> = date.split('-').collect();
    let time: Vec<&str> = time.split(':').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    date.len() == 3
        && date[0].len() >= 4
        && date[0].bytes().all(|b| b.is_ascii_digit())
        && digits(date[1], 2)
        && digits(date[2], 2)
        && time.len() == 2
        && digits(time[0], 2)
        && digits(time[1], 2)
}

fuzz_target!(|input: &[u8]| {
    let Some((&flags, rest)) = input.split_first() else {
        return;
    };
    let mut sections = rest.split(|&b| b == 0xFF);
    let key = String::from_utf8_lossy(sections.next().unwrap_or_default()).into_owned();
    let cursor = String::from_utf8_lossy(sections.next().unwrap_or_default()).into_owned();
    let myself = response(sections.next().unwrap_or(b"200\n\n{\"timeZone\":\"UTC\"}"));
    let pages: Vec<Response> = sections.map(response).collect();

    let project = match ProjectRef::new(key.clone()) {
        Ok(p) => {
            assert!(project_key_model(&key), "ProjectRef accepted {key:?}");
            p
        }
        Err(_) => {
            assert!(!project_key_model(&key), "ProjectRef refused {key:?}");
            ProjectRef::new("DEMO").expect("a project key")
        }
    };

    let data_center = flags & 1 == 1;
    let (auth, api_base, empty): (JiraAuth, &str, &'static [u8]) = if data_center {
        (
            JiraAuth::Bearer {
                token: TOKEN.to_owned(),
            },
            "https://jira.example.com/rest/api/2",
            br#"{"startAt":0,"total":0,"issues":[]}"#,
        )
    } else {
        (
            JiraAuth::Basic {
                email: EMAIL.to_owned(),
                api_token: TOKEN.to_owned(),
            },
            "https://jira.example.com/rest/api/3",
            br#"{"issues":[]}"#,
        )
    };
    let secret = auth.header_value();
    let config = SyncConfig {
        projects: vec![project.clone()],
        auth,
        api_base: api_base.to_owned(),
        site_base: SITE.to_owned(),
        epic_link_field: FIELDS[usize::from(flags >> 1) % FIELDS.len()].map(str::to_owned),
        now_unix: 1_790_755_200,
    };

    let mut state = SyncState::default();
    state.projects.insert(
        project.as_str().to_owned(),
        ProjectState {
            cursor: Some(cursor),
            resume_without_margin: flags & 8 != 0,
            ..ProjectState::default()
        },
    );
    for _ in 0..2 {
        let fake = Fake {
            myself: myself.clone(),
            pages: Mutex::new(pages.clone()),
            empty,
            sent: Mutex::new(Vec::new()),
        };
        let outcome = if data_center {
            run(state, &fake, &JiraDataCenter, &config)
        } else {
            run(state, &fake, &JiraCloud, &config)
        };
        let sent = fake
            .sent
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            sent.len() <= 1 + MAX_PAGES_PER_CALL,
            "{} requests",
            sent.len()
        );
        for request in &sent {
            check_request(&request.url, api_base, data_center, project.as_str());
        }
        let shown = format!("{outcome:?}");
        for needle in [TOKEN, secret.as_str()] {
            assert!(!shown.contains(needle), "the credential in the outcome");
            for issue in &outcome.errors {
                assert!(
                    !issue.message.contains(needle),
                    "the credential in an error"
                );
            }
        }
        for change in &outcome.changes {
            check_change(change);
            roundtrip(change);
        }
        roundtrip(&outcome.state);
        state = outcome.state;
    }
});

fn run<D: Deployment>(
    state: SyncState,
    fake: &Fake,
    deployment: &D,
    config: &SyncConfig,
) -> SyncOutcome {
    runtime().block_on(sync(state, fake, deployment, config))
}

fn check_request(url: &str, api_base: &str, data_center: bool, key: &str) {
    if url == format!("{api_base}/myself") {
        return;
    }
    let search = format!(
        "{api_base}{}?",
        if data_center {
            "/search"
        } else {
            "/search/jql"
        }
    );
    let query = url
        .strip_prefix(&search)
        .unwrap_or_else(|| panic!("a request off the search path: {url:?}"));
    let mut jql = None;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        assert!(
            value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)),
            "an unencoded query value in {url:?}"
        );
        match name {
            "jql" => jql = Some(percent_decode(value)),
            "maxResults" | "fields" | "nextPageToken" => {}
            "startAt" if data_center => {
                assert!(value.bytes().all(|b| b.is_ascii_digit()), "{url:?}");
            }
            _ => panic!("an unexpected query parameter {name:?} in {url:?}"),
        }
    }
    let jql = jql.unwrap_or_else(|| panic!("a search without jql: {url:?}"));
    let head = format!("project in (\"{key}\") ");
    let tail = "ORDER BY updated ASC, key ASC";
    let middle = jql
        .strip_prefix(&head)
        .and_then(|rest| rest.strip_suffix(tail))
        .unwrap_or_else(|| panic!("a query of another shape: {jql:?}"));
    if !middle.is_empty() {
        let cursor = middle
            .strip_prefix("AND updated >= \"")
            .and_then(|rest| rest.strip_suffix("\" "))
            .unwrap_or_else(|| panic!("a query of another shape: {jql:?}"));
        assert!(cursor_model(cursor), "a cursor of another shape: {jql:?}");
    }
}

fn check_ref(link: &ExternalRef) {
    assert!(
        issue_key_model(&link.key),
        "a reference to key {:?}",
        link.key
    );
    assert_eq!(
        link.url.as_deref(),
        Some(format!("{SITE}/browse/{}", link.key).as_str())
    );
}

fn check_text(what: &str, text: &str, cap: Option<usize>) {
    if let Some(cap) = cap {
        assert!(text.chars().count() <= cap, "{what} over its cap");
    }
    if let Some(c) = text.chars().find(|&c| is_hidden_char(c)) {
        panic!("a hidden character U+{:04X} in a {what}", u32::from(c));
    }
}

fn check_change(change: &UpstreamChange) {
    check_ref(change.source());
    match change {
        UpstreamChange::IssueCreated {
            title,
            body,
            labels,
            epic,
            ..
        } => {
            check_text("title", title, Some(MAX_TITLE_CHARS));
            check_text("body", body, Some(MAX_BODY_CHARS));
            check_labels(labels);
            if let Some(epic) = epic {
                check_ref(epic);
            }
        }
        UpstreamChange::IssueRetitled { title, .. }
        | UpstreamChange::EpicCreated { title, .. }
        | UpstreamChange::EpicRenamed { title, .. } => {
            check_text("title", title, Some(MAX_TITLE_CHARS));
        }
        UpstreamChange::IssueBodyEdited { body, .. } => {
            check_text("body", body, Some(MAX_BODY_CHARS));
        }
        UpstreamChange::IssueRelabelled { labels, .. } => check_labels(labels),
        UpstreamChange::IssueReparented {
            epic: Some(epic), ..
        } => check_ref(epic),
        UpstreamChange::IssueDone {
            resolution: Some(name),
            ..
        }
        | UpstreamChange::IssueReassigned {
            assignee: Some(name),
            ..
        } => check_text("name", name, None),
        _ => {}
    }
}

fn check_labels(labels: &[String]) {
    assert!(labels.len() <= MAX_LABELS, "too many labels");
    for label in labels {
        check_text("label", label, Some(MAX_LABEL_CHARS));
    }
}
