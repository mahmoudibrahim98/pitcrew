//! Opt-in measurements over the existing synthetic-history harness.
use super::*;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

type Written = (Vec<homes::Live>, usize);

/// A writer owns synthetic transcripts only. Even an early error stops and joins its thread.
struct Writer {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<Written>>>,
}
impl Writer {
    fn start(live: Vec<homes::Live>) -> Result<Self> {
        if live.len() != 50 {
            return Err(format!(
                "expected fifty growing transcripts, got {}",
                live.len()
            ));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut live = live;
            let mut written = 0;
            while !done.load(Ordering::Relaxed) {
                live[written % 50]
                    .append_line()
                    .map_err(|e| e.to_string())?;
                written += 1;
                // Stagger fifty writes: one line per transcript every five seconds.
                thread::sleep(Duration::from_millis(100));
            }
            Ok((live, written))
        });
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
    fn finish(mut self) -> Result<Written> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .ok_or("writer already stopped")?
            .join()
            .map_err(|_| "synthetic writer panicked")?
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn json_request(
    d: &Daemon,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    status: u16,
) -> Result<Value> {
    let text = body.map(|v| v.to_string());
    let r = client::request(d.port, method, path, Some(token), text.as_deref())
        .map_err(|e| e.to_string())?;
    if r.status != status {
        return Err(format!(
            "{method} {path}: status {}, expected {status}",
            r.status
        ));
    }
    if status == 202 && r.body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&r.body).map_err(|e| e.to_string())
}

