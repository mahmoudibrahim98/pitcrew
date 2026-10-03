//! `POST /v1/machines/{id}/scan` (api-v1.md, "Machine scan"): a read-only walk of this machine's
//! agent homes for onboarding, answered as a stream of progress frames and then the report.
//!
//! - **Which homes:** the runner's, as `serve` resolves them ([`crate::runner::homes`]): `--homes`
//!   when given, none with `--demo` alone, else this user's own. With `--no-runner` the hub reads
//!   no agent home, so a scan is `409`.
//! - **Which machine:** only the hub's own (the workspace's first local machine, as `serve` picks
//!   it). An unknown or malformed id is `404`; another machine of the workspace is `409`, since
//!   scanning a remote machine is not supported yet.
//! - **One at a time per machine:** a scan holds its machine's place from the moment it is
//!   accepted until its walk ends, and a second scan meanwhile is `409`. A client that goes away
//!   cancels further work between files. A ten-minute budget returns a partial report.
//! - **The answer** is `200` with `Content-Type: application/x-ndjson`: one [`ScanFrame`] per
//!   line. A `progress` frame at once (`scanned: 0`), then the walk's own ticks (at most every
//!   100 ms; the last one has `scanned == total` unless partial), then `done` with the report, or `error` if the
//!   walk panicked. Ticks are dropped rather than waited for when the client reads slowly; the
//!   last tick and the final frame wait at most 30 seconds before closing the stream.
//! - **Privacy:** the report names the person's folders and branches. The route is a device
//!   route (`RouterParts::device`), so an agent token gets `403`, and the log records counts,
//!   never paths.
//!
//! The walk runs on tokio's blocking pool; its reads are bounded (a prefix of each transcript, or
//! one indexed row of an OpenCode store) and run on the scan's own threads.

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pitcrew_auth::ErrorResponse;
use pitcrew_hub_work::WorkService;
use pitcrew_ingest::scan::{ScanHome, ScanOptions};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::MachineId;
use pitcrew_protocol::model::{Machine, MachineKind};
use pitcrew_protocol::scan::{ScanFrame, ScanProgress};
use pitcrew_runner::EngineHome;
use std::collections::HashSet;
use std::convert::Infallible;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Frames waiting for a client that reads slowly; ticks beyond these are dropped.
const FRAMES: usize = 16;
/// Maximum wait for a client to read a required frame.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the route waits to learn the workspace's machines.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);
/// The answer's media type: newline-delimited JSON.
const NDJSON: &str = "application/x-ndjson";

/// `serve --scan-hold-ms`, for tests and development only: how long each scan waits once it is
/// accepted, holding its machine's place, before it walks.
static HOLD: OnceLock<Duration> = OnceLock::new();

/// Sets `serve --scan-hold-ms` for this process (see [`HOLD`]). Only the first call counts.
pub fn hold_each(hold: Option<Duration>) {
    if let Some(hold) = hold {
        let _ = HOLD.set(hold);
    }
}

/// The route. Mount it as a **device** route (`RouterParts::device`). `homes` are the runner's
/// (`None` with `--no-runner`).
pub fn routes(work: Arc<WorkService>, homes: Option<&[EngineHome]>) -> Router {
    let hold = HOLD.get().copied().unwrap_or_default();
    if !hold.is_zero() {
        tracing::warn!(
            ms = hold.as_millis(),
            "for tests and development (--scan-hold-ms): each machine scan waits this long before \
             it walks"
        );
    }
    let scans = Scans {
        work,
        homes: homes.map(|homes| {
            homes
                .iter()
                .map(|h| ScanHome {
                    engine: h.engine,
                    home: h.path.clone(),
                })
                .collect()
        }),
        running: Running::default(),
        hold,
    };
    Router::new()
        .route("/v1/machines/{id}/scan", post(scan))
        .with_state(Arc::new(scans))
}

/// The route's state.
#[derive(Debug)]
struct Scans {
    work: Arc<WorkService>,
    /// What a scan walks; `None` with `--no-runner`.
    homes: Option<Vec<ScanHome>>,
    running: Running,
    hold: Duration,
}

/// The machines with a scan running.
#[derive(Clone, Debug, Default)]
struct Running(Arc<Mutex<HashSet<MachineId>>>);

impl Running {
    fn machines(&self) -> MutexGuard<'_, HashSet<MachineId>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `machine`'s place, unless a scan of it is running.
    fn claim(&self, machine: MachineId) -> Option<Claim> {
        self.machines().insert(machine).then(|| Claim {
            running: self.clone(),
            machine,
        })
    }
}

/// A running scan's place: given back when dropped, however the scan ends.
#[derive(Debug)]
struct Claim {
    running: Running,
    machine: MachineId,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.running.machines().remove(&self.machine);
    }
}

/// The hub's own machine: the workspace's first local one, as `serve` picks it.
fn own_machine(machines: &[Machine]) -> Option<&Machine> {
    machines.iter().find(|m| m.kind == MachineKind::Local)
}

