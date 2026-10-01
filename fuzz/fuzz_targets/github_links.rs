//! The links `pitcrew_sync_github` trusts from a server: the `Link: rel="next"` it follows with
//! the token (`origin::trusted_next_url`, called directly on the link as the client reads it from
//! the header, and reached through `sync::sync`), the `html_url` it keeps in an `ExternalRef`
//! (`change::trusted_html_url`), and the closing references it reads from a pull request's body
//! (`links::linked_issues`). A GitHub Enterprise server, or anything in front of it, controls the
//! first two (B10, U6); anyone who opens a pull request writes the third (U2).
//!
//! Input: a flags byte (bits 0-1: the API base), then three sections separated by `0xFF` bytes:
//! the `next` link, the `html_url` every item carries, and the pull request's body. A fake GitHub
//! answers by path: the first milestones page carries the link; issues, pull requests and any
//! other page answer with one item, or none.
//!
//! The oracle is `pitcrew_fuzz::url_model`, written from the URL Standard, not the `url` crate the
//! code under test uses.
//!
//! Checks, besides "no panic":
//! - **the token stays under the API base**: every link `trusted_next_url` accepts, and every
//!   request, goes to the base's scheme, host and port, with a path whose segments start with the
//!   base's (an empty segment counts: R31, fixed); a trusted link reads the same in the model
//!   before and after the client parses it (its segments compared percent-decoded);
//! - **kept links are pinned**: every `ExternalRef` URL is `https` on the expected web host, with
//!   no userinfo, the default port and at most 2,048 bytes (the R10 residuals, fixed), and nothing
//!   hidden;
//! - **closing references**: each `owner/repo#n` has an owner and repo of `[A-Za-z0-9._-]+` that
//!   are not `..`, and its link's path is exactly `/owner/repo/issues/n` (R32, fixed: a `.` repo
//!   was a dot segment).
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::url_model::{self, Parsed};
use pitcrew_fuzz::{is_hidden_char, percent_decode};
use pitcrew_protocol::model::ExternalRef;
use pitcrew_sync_github::link_header::next_link;
use pitcrew_sync_github::links::linked_issues;
use pitcrew_sync_github::sync::sync;
use pitcrew_sync_github::{
    AuthToken, GithubTimestamp, RepoRef, Request, Response, SyncConfig, SyncState, Transport,
    TransportError, UpstreamChange, trusted_next_url,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

const REPO: &str = "example-org/demo-repo";
const BASES: [Option<&str>; 3] = [
    None,
    Some("https://ghe.example.com/api/v3"),
    Some("https://ghe.example.com:8443/api/v3/"),
];
/// The longest link a person is shown (the console's `MAX_LINK_LENGTH`).
const MAX_LINK: usize = 2048;

struct Fake {
    link: String,
    html_url: String,
    body: String,
    linked: AtomicBool,
    sent: Mutex<Vec<Request>>,
}

impl Fake {
    /// The `Link` header the first milestones page carries.
    fn header(&self) -> String {
        format!("<{}>; rel=\"next\"", self.link)
    }

    fn answer(&self, url: &str) -> Response {
        let path = url.split(['?', '#']).next().unwrap_or("");
        let ok = |items: serde_json::Value, headers: Vec<(String, String)>| Response {
            status: 200,
            headers,
            body: items.to_string().into_bytes(),
        };
        let at = "2026-09-30T08:00:00Z";
        if path.ends_with("/milestones") && !self.linked.swap(true, Ordering::Relaxed) {
            let header = self.header();
            let milestone = json!({"number": 1, "title": "v1", "state": "open",
                                   "html_url": self.html_url});
            ok(json!([milestone]), vec![("Link".to_owned(), header)])
        } else if path.ends_with("/issues") {
            let issue = json!({"number": 7, "title": "t", "state": "open", "updated_at": at,
                               "html_url": self.html_url,
                               "milestone": {"number": 1, "html_url": self.html_url}});
            ok(json!([issue]), Vec::new())
        } else if path.ends_with("/pulls") {
            let pull = json!({"number": 9, "title": "p", "body": self.body, "state": "closed",
                              "merged_at": at, "updated_at": at, "html_url": self.html_url});
            ok(json!([pull]), Vec::new())
        } else {
            ok(json!([]), Vec::new())
        }
    }
}

impl Transport for Fake {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let response = self.answer(&request.url);
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        Ok(response)
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
    let base = BASES[usize::from(flags & 3) % BASES.len()];
    let mut sections = rest.split(|&b| b == 0xFF);
    let mut next = || String::from_utf8_lossy(sections.next().unwrap_or_default()).into_owned();
    let (link, html_url, body) = (next(), next(), next());

    closing_references(&body);

    let base_text = base.unwrap_or("https://api.github.com");
    let base_model = url_model::parse(base_text).expect("the model reads the base");
    let web_host = if base.is_some() {
        base_model.host.clone()
    } else {
        "github.com".to_owned()
    };
    let fake = Fake {
        link: link.clone(),
        html_url,
        body,
        linked: AtomicBool::new(false),
        sent: Mutex::new(Vec::new()),
    };
    let config = SyncConfig {
        repos: vec![RepoRef::new(REPO).expect("a repo")],
        token: AuthToken::new("ghp_fuzzTOKENfuzzTOKENfuzzTOKEN0000"),
        now_unix: 1_790_755_200,
        now: GithubTimestamp::new("2026-09-30T08:00:00Z"),
        api_base: base.map(str::to_owned),
    };
    // `origin::trusted_next_url`, the seam the client follows a `next` link through, checked
    // directly on the link as the client reads it from the header: what it trusts, the model must
    // put under the base, and must read the same as the link the server sent.
    let next = next_link(&fake.header());
    let trusted = next
        .as_deref()
        .and_then(|n| trusted_next_url(n, base_text))
        .map(|u| u.as_str().to_owned());
    if let (Some(next), Some(url)) = (&next, &trusted) {
        let model = url_model::parse(url)
            .unwrap_or_else(|| panic!("the model cannot read a trusted link: {url:?}"));
        check_under(&model, &base_model, url);
        check_reads_the_same(next, &model);
    }

    let outcome = runtime().block_on(sync(SyncState::default(), &fake, &config));

    let sent = fake
        .sent
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    for request in &sent {
        let model = url_model::parse(&request.url)
            .unwrap_or_else(|| panic!("the model cannot read a sent URL: {:?}", request.url));
        check_under(&model, &base_model, &request.url);
        if let Some(next) = &next
            && trusted.as_deref() == Some(request.url.as_str())
        {
            check_reads_the_same(next, &model);
        }
    }
    for change in &outcome.changes {
        for link in refs(change) {
            check_pinned(link, &web_host, base_model.port);
        }
    }
});