fn session_id(d: &Daemon, token: &str, native: &str) -> Result<String> {
    let sessions = json_request(d, token, "GET", "/v1/sessions", None, 200)?;
    sessions
        .as_array()
        .ok_or("sessions are not an array")?
        .iter()
        .find(|s| s["native_id"] == native)
        .and_then(|s| s["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "the probe session is not in the API".to_owned())
}

fn verify_consumed(env: &Env, live: &[homes::Live]) -> Result<()> {
    let mut watch = Watch::new(&env.state);
    let began = Instant::now();
    loop {
        let mut consumed = 0;
        if let Some(index) = watch.index() {
            for l in live {
                let offset: Option<i64> = index
                    .query_row(
                        "SELECT json_extract(cursor, '$.offset') FROM transcripts WHERE path = ?1",
                        [l.path.to_string_lossy().as_ref()],
                        |r| r.get(0),
                    )
                    .ok();
                if offset.is_some_and(|n| u64::try_from(n).is_ok_and(|n| n >= file_len(&l.path))) {
                    consumed += 1;
                }
            }
        }
        if consumed == live.len() {
            return Ok(());
        }
        if began.elapsed() > Duration::from_secs(30) {
            return Err(format!(
                "runner consumed only {consumed}/{} growing transcripts",
                live.len()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn cpu(d: &Daemon, window: Duration, name: &str) -> Result<()> {
    let ticks_per_second = Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .map_err(|e| e.to_string())?;
    if !ticks_per_second.status.success() {
        return Err("getconf CLK_TCK failed".to_owned());
    }
    let hz: f64 = String::from_utf8_lossy(&ticks_per_second.stdout)
        .trim()
        .parse()
        .map_err(|_| "bad CLK_TCK")?;
    if hz <= 0.0 {
        return Err("nonpositive CLK_TCK".to_owned());
    }
    let before = procfs::cpu_ticks(d.pid).ok_or("no initial CPU ticks")?;
    let began = Instant::now();
    while began.elapsed() < window {
        thread::sleep(Duration::from_millis(100));
        if !procfs::alive(d.pid) {
            return Err("daemon died in CPU window".to_owned());
        }
    }
    let elapsed = began.elapsed().as_secs_f64();
    let delta = procfs::cpu_ticks(d.pid)
        .ok_or("no final CPU ticks")?
        .saturating_sub(before);
    let percent = delta as f64 / hz / elapsed * 100.0;
    emit(
        name,
        percent,
        percent,
        "%cpu",
        &format!(
            "{delta} ticks / {elapsed:.2} s at {hz} Hz; whole daemon; budget 0.5%; {}",
            if percent <= 0.5 { "within" } else { "OVER" }
        ),
    );
    Ok(())
}

fn cli_command(options: &Options, env: &Env, port: u16, token: &str) -> Command {
    let mut c = Command::new(&options.pitcrew);
    pitcrew_fixtures::homes::private_home(&mut c, &env.work.join("cli-home"));
    for var in [
        "PITCREW_SOCKET",
        "PITCREW_PIPE",
        "PITCREW_URL",
        "PITCREW_TOKEN",
        "PITCREW_TOKEN_FILE",
        "PITCREW_HOOK_DEBUG",
    ] {
        c.env_remove(var);
    }
    c.env("PITCREW_URL", format!("http://127.0.0.1:{port}"))
        .env("PITCREW_TOKEN", token)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    pitcrew_fixtures::homes::check_private_home(&c);
    c
}

fn finish_cli(mut child: Child, started: Instant) -> Result<(Duration, std::process::Output)> {
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            let elapsed = started.elapsed();
            return Ok((
                elapsed,
                child.wait_with_output().map_err(|e| e.to_string())?,
            ));
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("CLI did not exit in five seconds".to_owned());
        }
        thread::sleep(Duration::from_micros(50));
    }
}

fn timings(name: &str, times: Vec<f64>, budget: f64, p: f64) {
    let times = sorted(times);
    let value = percentile(&times, p);
    println!(
        "scale: {name}: min {:.3}, p50 {:.3}, p95 {:.3}, p99 {:.3}, max {:.3} ms over {} (budget {budget} ms; {})",
        times[0],
        percentile(&times, 0.5),
        percentile(&times, 0.95),
        percentile(&times, 0.99),
        times[times.len() - 1],
        times.len(),
        if value <= budget { "within" } else { "OVER" }
    );
    let operation = if name.ends_with("hook_to_frame") {
        "POST to matching stream frame"
    } else {
        "spawn to exit"
    };
    emit(
        name,
        value,
        times[0],
        "ms",
        &format!("{operation}; percentile {}; budget {budget} ms", p * 100.0),
    );
}

fn verbs(options: &Options, env: &Env, d: &Daemon, token: &str, agent_token: &str) -> Result<()> {
    let p = json_request(
        d,
        token,
        "POST",
        "/v1/projects",
        Some(json!({"key":"BENCH","name":"Synthetic benchmark"})),
        201,
    )?;
    let task = json_request(
        d,
        token,
        "POST",
        "/v1/tasks",
        Some(json!({"project":p["id"],"title":"Measure round trips"})),
        201,
    )?;
    let key = task["key"].as_str().ok_or("no task key")?;
    for (name, args) in [
        ("more.cli.whoami", vec!["whoami"]),
        ("more.cli.task_list", vec!["task", "list"]),
        ("more.cli.task_show", vec!["task", "show", key]),
    ] {
        let mut times = Vec::new();
        for n in 0..options.probes + options.warmup {
            let mut command = cli_command(options, env, d.port, agent_token);
            command.arg("--json").args(&args);
            let began = Instant::now();
            let (elapsed, out) = finish_cli(command.spawn().map_err(|e| e.to_string())?, began)?;
            if !out.status.success() {
                return Err(format!("{name} exited {}", out.status));
            }
            let value: Value =
                serde_json::from_slice(&out.stdout).map_err(|e| format!("{name}: {e}"))?;
            if value.is_null() {
                return Err(format!("{name}: null output"));
            }
            if name.ends_with("task_show") && value["key"] != key {
                return Err("task show returned another task".to_owned());
            }
            if n >= options.warmup {
                times.push(ms(elapsed));
            }
        }
        timings(name, times, 50.0, 0.95);
    }
    Ok(())
}

fn reset_probe(d: &Daemon, token: &str, native: &str, id: &str) -> Result<()> {
    // A previous odd-length hook run can leave the probe working. Start idle so the
    // first UserPromptSubmit must produce a transition rather than an idempotent no-op.
    json_request(
        d,
        token,
        "POST",
        "/v1/hooks/claude/Stop",
        Some(json!({"session_id":native,"hook_event_name":"Stop"})),
        202,
    )?;
    let started = Instant::now();
    loop {
        let s = json_request(d, token, "GET", &format!("/v1/sessions/{id}"), None, 200)?;
        if s["state"] == "idle" {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(2) {
            return Err("probe did not become idle before hook measurements".to_owned());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn hooks_cli(options: &Options, env: &Env, d: &Daemon, token: &str, native: &str) -> Result<()> {
    let id = session_id(d, token, native)?;
    reset_probe(d, token, native, &id)?;
    let mut times = Vec::new();
    for n in 0..options.probes + options.warmup {
        let event = if n % 2 == 0 {
            "UserPromptSubmit"
        } else {
            "Stop"
        };
        let payload = env.work.join("hook-payload.json");
        fs::write(
            &payload,
            json!({"session_id":native,"hook_event_name":event}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        let mut command = cli_command(options, env, d.port, token);
        command
            .args(["hook", "claude", event])
            .stdin(File::open(&payload).map_err(|e| e.to_string())?);
        let began = Instant::now();
        let (elapsed, out) = finish_cli(command.spawn().map_err(|e| e.to_string())?, began)?;
        if !out.status.success() || !out.stdout.is_empty() || !out.stderr.is_empty() {
            return Err("hook did not silently exit zero".to_owned());
        }
        // Exit zero alone is not proof of delivery: it is also the failure behavior.
        let expected = if event == "Stop" { "idle" } else { "working" };
        let wait = Instant::now();
        loop {
            let s = json_request(d, token, "GET", &format!("/v1/sessions/{id}"), None, 200)?;
            if s["state"] == expected {
                break;
            }
            if wait.elapsed() > Duration::from_secs(2) {
                return Err("hook exited zero but its state change did not arrive".to_owned());
            }
            thread::sleep(Duration::from_millis(10));
        }
        if n >= options.warmup {
            times.push(ms(elapsed));
        }
    }
    timings("more.hook.up", times, 10.0, 0.99);
    Ok(())
}

fn hooks_down(options: &Options, env: &Env, port: u16, token: &str) -> Result<()> {
    let payload = env.work.join("hook-down.json");
    fs::write(
        &payload,
        r#"{"session_id":"00000000-0000-4000-8000-000000000000","hook_event_name":"Stop"}"#,
    )
    .map_err(|e| e.to_string())?;
    let mut times = Vec::new();
    for n in 0..options.probes + options.warmup {
        let mut c = cli_command(options, env, port, token);
        c.args(["hook", "claude", "Stop"])
            .stdin(File::open(&payload).map_err(|e| e.to_string())?);
        let began = Instant::now();
        let (elapsed, out) = finish_cli(c.spawn().map_err(|e| e.to_string())?, began)?;
        if !out.status.success() || !out.stdout.is_empty() || !out.stderr.is_empty() {
            return Err("down hook did not silently exit zero".to_owned());
        }
        if n >= options.warmup {
            times.push(ms(elapsed));
        }
    }
    timings("more.hook.down", times, 5.0, 0.99);
    Ok(())
}

fn hook_frame(text: &str, id: &str, to: &str, after: u64) -> bool {
    let Ok(pitcrew_protocol::api::StreamFrame::Events {
        from_rev, events, ..
    }) = serde_json::from_str(text)
    else {
        return false;
    };
    let Ok(id) = id.parse::<pitcrew_protocol::SessionId>() else {
        return false;
    };
    events.iter().enumerate().any(|(i, e)| {
        from_rev
            .checked_add(i as u64)
            .is_some_and(|rev| rev > after)
            && match &e.body {
                pitcrew_protocol::events::EventBody::SessionStateChanged {
                    session,
                    to: state,
                    ..
                } => *session == id && serde_json::to_value(state).ok() == Some(json!(to)),
                _ => false,
            }
    })
}

fn hooks_live(options: &Options, env: &Env, d: &Daemon, token: &str, native: &str) -> Result<()> {
    let id = session_id(d, token, native)?;
    reset_probe(d, token, native, &id)?;
    let mut ws = Ws::connect(d.port, "/v1/stream", token).map_err(|e| e.to_string())?;
    let hello = ws
        .next_text(Duration::from_secs(10))
        .map_err(|e| e.to_string())?
        .ok_or("no hello")?;
    if !matches!(
        serde_json::from_str::<pitcrew_protocol::api::StreamFrame>(&hello),
        Ok(pitcrew_protocol::api::StreamFrame::Hello { .. })
    ) {
        return Err("stream did not start with hello".to_owned());
    }
    let (send, frames) = mpsc::channel();
    let reader = thread::spawn(move || {
        while let Ok(Some(text)) = ws.next_text(Duration::from_secs(10)) {
            if send.send((Instant::now(), text)).is_err() {
                break;
            }
        }
    });
    let outcome = (|| {
        let mut times = Vec::new();
        let mut watch = Watch::new(&env.state);
        for n in 0..options.probes + options.warmup {
            thread::sleep(Duration::from_millis(400));
            while frames.try_recv().is_ok() {}
            let rev = watch.rev().ok_or("no log revision")?;
            let event = if n % 2 == 0 {
                "UserPromptSubmit"
            } else {
                "Stop"
            };
            let expected = if event == "Stop" { "idle" } else { "working" };
            let began = Instant::now();
            json_request(
                d,
                token,
                "POST",
                &format!("/v1/hooks/claude/{event}"),
                Some(json!({"session_id":native,"hook_event_name":event})),
                202,
            )?;
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut observed = Vec::new();
            loop {
                let (at, text) = frames
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| format!("no matching hook state frame for {event} in ten seconds; target {id}; observed {observed:?}"))?;
                if observed.len() < 5 {
                    match serde_json::from_str::<pitcrew_protocol::api::StreamFrame>(&text) {
                        Ok(pitcrew_protocol::api::StreamFrame::Events {
                            to_rev, events, ..
                        }) => observed.push(format!(
                            "rev={to_rev}, floor={rev}, states={:?}",
                            events
                                .iter()
                                .filter_map(|e| match &e.body {
                                    pitcrew_protocol::events::EventBody::SessionStateChanged {
                                        session,
                                        to,
                                        ..
                                    } if id.parse::<pitcrew_protocol::SessionId>().ok()
                                        == Some(*session) =>
                                        Some(to),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                        )),
                        Ok(_) => observed.push("other frame".to_owned()),
                        Err(e) => observed.push(format!("parse error: {e}")),
                    }
                }
                if hook_frame(&text, &id, expected, rev) {
                    if n >= options.warmup {
                        times.push(ms(at.duration_since(began)));
                    }
                    break;
                }
            }
        }
        timings("more.hook_to_frame", times, 300.0, 0.5);
        Ok(())
    })();
    drop(frames);
    // The reader has a bounded read timeout, and its socket closes when it exits.
    let _ = reader.join();
    outcome
}

pub(super) fn measure(
    options: &Options,
    env: &Env,
    made: &mut Generated,
    total: u64,
) -> Result<()> {
    if options.stage != Stage::Cpu && !options.pitcrew.is_file() {
        return Err("build pitcrew beside pitcrewd or pass --pitcrew".to_owned());
    }
    let native = made.probe.as_ref().ok_or("no probe")?.native_id.clone();
    // A fresh demo provides a real registered agent and minted token for the agent-only CLI.
    // --homes remains synthetic, so it also watches the full generated history.
    let mut d = Daemon::start_inner(env, Tmux::Refused, 80, true)?;
    let token = d.token(env)?;
    let agent_token =
        fs::read_to_string(env.state.join("demo-agent.token")).map_err(|e| e.to_string())?;
    let scan = Instant::now();
    let mut watch = Watch::new(&env.state);
    loop {
        if watch.indexed().is_some_and(|(_, caught)| caught >= total) {
            break;
        }
        if !procfs::alive(d.pid) {
            return Err("daemon died in initial history scan".to_owned());
        }
        if scan.elapsed() > options.scan_timeout {
            return Err("history scan timed out".to_owned());
        }
        thread::sleep(Duration::from_millis(100));
    }
    println!(
        "scale: indexed {total} generated transcripts in {:.2} s (not a cold-scan budget sample)",
        scan.elapsed().as_secs_f64()
    );
    if !wait_idle(d.pid, options.idle) {
        return Err("daemon did not settle before measurement".to_owned());
    }
    println!(
        "scale: remaining budgets; {total} transcripts; load {}",
        load_average()
    );
    if matches!(options.stage, Stage::Cpu | Stage::More) {
        cpu(&d, options.cpu_window, "more.cpu.static")?;
        let live = if made.live.len() == 51 {
            made.live.split_off(1)
        } else {
            std::mem::take(&mut made.live)
        };
        let writer = Writer::start(live)?;
        thread::sleep(Duration::from_secs(10));
        cpu(&d, options.cpu_window, "more.cpu.growing")?;
        let (live, written) = writer.finish()?;
        verify_consumed(env, &live)?;
        println!("scale: {written} lines written; all fifty runner cursors reached the new ends");
        made.live.extend(live);
    }
    if matches!(options.stage, Stage::Verbs | Stage::More) {
        verbs(options, env, &d, &token, agent_token.trim())?;
    }
    if matches!(options.stage, Stage::HookCli | Stage::More) {
        hooks_cli(options, env, &d, &token, &native)?;
    }
    if matches!(options.stage, Stage::HookLive | Stage::More) {
        let live = made.live.split_off(1);
        let writer = Writer::start(live)?;
        thread::sleep(Duration::from_secs(10));
        hooks_live(options, env, &d, &token, &native)?;
        let (live, written) = writer.finish()?;
        verify_consumed(env, &live)?;
        println!("scale: stream probes amid {written} lines; all fifty runner cursors caught up");
    }
    let port = d.port;
    d.stop()?;
    if matches!(options.stage, Stage::HookCli | Stage::More) {
        hooks_down(options, env, port, &token)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrelated_or_old_live_events_cannot_satisfy_a_probe() {
        // Unknown/bad schemas cannot pass the typed parser, nor can an old revision.
        assert!(!hook_frame(
            r#"{"type":"ping","at":1}"#,
            "session",
            "working",
            1
        ));
        assert!(!hook_frame(
            r#"{"type":"events","to_rev":2,"events":[]}"#,
            "session",
            "working",
            1
        ));
        let id = pitcrew_protocol::SessionId::new();
        let event = pitcrew_protocol::events::Event::now(
            pitcrew_protocol::WorkspaceId::new(),
            pitcrew_protocol::MemberId::new(),
            pitcrew_protocol::events::EventBody::SessionStateChanged {
                session: id,
                from: pitcrew_protocol::model::SessionState::Idle,
                to: pitcrew_protocol::model::SessionState::Working,
                status_line: None,
            },
        );
        let frame = serde_json::to_string(&pitcrew_protocol::api::StreamFrame::Events {
            from_rev: 2,
            to_rev: 2,
            events: vec![event],
        })
        .unwrap();
        assert!(hook_frame(&frame, &id.to_string(), "working", 1));
        assert!(hook_frame(&frame, &id.0.to_string(), "working", 1));
        assert!(!hook_frame(&frame, &id.to_string(), "idle", 1));
        assert!(!hook_frame(
            &frame,
            &pitcrew_protocol::SessionId::new().to_string(),
            "working",
            1
        ));
        assert!(!hook_frame(&frame, &id.to_string(), "working", 2));
    }
}
