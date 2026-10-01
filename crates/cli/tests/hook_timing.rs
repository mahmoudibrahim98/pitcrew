//! Wall time of `pitcrew hook`, from spawn to exit, over 200 runs: at most 10 ms with a daemon
//! up and 5 ms with none listening. Measure a release build:
//!
//! ```text
//! cargo test -p pitcrew-cli --release --test hook_timing -- --ignored --nocapture
//! ```
//!
//! The daemon is an in-process fake on the platform's transport (a unix socket; loopback TCP
//! elsewhere). Set `PITCREW_TIMING_URL` (and `PITCREW_TIMING_TOKEN`) to time a real server
//! instead, such as the mock hub.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RUNS: usize = 200;
const PAYLOAD: &str = r#"{"session_id":"00000000-0000-4000-8000-000000000000","transcript_path":"/work/example/session.jsonl","cwd":"/work/example","hook_event_name":"Stop","stop_hook_active":false}"#;

/// Spawns the hook `RUNS` times. Its stdin is a file, so the measurement does not depend on this
/// process being scheduled to write it.
fn time_runs(env: &[(String, String)]) -> Vec<Duration> {
    let tmp = tempfile::tempdir().unwrap();
    let payload = tmp.path().join("payload.json");
    std::fs::write(&payload, PAYLOAD).unwrap();
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pitcrew"));
        cmd.args(["hook", "claude", "Stop"]);
        for var in [
            "PITCREW_SOCKET",
            "PITCREW_PIPE",
            "PITCREW_URL",
            "PITCREW_TOKEN",
            "PITCREW_TOKEN_FILE",
        ] {
            cmd.env_remove(var);
        }
        cmd.envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(std::fs::File::open(&payload).unwrap())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let started = Instant::now();
        let status = cmd.status().unwrap();
        times.push(started.elapsed());
        assert!(status.success());
    }
    times.sort();
    times
}

fn percentile(sorted: &[Duration], p: usize) -> Duration {
    sorted[(sorted.len() * p / 100).min(sorted.len() - 1)]
}

/// Prints the percentiles; returns a failure if p99 is over `limit` (`None`: just report).
fn report(label: &str, times: &[Duration], limit: Option<Duration>) -> Option<String> {
    let (p50, p99) = (percentile(times, 50), percentile(times, 99));
    let limit_text = limit.map_or_else(
        || "for information".to_owned(),
        |l| format!("limit {} ms", l.as_millis()),
    );
    println!(
        "{label}: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms over {} runs ({limit_text})",
        p50.as_secs_f64() * 1e3,
        p99.as_secs_f64() * 1e3,
        times[times.len() - 1].as_secs_f64() * 1e3,
        times.len(),
    );
    limit
        .filter(|l| p99 > *l)
        .map(|l| format!("{label}: p99 {p99:?} is over {l:?}"))
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
#[ignore = "timing: run a release build with --ignored"]
fn hook_wall_time() {
    const UP: Option<Duration> = Some(Duration::from_millis(10));
    const DOWN: Option<Duration> = Some(Duration::from_millis(5));
    let accept: Handler = std::sync::Arc::new(|_| (202, String::new()));
    let mut failures = Vec::new();

    // The floor: start, find no token, exit.
    let floor = time_runs(&env(&[]));
    failures.extend(report(
        "not configured (process start and exit)",
        &floor,
        None,
    ));

    #[cfg(unix)]
    {
        let (server, socket) = FakeServer::unix(accept.clone(), 0o700);
        let socket = socket.to_str().unwrap().to_owned();
        let up = time_runs(&env(&[
            ("PITCREW_SOCKET", &socket),
            ("PITCREW_TOKEN", TOKEN),
        ]));
        assert_eq!(server.requests().len(), RUNS);
        failures.extend(report("up (unix socket)", &up, UP));

        // A socket file with nothing listening, as a crashed daemon leaves behind.
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("pitcrewd.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        let stale = stale.to_str().unwrap().to_owned();
        let down = time_runs(&env(&[
            ("PITCREW_SOCKET", &stale),
            ("PITCREW_TOKEN", TOKEN),
        ]));
        failures.extend(report("down (stale unix socket)", &down, DOWN));
    }

    let server = FakeServer::tcp(accept);
    let up = time_runs(&env(&[
        ("PITCREW_URL", &server.url),
        ("PITCREW_TOKEN", TOKEN),
    ]));
    assert_eq!(server.requests().len(), RUNS);
    failures.extend(report("up (loopback TCP)", &up, UP));

    let dead = dead_url();
    let down = time_runs(&env(&[("PITCREW_URL", &dead), ("PITCREW_TOKEN", TOKEN)]));
    failures.extend(report(
        "down (loopback TCP, nothing listening)",
        &down,
        DOWN,
    ));

    // Another server's own latency is not the hook's, so it is only reported.
    if let Ok(url) = std::env::var("PITCREW_TIMING_URL") {
        let token = std::env::var("PITCREW_TIMING_TOKEN").unwrap_or_else(|_| TOKEN.to_owned());
        let times = time_runs(&env(&[("PITCREW_URL", &url), ("PITCREW_TOKEN", &token)]));
        failures.extend(report(&format!("up ({url})"), &times, None));
    }

    assert!(failures.is_empty(), "{failures:#?}");
}
