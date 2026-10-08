//! HTTP file access with a synthetic workstream root; no runner or real agent homes.
mod common;
use common::{Daemon, id, request};
use serde_json::{Value, json};
use std::fs;

#[test]
fn device_files_routes_resolve_roots_and_enforce_limits() -> Result<(), Box<dyn std::error::Error>>
{
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("files");
    fs::create_dir(&root)?;
    fs::write(root.join("hello.txt"), "hello")?;
    fs::write(root.join(".gitignore"), "*.log\n")?;
    fs::write(root.join(".git"), "synthetic worktree marker")?;
    fs::write(root.join("debug.log"), "synthetic ignored file")?;
    fs::File::create(root.join("large"))?.set_len(8 * 1024 * 1024 + 1)?;
    let mut daemon = Daemon::start(
        &tmp.path().join("state"),
        &["--demo", "--no-runner", "--no-office"],
    );
    let token = daemon.device_token();
    let agent = daemon.agent_token();
    let stream = daemon.post("/v1/workstreams", Some(&token), &json!({ "project": id::PAPER, "name": "Synthetic files", "locations": [{ "machine": id::LAPTOP, "path": root }, { "machine": id::CLUSTER, "path": root }] }));
    assert_eq!(stream.status, 201);
    let stream = stream.json();
    let Some(stream_id) = stream["id"].as_str() else {
        panic!("stream id")
    };
    let list = format!("/v1/workstreams/{stream_id}/files?loc=0&path=");
    let file = format!("/v1/workstreams/{stream_id}/files/content?loc=0&path=hello.txt");
    assert_eq!(daemon.get(&list, Some(&token)).status, 200);
    let entries = daemon.get(&list, Some(&token)).json();
    let entries = entries["entries"].as_array().expect("entries");
    assert_eq!(
        entries
            .iter()
            .find(|e| e["name"] == "debug.log")
            .expect("ignored file")["ignored"],
        true
    );
    assert!(
        entries
            .iter()
            .any(|e| e["name"] == ".git" && e["kind"] == "file")
    );
    for path in [&list, &file] {
        assert_eq!(daemon.get(path, Some(&agent)).status, 403);
    }
    let read = daemon.get(&file, Some(&token));
    assert_eq!(read.status, 200);
    let read = read.json();
    assert_eq!(read["content"], "hello");
    let put = |body: Value, token: &str| {
        request(daemon.port, "PUT", &file, Some(token), Some(&body), &[])
    };
    let write = json!({ "revision": read["revision"], "encoding": "utf8", "content": "new" });
    assert_eq!(put(write.clone(), &agent).status, 403);
    let updated = put(write.clone(), &token);
    assert_eq!(updated.status, 200);
    let conflict = put(write, &token);
    assert_eq!(conflict.status, 409);
    assert_eq!(
        conflict.json()["current_revision"],
        updated.json()["revision"]
    );
    assert_eq!(
        put(json!({"encoding":"utf8", "content":"missing"}), &token).status,
        400
    );
    for (query, status) in [
        ("loc=0&path=..", 400),
        ("loc=0&path=/absolute", 400),
        ("loc=-1&path=", 400),
        ("loc=99&path=", 404),
        ("loc=1&path=", 501),
    ] {
        assert_eq!(
            daemon
                .get(
                    &format!("/v1/workstreams/{stream_id}/files?{query}"),
                    Some(&token)
                )
                .status,
            status
        );
    }
    assert_eq!(
        daemon
            .get(&file.replace("hello.txt", "large"), Some(&token))
            .status,
        413
    );
    assert_eq!(put(json!({ "revision": null, "encoding":"utf8", "content": "x".repeat(8 * 1024 * 1024 + 1) }), &token).status, 413);
    assert_eq!(
        put(
            json!({ "revision": null, "encoding":"utf8", "content": "x".repeat(12 * 1024 * 1024) }),
            &token
        )
        .status,
        413
    );
    assert_eq!(fs::read(root.join("hello.txt"))?, b"new");
    daemon.stop();
    Ok(())
}
