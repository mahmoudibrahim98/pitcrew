//! Is the tmux runtime usable here? tmux must be installed, new enough, and able to start a
//! server on PitCrew's private socket.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

use pitcrew_interfaces::runtime::RuntimeError;
use pitcrew_protocol::runner::Capability;

use super::TmuxOptions;
use crate::detect::TmuxVersion;

/// tmux is usable: the runner can report [`Capability::Tmux`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TmuxSupport {
    /// The installed tmux.
    pub version: TmuxVersion,
    /// The private server's socket, for `tmux -S <socket> attach`.
    pub socket: PathBuf,
}

impl TmuxSupport {
    /// The capability this gives a runner.
    pub fn capability(&self) -> Capability {
        Capability::Tmux
    }
}

/// Checks that tmux is installed, at least 3.2, and can start a server on the private socket
/// (creating its directory). Otherwise the runtime is [`RuntimeError::Unavailable`], with the
/// reason for people.
///
/// Blocks for up to about twice [`TmuxOptions::call_timeout`]: from async code use
/// [`detect_async`], or a blocking thread. A server it starts exits again at once (it has no
/// sessions); a running one is left alone.
pub fn detect(options: &TmuxOptions) -> Result<TmuxSupport, RuntimeError> {
    #[cfg(unix)]
    {
        unix::detect(options)
    }
    #[cfg(not(unix))]
    {
        let _ = options;
        Err(RuntimeError::Unavailable(
            "tmux runs on Unix-like systems only; this machine uses the PTY runtime".into(),
        ))
    }
}

/// [`detect`] on its own thread, as a future that any executor can await without blocking.
pub fn detect_async(options: TmuxOptions) -> Detecting {
    let slot = Arc::new(Mutex::new(Slot::default()));
    let filled = Arc::clone(&slot);
    let spawned = std::thread::Builder::new()
        .name("pitcrew-tmux-detect".into())
        .spawn(move || {
            let result = detect(&options);
            let waker = {
                let mut slot = filled.lock().unwrap_or_else(PoisonError::into_inner);
                slot.result = Some(result);
                slot.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        });
    if let Err(e) = spawned {
        slot.lock().unwrap_or_else(PoisonError::into_inner).result = Some(Err(
            RuntimeError::Unavailable(format!("cannot start the tmux check: {e}")),
        ));
    }
    Detecting { slot }
}

/// The future [`detect_async`] returns.
#[derive(Debug)]
pub struct Detecting {
    slot: Arc<Mutex<Slot>>,
}

#[derive(Debug, Default)]
struct Slot {
    result: Option<Result<TmuxSupport, RuntimeError>>,
    waker: Option<Waker>,
}

impl Future for Detecting {
    type Output = Result<TmuxSupport, RuntimeError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.result.take() {
            Some(result) => Poll::Ready(result),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::detect::{DetectError, detect_tmux};

    pub(super) fn detect(options: &TmuxOptions) -> Result<TmuxSupport, RuntimeError> {
        let version = detect_tmux(&options.tmux).map_err(|e| {
            RuntimeError::Unavailable(match e {
                DetectError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                    format!("tmux is not installed ({:?} was not found)", options.tmux)
                }
                other => other.to_string(),
            })
        })?;
        super::super::socket::ensure_private(&options.socket).map_err(RuntimeError::Unavailable)?;
        start_server(options)?;
        Ok(TmuxSupport {
            version,
            socket: options.socket.clone(),
        })
    }

    /// `tmux -S <socket> start-server`: proves a server can run there, or one already does and
    /// speaks this client's protocol.
    fn start_server(options: &TmuxOptions) -> Result<(), RuntimeError> {
        let unavailable = |why: String| RuntimeError::Unavailable(why);
        let mut child = Command::new(&options.tmux)
            .arg("-S")
            .arg(&options.socket)
            .args(["-f", "/dev/null", "start-server"])
            .current_dir("/")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .envs(options.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| unavailable(format!("cannot run tmux: {e}")))?;
        let deadline = Instant::now() + options.call_timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(unavailable(format!(
                        "tmux could not start a server on {} within {:?}",
                        options.socket.display(),
                        options.call_timeout
                    )));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(unavailable(format!("cannot wait for tmux: {e}")));
                }
            }
        };
        if status.success() {
            return Ok(());
        }
        // It has exited, so its stderr is complete and short.
        let mut text = String::new();
        if let Some(stderr) = child.stderr.take() {
            let _ = stderr.take(4096).read_to_string(&mut text);
        }
        Err(unavailable(format!(
            "tmux cannot use {}: {}",
            options.socket.display(),
            text.trim()
        )))
    }
}
