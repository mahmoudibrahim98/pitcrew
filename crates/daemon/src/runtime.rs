//! The runtime the runner's terminals run on ([`TerminalRuntime`]), chosen once at start.
//!
//! - **tmux where it is usable, else pitcrew-ptyd** (`pitcrew_runtime::choose_async`, off the
//!   async executor). tmux must be installed, 3.2 or newer, able to run a server on this daemon's
//!   private socket, and that server must have no sessions but PitCrew's (a person's own tmux, if a
//!   socket names it, is refused and left as it is). Otherwise the terminals are owned by
//!   `pitcrew-ptyd` (`PtyRuntime`; ConPTY on Windows), found next to this `pitcrewd`, never on
//!   `PATH`. Either way they outlive the daemon. Host info reports `tmux` or `pty`.
//! - **ptyd is not a planted binary.** It starts every agent, so it must pass the check the
//!   desktop makes of `pitcrewd` (`pitcrew_trust::check_trusted`: on Unix owner and mode of the
//!   program, the file it resolves to and both folders; on Windows no `Zone.Identifier`). One that
//!   fails is warned about and not used, as if it were missing; the PTY runtime checks it again
//!   just before each launch, which may come hours later.
//! - **Otherwise none** ([`NoRuntime`]): no session has a terminal here, and the log says why for
//!   both (tmux: not installed, too old, the socket's directory refused, another daemon holds the
//!   socket, a server that is not PitCrew's alone, not Unix; pitcrew-ptyd: not installed next to
//!   `pitcrewd`, failing the trust check, its endpoint refused, another daemon holds it).
//! - **One place per state directory** ([`default_socket`], [`default_endpoint`]): on Unix the tmux
//!   socket and ptyd's endpoint are `tmux` and `ptyd` in one directory,
//!   `<the runtime's private per-user directory>/<8 hex digits of the state directory's sha256>/`;
//!   on Windows ptyd's pipe is the user's own with `-<8 hex digits>` added. So two daemons of one
//!   user (a real one and a demo, say) never share a tmux server or a ptyd: not their terminals,
//!   not their offsets, not the environment their programs inherit.
//! - **One runtime per place**: an exclusive lock is held for the runtime's life, and a second
//!   daemon given the same socket or endpoint runs without terminals, warned. On Unix it is `lock`
//!   in that directory, the same lock whichever runtime runs. On Windows it is ptyd's endpoint's
//!   own lock file, `%LOCALAPPDATA%\PitCrew\ptyd-<16 hex digits of the endpoint's sha256>.lock`
//!   (`LockFileEx`), so two daemons cannot share one ptyd through `--ptyd-endpoint`.
//! - **Overrides, for tests and development** ([`Overrides`]), each warned when used: the hidden
//!   `serve --tmux-socket`, `--ptyd`, `--ptyd-endpoint`, `--ptyd-idle-exit-ms` and
//!   `--terminal-runtime pty`. **Tests always pass `--tmux-socket` and `--ptyd`.**
//! - **Detaching at stop** ([`TerminalRuntime::detach`]): once the runner has stopped, the runtime
//!   is let go of, then the lock is released; the terminals keep running. Dropping `TmuxRuntime`
//!   stores each terminal's exact output offset in tmux and closes its control client; dropping
//!   `PtyRuntime` closes its connection to ptyd, which keeps every terminal's output and offsets.
//!   The next start numbers their output on from there. The routes and the runner hold the runtime
//!   only through [`TerminalRuntime`], whose calls answer `Unavailable` once it is detached, so
//!   nothing keeps it alive past the stop but a call still in flight (bounded by the runner's own
//!   timeouts).

use crate::terminals::NoRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::{Capability, Key};
use pitcrew_runtime::Chosen;
use pitcrew_runtime::pty::{PtyOptions, PtySupport};
use pitcrew_runtime::tmux::{TmuxOptions, TmuxSupport};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

/// What `serve`'s hidden options change in how the runtime is chosen: for tests and development
/// only, each warned when used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `--tmux-socket`: the tmux server's socket, instead of the state directory's own.
    pub tmux_socket: Option<PathBuf>,
    /// `--ptyd`: the pitcrew-ptyd executable, instead of the one next to `pitcrewd`.
    pub ptyd: Option<PathBuf>,
    /// `--ptyd-endpoint`: where pitcrew-ptyd listens, instead of the state directory's own
    /// endpoint.
    pub ptyd_endpoint: Option<PathBuf>,
    /// `--ptyd-idle-exit-ms`: how long a pitcrew-ptyd this daemon starts waits, idle, before it
    /// exits.
    pub ptyd_idle_exit: Option<Duration>,
    /// `--terminal-runtime pty`: pitcrew-ptyd even where tmux is usable.
    pub force_pty: bool,
}

/// The runner's terminals' runtime: tmux, pitcrew-ptyd, or none. Cheap to clone; every clone is
/// the same runtime, and [`TerminalRuntime::detach`] lets go of it for all of them.
#[derive(Clone, Debug)]
pub struct TerminalRuntime {
    shared: Arc<Detachable>,
    /// What the terminals run in (`tmux` or `pty`) and where (the tmux socket, ptyd's endpoint).
    place: Option<(Capability, PathBuf)>,
}

