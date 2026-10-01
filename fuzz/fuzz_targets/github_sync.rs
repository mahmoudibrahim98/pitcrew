//! `pitcrew_sync_github::sync::sync` against a fake GitHub that answers with arbitrary responses:
//! status, headers (`Link`, `ETag`, rate-limit headers) and JSON bodies for milestones, issues
//! and pull requests. A GitHub Enterprise server, or anything between the client and it, controls
//! these bytes. The `Authorization` header goes with every request, so where the client goes next
//! is the security property.
//!
//! Input: a flags byte (which API base), then responses separated by `0xFF` bytes (never in
//! UTF-8 text). Each response is `STATUS\nName: value\n…\n\nBODY`; a status that does not parse
//! is 200. When the responses run out, the fake answers `200 []`. The sync runs twice: the second
//! time from the state the first returned, so stored resume links are followed too.
//!
//! Checks, besides "no panic":
//! - **no request leaves the API base**: every request URL, resolved the way a WHATWG URL parser
//!   (the `url` crate, which reqwest and ureq use) resolves it, has the base's scheme, host and
//!   port, and a path under the base's path after `.`/`..` segments are resolved. R8 and R9 are
//!   fixed; their inputs in `fuzz/regressions/github_sync/` must pass;
//! - **links kept from upstream are pinned** (R10, fixed): every `ExternalRef` URL is `https` on
//!   the expected web host (`github.com`, or the Enterprise server's own host);
//! - the `Link` header parser returns a URL that appears in the header;
//! - a call makes a bounded number of requests;
//! - errors, issues and the outcome's `Debug` never contain the token;
//! - titles, bodies and labels are within their caps, and the state and changes survive a JSON
//!   round trip;
//! - no arithmetic overflows on server numbers (R28: a `retry-after` near `i64::MAX`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::{is_hidden_char, roundtrip, scripted_response};
use pitcrew_protocol::model::ExternalRef;
use pitcrew_sync_github::bounds::{
    MAX_BODY_CHARS, MAX_LABEL_CHARS, MAX_LABELS, MAX_PAGES_PER_CALL, MAX_TITLE_CHARS,
};
use pitcrew_sync_github::link_header::next_link;
use pitcrew_sync_github::sync::sync;
use pitcrew_sync_github::{
    AuthToken, GithubTimestamp, RepoRef, Request, Response, SyncConfig, SyncState, Transport,
    TransportError, UpstreamChange,
};
use std::sync::{Mutex, OnceLock, PoisonError};

const TOKEN: &str = "ghp_fuzzTOKENfuzzTOKENfuzzTOKEN0000";
const BASES: [Option<&str>; 3] = [
    None,
    Some("https://ghe.example.com/api/v3"),
    Some("https://ghe.example.com:8443/api/v3/"),
];

/// Answers requests from a script and records them.
struct Fake {
    script: Mutex<Vec<Response>>,
    sent: Mutex<Vec<Request>>,
}