fn conflict(message: impl Into<String>) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::Conflict, message)
}

async fn scan(
    State(scans): State<Arc<Scans>>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    match start(&scans, id).await {
        Ok(frames) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(NDJSON)),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
                (
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                ),
            ],
            Body::from_stream(frames),
        )
            .into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// Checks the request and starts the walk; its frames, or why not.
async fn start(
    scans: &Arc<Scans>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Frames, ErrorResponse> {
    let Path(id) = id.map_err(|_| {
        ErrorResponse::new(ErrorCode::Invalid, "The machine id must be plain text.")
    })?;
    // A malformed id names no machine, as on the other routes.
    let no_machine = || ErrorResponse::not_found(format!("No machine {id}."));
    let machine: MachineId = id.parse().map_err(|_| no_machine())?;

    let work = Arc::clone(&scans.work);
    let lookup = tokio::task::spawn_blocking(move || work.machines());
    let machines = match tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await {
        Ok(Ok(Ok(machines))) => machines,
        Ok(Ok(Err(e))) => {
            tracing::error!(error = %e, "cannot list the machines for a scan");
            return Err(ErrorResponse::new(
                ErrorCode::Internal,
                "The workspace's machines could not be read.",
            ));
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "listing the machines for a scan failed");
            return Err(ErrorResponse::new(
                ErrorCode::Internal,
                "The workspace's machines could not be read.",
            ));
        }
        Err(_) => {
            return Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                "The workspace's machines took too long to read.",
            ));
        }
    };
    let Some(target) = machines.iter().find(|m| m.id == machine) else {
        return Err(no_machine());
    };
    match own_machine(&machines) {
        Some(own) if own.id == machine => {}
        own => {
            return Err(conflict(format!(
                "Scanning {} is not supported yet: this hub scans only its own machine{}.",
                target.name,
                own.map(|m| format!(", {}", m.name)).unwrap_or_default()
            )));
        }
    }
    let Some(homes) = scans.homes.clone() else {
        return Err(conflict(
            "This hub runs without its runner (--no-runner), so it reads no agent homes to scan.",
        ));
    };
    let Some(claim) = scans.running.claim(machine) else {
        return Err(conflict(format!(
            "A scan of {} is already running; wait for it to finish.",
            target.name
        )));
    };

    let (frames, rx) = mpsc::channel::<Bytes>(FRAMES);
    // At once, so the client knows the scan was accepted before the walk has found anything.
    let _ = frames.try_send(line(&ScanFrame::Progress(ScanProgress::default())));
    let hold = scans.hold;
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    drop(tokio::task::spawn_blocking(move || {
        walk(&homes, hold, &frames, worker_cancel);
        // Its place is given back only once the walk has ended, whether or not anyone listens.
        drop(claim);
    }));
    Ok(Frames(rx, cancel))
}

/// The walk, on the blocking pool: sends its ticks and then its last frame to `frames`.
fn walk(
    homes: &[ScanHome],
    hold: Duration,
    frames: &mpsc::Sender<Bytes>,
    cancel: Arc<AtomicBool>,
) {
    if !hold.is_zero() {
        std::thread::sleep(hold);
    }
    let started = Instant::now();
    let options = ScanOptions {
        cancel: Arc::clone(&cancel),
        ..ScanOptions::default()
    };
    let walked = catch_unwind(AssertUnwindSafe(|| {
        pitcrew_ingest::scan::scan(homes, &options, |tick| {
            let last = tick.path.is_none() || tick.total == Some(tick.scanned);
            let frame = line(&ScanFrame::Progress(tick));
            // Ordinary ticks may be dropped; required frames have a bounded wait.
            if frames.is_closed() {
                cancel.store(true, Ordering::Relaxed);
            }
            let sent = if last {
                send_with_timeout(frames, frame, SEND_TIMEOUT)
            } else {
                frames.try_send(frame).is_ok()
            };
            if last && !sent {
                cancel.store(true, Ordering::Relaxed);
            }
        })
    }));
    let last = match walked {
        Ok(report) => {
            tracing::info!(
                sessions = report.counts.sessions,
                subagent_sessions = report.counts.subagent_sessions,
                suggestions = report.suggestions.len(),
                unreadable = report.unreadable,
                ms = started.elapsed().as_millis(),
                "scanned this machine's agent homes"
            );
            ScanFrame::Done { report }
        }
        Err(_) => {
            tracing::error!("the scan of this machine's agent homes panicked");
            ScanFrame::Error {
                code: ErrorCode::Internal,
                message: "The scan failed; the hub's log says more.".to_owned(),
            }
        }
    };
    if !cancel.load(Ordering::Relaxed) && !send_with_timeout(frames, line(&last), SEND_TIMEOUT) {
        tracing::debug!("the scan's client went away before its result");
    }
}

