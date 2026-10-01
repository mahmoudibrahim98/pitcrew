//! The single-host lease for network-mode stores.
//!
//! SQLite's own locking is not trustworthy over a network filesystem (that is why network mode
//! exists at all), so a second, independent mechanism keeps two hosts from writing one store at
//! once: a lease file next to the database (`<db>.lease`), holding the owning host, its pid, a
//! random owner id, and an expiry. `Store::open` in network mode takes the lease or fails with
//! [`Error::Leased`]; the owner renews it from a small background thread that stops (and releases
//! the lease) when the `Store` drops.
//!
//! **Acquiring is exclusive-create, not check-then-write.** Two hosts racing to take a fresh or
//! expired lease must not both succeed. `take_exclusive` writes the candidate lease to a unique
//! temp file, then [`std::fs::hard_link`]s it onto the lease path: `link` either creates that
//! name or fails with `AlreadyExists`, atomically, with no window between a check and a write —
//! including over NFS, where `link(2)` is the standard exclusive-create idiom precisely because a
//! direct `open(O_CREAT | O_EXCL)` on the final name is not reliably atomic across clients on
//! NFSv3.
//!
//! Taking over an expired or dead-owner lease is the one place a decision (is it actually
//! takeable?) and an action (clearing it) cannot be the same atomic step, so `clear_if_takeable`
//! never trusts a read enough to act on it unverified: it renames the file aside first — `rename`
//! is atomic regardless of content, so it captures *whatever is currently there*, not what an
//! earlier read decided — and only after that checks whether the capture (now isolated under a
//! unique name nothing else can touch) really was takeable. If a different racer's own fresh,
//! legitimate lease had landed in the gap between the original read and the rename, this second
//! check catches it, and the capture is put back (`hard_link`, so a third racer's lease that has
//! since filled the path is never clobbered) instead of being treated as cleared. See
//! `clear_if_takeable`'s own doc for the full reasoning; `lease::tests::many_racers_on_an_*`
//! exercise it directly. Dropping a `Store` releases its lease the same way, in reverse: rename
//! aside first, then delete it only once that capture confirms it still names us — never
//! read-then-delete, which has the same kind of TOCTOU window. See the crate README for the
//! residual races this still cannot close (clock skew, NFS attribute caching, a resumed suspended
//! process).
//!
//! **Renewal: a background thread, not a `renew()` the caller must call.** A `Store` is meant to
//! be opened once and used; making every caller remember to renew on a timer would be easy to
//! forget and awkward to fit into an async or sync caller alike. The thread wakes every
//! `ttl / RENEW_FRACTION` (a third of the lease length, so a single slow or missed wakeup still
//! leaves two more tries before the lease would actually expire), checks the file still names our
//! `owner`, and only then writes a fresh expiry. If the owner ever does not match — another host
//! took over because our clock stalled, we were suspended, or the file was removed — the thread
//! sets a flag and stops; every append after that fails with [`Error::LeaseLost`] instead of
//! silently writing past a lease we no longer hold. Renewal overwrites unconditionally
//! (`write_lease`, a plain rename-into-place): unlike acquiring, it is refreshing a lease this
//! process already holds (just confirmed by the owner check), not racing anyone for it.
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

        // `take_exclusive` already confirms its own win before returning (narrowing the window a
        // straggler acting on a stale read could exploit to two adjacent syscalls — see its doc),
        // but under enough contention a displacement can still land in that narrow gap. Confirm
        // once more here, from a completely separate read, and retry the whole acquisition — not
        // just fail — if it did: from this caller's point of view that is still contention to
        // resolve, same as any other `AlreadyExists`, not a reason to give up.
        //
        // A `Leased` from `take_exclusive` is ordinarily reported immediately: it means a live
        // owner holds it, full stop, and the caller can act on `until`. But once *this* call has
        // already seen itself displaced at least once, it is itself part of an active shuffle —
        // whoever displaced it may not be stable either — so a `Leased` seen right after is only
        // a snapshot mid-shuffle, not necessarily the final word; keep retrying within the bound
        // instead of taking that snapshot as definitive.
        const MAX_DISPLACEMENT_RETRIES: u32 = 32;
        let mut verified = false;
        let mut displaced_at_least_once = false;
        for attempt in 0..MAX_DISPLACEMENT_RETRIES {
            match take_exclusive(&path, &host, pid, &owner, clock.as_ref(), ttl_ms) {
                Ok(()) if read_lease(&path).is_some_and(|l| l.owner == owner) => {
                    verified = true;
                    break;
                }
                Ok(()) => displaced_at_least_once = true,
                Err(_) if displaced_at_least_once => {}
                Err(e) => return Err(e),
            }
            backoff(attempt);
        }
        if !verified {
            return Err(Error::LeaseIo(std::io::Error::other(
                "lease verification kept failing after acquiring it",
            )));
        }

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
        release(&self.path, &self.owner);
    }
}