impl TerminalRuntime {
    /// No runtime: no session has a terminal here.
    pub fn none() -> Self {
        Self {
            shared: Arc::new(Detachable::new(Arc::new(NoRuntime))),
            place: None,
        }
    }

    /// tmux, if it is usable here, else pitcrew-ptyd, if it is installed next to `pitcrewd`, each
    /// on the state directory `state`'s own socket or endpoint unless `overrides` says otherwise;
    /// otherwise none, logging why. Detection and the runtime's setup run off the async executor.
    pub async fn choose(state: &Path, overrides: &Overrides) -> Self {
        Self::chosen(Plan::new(state, overrides).choose().await)
    }

    /// The runtime a plan chose, or none, logging why.
    fn chosen(plan: Result<Self, String>) -> Self {
        plan.unwrap_or_else(|why| {
            tracing::warn!(
                "the runner's terminals cannot use tmux or pitcrew-ptyd, so no session has a \
                 terminal here: {why}"
            );
            Self::none()
        })
    }

    /// `runtime`, holding `lock`, its terminals running in `capability` (the chosen runtime's,
    /// `Chosen::capability`) at `place`.
    fn locked(
        runtime: Box<dyn Runtime>,
        lock: Lock,
        capability: Capability,
        place: PathBuf,
    ) -> Self {
        Self {
            shared: Arc::new(Detachable::new(Arc::new(Locked {
                runtime,
                _lock: lock,
            }))),
            place: Some((capability, place)),
        }
    }

    /// The runtime, for the runner's terminals.
    pub fn runtime(&self) -> Arc<dyn Runtime> {
        Arc::clone(&self.shared) as Arc<dyn Runtime>
    }

    /// What the terminals run in: host info's `tmux` or `pty` capability; `None` without a
    /// runtime.
    pub fn capability(&self) -> Option<Capability> {
        self.place.as_ref().map(|(capability, _)| *capability)
    }

