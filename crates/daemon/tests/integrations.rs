//! GitHub and Jira integrations in `pitcrewd serve`, end to end: the real binary on the demo,
//! reading the recorded fixtures in `apps/mock-hub/fixtures` (`--integration-fixtures`, never the
//! network) with a stand-in `gh` alone on its `PATH` (never the machine's own).
//!
//! - every route is device-only, and malformed connections are refused;
//! - a sync turns the issues of a linked milestone into tasks of that workstream;
//! - **no credential reaches an answer, a log or a file but its own** (the daemon logs at debug).

#![allow(clippy::unwrap_used)]
#![cfg(unix)]

mod common;

use common::{Daemon, Tmux, request};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SEED_RUNS: &str = "01JB000000000000000WST0002";
const GH_CREDENTIAL: &str = "synthetic-gh-credential-e2e";
const JIRA_SECRET: &str = "synthetic-jira-secret-e2e";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/mock-hub/fixtures")
}

/// A folder holding only a stand-in `gh` that prints a synthetic credential.
fn stand_in_bin(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!("#!/bin/sh\n[ \"$1 $2\" = 'auth token' ] || exit 2\necho {GH_CREDENTIAL}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

struct Hub {
    daemon: Daemon,
    token: String,
    agent: String,
    /// Every answer's body, to check that no credential is in any of them.
    seen: std::cell::RefCell<String>,
}

impl Hub {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        self.call_as(&self.token, method, path, body)
    }

    fn call_as(&self, token: &str, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let reply = request(self.daemon.port, method, path, Some(token), body, &[]);
        self.seen.borrow_mut().push_str(&reply.body);
        let value = if reply.body.is_empty() {
            Value::Null
        } else {
            reply.json()
        };
        (reply.status, value)
    }
}

fn start(tmp: &Path) -> Hub {
    let state = tmp.join("state");
    let bin = stand_in_bin(tmp);
    let fixtures = fixtures();
    let daemon = Daemon::start_with(
        &state,
        &[
            "--demo",
            "--no-runner",
            "--integration-fixtures",
            fixtures.to_str().unwrap(),
        ],
        &[("PATH", OsString::from(bin))],
        Tmux::Refused,
    );
    let token = daemon.device_token();
    let agent = daemon.agent_token();
    Hub {
        daemon,
        token,
        agent,
        seen: std::cell::RefCell::new(String::new()),
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_linked_milestone_syncs_and_no_credential_leaks() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = start(tmp.path());

    // Device tokens only.
    let github = json!({
        "name": "Demo repository",
        "settings": { "kind": "github", "repos": ["example-org/demo-repo"] },
        "credential": "gh_cli"
    });
    let agent = hub.agent.clone();
    assert_eq!(hub.call_as(&agent, "GET", "/v1/integrations", None).0, 403);
    assert_eq!(
        hub.call_as(&agent, "POST", "/v1/integrations", Some(&github))
            .0,
        403
    );
    assert_eq!(
        hub.call(
            "POST",
            "/v1/integrations",
            Some(&json!({"name": "x", "settings": {"kind": "github", "repos": ["nope"]}, "credential": "gh_cli"}))
        )
        .0,
        400
    );
    assert_eq!(
        hub.call("POST", "/v1/integrations", Some(&json!([1]))).0,
        400
    );

    let (status, created) = hub.call("POST", "/v1/integrations", Some(&github));
    assert_eq!(status, 201, "{created}");
    let gh_id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created["credential"],
        json!({"source": "gh_cli", "stored": false})
    );
    assert_eq!(hub.call("POST", "/v1/integrations", Some(&github)).0, 409);

    // A stored-credential Jira connection.
    let (status, jira) = hub.call(
        "POST",
        "/v1/integrations",
        Some(&json!({
            "name": "Demo Jira",
            "settings": {"kind": "jira", "deployment": "cloud", "site": "https://jira.example.com",
                         "projects": ["DEMO"], "email": "sam@example.com"},
            "credential": "stored"
        })),
    );
    assert_eq!(status, 201, "{jira}");
    let jira_id = jira["id"].as_str().unwrap().to_owned();
    let credential = format!("/v1/integrations/{jira_id}/credential");
    assert_eq!(
        hub.call_as(
            &agent,
            "PUT",
            &credential,
            Some(&json!({"secret": JIRA_SECRET}))
        )
        .0,
        403
    );
    assert_eq!(
        hub.call("PUT", &credential, Some(&json!({"secret": 42}))).0,
        400
    );
    assert_eq!(
        hub.call(
            "PUT",
            &format!("/v1/integrations/{gh_id}/credential"),
            Some(&json!({"secret": JIRA_SECRET}))
        )
        .0,
        409
    );
    assert_eq!(
        hub.call("PUT", &credential, Some(&json!({"secret": JIRA_SECRET})))
            .0,
        204
    );
    let (_, jira) = hub.call("GET", &format!("/v1/integrations/{jira_id}"), None);
    assert_eq!(
        jira["credential"],
        json!({"source": "stored", "stored": true})
    );

    // Link Seed runs to milestone 1, then sync now.
    let (status, linked) = hub.call(
        "PATCH",
        &format!("/v1/workstreams/{SEED_RUNS}"),
        Some(&json!({"external": [{
            "system": "github",
            "key": "example-org/demo-repo#milestone:1",
            "url": "https://github.com/example-org/demo-repo/milestone/1"
        }]})),
    );
    assert_eq!(status, 200, "{linked}");
    let (status, _) = hub.call("POST", &format!("/v1/integrations/{gh_id}/sync"), None);
    assert_eq!(status, 202);
    wait_for("the task for example-org/demo-repo#1", || {
        let (_, tasks) = hub.call("GET", &format!("/v1/tasks?workstream={SEED_RUNS}"), None);
        tasks.as_array().unwrap().iter().any(|t| {
            t["source"]["key"] == "example-org/demo-repo#1" && t["title"] == "Fix flaky login test"
        })
    });
    wait_for("the sync to end without problems", || {
        let (_, one) = hub.call("GET", &format!("/v1/integrations/{gh_id}"), None);
        one["status"]["running"] == false
            && one["status"]["last_success_at"].is_i64()
            && one["status"]["problems"] == json!([])
            && one["links"][0]["title"] == "v1 launch"
    });

    // Testing reads once; the fixture's credential could push, so it warns.
    let (status, check) = hub.call("POST", &format!("/v1/integrations/{gh_id}/test"), None);
    assert_eq!(status, 200);
    assert_eq!(check["ok"], true, "{check}");
    assert!(!check["warnings"].as_array().unwrap().is_empty());
    let (_, check) = hub.call("POST", &format!("/v1/integrations/{jira_id}/test"), None);
    assert_eq!(check["ok"], true, "{check}");
    let (_, all) = hub.call("GET", "/v1/integrations", None);
    assert_eq!(all.as_array().unwrap().len(), 2);

    // Gone with its secret.
    assert_eq!(
        hub.call("DELETE", &format!("/v1/integrations/{jira_id}"), None)
            .0,
        204
    );
    assert_eq!(
        hub.call("GET", &format!("/v1/integrations/{jira_id}"), None)
            .0,
        404
    );
    assert_eq!(
        hub.call("DELETE", &format!("/v1/integrations/{jira_id}"), None)
            .0,
        404
    );
    assert_eq!(hub.call("GET", "/v1/integrations/not-an-id", None).0, 404);

    // No credential in any answer, the daemon's log (at debug), or its saved files.
    let seen = hub.seen.borrow().clone();
    for secret in [GH_CREDENTIAL, JIRA_SECRET] {
        assert!(!seen.contains(secret), "an answer holds a credential");
        assert!(
            !hub.daemon.stderr().contains(secret),
            "the log holds a credential"
        );
    }
    let state = tmp.path().join("state");
    let saved = std::fs::read_to_string(state.join("integrations.json")).unwrap();
    assert!(!saved.contains(JIRA_SECRET) && !saved.contains(GH_CREDENTIAL));
    for entry in std::fs::read_dir(state.join("integrations")).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(GH_CREDENTIAL), "gh's token is never kept");
    }
}