/// Releases the lease at `path` if it still names `owner`, without a read-then-delete race: a
/// plain `read_lease` followed by `remove_file` could delete another host's lease if it took over
/// between the two calls. Instead, rename the file aside first (atomic, and does not care what it
/// names — there is no window to race in) and only then look at what was captured:
/// - names `owner`: it is ours; delete the aside copy. The lease is released.
/// - names someone else, or does not parse: another host already took over since our last check.
///   Put it back, best effort, so their lease is not lost; if that now fails too (a third host
///   has since done the same to them), there is nothing more we can safely do.
/// - nothing to rename (already gone): nothing to release.
fn release(path: &Path, owner: &str) {
    let aside = path.with_file_name(format!("{}.drop.{}", file_name(path), ulid::Ulid::new()));
    if std::fs::rename(path, &aside).is_err() {
        return;
    }
    match read_lease(&aside) {
        Some(l) if l.owner == owner => {
            let _ = std::fs::remove_file(&aside);
        }
        _ => {
            let _ = std::fs::rename(&aside, path);
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

/// How many times [`take_exclusive`] retries after losing a benign race (another racer cleared
/// the same stale file, won a fresh create first, or this attempt had to undo a clear that turned
/// out to have captured something live — see [`clear_if_takeable`]). Each case resolves in a few
/// iterations at most, so this is generous headroom, not a tuning knob.
const MAX_ACQUIRE_ATTEMPTS: u32 = 64;

/// A short, jittered pause before [`take_exclusive`] retries. Same idea as `set_journal_mode`'s
/// retry jitter in `store.rs`: several racers retrying in perfect lockstep (every thread reacting
/// to the same event at the same instant) can keep re-displacing whichever one wins next,
/// indefinitely, when they are busy-looping with no delay between attempts at all; a random
/// pause, growing with the attempt number up to a few milliseconds, spreads retries out so one of
/// them lands in a quiet moment instead. Milliseconds, not microseconds, because the point is to
/// outrun ordinary OS scheduling delays under load, which are themselves millisecond-scale.
fn backoff(attempt: u32) {
    let jitter_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() % 1_000_000);
    let micros = u64::from(jitter_ns / 1_000) + u64::from(attempt.min(16)) * 300;
    std::thread::sleep(Duration::from_micros(micros));
}

/// Exclusively creates the lease file at `path`, naming `(host, pid, owner)`, taking over an
/// expired or (same-host) dead-owner lease first if one is in the way. See the module docs for
/// why this is `hard_link`-based exclusive-create plus rename-aside, not check-then-write.
///
/// `create_exclusive` returning `Ok` means *this call* momentarily created the name — not that it
/// still does. [`clear_if_takeable`] closes the gap between deciding a stale entry is takeable and
/// acting on that decision for the racer clearing it, but a straggler that read the *old* content
/// before we won, and only reaches its own clear-and-verify sequence afterward, can still displace
/// what we just created in the instant before we confirm it (that straggler's own verification
/// then fails and it retries, so this self-corrects — but only after the damage). So treat
/// `create_exclusive`'s `Ok` as provisional and read the name straight back before trusting it:
/// this narrows the window from "a whole read-decide-rename sequence" to two adjacent syscalls,
/// as tight as POSIX's primitives allow without a true compare-and-swap rename.
///
/// # Errors
///
/// [`Error::Leased`] if a live owner holds it; [`Error::LeaseIo`] for filesystem errors, or if
/// contention never lets this resolve within [`MAX_ACQUIRE_ATTEMPTS`] (a sign something is
/// pathological, e.g. a filesystem where renames never succeed, not an expected outcome).
fn take_exclusive(
    path: &Path,
    host: &str,
    pid: u32,
    owner: &str,
    clock: &dyn Clock,
    ttl_ms: i64,
) -> Result<()> {
    for attempt in 0..MAX_ACQUIRE_ATTEMPTS {
        let now_ms = clock.now_ms();
        let data = LeaseData {
            host: host.to_owned(),
            pid,
            owner: owner.to_owned(),
            until_ms: now_ms + ttl_ms,
        };
        match create_exclusive(path, &data) {
            Ok(()) if read_lease(path).is_some_and(|l| l.owner == owner) => return Ok(()),
            // Won the create, but lost the name again before this very next read: a straggler
            // displaced it (see this function's doc). Loop back and compete for it afresh rather
            // than believing a win that is already gone. Jittered backoff first: without it,
            // several racers retrying in lockstep can keep re-displacing whoever wins next,
            // instead of one attempt finally landing in a quiet moment.
            Ok(()) => backoff(attempt),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match clear_if_takeable(path, host, owner, now_ms, ttl_ms)? {
                    // Cleared it ourselves, or someone else changed it first (cleared it, took
                    // over, or removed it): either way, retry the create and re-read current
                    // state then.
                    Clear::Cleared | Clear::Retry => backoff(attempt),
                    Clear::Refused(e) => return Err(e),
                }
            }
            Err(e) => return Err(Error::LeaseIo(e)),
        }
    }
    Err(Error::LeaseIo(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "lease: too much contention acquiring it",
    )))
}

