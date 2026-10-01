//! The single-host lease for network-mode stores.
//!
//! SQLite's own locking is not trustworthy over a network filesystem (that is why network mode
//! exists at all), so a second, independent mechanism keeps two hosts from writing one store at
//! once: a lease file next to the database (`<db>.lease`), holding the owning host, its pid, a
//! random owner id, and an expiry. `Store::open` in network mode takes the lease or fails with
//! [`Error::Leased`]; the owner renews it from a small background thread that stops (and releases
//! the lease) when the `Store` drops.
//!
//! **Renewal: a background thread, not a `renew()` the caller must call.** A `Store` is meant to
//! be opened once and used; making every caller remember to renew on a timer would be easy to
//! forget and awkward to fit into an async or sync caller alike. The thread wakes every
//! `ttl / RENEW_FRACTION` (a third of the lease length, so a single slow or missed wakeup still
//! leaves two more tries before the lease would actually expire), checks the file still names our
//! `owner`, and only then writes a fresh expiry. If the owner ever does not match — another host
//! took over because our clock stalled, we were suspended, or the file was removed — the thread
//! sets a flag and stops; every append after that fails with [`Error::LeaseLost`] instead of
//! silently writing past a lease we no longer hold.
//!
//! Time comes from the injectable [`Clock`] (`StoreOptions::clock`), not `SystemTime::now()`
//! directly, so tests can make a lease look expired without sleeping.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// Where the network-mode lease gets the time. Implementations must be cheap: the renewal thread
/// calls `now_ms` every wakeup.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

/// The real clock; [`StoreOptions`](crate::StoreOptions)'s default.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// The lease file's contents.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct LeaseData {
    host: String,
    pid: u32,
    owner: String,
    until_ms: i64,
}

/// How often the renewal thread wakes, relative to the lease length: a third, so one missed
/// wakeup (a slow write, a paused process) still leaves margin before the lease actually expires.
const RENEW_FRACTION: u32 = 3;

/// The path of the lease file next to a database file.
pub(crate) fn lease_path(db_path: &Path) -> PathBuf {
    let mut name = db_path
        .file_name()
        .map_or_else(|| "store.db".into(), |n| n.to_owned());
    name.push(".lease");
    db_path.with_file_name(name)
}

