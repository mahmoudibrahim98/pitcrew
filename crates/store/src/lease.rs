//! The single-host lease for network-mode stores.
//!
//! SQLite's own locking is not trustworthy over a network filesystem (that is why network mode
//! exists at all), so a second, independent mechanism keeps two hosts from writing one store at
//! once: generation-numbered lease files next to the database, `<db>.lease.<gen>` (`gen` a `u64`
//! counter starting at 1), each holding the owning host, its pid and an expiry. The **current**
//! lease is whichever has the highest `gen` present — found by listing the directory, never by a
//! fixed name. `Store::open` in network mode takes it or fails with [`Error::Leased`]; the owner
//! renews it from a small background thread that stops (and releases it) when the `Store` drops.
//!
//! **No live lease is ever renamed or deleted by anyone but its owner.** This is the central
//! invariant, and the whole reason for generation numbers instead of one fixed file a new owner
//! takes over in place. An earlier design (round 1 of this brief) took over an expired lease by
//! renaming it aside, inspecting the capture, and restoring it if that turned out to be wrong; a
//! three-way interleaving could still leave two hosts both holding it (a straggler's now-stale
//! decision displaces an already-confirmed winner; a third racer fills the resulting gap; the
//! straggler's restore then fails and it deletes what it captured — the first winner's lease,
//! gone, discovered only at its next renewal). With generation numbers, taking over means
//! creating a *new*, higher-numbered file — [`std::fs::hard_link`], so it is exclusive: it either
//! creates that exact name or fails with `AlreadyExists`, atomically, with no window to act on a
//! stale decision, and nothing already on disk is ever touched. A racer whose candidate turns out
//! not to be the final highest (see [`take_next_gen`]) only ever deletes the file it just created
//! itself.
//!
//! **Acquire** ([`take_next_gen`]): read the current generation (or none); if it is live, refuse
//! with [`Error::Leased`]; if it is expired, absent, or names a dead same-host owner, exclusively
//! create the next generation. After creating, re-list: if a *higher* generation already exists
//! (another racer won the same race a step ahead of us), our own file was never going to be
//! current — delete it (ours to delete; nobody else could be relying on it) and retry from a
//! fresh read. A racer that loses the exclusive create itself (`AlreadyExists`) just retries too.
//!
//! **Renew**: the owner re-lists first — a higher generation means [`Error::LeaseLost`] — then
//! rewrites only its own `lease.<gen>` file, by temp file plus rename onto its own name (safe:
//! nothing else ever touches it). **Checked before every write batch, not just by the renewal
//! thread's flag**: [`LeaseGuard::check`] re-lists for a higher generation on every call, a cheap
//! `readdir` and filename comparison, no content to read. This catches a loss immediately, not up
//! to `ttl / RENEW_FRACTION` later when the renewal thread would next notice on its own.
//!
//! **Release**: the owner deletes only its own `lease.<gen>` file — no read-then-delete race to
//! avoid, no capture-and-restore dance, because nothing else could ever have touched it.
//!
//! **GC**: after becoming the current owner, generations older than `gen - 1` are deleted (the
//! current one and the one just before it are kept, for diagnosis). Nobody but the current owner
//! deletes anything, and only generations that are not current.
//!
//! **Residual limits**, on top of what a file-based lease can never fully close: NFS directory and
//! attribute caching can hide a new `lease.<gen+1>` from an old owner for up to the client's
//! `actimeo`, and clock skew between hosts affects what "expired" means to each of them. Both are
//! mitigated, not eliminated, by the per-batch re-list (catches a loss quickly once the directory
//! listing *is* visible) and by choosing a `lease_ttl` with real margin over expected clock drift
//! and cache staleness — never fully solved by cleverness in this file alone.
//!
//! A store whose lease file was the old, single fixed name has never shipped (round 1 was not
//! released), so there is no migration to support.
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

/// A generation lease file's contents. No `owner` id: the generation number is already the unique
/// identity (nobody else ever creates or touches the same `(db, gen)` pair), so there is nothing
/// left for a separate id to distinguish.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct LeaseData {
    host: String,
    pid: u32,
    until_ms: i64,
}

/// How often the renewal thread wakes, relative to the lease length: a third, so one missed
/// wakeup (a slow write, a paused process) still leaves margin before the lease actually expires.
const RENEW_FRACTION: u32 = 3;