/// What [`clear_if_takeable`] found.
enum Clear {
    /// The stale lease was renamed aside; the path should now be free for a retried create.
    Cleared,
    /// State changed under us (the file was cleared or removed by someone else, or is already
    /// gone); retry the create and re-evaluate, rather than treating this as a refusal.
    Retry,
    /// A live, unexpired lease: not ours to take.
    Refused(Error),
}

/// What reading the lease file says about whether it may be taken over right now.
enum ReadDecision {
    /// Nothing is there.
    Gone,
    /// Expired, or (same host) a dead pid: free to take.
    Takeable,
    /// A live owner holds it.
    Live(Error),
}

/// Reads `path` and runs the pure [`decide`] rules against it. Content and mtime come from one
/// open handle, so they describe the same moment: two separate calls (`read` then `metadata`, or
/// the reverse) could straddle another write to this path.
fn read_decision(path: &Path, host: &str, now_ms: i64, ttl_ms: i64) -> Result<ReadDecision> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ReadDecision::Gone),
        Err(e) => return Err(Error::LeaseIo(e)),
    };
    let meta = file.metadata().map_err(Error::LeaseIo)?;
    let mtime_ms = mtime_ms(&meta);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(Error::LeaseIo)?;
    drop(file);
    let existing: Option<LeaseData> = serde_json::from_slice(&bytes).ok();

    let decision = match &existing {
        Some(existing) => {
            let same_host_dead = existing.host == host && !pid_alive(existing.pid);
            decide(Some(existing), None, now_ms, ttl_ms, same_host_dead)
        }
        // Torn or garbage: not a lease we can name an owner for, so only mtime says whether it
        // might still be live.
        None => decide(None, Some(mtime_ms), now_ms, ttl_ms, false),
    };
    Ok(match decision {
        Ok(()) => ReadDecision::Takeable,
        Err((host, pid, until)) => ReadDecision::Live(Error::Leased { host, pid, until }),
    })
}

