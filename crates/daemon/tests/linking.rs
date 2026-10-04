//! Folder/branch linking and later location changes, with temporary synthetic Claude homes.
mod common;

use common::{Daemon, id};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

fn wait_session(daemon: &Daemon, token: &str, cwd: &Path, workstream: &str, basis: &str) -> Value {
    let started = Instant::now();
    loop {
        let sessions = daemon.get("/v1/sessions", Some(token)).json();
        if let Some(session) = sessions
            .as_array()
            .expect("synthetic test data")
            .iter()
            .find(|s| {
                s["cwd"].as_str() == cwd.to_str()
                    && s["workstream"] == workstream
                    && s["link_basis"] == basis
            })
        {
            return session.clone();
        }
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "sessions: {sessions}; log: {}",
            daemon.stderr()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn transcript(home: &Path, cwd: &Path, uuid: &str, branch: &str) {
    let text = std::fs::read_to_string(
        pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl"),
    )
    .expect("synthetic test data");
    let text = text
        .replace("2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b", uuid)
        .replace(
            "\"/home/sam/work/diffusion-paper/paper\"",
            &serde_json::to_string(cwd.to_str().expect("synthetic test data"))
                .expect("synthetic test data"),
        )
        .replace(
            "\"gitBranch\":\"main\"",
            &format!("\"gitBranch\":\"{branch}\""),
        );
    std::fs::write(
        home.join(".claude/projects/synthetic")
            .join(format!("{uuid}.jsonl")),
        text,
    )
    .expect("synthetic test data");
}

#[test]
fn locations_link_new_sessions_and_relink_old_ones_but_manual_links_stand() {
    let temp = tempfile::tempdir().expect("synthetic test data");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let deep = repo.join("sub/deep");
    std::fs::create_dir_all(home.join(".claude/projects/synthetic")).expect("synthetic test data");
    std::fs::create_dir_all(&deep).expect("synthetic test data");
    let daemon = Daemon::start(
        &temp.path().join("state"),
        &[
            "--demo",
            "--no-office",
            "--homes",
            home.to_str().expect("synthetic test data"),
        ],
    );
    let token = daemon.device_token();
    let create = |name: &str, path: &Path, branch: Option<&str>| {
        let reply = daemon.post(
            "/v1/workstreams",
            Some(&token),
            &json!({
                "project": id::PAPER, "name": name,
                "locations": [{"machine": id::LAPTOP, "path": path, "branch": branch}]
            }),
        );
        assert_eq!(reply.status, 201, "{}", reply.body);
        reply.json()["id"]
            .as_str()
            .expect("synthetic test data")
            .to_owned()
    };
    let folder = create("Folder", &repo, None);
    let branch = create("Branch", &repo, Some("topic"));
    transcript(
        &home,
        &deep,
        "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b",
        "main",
    );
    transcript(
        &home,
        &repo,
        "3b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b",
        "topic",
    );
    let session = wait_session(&daemon, &token, &deep, &folder, "folder");
    wait_session(&daemon, &token, &repo, &branch, "branch");
    let later = create("Later", &repo.join("sub"), None);
    wait_session(&daemon, &token, &deep, &later, "folder");
    transcript(
        &home,
        &deep,
        "4b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b",
        "probe",
    );
    wait_session(&daemon, &token, &deep, &later, "folder");
    let reply = daemon.post(
        &format!(
            "/v1/sessions/{}/link",
            session["id"].as_str().expect("synthetic test data")
        ),
        Some(&token),
        &json!({"workstream": id::SUBMISSION}),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let deepest = create("Deepest", &deep, None);
    wait_session(&daemon, &token, &deep, &deepest, "folder");
    wait_session(&daemon, &token, &deep, id::SUBMISSION, "manual");
}
