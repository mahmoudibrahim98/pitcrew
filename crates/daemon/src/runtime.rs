//! The runtime the runner's terminals run on ([`TerminalRuntime`]), chosen once at start.
//!
//! - **tmux where it is usable:** at start, `pitcrew_runtime::tmux::detect_async` checks, off the
//!   async executor, that tmux is installed, 3.2 or newer, and can run a server on this daemon's
//!   private socket. Then the terminals are windows of that server (`TmuxRuntime`): a person can
//!   `tmux -S <socket> attach -t pitcrew`, and they outlive the daemon.
//! - **Otherwise none** ([`NoRuntime`]): no session has a terminal here, and the log says why (no
//!   tmux, too old, the socket's directory refused, another daemon holds the socket, a server that
//!   is not PitCrew's alone, not Unix). The PTY runtime is stream B's next brief.
//! - **One server per state directory** ([`default_socket`]): the socket is
//!   `<the runtime's private per-user directory>/<8 hex digits of the state directory's sha256>/tmux`,
//!   so two daemons of one user (a real one and a demo, say) never share a server, its terminals or
//!   its environment. The hidden `serve --tmux-socket <path>` names another, for tests and
//!   development, with a warning. **Tests always pass it.**
//! - **One runtime per socket:** an exclusive lock in the socket's directory (`lock`) is held for
//!   the runtime's life; a second daemon on the same socket runs without terminals, warned.
//! - **PitCrew's server alone:** a server on the socket with sessions other than PitCrew's (a
//!   person's own tmux, if a socket names it) is refused, untouched.
//! - **Detaching at stop** ([`TerminalRuntime::detach`]): once the runner has stopped, the runtime
//!   is let go of. Dropping `TmuxRuntime` stores each terminal's exact output offset in tmux and
//!   closes its control client, and then the lock is released; the terminals keep running, and
//!   the next start numbers their output on from there. The routes and the runner hold the
//!   runtime only through [`TerminalRuntime`], whose calls answer `Unavailable` once it is
//!   detached, so nothing keeps it alive past the stop but a call still in flight (bounded by the
//!   runner's own timeouts).

