//! `pitcrew hook`, run as the real binary: it delivers the payload, and whatever happens it is
//! silent and exits 0 quickly.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use serde_json::json;
use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const PAYLOAD: &str = r#"{"session_id":"00000000-0000-4000-8000-000000000000","hook_event_name":"Stop","cwd":"/work/example"}"#;

/// Runs the binary with only `env` among the `PITCREW_*` variables.
fn hook(args: &[&str], env: &[(&str, &str)], stdin: Option<&[u8]>) -> (Output, Duration) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pitcrew"));
    cmd.arg("hook").args(args);
    for var in [
        "PITCREW_SOCKET",
        "PITCREW_PIPE",
        "PITCREW_URL",
        "PITCREW_TOKEN",
        "PITCREW_TOKEN_FILE",
        "PITCREW_HOOK_DEBUG",
    ] {
        cmd.env_remove(var);
    }
    cmd.envs(env.iter().copied())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap();
    if let Some(bytes) = stdin {
        // The hook may stop reading early (an oversized payload); that is fine.
        let _ = child.stdin.take().unwrap().write_all(bytes);
    }
    let output = child.wait_with_output().unwrap();
    (output, started.elapsed())
}

fn assert_silent_success(output: &Output) {
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn accepting() -> FakeServer {
    FakeServer::tcp(std::sync::Arc::new(|_| (202, String::new())))
}

#[test]
fn delivers_the_stdin_payload() {
    let server = accepting();
    let env = [
        ("PITCREW_URL", server.url.as_str()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let (output, _) = hook(&["claude", "Stop"], &env, Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "no version check, one request");
    let request = &requests[0];
    assert_eq!(request.route(), "POST /v1/hooks/claude/Stop");
    assert_eq!(request.body, PAYLOAD.as_bytes());
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(request.header("content-type"), Some("application/json"));
}

#[test]
fn takes_the_payload_as_an_argument_for_codex_notify() {
    let server = accepting();
    let env = [
        ("PITCREW_URL", server.url.as_str()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let notify = r#"{"type":"agent-turn-complete","turn-id":"1"}"#;
    let (output, _) = hook(&["codex", "notify", notify], &env, None);
    assert_silent_success(&output);
    let request = &server.requests()[0];
    assert_eq!(request.route(), "POST /v1/hooks/codex/notify");
    assert_eq!(request.body, notify.as_bytes());
}

#[test]
fn an_empty_payload_is_an_empty_object() {
    let server = accepting();
    let env = [
        ("PITCREW_URL", server.url.as_str()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let (output, _) = hook(&["claude", "SessionEnd"], &env, None);
    assert_silent_success(&output);
    assert_eq!(server.requests()[0].body, b"{}");
}

#[test]
fn silent_and_quick_when_the_daemon_is_down() {
    let url = dead_url();
    let env = [("PITCREW_URL", url.as_str()), ("PITCREW_TOKEN", TOKEN)];
    let (output, took) = hook(&["claude", "Stop"], &env, Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn silent_when_not_configured() {
    let (output, _) = hook(&["claude", "Stop"], &[], Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);
    let (output, _) = hook(&[], &[], None);
    assert_silent_success(&output);
}

#[test]
fn refusals_are_silent_unless_debugging() {
    let server = FakeServer::tcp(routes(&[(
        "POST /v1/hooks/claude/Stop",
        400,
        json!({"code": "invalid", "message": "Synthetic refusal."}),
    )]));
    let env = [
        ("PITCREW_URL", server.url.as_str()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let (output, _) = hook(&["claude", "Stop"], &env, Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);

    let debug = [env[0], env[1], ("PITCREW_HOOK_DEBUG", "1")];
    let (output, _) = hook(&["claude", "Stop"], &debug, Some(PAYLOAD.as_bytes()));
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("Synthetic refusal."), "{err}");
}

#[test]
fn bad_input_is_not_sent() {
    let server = accepting();
    let env = [
        ("PITCREW_URL", server.url.as_str()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let too_big = format!("{{\"x\":\"{}\"}}", "a".repeat(1 << 20));
    for (args, stdin) in [
        (&["gemini", "Stop"][..], PAYLOAD.as_bytes()),
        (&["claude", "../Stop"], PAYLOAD.as_bytes()),
        (&["claude", "Stop"], b"not json".as_slice()),
        (&["claude", "Stop"], too_big.as_bytes()),
    ] {
        let (output, _) = hook(args, &env, Some(stdin));
        assert_silent_success(&output);
    }
    assert!(server.requests().is_empty());
}

#[test]
fn gives_up_on_a_daemon_that_never_answers() {
    let server = FakeServer::tcp(std::sync::Arc::new(|_| (0, String::new())));
    let env = [
        ("PITCREW_URL", server.url.as_str()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let (output, took) = hook(&["claude", "Stop"], &env, Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert_eq!(server.requests().len(), 1);
}

#[cfg(unix)]
#[test]
fn over_a_socket_only_a_private_one_gets_the_token() {
    let handler: Handler = std::sync::Arc::new(|_| (202, String::new()));
    let (private, socket) = FakeServer::unix(handler.clone(), 0o700);
    let env = [
        ("PITCREW_SOCKET", socket.to_str().unwrap()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let (output, _) = hook(&["claude", "Stop"], &env, Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);
    assert_eq!(private.requests()[0].route(), "POST /v1/hooks/claude/Stop");

    let (open, socket) = FakeServer::unix(handler, 0o755);
    let env = [
        ("PITCREW_SOCKET", socket.to_str().unwrap()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let (output, _) = hook(&["claude", "Stop"], &env, Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);
    std::thread::sleep(Duration::from_millis(50));
    assert!(open.requests().is_empty());
}
