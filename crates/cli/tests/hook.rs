//! `pitcrew hook`, run as the real binary: it delivers the payload, and whatever happens it is
//! silent and exits 0 quickly.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use serde_json::json;
use std::io::Write as _;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

const PAYLOAD: &str = r#"{"session_id":"00000000-0000-4000-8000-000000000000","hook_event_name":"Stop","cwd":"/work/example"}"#;

/// Runs `pitcrew hook <args>` with only `env` among the `PITCREW_*` variables.
fn hook(args: &[&str], env: &[(&str, &str)], stdin: Option<&[u8]>) -> (Output, Duration) {
    let all: Vec<&str> = std::iter::once("hook")
        .chain(args.iter().copied())
        .collect();
    pitcrew(&all, env, stdin)
}

/// Runs `pitcrew <args>` with only `env` among the `PITCREW_*` variables, and a home of its own.
fn pitcrew(args: &[&str], env: &[(&str, &str)], stdin: Option<&[u8]>) -> (Output, Duration) {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = pitcrew_command(&home.path().join("home"));
    cmd.args(args);
    for var in [
        "PITCREW_SOCKET",
        "PITCREW_PIPE",
        "PITCREW_URL",
        "PITCREW_TOKEN",
        "PITCREW_TOKEN_FILE",
        "PITCREW_HOOK_DEBUG",
        "PITCREW_HOOK_TEST_PANIC",
        "PITCREW_CHAINED",
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
    checked(&mut cmd);
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
    let requests = server.wait_for_requests(1);
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
    let requests = server.wait_for_requests(1);
    let request = &requests[0];
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
    assert_eq!(server.wait_for_requests(1)[0].body, b"{}");
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
fn a_panic_in_the_hook_is_silent_unless_debugging() {
    // `PITCREW_HOOK_TEST_PANIC` makes a debug build panic on the hook path.
    let env = [("PITCREW_TOKEN", TOKEN), ("PITCREW_HOOK_TEST_PANIC", "1")];
    let (output, _) = hook(&["claude", "Stop"], &env, Some(PAYLOAD.as_bytes()));
    assert_silent_success(&output);

    let debug = [env[0], env[1], ("PITCREW_HOOK_DEBUG", "1")];
    let (output, _) = hook(&["claude", "Stop"], &debug, Some(PAYLOAD.as_bytes()));
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("a test panic in the hook"), "{err}");
}

#[test]
fn a_global_flag_before_hook_keeps_the_fast_path() {
    let server = accepting();
    let env = [
        ("PITCREW_URL", server.url.as_str()),
        ("PITCREW_TOKEN", TOKEN),
    ];
    let (output, _) = pitcrew(
        &["--json", "hook", "claude", "Stop"],
        &env,
        Some(PAYLOAD.as_bytes()),
    );
    assert_silent_success(&output);
    assert_eq!(
        server.wait_for_requests(1)[0].route(),
        "POST /v1/hooks/claude/Stop"
    );

    // Only the fast path has the test panic and its debug message.
    let debug = [
        env[0],
        env[1],
        ("PITCREW_HOOK_DEBUG", "1"),
        ("PITCREW_HOOK_TEST_PANIC", "1"),
    ];
    let (output, _) = pitcrew(&["--json", "hook", "claude", "Stop"], &debug, None);
    assert_eq!(output.status.code(), Some(0));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("a test panic in the hook"), "{err}");
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
    assert_eq!(server.wait_for_requests(1).len(), 1);
}

// The chain mechanism (`crate::hook::run_chained`) is a single, OS-independent code path —
// `std::process::Command::new(program).args(rest)`, no shell, no `cmd.exe` on either platform —
// so there is no separate Windows branch to exercise. These integration tests, run for real on
// Unix (where this suite runs), are the whole exercise; they are gated `cfg(unix)` only because
// building the fake original as an executable shell script needs `chmod`, not because the
// `--chain` logic itself differs on Windows.
#[cfg(unix)]
mod chain {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A fake original notifier (an executable shell script that records what it was given) plus
    /// the sidecar `install --chain` would have recorded for it — a `CODEX_HOME`'s worth of
    /// `--chain` state, built once per test.
    struct FakeChain {
        codex_home: tempfile::TempDir,
        captured: std::path::PathBuf,
    }