    /// Lets go of the runtime: from now on every call answers `Unavailable`. Dropping it stores
    /// the terminals' output offsets in tmux and detaches, or closes the connection to ptyd (which
    /// keeps them); either way the terminals keep running. Then the lock is released. This waits
    /// for that at most `within`, after which it goes on on the blocking pool, which the stop
    /// waits for only so long. Call it once the runner has stopped.
    pub async fn detach(&self, within: Duration) {
        let Some(runtime) = self.shared.take() else {
            return;
        };
        let Some((capability, place)) = self.place.clone() else {
            return;
        };
        let keeps = if capability == Capability::Tmux {
            "tmux, and their output offsets are stored there for the next start"
        } else {
            "pitcrew-ptyd, which keeps their output and its offsets for the next start"
        };
        let dropping = tokio::task::spawn_blocking(move || {
            // A call still in flight holds it too; it is then dropped when that call returns.
            let last = Arc::strong_count(&runtime) == 1;
            drop(runtime);
            last
        });
        match tokio::time::timeout(within, dropping).await {
            Ok(Ok(true)) => tracing::info!(
                at = %place.display(),
                "the terminals' runtime detached: the terminals keep running in {keeps}"
            ),
            Ok(Ok(false)) => tracing::info!(
                at = %place.display(),
                "the terminals' runtime detaches once its last call returns; the terminals keep \
                 running in {keeps}"
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
    let name = state_hash(state);
    let fallback = PathBuf::from(format!("/tmp/pitcrew-{}", pitcrew_auth::euid()));
    let per_user = TmuxOptions::default_socket()
        .parent()
        .map(Path::to_path_buf);
    per_user
        .map(|dir| dir.join(&name).join("tmux"))
        .filter(|socket| socket.as_os_str().len() <= unix::SOCKET_PATH_MAX)
        .unwrap_or_else(|| fallback.join(&name).join("tmux"))
}

/// pitcrew-ptyd's endpoint for the daemon of the state directory `state`: one ptyd per state
/// directory. On Unix, `ptyd` next to the tmux socket ([`default_socket`]: the same directory,
/// the same lock); on Windows the user's pipe (`\\.\pipe\pitcrew-ptyd-<SID>`, with `-elevated`
/// when this process is elevated) with `-<the same 8 hex digits>` added.
pub fn default_endpoint(state: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        default_socket(state).with_file_name(unix::PTYD_SOCKET)
    }
    #[cfg(not(unix))]
    {
        let mut name = PtyOptions::default_endpoint().into_os_string();
        name.push("-");
        name.push(state_hash(state));
        PathBuf::from(name)
    }
}

/// 8 hex digits of the sha256 of the canonical state directory (as given if it cannot be
/// resolved): what tells two state directories' sockets and endpoints apart.
fn state_hash(state: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let canonical = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
    let hash = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    hash[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// What `e` says, for people: an `Unavailable`'s reason as it is, without the prefixes
/// `pitcrew_runtime::choose` puts on the reasons it joins.
fn reason(e: RuntimeError) -> String {
    match e {
        RuntimeError::Unavailable(why) => why
            .strip_prefix("no terminal runtime: ")
            .unwrap_or(&why)
            .replace("runtime unavailable: ", ""),
        other => other.to_string(),
    }
}

/// `work` on the blocking pool; its failure to run, as a reason.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// True if `ptyd` is a program file at an absolute path (as the PTY runtime's own check).
fn installed(ptyd: &Path) -> bool {
    ptyd.is_absolute()
        && std::fs::metadata(ptyd).is_ok_and(|meta| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                meta.is_file() && meta.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                meta.is_file()
            }
        })
}

/// Where each runtime would run, from the state directory and the overrides.
struct Plan {
    tmux: TmuxOptions,
    /// The tmux socket is the state directory's own, so its per-user directory may be made.
    /// Read only on Unix, where tmux runs.
    #[cfg_attr(not(unix), allow(dead_code))]
    own_socket: bool,
    pty: PtyOptions,
    /// ptyd's endpoint is the state directory's own, so its per-user directory may be made.
    own_endpoint: bool,
    force_pty: bool,
    /// Where ptyd's endpoints' lock files are (Windows): `%LOCALAPPDATA%\PitCrew`, if it is
    /// known.
    #[cfg(windows)]
    ptyd_locks: Option<PathBuf>,
}

impl Plan {
    /// The plan for the state directory `state`, warning of each override.
    fn new(state: &Path, overrides: &Overrides) -> Self {
        let socket = match &overrides.tmux_socket {
            Some(socket) => {
                tracing::warn!(
                    socket = %socket.display(),
                    "--tmux-socket: the runner's tmux server is on this socket instead of this \
                     state directory's own; for tests and development only"
                );
                socket.clone()
            }
            #[cfg(unix)]
            None => default_socket(state),
            #[cfg(not(unix))]
            None => TmuxOptions::default_socket(),
        };
        let endpoint = match &overrides.ptyd_endpoint {
            Some(endpoint) => {
                tracing::warn!(
                    endpoint = %endpoint.display(),
                    "--ptyd-endpoint: pitcrew-ptyd listens here instead of on this state \
                     directory's own endpoint; for tests and development only"
                );
                endpoint.clone()
            }
            None => default_endpoint(state),
        };
        let mut pty = PtyOptions::new(endpoint);
        if let Some(ptyd) = &overrides.ptyd {
            let ptyd = std::path::absolute(ptyd).unwrap_or_else(|_| ptyd.clone());
            tracing::warn!(
                ptyd = %ptyd.display(),
                "--ptyd: the runner's terminals use this pitcrew-ptyd instead of the one next to \
                 pitcrewd; for tests and development only"
            );
            pty.ptyd = ptyd;
        }
        if let Some(idle) = overrides.ptyd_idle_exit {
            tracing::warn!(
                ms = idle.as_millis(),
                "--ptyd-idle-exit-ms: a pitcrew-ptyd this daemon starts exits once idle this \
                 long; for tests and development only"
            );
            pty.idle_exit = Some(idle);
        }
        if overrides.force_pty {
            tracing::warn!(
                "--terminal-runtime pty: the runner's terminals use pitcrew-ptyd even where tmux \
                 is usable; for tests and development only"
            );
        }
        Self {
            tmux: TmuxOptions::new(socket),
            own_socket: overrides.tmux_socket.is_none(),
            pty,
            own_endpoint: overrides.ptyd_endpoint.is_none(),
            force_pty: overrides.force_pty,
            #[cfg(windows)]
            ptyd_locks: windows::lock_dir(),
        }
    }

    /// The runtime, or why there is none.
    async fn choose(self) -> Result<TerminalRuntime, String> {
        // The socket's directory and its lock come first, so a socket another daemon holds is
        // refused before tmux is asked anything.
        let tmux = if self.force_pty {
            Err("tmux is not tried (--terminal-runtime pty)".to_owned())
        } else {
            self.prepare_tmux().await
        };
        // ptyd's endpoint's directory, made private as the socket's is, if ptyd is installed
        // and passes the trust check. A missing one makes nothing (detection says why); one that
        // fails the check is warned about, makes nothing, and is not used.
        let endpoint = {
            let (ptyd, endpoint, own) = (
                self.pty.ptyd.clone(),
                self.pty.endpoint.clone(),
                self.own_endpoint,
            );
            blocking(move || {
                if !installed(&ptyd) {
                    return Ok(());
                }
                if let Err(why) = pitcrew_runtime::pty::launch::check_trusted(&ptyd) {
                    tracing::warn!("{why}; the runner's terminals do not use it");
                    return Err(why);
                }
                prepare_endpoint(&endpoint, own)
            })
            .await
        };
        let (support, no_tmux) = match tmux {
            Ok(lock) => {
                match pitcrew_runtime::choose_async(self.tmux.clone(), self.pty.clone()).await {
                    Ok(Chosen::Tmux(support)) => match self.tmux_runtime(support, lock).await {
                        Ok(runtime) => return Ok(runtime),
                        Err(no_tmux) => self.pty_alone(no_tmux, endpoint).await?,
                    },
                    Ok(Chosen::Pty { support, no_tmux }) => {
                        let no_tmux = reason(RuntimeError::Unavailable(no_tmux));
                        if let Err(no_pty) = endpoint {
                            return Err(format!("{no_tmux}; {no_pty}"));
                        }
                        (support, no_tmux)
                    }
                    Ok(other) => return Err(format!("an unknown runtime, {:?}", other.kind())),
                    Err(e) => return Err(reason(e)),
                }
                // The tmux lock, if it was not used, is released here: on Unix the PTY runtime
                // takes the same one when its endpoint is next to the socket.
            }
            Err(no_tmux) => self.pty_alone(no_tmux, endpoint).await?,
        };
        self.pty_runtime(support, no_tmux).await
    }

    /// The PTY runtime's detection alone, tmux being unusable for `no_tmux`.
    async fn pty_alone(
        &self,
        no_tmux: String,
        endpoint: Result<(), String>,
    ) -> Result<(PtySupport, String), String> {
        if let Err(no_pty) = endpoint {
            return Err(format!("{no_tmux}; {no_pty}"));
        }
        match pitcrew_runtime::pty::detect_async(self.pty.clone()).await {
            Ok(support) => Ok((support, no_tmux)),
            Err(e) => Err(format!("{no_tmux}; {}", reason(e))),
        }
    }

    /// The tmux socket's directory, made private (0700 if missing; never repaired), and its lock.
    #[cfg(unix)]
    async fn prepare_tmux(&self) -> Result<Lock, String> {
        let (socket, own) = (self.tmux.socket.clone(), self.own_socket);
        blocking(move || unix::prepare(&socket, own)).await
    }

    #[cfg(not(unix))]
    async fn prepare_tmux(&self) -> Result<Lock, String> {
        Ok(Lock::default())
    }

    /// tmux, detected usable, on a server with no sessions but PitCrew's; or why not.
    #[cfg(unix)]
    async fn tmux_runtime(
        &self,
        support: TmuxSupport,
        lock: Lock,
    ) -> Result<TerminalRuntime, String> {
        let socket = self.tmux.socket.clone();
        let foreign = {
            let (tmux, socket) = (support.tmux.clone(), socket.clone());
            blocking(move || unix::foreign_sessions(&tmux, &socket)).await?
        };
        if !foreign.is_empty() {
            return Err(format!(
                "the tmux server on {} has {} session(s) that are not PitCrew's (a person's own \
                 tmux?); PitCrew runs its terminals on a server of its own",
                socket.display(),
                foreign.len()
            ));
        }
        let (version, tmux) = (support.version.clone(), support.tmux.clone());
        let (tmux_options, pty_options) = (self.tmux.clone(), self.pty.clone());
        let (runtime, capability) = blocking(move || {
            let chosen = Chosen::Tmux(support);
            let capability = chosen.capability();
            let runtime = chosen
                .into_runtime(tmux_options, pty_options)
                .map_err(reason)?;
            Ok((runtime, capability))
        })
        .await?;
        tracing::info!(
            %version,
            tmux = %tmux.display(),
            socket = %socket.display(),
            "the runner's terminals run in tmux; attach to them with `tmux -S {} attach -t {}`",
            socket.display(),
            pitcrew_runtime::tmux::SESSION,
        );
        Ok(TerminalRuntime::locked(runtime, lock, capability, socket))
    }

    #[cfg(not(unix))]
    async fn tmux_runtime(
        &self,
        _support: TmuxSupport,
        _lock: Lock,
    ) -> Result<TerminalRuntime, String> {
        Err("tmux runs on Unix-like systems only".to_owned())
    }

    /// The PTY runtime, detected usable, holding its endpoint's lock; or why not.
    async fn pty_runtime(
        &self,
        support: PtySupport,
        no_tmux: String,
    ) -> Result<TerminalRuntime, String> {
        let (ptyd, endpoint) = (support.ptyd.clone(), support.endpoint.clone());
        let built = {
            let (endpoint, why) = (endpoint.clone(), no_tmux.clone());
            let (tmux_options, pty_options) = (self.tmux.clone(), self.pty.clone());
            #[cfg(windows)]
            let locks = self.ptyd_locks.clone();
            blocking(move || {
                #[cfg(windows)]
                let lock = windows::lock_endpoint(&endpoint, locks.as_deref())?;
                #[cfg(not(windows))]
                let lock = lock_endpoint(&endpoint)?;
                let chosen = Chosen::Pty {
                    support,
                    no_tmux: why,
                };
                let capability = chosen.capability();
                let runtime = chosen
                    .into_runtime(tmux_options, pty_options)
                    .map_err(reason)?;
                Ok((runtime, lock, capability))
            })
            .await
        };
        let (runtime, lock, capability) = built.map_err(|no_pty| format!("{no_tmux}; {no_pty}"))?;
        tracing::info!(
            ptyd = %ptyd.display(),
            endpoint = %endpoint.display(),
            log = %ptyd_log(&endpoint),
            "the runner's terminals run in pitcrew-ptyd, which keeps them running when pitcrewd \
             stops; tmux is not used: {no_tmux}"
        );
        Ok(TerminalRuntime::locked(runtime, lock, capability, endpoint))
    }
}

/// Where a ptyd this daemon starts on `endpoint` writes its log.
fn ptyd_log(endpoint: &Path) -> String {
    #[cfg(unix)]
    {
        endpoint.parent().map_or_else(String::new, |dir| {
            dir.join("ptyd.log").display().to_string()
        })
    }
    #[cfg(not(unix))]
    {
        let _ = endpoint;
        "none (pitcrew-ptyd keeps no log on Windows)".to_owned()
    }
}

/// ptyd's endpoint's directory made private, as the tmux socket's is (Unix; nothing to make for
/// a pipe).
fn prepare_endpoint(endpoint: &Path, own: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        unix::make_private(endpoint, own).map(drop)
    }
    #[cfg(not(unix))]
    {
        let _ = (endpoint, own);
        Ok(())
    }
}

/// The lock of ptyd's endpoint: on Unix the one in its directory, the tmux socket's when they
/// share it; on Windows its own lock file (`windows::lock_endpoint`).
#[cfg(not(windows))]
fn lock_endpoint(endpoint: &Path) -> Result<Lock, String> {
    #[cfg(unix)]
    {
        let dir = endpoint
            .parent()
            .ok_or_else(|| format!("{} has no directory", endpoint.display()))?;
        unix::SocketLock::acquire(&dir.join(unix::LOCK_FILE), "pitcrew-ptyd endpoint")
    }
    #[cfg(not(unix))]
    {
        let _ = endpoint;
        Ok(Lock)
    }
}

/// The lock a runtime holds for its socket or endpoint.
#[cfg(unix)]
type Lock = unix::SocketLock;

/// The lock a runtime holds for ptyd's endpoint; none for tmux, which does not run here.
#[cfg(windows)]
#[derive(Debug, Default)]
struct Lock {
    _held: Option<pitcrew_runtime::pty::windows::FileLock>,
}

/// Nothing to hold.
#[cfg(not(any(unix, windows)))]
#[derive(Debug, Default)]
struct Lock;

#[cfg(windows)]
mod windows {
    use super::Lock;
    use pitcrew_runtime::pty::windows::FileLock;
    use std::path::{Path, PathBuf};