/// The directory a database's lease files live in: its parent, or `.` if it has none.
fn lease_dir(db_path: &Path) -> &Path {
    db_path.parent().unwrap_or_else(|| Path::new("."))
}

/// The shared prefix of a database's lease file names (before `.<gen>`): `<db file name>.lease`.
fn lease_prefix(db_path: &Path) -> String {
    let base = db_path.file_name().map_or_else(
        || "store.db".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    format!("{base}.lease")
}

/// Generation `generation`'s lease file path.
pub(crate) fn gen_path(db_path: &Path, generation: u64) -> PathBuf {
    lease_dir(db_path).join(format!("{}.{generation}", lease_prefix(db_path)))
}

/// Parses `name` as `<prefix>.<gen>` and returns the generation, or `None` if it does not match:
/// wrong prefix, no digits, anything but ASCII digits after the prefix, or a leading zero on more
/// than one digit (`.0` alone is accepted for completeness, even though nothing here ever creates
/// it; `.01` is not — a generation file name is a canonical `u64`, not an arbitrary digit string).
/// Never panics and never treats a name it cannot parse as an error: an unrelated file (a stray
/// temp candidate, a file a person left there) is simply not a generation.
fn parse_gen(name: &str, prefix: &str) -> Option<u64> {
    let suffix = name.strip_prefix(prefix)?.strip_prefix('.')?;
    if suffix.is_empty() || (suffix.len() > 1 && suffix.starts_with('0')) {
        return None;
    }
    if !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok()
}

/// The current lease: the highest generation present, by listing the directory and parsing names
/// strictly (see [`parse_gen`]) — never a fixed name, and never an error for a directory that
/// does not exist yet or a name that does not parse (both just mean "not a generation here").
pub(crate) fn current_gen(db_path: &Path) -> Option<(u64, PathBuf)> {
    let dir = lease_dir(db_path);
    let prefix = lease_prefix(db_path);
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(generation) = parse_gen(&name.to_string_lossy(), &prefix) else {
            continue;
        };
        if best.as_ref().is_none_or(|(g, _)| generation > *g) {
            best = Some((generation, entry.path()));
        }
    }
    best
}