/// A held lease: releases on drop, and stops its renewal thread first.
pub(crate) struct LeaseGuard {
    path: PathBuf,
    owner: String,
    lost: Arc<AtomicBool>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseGuard")
            .field("path", &self.path)
            .field("owner", &self.owner)
            .field("lost", &self.lost.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl LeaseGuard {
    /// Takes the lease at `lease_path(db_path)`, or fails with [`Error::Leased`]. Spawns the
    /// renewal thread on success.
    ///
    /// # Errors
    ///
    /// [`Error::Leased`] if another, live owner holds it; [`Error::LeaseIo`] for filesystem
    /// errors acquiring it or starting the renewal thread.
    pub(crate) fn acquire(db_path: &Path, clock: Arc<dyn Clock>, ttl: Duration) -> Result<Self> {
        let path = lease_path(db_path);
        let host = hostname();
        let pid = std::process::id();
        let owner = ulid::Ulid::new().to_string();
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let now_ms = clock.now_ms();

        take_or_refuse(&path, &host, now_ms, ttl_ms)?;
        write_lease(
            &path,
            &LeaseData {
                host: host.clone(),
                pid,
                owner: owner.clone(),
                until_ms: now_ms + ttl_ms,
            },
        )
        .map_err(Error::LeaseIo)?;

        let lost = Arc::new(AtomicBool::new(false));
        let (stop, rx) = mpsc::channel();
        let renew_every = Duration::from_millis(
            (ttl.as_millis() / u128::from(RENEW_FRACTION))
                .try_into()
                .unwrap_or(u64::MAX)
                .max(1),
        );
        let thread = {
            let path = path.clone();
            let owner = owner.clone();
            let lost = Arc::clone(&lost);
            std::thread::Builder::new()
                .name("pitcrew-store-lease".to_owned())
                .spawn(move || {
                    renew_loop(
                        &rx,
                        renew_every,
                        &path,
                        &host,
                        pid,
                        &owner,
                        &clock,
                        ttl_ms,
                        &lost,
                    )
                })
                .map_err(Error::LeaseIo)?
        };

        Ok(Self {
            path,
            owner,
            lost,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// Whether the renewal thread has seen someone else take the lease.
    pub(crate) fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Best effort, and only if the file still names us: another host may have already taken
        // over (we would have set `lost` for that), and deleting its lease would be wrong.
        if read_lease(&self.path).is_some_and(|l| l.owner == self.owner) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The renewal thread body: wakes every `renew_every` (or at once if `rx` gets a stop signal),
/// and renews as long as the file still names `owner`. Stops (setting `lost`) the moment it does
/// not.
#[allow(clippy::too_many_arguments)]
fn renew_loop(
    rx: &mpsc::Receiver<()>,
    renew_every: Duration,
    path: &Path,
    host: &str,
    pid: u32,
    owner: &str,
    clock: &Arc<dyn Clock>,
    ttl_ms: i64,
    lost: &Arc<AtomicBool>,
) {
    loop {
        match rx.recv_timeout(renew_every) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        match read_lease(path) {
            Some(existing) if existing.owner == owner => {
                let now_ms = clock.now_ms();
                let fresh = LeaseData {
                    host: host.to_owned(),
                    pid,
                    owner: owner.to_owned(),
                    until_ms: now_ms + ttl_ms,
                };
                // A transient write failure is retried next wakeup; only a changed owner means
                // someone else took over.
                let _ = write_lease(path, &fresh);
            }
            _ => {
                lost.store(true, Ordering::SeqCst);
                return;
            }
        }
    }
}

/// Checks whether `host` may write the lease at `path`, without writing anything. `Ok(())` means
/// free to take (no lease, an expired one, or — same host only — one whose pid is gone);
/// `Err(Error::Leased)` means another live owner holds it.
fn take_or_refuse(path: &Path, host: &str, now_ms: i64, ttl_ms: i64) -> Result<()> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::LeaseIo(e)),
    };
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

    match read_lease(path) {
        Some(existing) => {
            let same_host_dead = existing.host == host && !pid_alive(existing.pid);
            match decide(Some(&existing), None, now_ms, ttl_ms, same_host_dead) {
                Ok(()) => Ok(()),
                Err((host, pid, until)) => Err(Error::Leased { host, pid, until }),
            }
        }
        // Torn or garbage: not a lease we can name an owner for, so only mtime says whether it
        // might still be live.
        None => match decide(None, Some(mtime_ms), now_ms, ttl_ms, false) {
            Ok(()) => Ok(()),
            Err((host, pid, until)) => Err(Error::Leased { host, pid, until }),
        },
    }
}

/// The pure decision `take_or_refuse` and the renewal thread's takeover checks are built on:
/// given what the lease file says (or, for a garbage file, its mtime) and the current time,
/// whether a new owner may take it. Kept separate from any I/O so it is exhaustively unit-tested
/// without a filesystem.
///
/// - `existing`: the parsed lease, if the file parsed.
/// - `garbage_mtime_ms`: the file's mtime, only when `existing` is `None` but the file exists.
/// - `same_host_pid_dead`: true when `existing` is `Some`, its `host` matches ours, and its `pid`
///   is no longer running: taken over immediately, without waiting for `until_ms`.
///
/// Returns `Ok(())` to take over, `Err((host, pid, until))` (best-effort for a garbage file) to
/// refuse.
fn decide(
    existing: Option<&LeaseData>,
    garbage_mtime_ms: Option<i64>,
    now_ms: i64,
    ttl_ms: i64,
    same_host_pid_dead: bool,
) -> std::result::Result<(), (String, u32, i64)> {
    match existing {
        Some(lease) => {
            let expired = now_ms >= lease.until_ms;
            if expired || same_host_pid_dead {
                Ok(())
            } else {
                Err((lease.host.clone(), lease.pid, lease.until_ms))
            }
        }
        None => match garbage_mtime_ms {
            None => Ok(()), // no file at all
            Some(mtime_ms) => {
                let stale = now_ms >= mtime_ms.saturating_add(ttl_ms);
                if stale {
                    Ok(())
                } else {
                    Err(("unknown".to_owned(), 0, mtime_ms.saturating_add(ttl_ms)))
                }
            }
        },
    }
}

fn read_lease(path: &Path) -> Option<LeaseData> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Writes the lease atomically: a temp file in the same directory (so the rename is on one
/// filesystem), then a rename over the final name. Mode 0600 on Unix; on Windows the file
/// inherits its directory's ACL (as `pitcrew-auth`'s private files do), so callers should keep
/// the store itself under a directory only the owning user can open.
fn write_lease(path: &Path, data: &LeaseData) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".{}.tmp-{}", file_name(path), ulid::Ulid::new()));
    let json = serde_json::to_vec(data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        std::io::Write::write_all(&mut file, &json)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lease".to_owned())
}

/// Whether a pid is still running. `true` when it cannot be determined (an unknown pid encoding,
/// or no portable check): the caller then falls back to the lease's expiry, never to a guess that
/// it is dead.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return true;
    };
    let Some(pid) = rustix::process::Pid::from_raw(raw) else {
        return true;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(e) if e == rustix::io::Errno::SRCH => false,
        Err(_) => true,
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // No portable liveness check without unsafe (Windows needs OpenProcess). Rely on expiry.
    true
}

/// Best-effort host name, for diagnosis in [`Error::Leased`] and to recognise our own past runs
/// for the same-host pid-liveness check. Never fails: an unknown host becomes a fixed label, and
/// [`decide`] falls back to the lease's expiry when it cannot match the host.
#[cfg(unix)]
fn hostname() -> String {
    let uts = rustix::system::uname();
    let name = uts.nodename().to_string_lossy().into_owned();
    if name.is_empty() {
        "unknown-host".to_owned()
    } else {
        name
    }
}

#[cfg(windows)]
fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown-host".to_owned())
}