    /// Where ptyd's endpoints' lock files are: `PitCrew` in `%LOCALAPPDATA%` when that is set to
    /// an absolute path, and only otherwise in the local data folder Windows knows for the user
    /// (`FOLDERID_LocalAppData`). The variable comes first, so pointing it elsewhere (as the tests
    /// do) moves the locks too. The default state directory (`…\PitCrew\data`) is always in the
    /// known folder, so the two share `PitCrew` only while the variable names that folder, as it
    /// does unless changed. One user's daemons share it, whatever their state directories.
    pub(super) fn lock_dir() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| {
                directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().to_path_buf())
            })
            .map(|dir| dir.join("PitCrew"))
    }

    /// `ptyd-<16 hex digits>.lock` in `dir`: the digits are the start of the sha256 of the
    /// endpoint's name in lower case, as Windows does not tell pipe names apart by case.
    pub(super) fn lock_file(dir: &Path, endpoint: &Path) -> PathBuf {
        use sha2::{Digest as _, Sha256};
        let name = endpoint.to_string_lossy().to_ascii_lowercase();
        let hash = Sha256::digest(name.as_bytes());
        let digits: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
        dir.join(format!("ptyd-{digits}.lock"))
    }

    /// ptyd's endpoint's own lock (`LockFileEx` on its lock file in `dir`), held while the
    /// runtime lives: a second daemon given the same endpoint is refused.
    pub(super) fn lock_endpoint(endpoint: &Path, dir: Option<&Path>) -> Result<Lock, String> {
        let dir = dir.ok_or_else(|| {
            "cannot find this user's local data folder (%LOCALAPPDATA%) for pitcrew-ptyd's \
             endpoint lock"
                .to_owned()
        })?;
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let path = lock_file(dir, endpoint);
        match FileLock::try_exclusive(&path) {
            Ok(Some(lock)) => Ok(Lock { _held: Some(lock) }),
            Ok(None) => Err(format!(
                "another pitcrewd uses this pitcrew-ptyd endpoint (it holds {})",
                path.display()
            )),
            Err(e) => Err(format!("cannot lock {}: {e}", path.display())),
        }
    }
}

