//! The runtime the runner's terminals run on ([`TerminalRuntime`]), chosen once at start.
//!
//! - **tmux where it is usable:** at start, `pitcrew_runtime::tmux::detect_async` checks, off the
//!   async executor, that tmux is installed, 3.2 or newer, and can run a server on PitCrew's
//!   private socket. Then the terminals are windows of that server (`TmuxRuntime`): a person can
//!   `tmux -S <socket> attach -t pitcrew`, and they outlive the daemon.
//! - **Otherwise none** ([`NoRuntime`]): no session has a terminal here, and the log says why (no
//!   tmux, too old, the socket's directory refused, not Unix). The PTY runtime is stream B's next
//!   brief.
//! - **The socket** is the runtime's default (`TmuxOptions::default_socket`: a private directory
//!   under `$TMUX_TMPDIR` or `$XDG_RUNTIME_DIR`, else `/tmp/pitcrew-<uid>/tmux`), unless
//!   [`SOCKET_VAR`] names another, for tests and development. **Tests always set it**, so they
//!   never reach a real PitCrew's terminals.
//! - **Detaching at stop** ([`TerminalRuntime::detach`]): once the runner has stopped, the runtime
//!   is let go of. Dropping `TmuxRuntime` stores each terminal's exact output offset in tmux and
//!   closes its control client; the terminals keep running, and the next start numbers their
//!   output on from there. The routes and the runner hold the runtime only through
//!   [`TerminalRuntime`], whose calls answer `Unavailable` once it is detached, so nothing keeps
//!   it alive past the stop but a call still in flight (bounded by the runner's own timeouts).

use crate::terminals::NoRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::Key;
use pitcrew_runtime::tmux::TmuxOptions;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

/// The environment variable that sets the tmux server's socket, for tests and development.
pub const SOCKET_VAR: &str = "PITCREW_TMUX_SOCKET";

/// The socket [`SOCKET_VAR`] names, if it is set and not empty.
pub fn socket_from_env() -> Option<PathBuf> {
    std::env::var_os(SOCKET_VAR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The runner's terminals' runtime: tmux, or none. Cheap to clone; every clone is the same
/// runtime, and [`TerminalRuntime::detach`] lets go of it for all of them.
#[derive(Clone, Debug)]
pub struct TerminalRuntime {
    shared: Arc<Detachable>,
    /// The tmux server's socket, when the terminals run in tmux.
    socket: Option<PathBuf>,
}

impl TerminalRuntime {
    /// No runtime: no session has a terminal here.
    pub fn none() -> Self {
        Self {
            shared: Arc::new(Detachable::new(Arc::new(NoRuntime))),
            socket: None,
        }
    }

    /// tmux on `socket` (the runtime's default when `None`), if it is usable here; otherwise
    /// none, logging why. Detection and the runtime's setup run off the async executor.
    pub async fn choose(socket: Option<PathBuf>) -> Self {
        let options = socket.map_or_else(TmuxOptions::default, TmuxOptions::new);
        let support = match pitcrew_runtime::tmux::detect_async(options.clone()).await {
            Ok(support) => support,
            Err(e) => {
                let why = match e {
                    RuntimeError::Unavailable(why) => why,
                    other => other.to_string(),
                };
                if cfg!(unix) {
                    tracing::warn!(
                        socket = %options.socket.display(),
                        "the runner's terminals cannot use tmux, so no session has a terminal \
                         here: {why}"
                    );
                } else {
                    tracing::info!(
                        "the runner's terminals have no runtime on this system yet, so no session \
                         has a terminal here: {why}"
                    );
                }
                return Self::none();
            }
        };
        #[cfg(unix)]
        {
            let mut options = options;
            options.tmux.clone_from(&support.tmux);
            let socket = options.socket.clone();
            let opened =
                tokio::task::spawn_blocking(move || pitcrew_runtime::TmuxRuntime::new(options))
                    .await;
            match opened {
                Ok(Ok(tmux)) => {
                    tracing::info!(
                        version = %support.version,
                        tmux = %support.tmux.display(),
                        socket = %socket.display(),
                        "the runner's terminals run in tmux; attach to them with `tmux -S {} \
                         attach -t {}`",
                        socket.display(),
                        pitcrew_runtime::tmux::SESSION,
                    );
                    Self {
                        shared: Arc::new(Detachable::new(Arc::new(tmux))),
                        socket: Some(socket),
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        socket = %socket.display(),
                        "the runner's terminals cannot use tmux, so no session has a terminal \
                         here: {e}"
                    );
                    Self::none()
                }
                Err(e) => {
                    tracing::error!(error = %e, "setting up the tmux runtime failed");
                    Self::none()
                }
            }
        }
        #[cfg(not(unix))]
        {
            // `detect` says tmux is unavailable on every other system.
            let _ = support;
            Self::none()
        }
    }

    /// The runtime, for the runner's terminals.
    pub fn runtime(&self) -> Arc<dyn Runtime> {
        Arc::clone(&self.shared) as Arc<dyn Runtime>
    }

    /// Whether the terminals run in tmux (host info's `tmux` capability).
    pub fn tmux(&self) -> bool {
        self.socket.is_some()
    }

    /// Lets go of the runtime: from now on every call answers `Unavailable`. For tmux, dropping it
    /// stores the terminals' output offsets in tmux and detaches (they keep running); this waits
    /// for that at most `within`, after which it goes on on the blocking pool, which the stop
    /// waits for only so long. Call it once the runner has stopped.
    pub async fn detach(&self, within: Duration) {
        let Some(runtime) = self.shared.take() else {
            return;
        };
        let Some(socket) = self.socket.clone() else {
            return;
        };
        let dropping = tokio::task::spawn_blocking(move || {
            // A call still in flight holds it too; it is then dropped when that call returns.
            let last = Arc::strong_count(&runtime) == 1;
            drop(runtime);
            last
        });
        match tokio::time::timeout(within, dropping).await {
            Ok(Ok(true)) => tracing::info!(
                socket = %socket.display(),
                "the terminals' runtime detached: the terminals keep running in tmux, and their \
                 output offsets are stored for the next start"
            ),
            Ok(Ok(false)) => tracing::info!(
                socket = %socket.display(),
                "the terminals' runtime detaches once its last call returns; the terminals keep \
                 running in tmux"
            ),
            Ok(Err(e)) => tracing::warn!(error = %e, "detaching the terminals' runtime failed"),
            Err(_) => tracing::warn!(
                seconds = within.as_secs(),
                "the terminals' runtime is still detaching; it is left to finish on the blocking \
                 pool"
            ),
        }
    }
}

/// A runtime that can be let go of: every call goes to the runtime inside until
/// [`Detachable::take`], and answers `Unavailable` after.
struct Detachable {
    kind: RuntimeKind,
    inner: RwLock<Option<Arc<dyn Runtime>>>,
}

impl fmt::Debug for Detachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let attached = self
            .inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some();
        f.debug_struct("Detachable")
            .field("kind", &self.kind)
            .field("attached", &attached)
            .finish()
    }
}