/// Called after `create_exclusive` found `path` already occupied: decides whether the lease there
/// may be taken over, and if so, clears it.
///
/// **Never decide-then-rename on the read alone**: `path` can change between that read and a
/// later rename, including to another racer's own fresh, legitimate lease landing in the gap (its
/// `create_exclusive` succeeding right after we read the old, expired content) — renaming *then*
/// would steal that racer's lease, not clear a stale one, and both racers would end up thinking
/// they hold it. So a first, read-only check answers the common case cheaply and refuses at once
/// without touching the file; only when it says "takeable" do we act, and even then the rename
/// captures *whatever is currently there* (not what we decided about), and a second read —
/// against the now-isolated capture, which nothing else can touch — confirms it is still what we
/// thought before treating it as cleared. If that second read disagrees, we put it back with the
/// same exclusive-create primitive acquiring uses (`hard_link`, not `rename`), so a third racer's
/// own fresh lease that has since filled `path` is never clobbered — only a `path` that is still
/// empty accepts the restore — and retry from scratch rather than act on a decision already known
/// to be stale.
fn clear_if_takeable(
    path: &Path,
    host: &str,
    owner: &str,
    now_ms: i64,
    ttl_ms: i64,
) -> Result<Clear> {
    match read_decision(path, host, now_ms, ttl_ms)? {
        ReadDecision::Gone => return Ok(Clear::Retry),
        ReadDecision::Live(err) => return Ok(Clear::Refused(err)),
        ReadDecision::Takeable => {}
    }

    let aside = path.with_file_name(format!(
        "{}.stale.{owner}.{}",
        file_name(path),
        ulid::Ulid::new()
    ));
    match std::fs::rename(path, &aside) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Clear::Retry),
        Err(e) => return Err(Error::LeaseIo(e)),
    }

    match read_decision(&aside, host, now_ms, ttl_ms)? {
        // Confirmed against the isolated capture: genuinely takeable (or, impossibly, already
        // gone — we just created this name). Either way, nothing left to protect.
        ReadDecision::Takeable | ReadDecision::Gone => {
            let _ = std::fs::remove_file(&aside);
            Ok(Clear::Cleared)
        }
        // Our first read was stale: this is actually live now (another racer's create landed in
        // the gap). Put it back if `path` is still empty; if a third racer has since filled it,
        // leave our capture to be dropped — their lease, not ours to touch.
        ReadDecision::Live(_) => {
            let _ = std::fs::hard_link(&aside, path);
            let _ = std::fs::remove_file(&aside);
            Ok(Clear::Retry)
        }
    }
}

/// A file's mtime in milliseconds since the Unix epoch, or 0 if it cannot be read.
fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The pure decision `clear_if_takeable` and the renewal thread's takeover checks are built on:
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

/// Writes `data` as JSON to a fresh, uniquely-named temp file next to `path` (so a later link or
/// rename lands on the same filesystem) and returns its path. Mode 0600 on Unix; on Windows the
/// file inherits its directory's ACL (as `pitcrew-auth`'s private files do), so callers should
/// keep the store itself under a directory only the owning user can open. The caller links or
/// renames this into place and is responsible for removing the temp name afterward either way.
fn write_candidate(path: &Path, data: &LeaseData) -> std::io::Result<PathBuf> {
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
    Ok(tmp)
}

/// Overwrites the lease at `path` unconditionally (a rename lands on whatever is there, or
/// creates it if nothing is). Only for the renewal thread refreshing a lease it has just
/// confirmed (by the owner check in `renew_loop`) it already holds — **never** for acquiring one:
/// here, unlike [`create_exclusive`], a rename onto a fresh lease someone else just took would
/// silently steal it.
fn write_lease(path: &Path, data: &LeaseData) -> std::io::Result<()> {
    let tmp = write_candidate(path, data)?;
    std::fs::rename(&tmp, path)
}