#[cfg(unix)]
mod unix {
    use pitcrew_runtime::tmux::SESSION;
    use std::io::Read as _;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// The longest socket path: macOS holds 104 bytes with the NUL (Linux 108).
    pub(super) const SOCKET_PATH_MAX: usize = 103;
    /// The lock file in the socket's directory.
    pub(super) const LOCK_FILE: &str = "lock";
    /// ptyd's endpoint's file name, next to the tmux socket.
    pub(super) const PTYD_SOCKET: &str = "ptyd";
    /// The longest the check of the server's sessions may take.
    const LIST_TIMEOUT: Duration = Duration::from_secs(5);
    /// The most of tmux's answer that is read.
    const LIST_MAX: u64 = 1024 * 1024;

    /// Makes the socket's directory private (and, for the default socket, the per-user directory
    /// above it), creating them 0700 if missing; never repairs one that is not; and takes the
    /// socket's lock.
    pub(super) fn prepare(socket: &Path, default: bool) -> Result<SocketLock, String> {
        let dir = make_private(socket, default)?;
        SocketLock::acquire(&dir.join(LOCK_FILE), "tmux socket")
    }

    /// The directory of `socket` (a tmux socket or ptyd's endpoint), checked and made private as
    /// [`prepare`] says, without the lock.
    pub(super) fn make_private(socket: &Path, default: bool) -> Result<&Path, String> {
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
        Ok(dir)
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

    /// An exclusive lock on a socket's directory, held while the runtime lives.
    #[derive(Debug)]
    pub(crate) struct SocketLock {
        _fd: OwnedFd,
    }

    impl SocketLock {
        /// Locks `path`; `what` names what it guards (`tmux socket`, `pitcrew-ptyd endpoint`)
        /// when another daemon holds it.
        pub(super) fn acquire(path: &Path, what: &str) -> Result<Self, String> {
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
                    "another pitcrewd uses this {what} (it holds {})",
                    path.display()
                )),
                Err(e) => Err(format!("cannot lock {}: {e}", path.display())),
            }
        }
    }

    impl Drop for SocketLock {
        /// Unlocks before the descriptor is closed: a process another thread is starting holds
        /// a copy of it until it runs its program, and an flock lasts while any copy is open.
        /// `LOCK_UN` releases the lock of the open file every copy shares, so a child forked to
        /// keep the lock would lose it here; none is meant to.
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
}