    impl FakeChain {
        fn new() -> Self {
            let codex_home = tempfile::TempDir::new().unwrap();
            let captured = codex_home.path().join("captured.txt");
            let fake_original = codex_home.path().join("fake-original.sh");
            std::fs::write(
                &fake_original,
                format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", captured.display()),
            )
            .unwrap();
            std::fs::set_permissions(&fake_original, std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let record = json!({
                "values": [fake_original.to_str().unwrap()],
                "toml": "[\"terminal-notifier\"]",
            });
            std::fs::write(
                codex_home.path().join("pitcrew-notify-original.json"),
                record.to_string(),
            )
            .unwrap();
            Self {
                codex_home,
                captured,
            }
        }

        fn codex_home(&self) -> &str {
            self.codex_home.path().to_str().unwrap()
        }

        /// Polls for the original to have run (it is spawned detached, never waited on, by
        /// design), returning what it captured, or `None` if it never did within the deadline.
        fn wait_for_capture(&self) -> Option<String> {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if let Ok(s) = std::fs::read_to_string(&self.captured)
                    && !s.is_empty()
                {
                    return Some(s);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            None
        }
    }

    #[test]
    fn runs_the_recorded_original_with_the_exact_payload_codex_passed() {
        let server = accepting();
        let chain = FakeChain::new();
        let notify_payload = r#"{"type":"agent-turn-complete","turn-id":"1"}"#;
        let env = [
            ("PITCREW_URL", server.url.as_str()),
            ("PITCREW_TOKEN", TOKEN),
            ("CODEX_HOME", chain.codex_home()),
        ];
        let (output, _) = hook(&["codex", "notify", "--chain", notify_payload], &env, None);
        assert_silent_success(&output);

        // Our own hook still got delivered.
        let requests = server.wait_for_requests(1);
        assert_eq!(requests[0].route(), "POST /v1/hooks/codex/notify");
        assert_eq!(requests[0].body, notify_payload.as_bytes());

        assert_eq!(chain.wait_for_capture().as_deref(), Some(notify_payload));
    }

    /// The blocking fix: the original must run even when *our own* delivery cannot even start —
    /// no token configured at all, exactly as `codex` run from a plain terminal would have it.
    #[test]
    fn runs_the_original_even_with_no_token_and_no_daemon_configured() {
        let chain = FakeChain::new();
        let notify_payload = r#"{"type":"agent-turn-complete","turn-id":"2"}"#;
        // No PITCREW_URL/PITCREW_SOCKET/PITCREW_TOKEN at all.
        let env = [("CODEX_HOME", chain.codex_home())];
        let (output, _) = hook(&["codex", "notify", "--chain", notify_payload], &env, None);
        assert_silent_success(&output);

        assert_eq!(chain.wait_for_capture().as_deref(), Some(notify_payload));
    }

    /// Same, but the daemon is configured and simply unreachable instead of unconfigured.
    #[test]
    fn runs_the_original_even_when_the_daemon_is_down() {
        let chain = FakeChain::new();
        let notify_payload = r#"{"type":"agent-turn-complete","turn-id":"3"}"#;
        let url = dead_url();
        let env = [
            ("PITCREW_URL", url.as_str()),
            ("PITCREW_TOKEN", TOKEN),
            ("CODEX_HOME", chain.codex_home()),
        ];
        let (output, took) = hook(&["codex", "notify", "--chain", notify_payload], &env, None);
        assert_silent_success(&output);
        assert!(took < Duration::from_secs(2), "{took:?}");

        assert_eq!(chain.wait_for_capture().as_deref(), Some(notify_payload));
    }

    /// The recursion guard: if we are, ourselves, something a chain already spawned
    /// (`PITCREW_CHAINED` set), `--chain` must not run the original again.
    #[test]
    fn does_not_chain_again_when_already_inside_a_chain() {
        let server = accepting();
        let chain = FakeChain::new();
        let notify_payload = r#"{"type":"agent-turn-complete","turn-id":"4"}"#;
        let env = [
            ("PITCREW_URL", server.url.as_str()),
            ("PITCREW_TOKEN", TOKEN),
            ("CODEX_HOME", chain.codex_home()),
            ("PITCREW_CHAINED", "1"),
        ];
        let (output, _) = hook(&["codex", "notify", "--chain", notify_payload], &env, None);
        assert_silent_success(&output);

        // Our own hook is still delivered: only the recursive re-chain is skipped.
        let requests = server.wait_for_requests(1);
        assert_eq!(requests[0].route(), "POST /v1/hooks/codex/notify");

        // Give a wrongly-spawned original a real chance to have written before asserting absence.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            std::fs::read_to_string(&chain.captured)
                .unwrap_or_default()
                .is_empty(),
            "must not have run the original again"
        );
    }

    /// `--chain` is set on the spawned original's own environment, so a chain that (directly, or
    /// through a shell) calls `pitcrew hook codex notify --chain` again cannot recurse.
    #[test]
    fn sets_pitcrew_chained_on_the_spawned_original() {
        let server = accepting();
        let codex_home = tempfile::TempDir::new().unwrap();
        let captured = codex_home.path().join("captured.txt");
        let fake_original = codex_home.path().join("fake-original.sh");
        std::fs::write(
            &fake_original,
            format!(
                "#!/bin/sh\nprintf '%s' \"$PITCREW_CHAINED\" > '{}'\n",
                captured.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_original, std::fs::Permissions::from_mode(0o755)).unwrap();
        let record = json!({"values": [fake_original.to_str().unwrap()], "toml": "[\"x\"]"});
        std::fs::write(
            codex_home.path().join("pitcrew-notify-original.json"),
            record.to_string(),
        )
        .unwrap();

        let env = [
            ("PITCREW_URL", server.url.as_str()),
            ("PITCREW_TOKEN", TOKEN),
            ("CODEX_HOME", codex_home.path().to_str().unwrap()),
        ];
        let (output, _) = hook(&["codex", "notify", "--chain", "{}"], &env, None);
        assert_silent_success(&output);

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut seen = String::new();
        while Instant::now() < deadline {
            seen = std::fs::read_to_string(&captured).unwrap_or_default();
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(seen, "1");
    }
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
    assert_eq!(
        private.wait_for_requests(1)[0].route(),
        "POST /v1/hooks/claude/Stop"
    );

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