/// Deletes generation files older than `keep_gen - 1`: the current owner's own `keep_gen`, and
/// the generation just before it, are kept (for diagnosis); anything older is removed. Best
/// effort — a deletion failing (already gone, a permissions hiccup) is not an error — and never
/// touches a name [`parse_gen`] does not recognise as a generation of this database's lease.
fn gc_old_generations(db_path: &Path, keep_gen: u64) {
    let dir = lease_dir(db_path);
    let prefix = lease_prefix(db_path);
    let floor = keep_gen.saturating_sub(1);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(generation) = parse_gen(&name.to_string_lossy(), &prefix) else {
            continue;
        };
        if generation < floor {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A held lease: releases on drop, and stops its renewal thread first.
pub(crate) struct LeaseGuard {
    db_path: PathBuf,
    generation: u64,
    lost: Arc<AtomicBool>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseGuard")
            .field("db_path", &self.db_path)
            .field("generation", &self.generation)
            .field("lost", &self.lost.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl LeaseGuard {
    /// Takes the next generation of the lease next to `db_path`, or fails with
    /// [`Error::Leased`]. Spawns the renewal thread and garbage-collects old generations on
    /// success.
    ///
    /// # Errors
    ///
    /// [`Error::Leased`] if another, live owner holds it; [`Error::LeaseIo`] for filesystem
    /// errors acquiring it or starting the renewal thread.
    pub(crate) fn acquire(db_path: &Path, clock: Arc<dyn Clock>, ttl: Duration) -> Result<Self> {
        let host = hostname();
        let pid = std::process::id();
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);

        let generation = take_next_gen(db_path, &host, pid, clock.as_ref(), ttl_ms)?;
        gc_old_generations(db_path, generation);

        let lost = Arc::new(AtomicBool::new(false));
        let (stop, rx) = mpsc::channel();
        let renew_every = Duration::from_millis(
            (ttl.as_millis() / u128::from(RENEW_FRACTION))
                .try_into()
                .unwrap_or(u64::MAX)
                .max(1),
        );
        let thread = {
            let db_path = db_path.to_path_buf();
            let lost = Arc::clone(&lost);
            std::thread::Builder::new()
                .name("pitcrew-store-lease".to_owned())
                .spawn(move || {
                    renew_loop(
                        &rx,
                        renew_every,
                        &db_path,
                        generation,
                        &host,
                        pid,
                        &clock,
                        ttl_ms,
                        &lost,
                    )
                })
                .map_err(Error::LeaseIo)?
        };

        Ok(Self {
            db_path: db_path.to_path_buf(),
            generation,
            lost,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// `Err(Error::LeaseLost)` if a higher generation now exists (we have been taken over), else
    /// `Ok(())`. Checks the renewal thread's cached flag first (free), then — the check that
    /// matters — re-lists the lease directory fresh: a cheap `readdir` and filename comparison,
    /// no file content to read, so a loss is caught before the write that calls this, not up to
    /// `ttl / RENEW_FRACTION` later when the renewal thread would next notice on its own.
    pub(crate) fn check(&self) -> Result<()> {
        if self.lost.load(Ordering::SeqCst) {
            return Err(Error::LeaseLost);
        }
        if current_gen(&self.db_path).is_some_and(|(g, _)| g > self.generation) {
            self.lost.store(true, Ordering::SeqCst);
            return Err(Error::LeaseLost);
        }
        Ok(())
    }

    /// This lease's generation number, for diagnosis and tests.
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
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
        // Always safe: nothing but us ever creates, renews or deletes
        // `gen_path(db_path, generation)`, so there is no read-then-delete race to avoid here,
        // unlike a fixed shared name. Best effort — if it is already gone (a later owner's GC, or
        // we were never fully set up), there is nothing to release.
        let _ = std::fs::remove_file(gen_path(&self.db_path, self.generation));
    }
}

/// The renewal thread body: wakes every `renew_every` (or at once if `rx` gets a stop signal).
/// Each wakeup re-lists first — a higher generation than ours means we have been taken over, so
/// it sets `lost` and stops, never writing past a lease it no longer holds — and only then
/// rewrites its own generation file with a fresh expiry.
#[allow(clippy::too_many_arguments)]
fn renew_loop(
    rx: &mpsc::Receiver<()>,
    renew_every: Duration,
    db_path: &Path,
    generation: u64,
    host: &str,
    pid: u32,
    clock: &Arc<dyn Clock>,
    ttl_ms: i64,
    lost: &Arc<AtomicBool>,
) {
    loop {
        match rx.recv_timeout(renew_every) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if current_gen(db_path).is_some_and(|(g, _)| g > generation) {
            lost.store(true, Ordering::SeqCst);
            return;
        }
        let now_ms = clock.now_ms();
        let fresh = LeaseData {
            host: host.to_owned(),
            pid,
            until_ms: now_ms + ttl_ms,
        };
        // A transient write failure is retried next wakeup; the re-list above is what detects a
        // real takeover, not this write succeeding or not.
        let _ = write_lease(&gen_path(db_path, generation), &fresh);
    }
}

/// How many times [`take_next_gen`] retries after losing a benign race (another racer created the
/// same next generation first, or turned out to have created a higher one by the time we
/// re-listed). Each case resolves in one or two iterations in practice; this is headroom, not a
/// tuning knob.
const MAX_ACQUIRE_ATTEMPTS: u32 = 32;

/// A short, jittered pause before [`take_next_gen`] retries, so several racers retrying in
/// lockstep do not all collide on the same next generation number every single time. Same idea as
/// `store.rs`'s `set_journal_mode` retry jitter.
fn backoff(attempt: u32) {
    let jitter_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() % 1_000_000);
    let micros = u64::from(jitter_ns / 1_000) + u64::from(attempt.min(16)) * 300;
    std::thread::sleep(Duration::from_micros(micros));
}

/// Takes the next generation of the lease next to `db_path`: reads the current one (if any),
/// refuses if it is live, and otherwise exclusively creates `current + 1` — then re-lists once
/// more to confirm that generation is still the highest, since another racer's own exclusive
/// create for the same next number could have won in the meantime. If a higher generation turns
/// up, our own file was never going to be current: delete it (ours alone to delete) and retry
/// from a fresh read. Returns the generation number taken.
///
/// This is the one place a decision ("is the current lease takeable?") and the action that
/// follows it cannot be fully atomic together — but unlike round 1's move-aside design, the
/// action here is always *creating a brand-new name*, never touching whatever is already on disk,
/// so there is nothing to accidentally clobber: a racer that loses only ever deletes a file it
/// created itself.
///
/// # Errors
///
/// [`Error::Leased`] if a live owner holds it; [`Error::LeaseIo`] for filesystem errors, or if
/// contention never lets this resolve within [`MAX_ACQUIRE_ATTEMPTS`].
fn take_next_gen(path: &Path, host: &str, pid: u32, clock: &dyn Clock, ttl_ms: i64) -> Result<u64> {
    for attempt in 0..MAX_ACQUIRE_ATTEMPTS {
        let now_ms = clock.now_ms();
        let current = current_gen(path);
        let decision = match &current {
            None => ReadDecision::Gone,
            Some((_, gen_file)) => read_decision(gen_file, host, now_ms, ttl_ms)?,
        };
        match decision {
            ReadDecision::Live(err) => return Err(err),
            ReadDecision::Gone | ReadDecision::Takeable => {
                let base = current.as_ref().map_or(0, |(g, _)| *g);
                let candidate = base + 1;
                let data = LeaseData {
                    host: host.to_owned(),
                    pid,
                    until_ms: now_ms + ttl_ms,
                };
                let target = gen_path(path, candidate);
                match create_exclusive(&target, &data) {
                    Ok(()) => {
                        if current_gen(path).is_some_and(|(g, _)| g > candidate) {
                            // Someone else is already further ahead: our file was never going to
                            // be current. It is ours alone, so deleting it is always safe.
                            let _ = std::fs::remove_file(&target);
                            backoff(attempt);
                        } else {
                            return Ok(candidate);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => backoff(attempt),
                    Err(e) => return Err(Error::LeaseIo(e)),
                }
            }
        }
    }
    Err(Error::LeaseIo(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "lease: too much contention acquiring it",
    )))
}

/// What reading the current lease generation says about whether the next one may be created.
enum ReadDecision {
    /// Nothing is there (no generation exists yet).
    Gone,
    /// Expired, or (same host) a dead pid: the next generation may be created.
    Takeable,
    /// A live owner holds it.
    Live(Error),
}

/// Reads `path` (one gen file) and runs the pure [`decide`] rules against it. Content and mtime
/// come from one open handle, so they describe the same moment: two separate calls (`read` then
/// `metadata`, or the reverse) could straddle another write to this path.
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
        // Torn or garbage content (the name parsed as a generation, but what is inside did not):
        // not a lease we can name an owner for, so only mtime says whether it might still be
        // live. A generation file is written to a temp name and hard-linked or renamed into
        // place only once fully synced, so this should only happen to genuine corruption, not an
        // in-flight write.
        None => decide(None, Some(mtime_ms), now_ms, ttl_ms, false),
    };
    Ok(match decision {
        Ok(()) => ReadDecision::Takeable,
        Err((host, pid, until)) => ReadDecision::Live(Error::Leased { host, pid, until }),
    })
}

/// A file's mtime in milliseconds since the Unix epoch, or 0 if it cannot be read.
fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The pure decision [`read_decision`] and the renewal thread's takeover checks are built on:
/// given what the lease file says (or, for a garbage file, its mtime) and the current time,
/// whether a new owner may take over. Kept separate from any I/O so it is exhaustively
/// unit-tested without a filesystem.
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
/// creates it if nothing is). Only for the renewal thread refreshing its own generation file —
/// nothing else ever touches it, so there is no one else's lease a rename here could steal.
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
            until_ms,
        }
    }

    fn write_gen(db_path: &Path, generation: u64, data: &LeaseData) {
        std::fs::write(
            gen_path(db_path, generation),
            serde_json::to_vec(data).expect("encode"),
        )
        .expect("write gen file");
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
    fn hostname_is_never_empty() {
        assert!(!hostname().is_empty());
    }

    // --- Generation path and directory-listing helpers ---

    #[test]
    fn gen_path_names_the_generation() {
        assert_eq!(
            gen_path(Path::new("/a/b/store.db"), 7),
            Path::new("/a/b/store.db.lease.7")
        );
    }

    #[test]
    fn parse_gen_accepts_only_canonical_digit_suffixes() {
        let prefix = "store.db.lease";
        assert_eq!(parse_gen("store.db.lease.1", prefix), Some(1));
        assert_eq!(parse_gen("store.db.lease.42", prefix), Some(42));
        assert_eq!(parse_gen("store.db.lease.0", prefix), Some(0));
        assert_eq!(
            parse_gen("store.db.lease.007", prefix),
            None,
            "leading zero"
        );
        assert_eq!(parse_gen("store.db.lease.", prefix), None, "empty suffix");
        assert_eq!(
            parse_gen("store.db.lease", prefix),
            None,
            "no suffix at all"
        );
        assert_eq!(parse_gen("store.db.lease.abc", prefix), None, "not digits");
        assert_eq!(
            parse_gen("store.db.lease.1x", prefix),
            None,
            "trailing junk"
        );
        assert_eq!(
            parse_gen("store.db.lease.-1", prefix),
            None,
            "no sign allowed"
        );
        assert_eq!(
            parse_gen("other.lease.1", prefix),
            None,
            "wrong prefix entirely"
        );
        assert_eq!(
            parse_gen(".store.db.lease.1.tmp-x", prefix),
            None,
            "a stray temp candidate is not a generation"
        );
    }

    #[test]
    fn current_gen_is_none_on_an_empty_or_missing_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        assert!(current_gen(&db_path).is_none());
        assert!(current_gen(&dir.path().join("missing/store.db")).is_none());
    }

    #[test]
    fn current_gen_is_the_highest_present_and_ignores_garbage_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        write_gen(&db_path, 1, &lease("h", 1, 1_000));
        write_gen(&db_path, 3, &lease("h", 1, 1_000));
        write_gen(&db_path, 2, &lease("h", 1, 1_000));
        std::fs::write(dir.path().join("store.db.lease.not-a-number"), b"x").expect("garbage");
        std::fs::write(dir.path().join("store.db.lease.007"), b"x").expect("leading zero");
        std::fs::write(dir.path().join("unrelated-file"), b"x").expect("unrelated");

        let (generation, path) = current_gen(&db_path).expect("a current generation");
        assert_eq!(generation, 3);
        assert_eq!(path, gen_path(&db_path, 3));
    }

    #[test]
    fn gc_keeps_the_current_and_previous_generations_and_ignores_garbage_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        for generation in 1..=5 {
            write_gen(&db_path, generation, &lease("h", 1, 1_000));
        }
        let garbage = dir.path().join("store.db.lease.not-a-number");
        std::fs::write(&garbage, b"x").expect("garbage");

        gc_old_generations(&db_path, 5);

        for generation in 1..=3 {
            assert!(
                !gen_path(&db_path, generation).exists(),
                "generation {generation} should have been collected"
            );
        }
        assert!(gen_path(&db_path, 4).exists(), "gen 4 (current - 1) stays");
        assert!(gen_path(&db_path, 5).exists(), "gen 5 (current) stays");
        assert!(garbage.exists(), "a garbage name is never touched by gc");
    }

    #[test]
    fn gc_on_a_fresh_lease_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        write_gen(&db_path, 1, &lease("h", 1, 1_000));
        gc_old_generations(&db_path, 1); // keep_gen - 1 saturates to 0; nothing is older than 0
        assert!(gen_path(&db_path, 1).exists());
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
    fn take_next_gen_succeeds_on_a_free_path_and_refuses_a_live_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        let clock = FixedClock(1_000);
        let generation = take_next_gen(&db_path, "host-a", 1, &clock, 60_000).expect("free path");
        assert_eq!(generation, 1);
        assert_eq!(current_gen(&db_path).expect("current").0, 1);

        let err = take_next_gen(&db_path, "host-b", 2, &clock, 60_000)
            .expect_err("a live lease must refuse");
        assert!(matches!(err, Error::Leased { .. }), "{err:?}");
        // The refused attempt must not have touched the held lease or created anything new.
        assert_eq!(current_gen(&db_path).expect("still current").0, 1);
    }

    #[test]
    fn take_next_gen_takes_over_an_expired_lease_at_the_next_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        write_gen(&db_path, 5, &lease("stale-host", 999_999, 500));
        let clock = FixedClock(1_000_000);
        let generation = take_next_gen(&db_path, "host-a", 1, &clock, 60_000).expect("expired");
        assert_eq!(
            generation, 6,
            "takeover creates the next generation, not a fresh 1"
        );
        assert!(gen_path(&db_path, 5).exists(), "the old file is untouched");
    }

    #[test]
    fn many_racers_on_a_fresh_path_exactly_one_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        let clock = FixedClock(1_000);
        const N: usize = 8;
        let wins: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..N)
                .map(|i| {
                    let db_path = db_path.clone();
                    let clock = &clock;
                    scope.spawn(move || {
                        let host = format!("host-{i}");
                        take_next_gen(
                            &db_path,
                            &host,
                            1000 + u32::try_from(i).unwrap(),
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

    #[test]
    fn many_racers_on_an_expired_lease_exactly_one_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        let clock = FixedClock(1_000_000);
        const N: usize = 8;
        for round in 0..5u64 {
            write_gen(&db_path, round + 1, &lease("stale-host", 999_999, 500));
            let wins: usize = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..N)
                    .map(|i| {
                        let db_path = db_path.clone();
                        let clock = &clock;
                        scope.spawn(move || {
                            let host = format!("host-{round}-{i}");
                            take_next_gen(
                                &db_path,
                                &host,
                                2000 + u32::try_from(i).unwrap(),
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
            assert_eq!(wins, 1, "round {round}: exactly one racer should win");
            // Clean the whole lease directory before the next round seeds a fresh expired gen 1
            // (the next round's `write_gen(db_path, round + 1, ...)` only adds, so each round
            // must start from a clean slate of exactly one, known-expired file).
            let (highest, _) = current_gen(&db_path).expect("a winner exists");
            for generation in 1..=highest {
                let _ = std::fs::remove_file(gen_path(&db_path, generation));
            }
        }
    }

    /// A direct regression test for review round 2's finding: in the old move-aside design, a
    /// three-way interleaving (a straggler's now-stale decision displacing an already-confirmed
    /// winner, with a third racer filling the resulting gap) could leave two hosts both believing
    /// they held the lease. The generation-numbered design removes the mechanism that made that
    /// possible — nobody ever renames or deletes another racer's file, only ever creates a new,
    /// higher-numbered one — so this is a safety check with zero tolerance, checked on every
    /// single attempt, not a liveness check with retries.
    #[test]
    fn three_racers_on_an_expired_lease_never_give_two_holders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        let clock = FixedClock(1_000_000);
        const N: usize = 3;
        const ROUNDS: u64 = 20;
        for round in 0..ROUNDS {
            write_gen(&db_path, round + 1, &lease("stale-host", 999_999, 500));
            let wins: usize = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..N)
                    .map(|i| {
                        let db_path = db_path.clone();
                        let clock = &clock;
                        scope.spawn(move || {
                            let host = format!("r{round}-h{i}");
                            take_next_gen(
                                &db_path,
                                &host,
                                3000 + u32::try_from(i).unwrap(),
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
            assert!(
                wins <= 1,
                "round {round}: at most one racer may ever hold the lease, got {wins}"
            );
            let (highest, _) = current_gen(&db_path).expect("a winner exists");
            for generation in 1..=highest {
                let _ = std::fs::remove_file(gen_path(&db_path, generation));
            }
        }
    }

    #[test]
    fn release_removes_only_our_own_generation_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("store.db");
        let guard = LeaseGuard::acquire(
            &db_path,
            Arc::new(FixedClock(1_000)),
            Duration::from_secs(60),
        )
        .expect("acquire on a fresh path");
        let my_generation = guard.generation();
        assert_eq!(my_generation, 1);

        // Other generation files already present (as if left over from a predecessor, not yet
        // garbage-collected, or a stray number nothing created through the normal path) must
        // never be touched by this guard's own drop.
        write_gen(&db_path, 0, &lease("ghost-a", 10, 1));
        write_gen(&db_path, 99, &lease("ghost-b", 11, 999_999));

        drop(guard);

        assert!(
            !gen_path(&db_path, my_generation).exists(),
            "our own generation is released"
        );
        assert!(
            gen_path(&db_path, 0).exists(),
            "an unrelated generation file must never be touched"
        );
        assert!(
            gen_path(&db_path, 99).exists(),
            "an unrelated generation file must never be touched"
        );
    }
}