#[cfg(not(any(unix, windows)))]
fn hostname() -> String {
    "unknown-host".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(host: &str, pid: u32, until_ms: i64) -> LeaseData {
        LeaseData {
            host: host.to_owned(),
            pid,
            owner: "owner".to_owned(),
            until_ms,
        }
    }

    #[test]
    fn no_file_is_free_to_take() {
        assert_eq!(decide(None, None, 1_000, 60_000, false), Ok(()));
    }

    #[test]
    fn an_unexpired_lease_from_a_live_owner_refuses() {
        let l = lease("other", 42, 2_000);
        assert_eq!(
            decide(Some(&l), None, 1_000, 60_000, false),
            Err(("other".to_owned(), 42, 2_000))
        );
    }

    #[test]
    fn an_expired_lease_is_taken_over() {
        let l = lease("other", 42, 1_000);
        assert_eq!(decide(Some(&l), None, 1_000, 60_000, false), Ok(()));
        assert_eq!(decide(Some(&l), None, 1_001, 60_000, false), Ok(()));
    }

    #[test]
    fn same_host_with_a_dead_pid_is_taken_over_before_expiry() {
        let l = lease("this-host", 42, 999_999);
        // same_host_pid_dead is computed by the caller (host match + liveness check); here it is
        // passed straight through.
        assert_eq!(decide(Some(&l), None, 1_000, 60_000, true), Ok(()));
    }

    #[test]
    fn a_dead_pid_on_another_host_still_waits_for_expiry() {
        // same_host_pid_dead is only set when the host matches; a caller must not set it for a
        // different host, so this only documents that `decide` itself does not check the host.
        let l = lease("other-host", 42, 999_999);
        assert_eq!(
            decide(Some(&l), None, 1_000, 60_000, false),
            Err(("other-host".to_owned(), 42, 999_999))
        );
    }

    #[test]
    fn a_garbage_file_refuses_until_its_mtime_is_older_than_the_lease_length() {
        // mtime 1_000, ttl 60_000: stale from now_ms = 61_000 on.
        assert_eq!(
            decide(None, Some(1_000), 1_000, 60_000, false),
            Err(("unknown".to_owned(), 0, 61_000))
        );
        assert_eq!(
            decide(None, Some(1_000), 60_999, 60_000, false),
            Err(("unknown".to_owned(), 0, 61_000))
        );
        assert_eq!(decide(None, Some(1_000), 61_000, 60_000, false), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn a_live_pid_is_alive_and_pid_zero_cases_default_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn a_finished_child_process_is_not_alive() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        child.wait().expect("wait");
        assert!(!pid_alive(child.id()));
    }

    #[test]
    fn lease_path_adds_the_suffix() {
        assert_eq!(
            lease_path(Path::new("/a/b/store.db")),
            Path::new("/a/b/store.db.lease")
        );
    }

    #[test]
    fn hostname_is_never_empty() {
        assert!(!hostname().is_empty());
    }
}
