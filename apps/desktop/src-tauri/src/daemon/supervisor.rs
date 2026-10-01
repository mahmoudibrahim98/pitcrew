//! Starting and supervising the local `pitcrewd`.
//!
//! - **Find or start.** If a daemon already answers on this user's private socket or pipe
//!   (`GET /v1/host/info`), it is used as it is. Otherwise the supervisor starts
//!   `pitcrewd [--state-dir <dir>] serve --listen private` and waits for its ready line,
//!   `pitcrewd listening on …`.
//! - **The token.** `pitcrewd token show-path` names the device token's file once the daemon is
//!   up. Only the path is kept; the token is read on every connection ([`super::LocalConnector`]).
//! - **Restarts.** A daemon the app started that stops is restarted after a backoff that doubles
//!   from 0.5 s to 15 s. A run of failures (five, each lasting less than a minute) gives up:
//!   the state is `unreachable` with the last thing the daemon said, until the app restarts.
//! - **Someone else's daemon** (started by hand) is watched, not owned: if it goes away, the
//!   supervisor starts its own.
//! - **Stop.** When the app quits, the supervisor stops the daemon only if it started it: SIGTERM
//!   and up to 8 s to finish (then a kill) on Unix. Windows has no signal a process can send
//!   another without a shared console, so there it is terminated.

use super::endpoint::{BoxIo, ConnectError, Endpoint};
use crate::gateway::http;
use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

/// What the daemon prints on stdout once it listens.
pub const READY_LINE: &str = "pitcrewd listening on ";

/// The local daemon's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonState {
    /// Looking for the daemon, starting it, or waiting before a restart.
    Connecting,
    /// Up. `token` is the file `pitcrewd token show-path` named.
    Ready {
        /// Whether this app started it (and so stops it when quitting).
        started_here: bool,
        /// The device token's file.
        token: PathBuf,
    },
    /// Given up, or there is a daemon that is not provably ours. `detail` says why.
    Unreachable {
        /// For people.
        detail: String,
    },
}

/// How to supervise.
#[derive(Clone, Debug)]
pub struct Options {
    /// The `pitcrewd` to run, or why there is none.
    pub program: Result<PathBuf, String>,
    /// `--state-dir`, if any.
    pub state_dir: Option<PathBuf>,
    /// Where the daemon listens.
    pub endpoint: Endpoint,
    /// How long a starting daemon has to print its ready line.
    pub ready_timeout: Duration,
    /// The first wait before a restart; it doubles up to `max_backoff`.
    pub first_backoff: Duration,
    /// The longest wait before a restart.
    pub max_backoff: Duration,
    /// Failures in a row before giving up.
    pub max_failures: u32,
    /// A daemon that ran this long before stopping starts a new run of failures.
    pub healthy_after: Duration,
    /// How often someone else's daemon is checked.
    pub watch_every: Duration,
    /// How long a stopping daemon has before it is killed.
    pub stop_timeout: Duration,
}

impl Options {
    /// The defaults for `program`, `state_dir` and `endpoint`.
    #[must_use]
    pub fn new(
        program: Result<PathBuf, String>,
        state_dir: Option<PathBuf>,
        endpoint: Endpoint,
    ) -> Self {
        Self {
            program,
            state_dir,
            endpoint,
            ready_timeout: Duration::from_secs(20),
            first_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(15),
            max_failures: 5,
            healthy_after: Duration::from_secs(60),
            watch_every: Duration::from_secs(5),
            stop_timeout: Duration::from_secs(8),
        }
    }
}

/// The running supervisor.
pub struct Supervisor {
    state: watch::Receiver<DaemonState>,
    poke: Arc<Notify>,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Starts supervising on `runtime`.
    #[must_use]
    pub fn start(options: Options, runtime: &tokio::runtime::Handle) -> Self {
        let (state_tx, state) = watch::channel(DaemonState::Connecting);
        let (stop, stop_rx) = watch::channel(false);
        let poke = Arc::new(Notify::new());
        let task = runtime.spawn(supervise(options, state_tx, Arc::clone(&poke), stop_rx));
        Self {
            state,
            poke,
            stop,
            task: Some(task),
        }
    }

    /// Follows the daemon's state.
    #[must_use]
    pub fn state(&self) -> watch::Receiver<DaemonState> {
        self.state.clone()
    }

    /// Notified when a connection fails, so the supervisor looks again at once.
    #[must_use]
    pub fn poke_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.poke)
    }

    /// Stops supervising, and stops the daemon if this supervisor started it.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// Whether to go on after a wait.
#[derive(PartialEq, Eq)]
enum Next {
    Go,
    Stop,
}