impl Transport for Fake {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        let mut script = self.script.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(if script.is_empty() {
            Response {
                status: 200,
                headers: Vec::new(),
                body: b"[]".to_vec(),
            }
        } else {
            script.remove(0)
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

fuzz_target!(|input: &[u8]| {
    let Some((&flags, rest)) = input.split_first() else {
        return;
    };
    let base = BASES[usize::from(flags) % BASES.len()];
    let responses: Vec<Response> = rest.split(|&b| b == 0xFF).map(response).collect();
    for r in &responses {
        if let Some(link) = r
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("link"))
        {
            if let Some(next) = next_link(&link.1) {
                assert!(link.1.contains(&next), "a next link not in the header");
            }
        }
    }
    let config = SyncConfig {
        repos: vec![RepoRef::new("example-org/demo-repo").expect("a repo")],
        token: AuthToken::new(TOKEN),
        now_unix: 1_790_755_200,
        now: GithubTimestamp::new("2026-09-30T08:00:00Z"),
        api_base: base.map(str::to_owned),
    };
    let base_url = url::Url::parse(base.unwrap_or("https://api.github.com")).expect("a base");
    let web_host = base_url
        .host_str()
        .filter(|_| base.is_some())
        .unwrap_or("github.com")
        .to_owned();

    let mut state = SyncState::default();
    for _ in 0..2 {
        let fake = Fake {
            script: Mutex::new(responses.clone()),
            sent: Mutex::new(Vec::new()),
        };
        let outcome = runtime().block_on(sync(state, &fake, &config));
        let sent = fake
            .sent
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        // Three resources, each at most `MAX_PAGES_PER_CALL` pages.
        assert!(
            sent.len() <= 3 * MAX_PAGES_PER_CALL,
            "{} requests",
            sent.len()
        );
        for request in &sent {
            check_destination(&request.url, &base_url);
        }
        let shown = format!("{outcome:?}");
        assert!(!shown.contains(TOKEN), "the token in the outcome");
        for issue in &outcome.errors {
            assert!(!issue.message.contains(TOKEN), "the token in an error");
        }
        for change in &outcome.changes {
            check_change(change);
            for link in refs(change) {
                check_pinned(link, &web_host);
            }
            roundtrip(change);
        }
        roundtrip(&outcome.state);
        state = outcome.state;
    }
});

/// The request must stay on the base's origin, under its path.
fn check_destination(url: &str, base: &url::Url) {
    let Ok(target) = url::Url::parse(url) else {
        // A URL a WHATWG client cannot parse is never sent.
        return;
    };
    let same_origin = target.scheme() == base.scheme()
        && target.host_str() == base.host_str()
        && target.port_or_known_default() == base.port_or_known_default();
    let base_path = base.path().trim_end_matches('/');
    let path = target.path();
    let under = path == base_path || path.starts_with(&format!("{base_path}/"));
    assert!(
        same_origin,
        "a request with the token left the API base's origin: {url:?} goes to {:?}",
        target.host_str()
    );
    assert!(
        under,
        "a request with the token left the API base's path: {url:?} resolves to {path:?}"
    );
}

/// Every reference a change carries.
fn refs(change: &UpstreamChange) -> Vec<&ExternalRef> {
    let mut out = vec![change.source()];
    match change {
        UpstreamChange::IssueOpened {
            milestone: Some(m), ..
        }
        | UpstreamChange::IssueMilestoned {
            milestone: Some(m), ..
        } => out.push(m),
        UpstreamChange::PullRequestMerged { closes, .. } => out.extend(closes),
        _ => {}
    }
    out
}

/// An upstream link is kept only as `https` on the expected web host, with nothing hidden in it.
/// (Closing references always point at `github.com`, which the crate builds itself.)
fn check_pinned(link: &ExternalRef, web_host: &str) {
    let Some(url) = &link.url else { return };
    let parsed = url::Url::parse(url).unwrap_or_else(|e| panic!("a kept link {url:?}: {e}"));
    assert_eq!(
        parsed.scheme(),
        "https",
        "a kept link that is not https: {url:?}"
    );
    let host = parsed.host_str().unwrap_or("");
    assert!(
        host == web_host || host == "github.com",
        "a kept link on another host: {url:?}"
    );
    assert!(
        !url.chars().any(is_hidden_char),
        "a hidden character in {url:?}"
    );
}

fn check_change(change: &UpstreamChange) {
    let chars = |s: &str| s.chars().count();
    match change {
        UpstreamChange::IssueOpened {
            title,
            body,
            labels,
            ..
        } => {
            assert!(chars(title) <= MAX_TITLE_CHARS + 1, "title over its cap");
            assert!(chars(body) <= MAX_BODY_CHARS + 1, "body over its cap");
            check_labels(labels);
        }
        UpstreamChange::IssueRetitled { title, .. }
        | UpstreamChange::MilestoneCreated { title, .. }
        | UpstreamChange::MilestoneRenamed { title, .. }
        | UpstreamChange::PullRequestOpened { title, .. } => {
            assert!(chars(title) <= MAX_TITLE_CHARS + 1, "title over its cap");
        }
        UpstreamChange::IssueBodyEdited { body, .. } => {
            assert!(chars(body) <= MAX_BODY_CHARS + 1, "body over its cap");
        }
        UpstreamChange::IssueRelabelled { labels, .. } => check_labels(labels),
        _ => {}
    }
}

fn check_labels(labels: &[String]) {
    assert!(labels.len() <= MAX_LABELS, "too many labels");
    for label in labels {
        assert!(
            label.chars().count() <= MAX_LABEL_CHARS + 1,
            "label over its cap"
        );
    }
}

/// `STATUS\nName: value\n…\n\nBODY`.
fn response(bytes: &[u8]) -> Response {
    let (status, headers, body) = scripted_response(bytes);
    Response {
        status,
        headers,
        body,
    }
}