/// A runtime holding its lock: dropped, the runtime goes first (tmux stores the terminals'
/// offsets; ptyd's connection closes), then the lock.
struct Locked {
    runtime: Box<dyn Runtime>,
    _lock: Lock,
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

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
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

    /// Without tmux or ptyd, the terminals have no runtime: nothing to detach, nothing reported.
    #[test]
    fn none_is_no_runtime() {
        let none = TerminalRuntime::none();
        assert_eq!(none.capability(), None);
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
    /// the reason; elsewhere than Unix, there is no tmux either. With no pitcrew-ptyd where it is
    /// looked for, there is no runtime at all, and nothing was made for either.
    #[test]
    fn a_refused_socket_and_no_ptyd_mean_no_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let overrides = Overrides {
            tmux_socket: Some(tmp.path().join("missing").join("dir").join("tmux")),
            ptyd: Some(tmp.path().join("bin").join("pitcrew-ptyd")),
            ptyd_endpoint: Some(tmp.path().join("p").join("ptyd")),
            ..Overrides::default()
        };
        let chosen = block_on(TerminalRuntime::choose(tmp.path(), &overrides));
        assert_eq!(chosen.capability(), None);
        assert!(chosen.runtime().list().unwrap().is_empty());
        assert!(!tmp.path().join("missing").exists(), "nothing was made");
        assert!(!tmp.path().join("p").exists(), "nothing was made");
    }

    /// A stand-in pitcrew-ptyd in `dir` (both 0755 on Unix), which leaves `ran` next to `dir`
    /// if it is ever run.
    fn fake_ptyd(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let ptyd = dir.join(pitcrew_runtime::pty::launch::PTYD);
        let ran = dir.with_file_name("ran");
        std::fs::write(
            &ptyd,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for path in [dir, &ptyd] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        ptyd
    }

    /// Overrides for the state directory `tmp` that force the PTY runtime with `ptyd`, on an
    /// endpoint of the test's own, `name`: a socket in `tmp/p`, or a pipe.
    fn forced(tmp: &Path, ptyd: &Path, name: &str) -> Overrides {
        let endpoint = if cfg!(windows) {
            PathBuf::from(format!(
                r"\\.\pipe\pitcrew-ptyd-unit-{name}-{}-{}",
                std::process::id(),
                state_hash(tmp)
            ))
        } else {
            tmp.join("p").join("ptyd")
        };
        Overrides {
            tmux_socket: Some(tmp.join("missing").join("dir").join("tmux")),
            ptyd: Some(ptyd.to_path_buf()),
            ptyd_endpoint: Some(endpoint),
            ptyd_idle_exit: Some(Duration::from_millis(500)),
            force_pty: true,
        }
    }

    /// [`TerminalRuntime::choose`] for the state directory `tmp`, with ptyd's endpoint lock files
    /// (Windows) in `tmp/locks`, never in the user's own folder.
    fn choose_in(tmp: &Path, overrides: &Overrides) -> TerminalRuntime {
        let plan = Plan::new(tmp, overrides);
        #[cfg(windows)]
        let plan = Plan {
            ptyd_locks: Some(tmp.join("locks")),
            ..plan
        };
        TerminalRuntime::chosen(block_on(plan.choose()))
    }

    /// What makes a ptyd fail the trust check here, and what the reason then says: on Unix
    /// another user could write it (group-writable), on Windows it was downloaded from the web.
    #[cfg(unix)]
    const UNTRUSTED: &str = "can be written by other users (mode 775)";
    #[cfg(windows)]
    const UNTRUSTED: &str = "Zone.Identifier";

    fn untrust(ptyd: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(ptyd, std::fs::Permissions::from_mode(0o775)).unwrap();
        }
        #[cfg(windows)]
        {
            let mut stream = ptyd.as_os_str().to_owned();
            stream.push(":Zone.Identifier");
            std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        }
    }

