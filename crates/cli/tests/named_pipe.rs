//! Verbs and the hook over a real named pipe (Windows only).
//!
//! The pipe is the daemon's own listener (`pitcrew_api::NamedPipe`): its descriptor names the
//! current user as the owner, which the CLI requires. A pipe created with the default descriptor
//! is owned by the token's default owner instead, which for an elevated process (or on a machine
//! whose policy says so) is the Administrators group, and the CLI rightly refuses it.

#![cfg(windows)]
#![allow(clippy::unwrap_used)]

mod common;

use axum::serve::Listener as _;
use common::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::NamedPipeServer;

/// Serves `handler` on a new pipe from a background thread. Returns the pipe's name and the
/// requests it receives.
fn serve_pipe(handler: Handler) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!(r"\\.\pipe\pitcrew-cli-test-{}-{nanos}", std::process::id());
    let log = Arc::new(Mutex::new(Vec::new()));
    let (ready, is_ready) = std::sync::mpsc::channel();
    let (pipe, requests) = (name.clone(), Arc::clone(&log));
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut listener = pitcrew_api::NamedPipe::bind(&pipe).unwrap();
            ready.send(()).unwrap();
            loop {
                let (mut conn, _) = listener.accept().await;
                let Some(request) = read_request(&mut conn).await else {
                    continue;
                };
                requests.lock().unwrap().push(request.clone());
                let (status, body) = handler(&request);
                let _ = conn.write_all(&reply_bytes(status, &body)).await;
                let _ = conn.flush().await;
            }
        });
    });
    is_ready.recv().unwrap();
    (name, log)
}

async fn read_request(conn: &mut NamedPipeServer) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(request) = parse_request(&buf) {
            return Some(request);
        }
        let n = conn.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[test]
fn verbs_work_over_a_named_pipe() {
    let (pipe, requests) = serve_pipe(routes(&[
        ("GET /v1/me", 200, writer()),
        ("GET /v1/members", 200, members()),
    ]));
    let (code, out, err) = run(
        &["whoami"],
        &[("PITCREW_PIPE", &pipe), ("PITCREW_TOKEN", TOKEN)],
        "",
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(out, "@writer (Writer), an agent of @sam\n");
    let requests = requests.lock().unwrap();
    assert_eq!(requests[0].target, "/v1/host/info");
    assert_eq!(requests[1].header("host"), Some("localhost"));
    assert_eq!(
        requests[1].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

#[test]
fn a_missing_pipe_is_unavailable() {
    let (code, _, err) = run(
        &["whoami"],
        &[
            ("PITCREW_PIPE", r"\\.\pipe\pitcrew-cli-test-nobody-here"),
            ("PITCREW_TOKEN", TOKEN),
        ],
        "",
    );
    assert_eq!(code, 5, "{err}");
}

#[test]
fn the_hook_delivers_over_a_named_pipe() {
    let (pipe, requests) = serve_pipe(Arc::new(|_| (202, String::new())));
    let home = tempfile::tempdir().unwrap();
    let mut command = pitcrew_command(&home.path().join("home"));
    command
        .args(["hook", "claude", "Stop"])
        .env_remove("PITCREW_URL")
        .env_remove("PITCREW_TOKEN_FILE")
        .env("PITCREW_PIPE", &pipe)
        .env("PITCREW_TOKEN", TOKEN)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = checked(&mut command).spawn().unwrap();
    {
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(b"{\"a\":1}").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests[0].route(), "POST /v1/hooks/claude/Stop");
    assert_eq!(requests[0].body, b"{\"a\":1}");
}