fn check_under(model: &Parsed, base: &Parsed, url: &str) {
    assert!(
        model.same_origin(base),
        "a request with the token left the API base's origin: {url:?} is {model:?}"
    );
    assert!(
        model.is_under(base),
        "a request with the token left the API base's path: {url:?} is {model:?}"
    );
}

/// The link the server sent and the URL the client parsed from it are the same place. The client
/// percent-encodes some characters in the path (`<`, `{`, …); the model keeps them as written, so
/// the two compare decoded.
fn check_reads_the_same(link: &str, sent: &Parsed) {
    let raw = url_model::parse(link)
        .unwrap_or_else(|| panic!("the model cannot read a followed link: {link:?}"));
    let decoded = |p: &Parsed| -> Vec<String> { p.segments.iter().map(|s| percent_decode(s)).collect() };
    assert!(
        raw.same_origin(sent) && decoded(&raw) == decoded(sent),
        "the followed link {link:?} reads as {raw:?}, the request as {sent:?}"
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

fn check_pinned(link: &ExternalRef, web_host: &str, base_port: u16) {
    let Some(url) = &link.url else { return };
    assert!(
        !url.chars().any(is_hidden_char),
        "a hidden character in {url:?}"
    );
    let model = url_model::parse(url)
        .unwrap_or_else(|| panic!("the model cannot read a kept link {url:?}"));
    assert_eq!(
        model.scheme, "https",
        "a kept link that is not https: {url:?}"
    );
    assert!(
        model.host == web_host || model.host == "github.com",
        "a kept link on another host: {url:?}"
    );
    assert!(!model.userinfo, "a kept link with userinfo: {url:?}");
    assert!(
        model.port == 443 || model.port == base_port,
        "a kept link on another port: {url:?}"
    );
    assert!(url.len() <= MAX_LINK, "a kept link of {} bytes", url.len());
}

fn closing_references(body: &str) {
    for r in linked_issues(body, REPO) {
        let (owner_repo, number) = r
            .key
            .rsplit_once('#')
            .unwrap_or_else(|| panic!("a reference without #: {:?}", r.key));
        let (owner, repo) = owner_repo
            .split_once('/')
            .unwrap_or_else(|| panic!("a reference without /: {:?}", r.key));
        for part in [owner, repo] {
            assert!(
                !part.is_empty()
                    && part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')),
                "a reference to {:?}",
                r.key
            );
        }
        assert!(number.parse::<u64>().is_ok(), "a reference to {:?}", r.key);
        let url = r.url.as_deref().unwrap_or("");
        let model = url_model::parse(url).unwrap_or_else(|| panic!("a reference link {url:?}"));
        assert_eq!(
            (model.scheme.as_str(), model.host.as_str()),
            ("https", "github.com")
        );
        assert_eq!(
            model.segments,
            [owner, repo, "issues", number],
            "the link of {:?} goes elsewhere: {url:?}",
            r.key
        );
    }
}