use crate::terminals::NoRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::Key;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

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

    /// tmux, if it is usable here, on the socket of the daemon of the state directory `state`
    /// ([`default_socket`]), or on `socket` (`--tmux-socket`, for tests and development);
    /// otherwise none, logging why. Detection and the runtime's setup run off the async executor.
    pub async fn choose(state: &Path, socket: Option<PathBuf>) -> Self {
        #[cfg(unix)]
        {
            unix::choose(state, socket).await
        }
        #[cfg(not(unix))]
        {
            let _ = (state, socket);
            tracing::info!(
                "the runner's terminals have no runtime on this system yet, so no session has a \
                 terminal here: tmux runs on Unix-like systems only"
            );
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
    /// stores the terminals' output offsets in tmux and detaches (they keep running), then
    /// releases the socket's lock; this waits for that at most `within`, after which it goes on
    /// on the blocking pool, which the stop waits for only so long. Call it once the runner has
    /// stopped.
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

/// The tmux socket of the daemon of the state directory `state`: one server per state directory.
/// `<dir>/<8 hex digits of the sha256 of the canonical state directory>/tmux`, where `<dir>` is
/// the runtime's private per-user directory (`$TMUX_TMPDIR/pitcrew-<uid>` or
/// `$XDG_RUNTIME_DIR/pitcrew` when private, else `/tmp/pitcrew-<uid>`), or `/tmp/pitcrew-<uid>`
/// if that path would pass the socket path limit.
#[cfg(unix)]
pub fn default_socket(state: &Path) -> PathBuf {
    use sha2::{Digest as _, Sha256};
    let canonical = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
    let hash = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let name: String = hash[..4].iter().map(|b| format!("{b:02x}")).collect();
    let fallback = PathBuf::from(format!("/tmp/pitcrew-{}", pitcrew_auth::euid()));
    let per_user = pitcrew_runtime::tmux::TmuxOptions::default_socket()
        .parent()
        .map(Path::to_path_buf);
    per_user
        .map(|dir| dir.join(&name).join("tmux"))
        .filter(|socket| socket.as_os_str().len() <= unix::SOCKET_PATH_MAX)
        .unwrap_or_else(|| fallback.join(&name).join("tmux"))
}

#[cfg(unix)]
mod unix {
    use super::{Detachable, TerminalRuntime, default_socket};
    use pitcrew_interfaces::runtime::{
        OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
    };
    use pitcrew_protocol::ids::TerminalId;
    use pitcrew_protocol::runner::Key;
    use pitcrew_runtime::TmuxRuntime;
    use pitcrew_runtime::tmux::{SESSION, TmuxOptions};
    use std::io::Read as _;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// The longest socket path: macOS holds 104 bytes with the NUL (Linux 108).
    pub(super) const SOCKET_PATH_MAX: usize = 103;
    /// The lock file in the socket's directory.
    pub(super) const LOCK_FILE: &str = "lock";
    /// The longest the check of the server's sessions may take.
    const LIST_TIMEOUT: Duration = Duration::from_secs(5);
    /// The most of tmux's answer that is read.
    const LIST_MAX: u64 = 1024 * 1024;

    pub(super) async fn choose(state: &Path, socket: Option<PathBuf>) -> TerminalRuntime {
        let given = socket.is_some();
        let socket = match socket {
            Some(socket) => {
                tracing::warn!(
                    socket = %socket.display(),
                    "--tmux-socket: the runner's tmux server is on this socket instead of this \
                     state directory's own; for tests and development only"
                );
                socket
            }
            None => default_socket(state),
        };
        let refuse = |why: String| {
            tracing::warn!(
                socket = %socket.display(),
                "the runner's terminals cannot use tmux, so no session has a terminal here: {why}"
            );
            TerminalRuntime::none()
        };

        let prepared = {
            let socket = socket.clone();
            tokio::task::spawn_blocking(move || prepare(&socket, !given)).await
        };
        let lock = match prepared {
            Ok(Ok(lock)) => lock,
            Ok(Err(why)) => return refuse(why),
            Err(e) => return refuse(e.to_string()),
        };
        let options = TmuxOptions::new(socket.clone());
        let support = match pitcrew_runtime::tmux::detect_async(options.clone()).await {
            Ok(support) => support,
            Err(RuntimeError::Unavailable(why)) => return refuse(why),
            Err(e) => return refuse(e.to_string()),
        };
        let listed = {
            let (tmux, socket) = (support.tmux.clone(), socket.clone());
            tokio::task::spawn_blocking(move || foreign_sessions(&tmux, &socket)).await
        };
        match listed {
            Ok(Ok(foreign)) if foreign.is_empty() => {}
            Ok(Ok(foreign)) => {
                return refuse(format!(
                    "the tmux server there has {} session(s) that are not PitCrew's (a person's \
                     own tmux?); PitCrew runs its terminals on a server of its own",
                    foreign.len()
                ));
            }
            Ok(Err(why)) => return refuse(why),
            Err(e) => return refuse(e.to_string()),
        }
        let mut options = options;
        options.tmux.clone_from(&support.tmux);
        match tokio::task::spawn_blocking(move || TmuxRuntime::new(options)).await {
            Ok(Ok(runtime)) => {
                tracing::info!(
                    version = %support.version,
                    tmux = %support.tmux.display(),
                    socket = %socket.display(),
                    "the runner's terminals run in tmux; attach to them with `tmux -S {} attach \
                     -t {SESSION}`",
                    socket.display(),
                );
                TerminalRuntime {
                    shared: Arc::new(Detachable::new(Arc::new(Locked {
                        runtime,
                        _lock: lock,
                    }))),
                    socket: Some(socket),
                }
            }
            Ok(Err(e)) => refuse(e.to_string()),
            Err(e) => refuse(e.to_string()),
        }
    }

    /// Makes the socket's directory private (and, for the default socket, the per-user directory
    /// above it), creating them 0700 if missing; never repairs one that is not; and takes the
    /// socket's lock.
    pub(super) fn prepare(socket: &Path, default: bool) -> Result<SocketLock, String> {
        if socket.as_os_str().len() > SOCKET_PATH_MAX {
            return Err(format!(
                "the socket path {} is longer than {SOCKET_PATH_MAX} bytes",
                socket.display()
            ));
        }
        if !socket.is_absolute() {
            return Err(format!(
                "the socket path {} is not absolute",
                socket.display()
            ));
        }
        let dir = socket
            .parent()
            .ok_or_else(|| format!("{} has no directory", socket.display()))?;
        if default && let Some(per_user) = dir.parent() {
            private_dir(per_user)?;
        }
        private_dir(dir)?;
        SocketLock::acquire(&dir.join(LOCK_FILE))
    }

    /// `dir`, created 0700 if missing (its parent must exist), and checked: a real directory of
    /// this user's, closed to everyone else.
    fn private_dir(dir: &Path) -> Result<(), String> {
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
        }
        pitcrew_auth::check_private_dir(dir).map_err(|e| e.to_string())
    }

    /// An exclusive lock on the socket's directory, held while the runtime lives.
    #[derive(Debug)]
    pub(super) struct SocketLock {
        _fd: OwnedFd,
    }

    impl SocketLock {
        pub(super) fn acquire(path: &Path) -> Result<Self, String> {
            use rustix::fs::{FlockOperation, Mode, OFlags};
            let fd = rustix::fs::open(
                path,
                OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
            match rustix::fs::flock(&fd, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => Ok(Self { _fd: fd }),
                Err(rustix::io::Errno::WOULDBLOCK) => Err(format!(
                    "another pitcrewd uses this tmux socket (it holds {})",
                    path.display()
                )),
                Err(e) => Err(format!("cannot lock {}: {e}", path.display())),
            }
        }
    }

    impl Drop for SocketLock {
        /// Unlocks before the descriptor is closed: a process another thread is starting holds
        /// a copy of it until it runs its program, and an flock lasts while any copy is open.
        fn drop(&mut self) {
            let _ = rustix::fs::flock(&self._fd, rustix::fs::FlockOperation::Unlock);
        }
    }

    /// The sessions of the tmux server on `socket` that are not PitCrew's (none if no server
    /// runs there).
    pub(super) fn foreign_sessions(tmux: &Path, socket: &Path) -> Result<Vec<String>, String> {
        let mut child = Command::new(tmux)
            .arg("-S")
            .arg(socket)
            .args(["list-sessions", "-F", "#{session_name}"])
            .current_dir("/")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run tmux: {e}"))?;
        let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut text = Vec::new();
                if let Some(pipe) = pipe {
                    let _ = pipe.take(LIST_MAX).read_to_end(&mut text);
                }
                text
            })
        };
        let stdout = read(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        );
        let stderr = read(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        );
        let deadline = Instant::now() + LIST_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "tmux did not list the sessions on {} within {LIST_TIMEOUT:?}",
                        socket.display()
                    ));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("cannot wait for tmux: {e}"));
                }
            }
        };
        let out = stdout.join().unwrap_or_default();
        let err = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
        if status.success() {
            return Ok(String::from_utf8_lossy(&out)
                .lines()
                .filter(|name| *name != SESSION)
                .map(str::to_owned)
                .collect());
        }
        let none = ["no server running", "error connecting", "no sessions"];
        if none.iter().any(|said| err.contains(said)) {
            return Ok(Vec::new());
        }
        Err(format!(
            "tmux cannot list the sessions on {}: {}",
            socket.display(),
            err.trim()
        ))
    }

    /// `TmuxRuntime`, holding its socket's lock: dropped, the runtime goes first (storing the
    /// terminals' offsets), then the lock.
    struct Locked {
        runtime: TmuxRuntime,
        _lock: SocketLock,
    }

    impl Runtime for Locked {
        fn kind(&self) -> RuntimeKind {
            self.runtime.kind()
        }

        fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
            self.runtime.start(spec)
        }

        fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
            self.runtime.write(id, bytes)
        }

        fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
            self.runtime.send_keys(id, keys)
        }

        fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
            self.runtime.resize(id, cols, rows)
        }

        fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
            self.runtime.screen(id)
        }

        fn read_output(
            &self,
            id: TerminalId,
            from: u64,
            max: usize,
        ) -> Result<OutputChunk, RuntimeError> {
            self.runtime.read_output(id, from, max)
        }

        fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
            self.runtime.info(id)
        }

        fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
            self.runtime.list()
        }

        fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
            self.runtime.kill(id)
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
        let chosen = rt.block_on(TerminalRuntime::choose(tmp.path(), Some(socket)));
        assert!(!chosen.tmux());
        assert!(chosen.runtime().list().unwrap().is_empty());
        assert!(!tmp.path().join("missing").exists(), "nothing was made");
    }

    /// One socket per state directory: the same directory (however it is spelled) always gets
    /// the same one, another directory another, each in a directory of its own under the
    /// runtime's per-user directory, within the socket path limit.
    #[cfg(unix)]
    #[test]
    fn each_state_directory_has_a_socket_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let socket = default_socket(&a);
        assert_eq!(socket, default_socket(&a));
        assert_eq!(socket, default_socket(&tmp.path().join("b/../a")));
        assert_ne!(socket, default_socket(&b));
        assert!(socket.ends_with("tmux"), "{}", socket.display());
        let dir = socket.parent().unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 8, "{name}");
        assert!(name.bytes().all(|c| c.is_ascii_hexdigit()), "{name}");
        let per_user = pitcrew_runtime::tmux::TmuxOptions::default_socket();
        let fallback = PathBuf::from(format!("/tmp/pitcrew-{}", pitcrew_auth::euid()));
        let parent = dir.parent().unwrap();
        assert!(
            Some(parent) == per_user.parent() || parent == fallback,
            "{}",
            socket.display()
        );
        assert!(socket.as_os_str().len() <= unix::SOCKET_PATH_MAX);
    }

    /// The default socket's per-user directory is made private if missing, and so is its own;
    /// one that is open to others is refused, not repaired. A given socket's directory needs its
    /// parent. The lock has one holder at a time.
    #[cfg(unix)]
    #[test]
    fn socket_directories_are_private_and_locked_once() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let socket = tmp.path().join("user").join("0123abcd").join("tmux");
        let lock = unix::prepare(&socket, true).unwrap();
        assert_eq!(mode(&tmp.path().join("user")), 0o700);
        assert_eq!(mode(&tmp.path().join("user/0123abcd")), 0o700);
        let again = unix::prepare(&socket, true).unwrap_err();
        assert!(again.contains("another pitcrewd"), "{again}");
        drop(lock);
        drop(unix::prepare(&socket, true).unwrap());

        // A given socket: only its own directory is made.
        let given = tmp.path().join("missing").join("t").join("s");
        assert!(unix::prepare(&given, false).is_err());
        assert!(!tmp.path().join("missing").exists());

        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(unix::prepare(&open.join("x").join("tmux"), true).is_err());
        assert_eq!(mode(&open), 0o755, "not repaired");
        assert!(!open.join("x").exists());
        assert!(unix::prepare(&PathBuf::from("rel/s"), false).is_err());
    }

    /// A server with sessions other than PitCrew's is foreign; no server is none; tmux failing
    /// otherwise is an error. (A stand-in `tmux` answers.)
    #[cfg(unix)]
    #[test]
    fn foreign_sessions_are_found() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let fake = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let socket = tmp.path().join("s");
        let mixed = fake("mixed", "printf 'pitcrew\\nmine\\nwork\\n'");
        assert_eq!(
            unix::foreign_sessions(&mixed, &socket).unwrap(),
            ["mine", "work"]
        );
        let ours = fake("ours", "printf 'pitcrew\\n'");
        assert!(unix::foreign_sessions(&ours, &socket).unwrap().is_empty());
        let none = fake("none", "echo \"no server running on $2\" >&2; exit 1");
        assert!(unix::foreign_sessions(&none, &socket).unwrap().is_empty());
        let broken = fake("broken", "echo 'protocol version mismatch' >&2; exit 1");
        let err = unix::foreign_sessions(&broken, &socket).unwrap_err();
        assert!(err.contains("protocol version mismatch"), "{err}");
    }
}