/// Waits for `stop`, a poke, or `delay`.
async fn wait(stop: &mut watch::Receiver<bool>, poke: &Notify, delay: Duration) -> Next {
    tokio::select! {
        () = stopped(stop) => Next::Stop,
        () = poke.notified() => Next::Go,
        () = tokio::time::sleep(delay) => Next::Go,
    }
}

/// Completes once stop is asked for (or the supervisor is gone).
async fn stopped(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

async fn supervise(
    options: Options,
    state: watch::Sender<DaemonState>,
    poke: Arc<Notify>,
    mut stop: watch::Receiver<bool>,
) {
    let set = |next: DaemonState| {
        state.send_if_modified(|current| {
            if *current == next {
                false
            } else {
                tracing::info!(state = ?next, "local daemon");
                *current = next;
                true
            }
        });
    };
    let mut failures = 0u32;
    let mut backoff = options.first_backoff;

    loop {
        set(DaemonState::Connecting);
        match probe(&options.endpoint).await {
            Probe::Up => {
                match show_path(&options).await {
                    Ok(token) => {
                        failures = 0;
                        backoff = options.first_backoff;
                        tracing::info!(at = %options.endpoint.describe(), "using the pitcrewd that is already running");
                        set(DaemonState::Ready {
                            started_here: false,
                            token,
                        });
                        loop {
                            if wait(&mut stop, &poke, options.watch_every).await == Next::Stop {
                                return;
                            }
                            if probe(&options.endpoint).await != Probe::Up {
                                tracing::info!("the running pitcrewd went away");
                                break;
                            }
                        }
                    }
                    Err(detail) => {
                        set(DaemonState::Unreachable { detail });
                        if wait(&mut stop, &poke, options.watch_every).await == Next::Stop {
                            return;
                        }
                    }
                }
                continue;
            }
            Probe::Untrusted(detail) => {
                set(DaemonState::Unreachable { detail });
                if wait(&mut stop, &poke, options.watch_every).await == Next::Stop {
                    return;
                }
                continue;
            }
            Probe::Down => {}
        }

        let program = match &options.program {
            Ok(program) => program.clone(),
            Err(detail) => {
                set(DaemonState::Unreachable {
                    detail: detail.clone(),
                });
                if wait(&mut stop, &poke, options.watch_every).await == Next::Stop {
                    return;
                }
                continue;
            }
        };

        let last_words = match start(&program, &options).await {
            Ok(mut daemon) => {
                let started = Instant::now();
                match show_path(&options).await {
                    Ok(token) => {
                        set(DaemonState::Ready {
                            started_here: true,
                            token,
                        });
                        tokio::select! {
                            () = stopped(&mut stop) => {
                                daemon.stop(options.stop_timeout).await;
                                return;
                            }
                            status = daemon.child.wait() => {
                                let said = daemon.last_words(&status).await;
                                tracing::warn!(detail = %said, "pitcrewd stopped");
                                failures = if started.elapsed() >= options.healthy_after {
                                    1
                                } else {
                                    failures + 1
                                };
                                said
                            }
                        }
                    }
                    Err(detail) => {
                        failures += 1;
                        daemon.stop(options.stop_timeout).await;
                        detail
                    }
                }
            }
            Err(detail) => {
                tracing::warn!(%detail, "pitcrewd did not start");
                failures += 1;
                detail
            }
        };

        if failures >= options.max_failures {
            set(DaemonState::Unreachable {
                detail: format!("pitcrewd stopped {failures} times in a row; last: {last_words}"),
            });
            stopped(&mut stop).await;
            return;
        }
        set(DaemonState::Connecting);
        tokio::select! {
            () = stopped(&mut stop) => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(options.max_backoff);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// A daemon of ours answers.
    Up,
    /// Nothing answers.
    Down,
    /// Something listens there that is not provably ours.
    Untrusted(String),
}

/// Asks `GET /v1/host/info` (no token) over the checked transport.
async fn probe(endpoint: &Endpoint) -> Probe {
    let io: BoxIo = match endpoint.connect().await {
        Ok(io) => io,
        Err(e @ ConnectError::Untrusted { .. }) => return Probe::Untrusted(e.to_string()),
        Err(_) => return Probe::Down,
    };
    match http::send(
        io,
        None,
        ::http::Method::GET,
        "/v1/host/info",
        None,
        64 * 1024,
        Duration::from_secs(5),
    )
    .await
    {
        Ok(reply) if reply.status == 200 => Probe::Up,
        _ => Probe::Down,
    }
}

/// `pitcrewd [--state-dir <dir>] <args…>`, with nothing on stdin and no console window.
fn command(program: &Path, options: &Options, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    if let Some(dir) = &options.state_dir {
        command.arg("--state-dir").arg(dir);
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // winbase.h: CREATE_NO_WINDOW.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// `pitcrewd token show-path`: the device token's file. Never the token.
async fn show_path(options: &Options) -> Result<PathBuf, String> {
    let program = options.program.as_ref().map_err(Clone::clone)?;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        command(program, options, &["token", "show-path"]).output(),
    )
    .await
    .map_err(|_| "pitcrewd token show-path did not answer within 10 s".to_owned())?
    .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "pitcrewd token show-path failed: {}",
            said.lines().last().unwrap_or("").trim()
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| "pitcrewd token show-path printed something that is not a path".to_owned())?;
    let path = PathBuf::from(text.trim());
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("pitcrewd token show-path printed a relative path".into())
    }
}

/// A daemon this supervisor started.
struct Running {
    child: Child,
    stderr: Arc<Mutex<VecDeque<String>>>,
    stderr_reader: Option<tokio::task::JoinHandle<()>>,
}

/// Lines of the daemon's stderr kept to explain why it stopped.
const KEEP_LINES: usize = 5;

impl Running {
    /// What to say about how it ended, once its last stderr lines are in.
    async fn last_words(&mut self, status: &std::io::Result<std::process::ExitStatus>) -> String {
        if let Some(reader) = self.stderr_reader.take() {
            // The pipe closes when the daemon exits; don't wait on a child it left behind.
            let _ = tokio::time::timeout(Duration::from_secs(1), reader).await;
        }
        let status = match status {
            Ok(status) => status.to_string(),
            Err(e) => format!("unknown status ({e})"),
        };
        let lines = self
            .stderr
            .lock()
            .map(|l| l.iter().cloned().collect::<Vec<_>>().join(" / "))
            .unwrap_or_default();
        if lines.is_empty() {
            status
        } else {
            format!("{status}: {lines}")
        }
    }

    /// Stops it: SIGTERM and up to `timeout` to finish on Unix, then a kill.
    async fn stop(mut self, timeout: Duration) {
        #[cfg(unix)]
        if let Some(pid) = self
            .child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
            && rustix::process::kill_process(pid, rustix::process::Signal::TERM).is_ok()
            && tokio::time::timeout(timeout, self.child.wait())
                .await
                .is_ok()
        {
            tracing::info!("stopped pitcrewd");
            return;
        }
        #[cfg(not(unix))]
        let _ = timeout;
        let _ = self.child.kill().await;
        tracing::info!("killed pitcrewd");
    }
}

/// Starts `pitcrewd serve --listen private` and waits for its ready line.
async fn start(program: &Path, options: &Options) -> Result<Running, String> {
    let mut child = command(program, options, &["serve", "--listen", "private"])
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", program.display()))?;
    tracing::info!(pid = child.id(), program = %program.display(), "starting pitcrewd");

    let stderr = Arc::new(Mutex::new(VecDeque::with_capacity(KEEP_LINES)));
    let stderr_reader = child.stderr.take().map(|pipe| {
        let kept = Arc::clone(&stderr);
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // The daemon never logs tokens; its lines are kept short anyway.
                let line: String = line.chars().take(500).collect();
                tracing::debug!(target: "pitcrewd", "{line}");
                if let Ok(mut kept) = kept.lock() {
                    if kept.len() == KEEP_LINES {
                        kept.pop_front();
                    }
                    kept.push_back(line);
                }
            }
        })
    });
    let Some(stdout) = child.stdout.take() else {
        return Err("pitcrewd has no stdout".into());
    };
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(options.ready_timeout, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(at) = line.strip_prefix(READY_LINE) {
                return Some(at.trim().to_owned());
            }
        }
        None
    })
    .await;
    let mut running = Running {
        child,
        stderr,
        stderr_reader,
    };
    match ready {
        Ok(Some(at)) => {
            if at != options.endpoint.describe() {
                tracing::warn!(%at, expected = %options.endpoint.describe(), "pitcrewd listens somewhere else than expected");
            }
            // Nothing else comes on stdout, but keep it drained.
            tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
            Ok(running)
        }
        Ok(None) => {
            let status = running.child.wait().await;
            Err(format!(
                "pitcrewd stopped before it was ready ({})",
                running.last_words(&status).await
            ))
        }
        Err(_) => {
            running.stop(Duration::from_secs(2)).await;
            Err(format!(
                "pitcrewd was not ready within {} s",
                options.ready_timeout.as_secs()
            ))
        }
    }
}