    /// A program file where pitcrew-ptyd is looked for, a private endpoint: forced, the PTY
    /// runtime is chosen (nothing is started: ptyd starts with the first terminal). Its endpoint
    /// is locked (on Unix in its directory, made private; on Windows by its own lock file), so a
    /// second daemon given the same endpoint gets no runtime until the first lets go of it.
    #[test]
    fn a_forced_pty_runtime_is_chosen_and_its_endpoint_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let ptyd = fake_ptyd(&tmp.path().join("bin"));
        let overrides = forced(tmp.path(), &ptyd, "forced");
        let endpoint = overrides.ptyd_endpoint.clone().unwrap();
        let first = choose_in(tmp.path(), &overrides);
        assert_eq!(first.capability(), Some(Capability::Pty));
        assert_eq!(first.runtime().kind(), RuntimeKind::Pty);
        // No ptyd runs on that endpoint: there are no terminals.
        assert!(first.runtime().list().unwrap().is_empty());
        assert!(!tmp.path().join("missing").exists(), "tmux was not tried");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tmp.path().join("p");
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
            assert!(dir.join("lock").exists());
        }
        #[cfg(windows)]
        {
            let locks = tmp.path().join("locks");
            assert!(windows::lock_file(&locks, &endpoint).is_file());
            // Pipe names do not differ by case: the same endpoint in capitals is locked too.
            let shouting = Overrides {
                ptyd_endpoint: Some(PathBuf::from(
                    endpoint.to_string_lossy().to_ascii_uppercase(),
                )),
                ..overrides.clone()
            };
            let second = choose_in(tmp.path(), &shouting);
            assert_eq!(second.capability(), None, "the endpoint is locked");
        }
        let second = choose_in(tmp.path(), &overrides);
        assert_eq!(second.capability(), None, "the endpoint is locked");
        block_on(first.detach(Duration::from_secs(5)));
        let third = choose_in(tmp.path(), &overrides);
        assert_eq!(
            third.capability(),
            Some(Capability::Pty),
            "{}",
            endpoint.display()
        );
        block_on(third.detach(Duration::from_secs(5)));
        assert!(!tmp.path().join("ran").exists(), "ptyd was never started");
    }

    /// A ptyd that fails the trust check (here group-writable on Unix, downloaded on Windows) is
    /// not used, as if it were missing: no runtime, and nothing made or locked for its endpoint.
    #[test]
    fn an_untrusted_ptyd_is_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        let ptyd = fake_ptyd(&tmp.path().join("bin"));
        untrust(&ptyd);
        let overrides = forced(tmp.path(), &ptyd, "untrusted");
        let chosen = choose_in(tmp.path(), &overrides);
        assert_eq!(chosen.capability(), None);
        assert!(
            !tmp.path().join("p").exists(),
            "nothing made for its endpoint"
        );
        assert!(!tmp.path().join("locks").exists(), "nothing locked");
        assert!(!tmp.path().join("ran").exists(), "ptyd was never started");
        // The reason, as `Plan::choose` gives it (and the warning logs it).
        let why = match block_on(Plan::new(tmp.path(), &overrides).choose()) {
            Err(why) => why,
            Ok(_) => panic!("chosen"),
        };
        assert!(why.contains("not running pitcrew-ptyd at"), "{why}");
        assert!(why.contains(UNTRUSTED), "{why}");
    }

    /// A ptyd that passed when the runtime was chosen, and fails the check when the first
    /// terminal would start it (hours later, say), is refused then: the start fails cleanly,
    /// with the reason, and ptyd never runs.
    #[test]
    fn a_ptyd_changed_after_choose_is_refused_at_launch() {
        let tmp = tempfile::tempdir().unwrap();
        let ptyd = fake_ptyd(&tmp.path().join("bin"));
        let overrides = forced(tmp.path(), &ptyd, "changed");
        let chosen = choose_in(tmp.path(), &overrides);
        assert_eq!(chosen.capability(), Some(Capability::Pty));
        untrust(&ptyd);
        match chosen.runtime().start(&spec()) {
            Err(RuntimeError::Unavailable(why)) => {
                assert!(why.contains("not running pitcrew-ptyd at"), "{why}");
                assert!(why.contains(UNTRUSTED), "{why}");
            }
            other => panic!("{other:?}"),
        }
        assert!(chosen.runtime().list().unwrap().is_empty());
        assert!(!tmp.path().join("ran").exists(), "ptyd was never started");
        block_on(chosen.detach(Duration::from_secs(5)));
    }

    /// Where tmux cannot be used (here its socket's directory is refused; elsewhere than Unix
    /// there is none), the PTY runtime is chosen without being forced.
    #[test]
    fn without_tmux_the_pty_runtime_is_chosen() {
        let tmp = tempfile::tempdir().unwrap();
        let ptyd = fake_ptyd(&tmp.path().join("bin"));
        let overrides = Overrides {
            force_pty: false,
            ptyd_idle_exit: None,
            ..forced(tmp.path(), &ptyd, "auto")
        };
        let chosen = choose_in(tmp.path(), &overrides);
        assert_eq!(chosen.capability(), Some(Capability::Pty));
        assert!(
            !tmp.path().join("missing").exists(),
            "nothing made for tmux"
        );
        block_on(chosen.detach(Duration::from_secs(5)));
    }

    /// By default pitcrew-ptyd is looked for next to the running executable, never on `PATH`,
    /// and listens on the state directory's own endpoint.
    #[test]
    fn ptyd_is_looked_for_next_to_pitcrewd() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = Plan::new(tmp.path(), &Overrides::default());
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            plan.pty.ptyd,
            exe.with_file_name(pitcrew_runtime::pty::launch::PTYD)
        );
        assert_eq!(plan.pty.endpoint, default_endpoint(tmp.path()));
        assert_eq!(plan.pty.idle_exit, None, "ptyd's own default");
        assert!(plan.own_endpoint && plan.own_socket && !plan.force_pty);
    }

    /// One socket and one ptyd endpoint per state directory: the same directory (however it is
    /// spelled) always gets the same ones, another directory others. On Unix both are in one
    /// directory of their own under the runtime's per-user directory, within the socket path
    /// limit; on Windows the endpoint is the user's pipe with the state directory's digits.
    #[test]
    fn each_state_directory_has_a_socket_and_an_endpoint_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let endpoint = default_endpoint(&a);
        assert_eq!(endpoint, default_endpoint(&a));
        assert_eq!(endpoint, default_endpoint(&tmp.path().join("b/../a")));
        assert_ne!(endpoint, default_endpoint(&b));
        let digits = state_hash(&a);
        assert_eq!(digits.len(), 8, "{digits}");
        assert!(digits.bytes().all(|c| c.is_ascii_hexdigit()), "{digits}");
        #[cfg(windows)]
        {
            let name = endpoint.to_string_lossy().into_owned();
            let user = PtyOptions::default_endpoint()
                .to_string_lossy()
                .into_owned();
            assert_eq!(name, format!("{user}-{digits}"));
            assert!(
                pitcrew_runtime::pty::check_endpoint(&endpoint).is_ok(),
                "{name}"
            );
        }
        #[cfg(unix)]
        {
            let socket = default_socket(&a);
            assert_eq!(socket, default_socket(&a));
            assert_eq!(socket, default_socket(&tmp.path().join("b/../a")));
            assert_ne!(socket, default_socket(&b));
            assert!(socket.ends_with("tmux"), "{}", socket.display());
            assert_eq!(
                endpoint,
                socket.with_file_name("ptyd"),
                "next to the socket"
            );
            let dir = socket.parent().unwrap();
            assert_eq!(dir.file_name().unwrap().to_str().unwrap(), digits);
            let per_user = TmuxOptions::default_socket();
            let fallback = PathBuf::from(format!("/tmp/pitcrew-{}", pitcrew_auth::euid()));
            let parent = dir.parent().unwrap();
            assert!(
                Some(parent) == per_user.parent() || parent == fallback,
                "{}",
                socket.display()
            );
            assert!(socket.as_os_str().len() <= unix::SOCKET_PATH_MAX);
            assert!(endpoint.as_os_str().len() <= unix::SOCKET_PATH_MAX);
        }
    }

    /// The default socket's per-user directory is made private if missing, and so is its own;
    /// one that is open to others is refused, not repaired. A given socket's directory needs its
    /// parent. The lock has one holder at a time, whichever runtime takes it.
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
        assert!(
            again.contains("another pitcrewd uses this tmux socket"),
            "{again}"
        );
        // ptyd's endpoint next to it: the same lock.
        let endpoint = socket.with_file_name("ptyd");
        let held = lock_endpoint(&endpoint).unwrap_err();
        assert!(
            held.contains("another pitcrewd uses this pitcrew-ptyd endpoint"),
            "{held}"
        );
        drop(lock);
        let lock = lock_endpoint(&endpoint).unwrap();
        assert!(unix::prepare(&socket, true).is_err());
        drop(lock);
        drop(unix::prepare(&socket, true).unwrap());

        // A given socket: only its own directory is made.
        let given = tmp.path().join("missing").join("t").join("s");
        assert!(unix::prepare(&given, false).is_err());
        assert!(prepare_endpoint(&given, false).is_err());
        assert!(!tmp.path().join("missing").exists());

        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(unix::prepare(&open.join("x").join("tmux"), true).is_err());
        assert!(prepare_endpoint(&open.join("x").join("ptyd"), true).is_err());
        assert_eq!(mode(&open), 0o755, "not repaired");
        assert!(!open.join("x").exists());
        assert!(unix::prepare(&PathBuf::from("rel/s"), false).is_err());
        assert!(prepare_endpoint(&PathBuf::from("rel/ptyd"), false).is_err());
    }

    /// A server with sessions other than PitCrew's is foreign; no server is none; tmux failing
    /// otherwise is an error. (A stand-in `tmux` answers.)
    #[cfg(unix)]
    #[test]
    fn foreign_sessions_are_found() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            crate::test_scripts::write_script(&path, &format!("#!/bin/sh\n{body}\n"), 0o755);
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

    /// A runtime's refusal reads as its reason; choose's own prefix is not repeated.
    #[test]
    fn reasons_read_plainly() {
        assert_eq!(
            reason(RuntimeError::Unavailable(
                "no terminal runtime: a; b".into()
            )),
            "a; b"
        );
        assert_eq!(reason(RuntimeError::Unavailable("why".into())), "why");
        let joined =
            "no terminal runtime: runtime unavailable: no tmux; runtime unavailable: no ptyd";
        assert_eq!(
            reason(RuntimeError::Unavailable(joined.into())),
            "no tmux; no ptyd"
        );
        assert!(reason(RuntimeError::NotFound(TerminalId::new())).contains("no such terminal"));
    }
}