impl Detachable {
    fn new(runtime: Arc<dyn Runtime>) -> Self {
        Self {
            kind: runtime.kind(),
            inner: RwLock::new(Some(runtime)),
        }
    }

    /// The runtime, unless it was let go of. The lock is not held during the call.
    fn get(&self) -> Result<Arc<dyn Runtime>, RuntimeError> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                RuntimeError::Unavailable(
                    "the daemon is stopping; its terminals keep running".to_owned(),
                )
            })
    }

    fn take(&self) -> Option<Arc<dyn Runtime>> {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Runtime for Detachable {
    fn kind(&self) -> RuntimeKind {
        self.kind
    }

    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        self.get()?.start(spec)
    }

    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.get()?.write(id, bytes)
    }

    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        self.get()?.send_keys(id, keys)
    }

    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        self.get()?.resize(id, cols, rows)
    }

    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        self.get()?.screen(id)
    }

    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        self.get()?.read_output(id, from, max)
    }

    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.get()?.info(id)
    }

    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        self.get()?.list()
    }

    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.get()?.kill(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_interfaces::fake::FakeRuntime;

    fn spec() -> StartSpec {
        StartSpec {
            program: "claude".into(),
            args: Vec::new(),
            cwd: "/w".into(),
            env: Vec::new(),
            name: "x".into(),
            cols: 80,
            rows: 24,
        }
    }

    /// Calls reach the runtime inside until it is let go of; then every one is `Unavailable`,
    /// and the runtime is dropped once nothing else holds it.
    #[test]
    fn a_detached_runtime_answers_unavailable_and_is_dropped() {
        let fake = Arc::new(FakeRuntime::default());
        let detachable = Detachable::new(Arc::clone(&fake) as Arc<dyn Runtime>);
        let started = detachable.start(&spec()).unwrap();
        assert_eq!(detachable.list().unwrap().len(), 1);
        detachable.write(started.id, b"hi").unwrap();
        assert_eq!(
            detachable.read_output(started.id, 0, 10).unwrap().data,
            b"hi"
        );

        let taken = detachable.take().expect("the runtime");
        assert!(detachable.take().is_none(), "taken once");
        let unavailable = |r: Result<(), RuntimeError>| {
            assert!(matches!(r, Err(RuntimeError::Unavailable(_))), "{r:?}");
        };
        unavailable(detachable.start(&spec()).map(drop));
        unavailable(detachable.write(started.id, b"x"));
        unavailable(detachable.send_keys(started.id, &[Key::Enter]));
        unavailable(detachable.resize(started.id, 10, 10));
        unavailable(detachable.screen(started.id).map(drop));
        unavailable(detachable.read_output(started.id, 0, 1).map(drop));
        unavailable(detachable.info(started.id).map(drop));
        unavailable(detachable.list().map(drop));
        unavailable(detachable.kill(started.id));
        // The terminal itself was never touched.
        assert!(fake.info(started.id).unwrap().alive);
        drop(taken);
        assert_eq!(Arc::strong_count(&fake), 1, "nothing else holds it");
    }

    /// Without tmux, the terminals have no runtime: nothing to detach, nothing reported.
    #[test]
    fn none_is_no_runtime() {
        let none = TerminalRuntime::none();
        assert!(!none.tmux());
        let runtime = none.runtime();
        assert!(runtime.list().unwrap().is_empty());
        assert!(matches!(
            runtime.start(&spec()),
            Err(RuntimeError::Unavailable(_))
        ));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(none.detach(Duration::from_secs(1)));
        assert!(matches!(runtime.list(), Err(RuntimeError::Unavailable(_))));
    }

    /// A socket directory that is refused (here its parent does not exist) means no tmux, with
    /// the reason; elsewhere than Unix, there is no tmux either.
    #[test]
    fn a_refused_socket_means_no_tmux() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("missing").join("dir").join("tmux");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let chosen = rt.block_on(TerminalRuntime::choose(Some(socket)));
        assert!(!chosen.tmux());
        assert!(chosen.runtime().list().unwrap().is_empty());
        assert!(!tmp.path().join("missing").exists(), "nothing was made");
    }
}