/// Exclusively creates the lease file at `path` with `data`: write the candidate to a unique temp
/// file, then [`std::fs::hard_link`] it onto `path`. `hard_link` creates the destination name or
/// fails with `AlreadyExists`, atomically and with no check-then-write window — including over
/// NFS, where this (not a direct `open(O_CREAT | O_EXCL)` on the final name, unreliable across
/// clients on NFSv3) is the standard exclusive-create idiom. On success the lease now lives at
/// both names; the temp name is removed either way, since it served only to get the content onto
/// disk before the atomic step.
fn create_exclusive(path: &Path, data: &LeaseData) -> std::io::Result<()> {
    let tmp = write_candidate(path, data)?;
    let result = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    result
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

    /// A clock that never advances, for the exclusive-create and race tests below: they do not
    /// want a lease to expire mid-test from real elapsed time.
    #[derive(Debug)]
    struct FixedClock(i64);

    impl Clock for FixedClock {
        fn now_ms(&self) -> i64 {
            self.0
        }
    }

    #[test]
    fn take_exclusive_succeeds_on_a_free_path_and_refuses_a_live_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.db.lease");
        let clock = FixedClock(1_000);
        take_exclusive(&path, "host-a", 1, "owner-a", &clock, 60_000).expect("free path");
        assert_eq!(read_lease(&path).expect("parses").owner, "owner-a");

        let err = take_exclusive(&path, "host-b", 2, "owner-b", &clock, 60_000)
            .expect_err("a live lease must refuse");
        assert!(matches!(err, Error::Leased { .. }), "{err:?}");
        // The refused attempt must not have touched the held lease.
        assert_eq!(read_lease(&path).expect("still parses").owner, "owner-a");
    }

    #[test]
    fn many_racers_on_a_fresh_path_exactly_one_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.db.lease");
        let clock = FixedClock(1_000);
        const N: usize = 8;
        let wins: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..N)
                .map(|i| {
                    let path = path.clone();
                    let clock = &clock;
                    scope.spawn(move || {
                        let host = format!("host-{i}");
                        let owner = format!("owner-{i}");
                        take_exclusive(
                            &path,
                            &host,
                            1000 + u32::try_from(i).unwrap(),
                            &owner,
                            clock,
                            60_000,
                        )
                        .is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("thread"))
                .filter(|&ok| ok)
                .count()
        });
        assert_eq!(wins, 1, "exactly one racer should win a fresh path");
    }

    /// One round of `N` threads racing `LeaseGuard::acquire` on the same (already-seeded) path.
    /// Returns how many won (`Ok`) and the full set of results.
    fn race_acquire(db_path: &Path, clock: &Arc<dyn Clock>, n: usize) -> Vec<Result<LeaseGuard>> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..n)
                .map(|_| {
                    let db_path = db_path.to_path_buf();
                    let clock = Arc::clone(clock);
                    scope.spawn(move || {
                        LeaseGuard::acquire(&db_path, clock, Duration::from_secs(60))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("thread"))
                .collect()
        })
    }

    #[test]
    fn many_racers_on_an_expired_lease_exactly_one_wins() {
        // Through `LeaseGuard::acquire`, not bare `take_exclusive`: taking over an expired lease
        // is the one case where a straggler, acting on a read that was correct when it made the
        // read but stale by the time it acts, can momentarily displace an already-confirmed
        // winner (see `take_exclusive`'s doc) — `acquire` is what actually guards against that
        // (its own re-check-and-retry, layered on `take_exclusive`'s own narrowed window), and it
        // is what `Store::open` really calls, so this is the guarantee that matters end to end.
        //
        // Two different properties, checked differently: **safety** — never more than one
        // racer wins — is non-negotiable and asserted on every single attempt, no retries. Two
        // winners would mean two hosts believe they hold one database's lease at once, which is
        // exactly what this whole mechanism exists to prevent. **Liveness** — at least one racer
        // *does* win — is checked too, but a single attempt failing it is not treated as a bug by
        // itself: `N` racers with zero network latency between them, all reacting to the exact
        // same seeded file at once, synchronize far more tightly than real, separate hosts ever
        // would (real ones have actual latency between their syscalls, which is what settles a
        // real race quickly). So a round with zero winners is retried with a fresh seed, up to a
        // bound, before treating it as a genuine problem — the retried rounds still each assert
        // the safety property in full.
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        let path = lease_path(&db_path);
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(1_000_000));
        const N: usize = 4;
        const ROUNDS: u32 = 5;
        const LIVENESS_RETRIES_PER_ROUND: u32 = 5;
        for round in 0..ROUNDS {
            let mut wins = 0;
            let mut results = Vec::new();
            for liveness_attempt in 0..LIVENESS_RETRIES_PER_ROUND {
                std::fs::write(
                    &path,
                    serde_json::to_vec(&lease("stale-host", 999_999, 500)).expect("encode"),
                )
                .expect("seed stale lease");
                results = race_acquire(&db_path, &clock, N);
                wins = results.iter().filter(|r| r.is_ok()).count();
                // Safety: checked on every attempt, not just the one that ends the loop.
                assert!(
                    wins <= 1,
                    "round {round} attempt {liveness_attempt}: at most one racer may ever win, \
                     got {wins}"
                );
                for r in &results {
                    if let Err(e) = r {
                        // Ordinarily `Error::Leased`; `Error::LeaseIo` only if a racer exhausted
                        // its own retries against repeated displacement. Either way: this racer
                        // did not win — never a database error reaching past the lease.
                        assert!(
                            matches!(e, Error::Leased { .. } | Error::LeaseIo(_)),
                            "round {round} attempt {liveness_attempt}: {e:?}"
                        );
                    }
                }
                if wins == 1 {
                    break;
                }
                // `wins == 0`: nothing held to clean up. The next iteration's `results = ...`
                // reassignment (or the loop ending) drops this attempt's (all-`Err`) results.
            }
            assert_eq!(
                wins, 1,
                "round {round}: no racer won an expired lease within \
                 {LIVENESS_RETRIES_PER_ROUND} attempts"
            );
            // Dropping the winner here (end of the round) stops its renewal thread and releases
            // the lease before the next round reseeds it.
            drop(results);
            // Every racer that cleared the stale file aside removes its own unique aside name
            // right after (see `clear_if_takeable`), and a racer that had to put a capture back
            // removes it too: none should be left behind.
            let leftovers: Vec<_> = std::fs::read_dir(dir.path())
                .expect("read_dir")
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".stale."))
                .collect();
            assert!(leftovers.is_empty(), "round {round}: {leftovers:?}");
        }
    }

    #[test]
    fn release_deletes_a_lease_that_still_names_us() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.db.lease");
        let clock = FixedClock(1_000);
        take_exclusive(&path, "host-a", 1, "owner-a", &clock, 60_000).expect("acquire");
        release(&path, "owner-a");
        assert!(
            !path.exists(),
            "release must remove a lease that is still ours"
        );
    }

    #[test]
    fn release_restores_a_lease_that_now_names_someone_else() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.db.lease");
        // As if another host took over between our last check and this `release` call.
        std::fs::write(
            &path,
            serde_json::to_vec(&lease("other-host", 2, 999_999)).expect("encode"),
        )
        .expect("seed");
        release(&path, "owner-a"); // not the owner named in the file
        let remaining =
            read_lease(&path).expect("release must not delete a lease naming someone else");
        assert_eq!(remaining.host, "other-host");
    }

    #[test]
    fn release_of_an_already_gone_lease_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.db.lease");
        release(&path, "owner-a"); // nothing there at all
        assert!(!path.exists());
    }
}
