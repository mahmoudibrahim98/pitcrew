//! Idle CPU with 50 watched transcripts and no changes. Linux only (reads `/proc/self/stat`).
//! Slow, so ignored by default:
//!
//! ```text
//! cargo test -p pitcrew-runner --release --test idle_cpu -- --ignored --nocapture
//! ```

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, fixture_transcript};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
use pitcrew_protocol::model::Engine;
use pitcrew_runner::{PollMode, RunnerConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TRANSCRIPTS: usize = 50;
const WINDOW: Duration = Duration::from_secs(20);
const BUDGET_PERCENT: f64 = 0.5;

/// User plus system CPU time of this process, in clock ticks (100 per second on Linux).
fn cpu_ticks() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let after_name = &stat[stat.rfind(')').unwrap() + 2..];
    let fields: Vec<&str> = after_name.split(' ').collect();
    // Fields 14 and 15 of the file (utime, stime); the text after the name starts at field 3.
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}

fn measure(poll: PollMode) -> f64 {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let fixture = std::fs::read(fixture_transcript()).unwrap();
    for i in 0..TRANSCRIPTS {
        // Spread over ten project folders, like a busy machine.
        let dir = home.path().join("projects").join(format!("-w-p{}", i % 10));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("s{i:02}.jsonl")), &fixture).unwrap();
    }
    let mut cfg = RunnerConfig::new(
        WorkspaceId::new(),
        MachineId::new(),
        MemberId::new(),
        state.path(),
    )
    .with_home(Engine::Claude, home.path());
    cfg.poll = poll;
    let sink = Arc::new(CollectSink::default());
    let runner =
        pitcrew_runner::start(cfg, vec![Arc::new(ClaudeAdapter::new())], sink.clone()).unwrap();

    // Let discovery finish (one discovered event per transcript, plus their items).
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let discovered = sink
            .events()
            .iter()
            .filter(|e| {
                matches!(
                    e.body,
                    pitcrew_protocol::events::EventBody::SessionDiscovered { .. }
                )
            })
            .count();
        if discovered == TRANSCRIPTS {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_secs(1));

    let events = sink.len();
    let t0 = cpu_ticks();
    std::thread::sleep(WINDOW);
    let ticks = cpu_ticks() - t0;
    runner.stop();
    assert_eq!(sink.len(), events, "no events while idle");
    #[allow(clippy::cast_precision_loss)]
    let percent = ticks as f64 / (WINDOW.as_secs_f64() * 100.0) * 100.0;
    println!(
        "idle CPU, {TRANSCRIPTS} transcripts, {poll:?}: {ticks} ticks in {WINDOW:?} = {percent:.3}% of one core"
    );
    percent
}

#[test]
#[ignore = "slow: measures idle CPU over 20 s per mode"]
fn idle_cpu_with_50_transcripts() {
    let notify = measure(PollMode::Never);
    let polled = measure(PollMode::Always);
    assert!(notify <= BUDGET_PERCENT, "notify: {notify}%");
    assert!(polled <= BUDGET_PERCENT, "polling: {polled}%");
}