/// Bounded sends on the blocking pool, including tests without a Tokio runtime.
fn send_with_timeout(frames: &mpsc::Sender<Bytes>, mut frame: Bytes, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        match frames.try_send(frame) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(returned)) => frame = returned,
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())));
    }
}

/// `frame` as one line of JSON.
fn line(frame: &ScanFrame) -> Bytes {
    let mut json = serde_json::to_vec(frame).unwrap_or_else(|e| {
        // Nothing in a frame fails to serialise; if one ever did, say so in the stream.
        tracing::error!(error = %e, "a scan frame could not be written");
        br#"{"type":"error","code":"internal","message":"A scan frame could not be written."}"#
            .to_vec()
    });
    json.push(b'\n');
    Bytes::from(json)
}

/// The answer's body: the frames, as the walk sends them, until it drops its sender.
#[derive(Debug)]
struct Frames(mpsc::Receiver<Bytes>, Arc<AtomicBool>);

impl Drop for Frames {
    fn drop(&mut self) {
        self.1.store(true, Ordering::Relaxed);
    }
}

impl futures_core::Stream for Frames {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|frame| frame.map(Ok))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::model::Liveness;

    fn machine(name: &str, kind: MachineKind) -> Machine {
        Machine {
            id: MachineId::new(),
            name: name.into(),
            kind,
            info: None,
            liveness: Liveness::Live,
        }
    }

    /// One scan per machine: a second claim waits for the first to be given back, and another
    /// machine's is its own.
    #[test]
    fn a_machine_has_one_scan_at_a_time() {
        let running = Running::default();
        let (a, b) = (MachineId::new(), MachineId::new());
        let first = running.claim(a).expect("a's first scan");
        assert!(running.claim(a).is_none(), "a second scan of a");
        let other = running.claim(b).expect("b is another machine");
        drop(first);
        let again = running.claim(a).expect("a's scan was given back");
        drop((again, other));
        assert!(running.machines().is_empty());
    }

    /// The place is given back even when the walk holding it panics.
    #[test]
    fn a_panicking_walk_gives_its_place_back() {
        let running = Running::default();
        let a = MachineId::new();
        let claim = running.claim(a).unwrap();
        let unwound = std::thread::spawn(move || {
            let _claim = claim;
            panic!("synthetic panic for the test");
        })
        .join();
        assert!(unwound.is_err());
        assert!(running.claim(a).is_some());
    }

    #[test]
    fn the_hub_scans_its_first_local_machine() {
        let cluster = machine("a SLURM cluster", MachineKind::Ssh);
        let laptop = machine("This laptop", MachineKind::Local);
        let other = machine("Another local", MachineKind::Local);
        let machines = [cluster, laptop.clone(), other];
        assert_eq!(own_machine(&machines).map(|m| m.id), Some(laptop.id));
        assert_eq!(own_machine(&[]).map(|m| m.id), None);
    }

    #[test]
    fn a_frame_is_one_line_of_json() {
        let frame = line(&ScanFrame::Progress(ScanProgress {
            scanned: 2,
            total: Some(5),
            path: Some("/home/sam/.codex/sessions/a\nb.jsonl".into()),
        }));
        let text = std::str::from_utf8(&frame).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(text.matches('\n').count(), 1, "{text:?}");
        let back: ScanFrame = serde_json::from_str(text.trim_end()).unwrap();
        assert!(matches!(back, ScanFrame::Progress(p) if p.scanned == 2));
    }

    /// The walk over no home: the last tick, then `done` with an empty report; a client that went
    /// away does not stop it.
    #[test]
    fn a_walk_ends_with_its_last_tick_and_the_report() {
        let (frames, mut rx) = mpsc::channel(FRAMES);
        walk(&[], Duration::ZERO, &frames, Arc::new(AtomicBool::new(false)));
        drop(frames);
        let mut got = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            got.push(serde_json::from_slice::<ScanFrame>(&bytes).unwrap());
        }
        assert_eq!(
            got,
            [
                ScanFrame::Progress(ScanProgress {
                    scanned: 0,
                    total: Some(0),
                    path: None
                }),
                ScanFrame::Done {
                    report: Default::default()
                },
            ]
        );

        let (frames, rx) = mpsc::channel(FRAMES);
        drop(rx);
        walk(&[], Duration::ZERO, &frames, Arc::new(AtomicBool::new(false)));
    }

    #[test]
    fn a_nonreading_client_times_out_and_a_dropped_body_cancels() {
        let (tx, rx) = mpsc::channel(1);
        tx.try_send(Bytes::new()).unwrap();
        assert!(!send_with_timeout(&tx, Bytes::new(), Duration::from_millis(20)));
        let cancel = Arc::new(AtomicBool::new(false));
        drop(Frames(rx, Arc::clone(&cancel)));
        assert!(cancel.load(Ordering::Relaxed));
        assert!(!send_with_timeout(&tx, Bytes::new(), SEND_TIMEOUT));
    }
}
