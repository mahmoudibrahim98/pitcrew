//! The back office: its member, `@office`, and the loop that applies what it decides.
//!
//! **Its member.** The office acts as an agent of the workspace, `@office`, owned by the
//! workspace's owner (its first person, whom the device token also acts as). It is found or added
//! through the hub's one writer (`WorkService::ensure_office_member`): with `--demo` it is the
//! demo's own `@office`, which the seed adds with everything else; otherwise the workspace's
//! `@office` when it is an agent of that owner, else a new one, appended in a `member_added`
//! authored by the owner, at the first start with a person or right after setup ([`start`]). The
//! office acts inside this process, through the hub's one `WorkService`, so it has no token.
//!
//! **The loop** (hub-work's "Wiring"): subscribe to the store's appends, read the newest revision
//! straight away, then run `WorkService::run_office` over every revision from the first one not
//! looked at (`last + 1`) to the newest announced one, on the blocking pool. `last` moves forward
//! only when a run succeeds: the run itself, and every action in it that the hub did not refuse by
//! its rules (an internal error, such as a full disk, fails the run). A failed range is tried
//! again after a wait that doubles from 1 s to 60 s; appends meanwhile only extend it. On a lag,
//! the upper bound is `latest_rev()`. What the office appends is announced like any other append,
//! so it looks at that too.
//!
//! **Restarts.** `office.json` records `last` for this store's log. The next start runs from
//! there again: `run_office` is idempotent, so what was applied is skipped (`replayed`) and what
//! was not, say after a crash between an append and its run, is applied then. It is written when
//! the office is set up (so a start that fails later still runs from there next time), within a
//! second of each run (at most once a second, and again a second later when a write fails), and
//! when the loop stops. Without it (the first start with the office on, or after a start with the
//! office off, which removes it) the office starts at the end of the log, so it never acts on what
//! was appended while it was off.

use crate::state::{StateDir, read_json, remove, write_json};
use anyhow::Context as _;
use pitcrew_hub_work::{BackOffice, OFFICE_HANDLE, OfficeRun, WorkError, WorkService};
use pitcrew_office::ApplyError;
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::MemberId;
use pitcrew_store::{RevRange, Store};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// How often, at most, `office.json` is rewritten while the office runs; progress is saved this
/// long after a run at the latest.
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// The first wait before a failed range is tried again without an append; it doubles from there.
const RETRY_FIRST: Duration = Duration::from_secs(1);
/// The longest wait before trying a failed range again.
const RETRY_MAX: Duration = Duration::from_secs(60);

/// The back office of `work`'s store, ready to run, as described in the [module docs](self): it
/// acts as `@office`, owned by `owner` (the workspace's person), found or added through `work`
/// (`ensure_office_member`), and its run log is registered on the open store
/// (`Store::register`, so a network filesystem's lease is never let go of). `fresh` (a store just
/// seeded with `--demo`): the office looks at it from the first revision (see [`Office::new`]).
///
/// Runs at start, before anything is served, and once a fresh workspace is set up, while the hub
/// serves: the member is added through the one writer either way.
///
/// `None`, logged, when the office cannot run: no `owner` yet (a workspace not set up), or
/// `@office` is a person, an agent of someone else, or an agent of no one. Its progress
/// (`office.json`) is then forgotten.
///
/// # Errors
/// The store cannot be read or appended to, or the run log cannot be registered.
pub fn start(
    state: &StateDir,
    work: &WorkService,
    owner: Option<MemberId>,
    fresh: bool,
) -> anyhow::Result<Option<Office>> {
    let Some(owner) = owner else {
        tracing::warn!(
            "the workspace has no person yet to own the back office's member, so the back \
             office is off; it starts once the workspace is set up"
        );
        forget(state);
        return Ok(None);
    };
    let member = match work.ensure_office_member(owner) {
        Ok(member) => member,
        Err(e) if e.code() == ErrorCode::Conflict => {
            tracing::warn!(error = %e, "the back office is off: {OFFICE_HANDLE} cannot be it");
            forget(state);
            return Ok(None);
        }
        Err(e) => {
            return Err(
                anyhow::Error::new(e).context("cannot find or add the back office's member")
            );
        }
    };
    tracing::info!(member = %member.id, %owner, "the back office's member is {OFFICE_HANDLE}");
    // Built once: its run log in the store and `run_office` must use the same settings.
    let office = Arc::new(BackOffice::new(member.id));
    let store = work.store();
    // On the store as it is open. The run log catches up as it would at an open, over a
    // `member_added` just appended too.
    store
        .register(Box::new(office.run_log()))
        .context("cannot add the back office's run log to the store")?;
    Office::new(office, state, store, fresh).map(Some)
}

/// Forgets where the back office got to, for a start without it: the next start with it begins at
/// the end of the log, so what is appended meanwhile is never acted on.
pub fn forget(state: &StateDir) {
    let path = state.office();
    if let Err(e) = remove(&path) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "cannot remove the back office's progress"
        );
    }
}

/// What `office.json` holds.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    /// The log it is about (`Store::log_id`): revisions only compare within one log.
    log: String,
    /// The last revision the back office looked at.
    done: u64,
}

/// `office.json`: where the back office got to in this store's log.
#[derive(Debug)]
struct Progress {
    path: PathBuf,
    log: String,
    /// Whether the last save failed.
    failing: AtomicBool,
}

impl Progress {
    fn new(path: PathBuf, log: String) -> Self {
        Self {
            path,
            log,
            failing: AtomicBool::new(false),
        }
    }
}

impl Progress {
    /// The last revision looked at, if `office.json` says so for this log.
    fn read(&self) -> Option<u64> {
        match read_json::<Record>(&self.path, "the back office's progress") {
            Ok(Some(record)) if record.log == self.log => Some(record.done),
            Ok(Some(_)) => {
                tracing::info!(
                    path = %self.path.display(),
                    "the back office's progress is about another store; starting at the end of \
                     the log"
                );
                None
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "the back office's progress does not read; starting at the end of the log"
                );
                None
            }
        }
    }

    /// Records `done`; whether it was written. The first of a run of failures is logged as a
    /// warning, the rest at debug, and the save that ends them at info.
    fn save(&self, done: u64) -> bool {
        let record = Record {
            log: self.log.clone(),
            done,
        };
        let path = self.path.display();
        match write_json(&self.path, &record) {
            Ok(()) => {
                if self.failing.swap(false, Ordering::Relaxed) {
                    tracing::info!(%path, done, "saved the back office's progress again");
                }
                true
            }
            Err(e) if self.failing.swap(true, Ordering::Relaxed) => {
                tracing::debug!(%path, error = %e, "still cannot save the back office's progress");
                false
            }
            Err(e) => {
                tracing::warn!(
                    %path,
                    error = %e,
                    "cannot save the back office's progress; trying again every second"
                );
                false
            }
        }
    }
}

/// The back office of this hub, ready to run.
#[derive(Debug)]
pub struct Office {
    office: Arc<BackOffice>,
    /// The last revision looked at: the loop runs from the next one.
    last: u64,
    /// Whether `office.json` holds `last`.
    saved: bool,
    progress: Arc<Progress>,
}

impl Office {
    /// The back office over `store`, which must have its run log registered
    /// (`Store::register(Box::new(office.run_log()))`, or `projections_with_office` at an open). A
    /// `fresh` store (just seeded with `--demo`) is looked at from its first revision, so the
    /// office sees the seed. Otherwise from where `office.json` says it got to, which a restart
    /// runs again; without it, from the end of the log.
    ///
    /// Where it starts is written to `office.json` now, so a start that fails before the loop
    /// runs (say the listener cannot bind) still runs from there next time.
    ///
    /// # Errors
    /// The store cannot be read.
    pub fn new(
        office: Arc<BackOffice>,
        state: &StateDir,
        store: &Store,
        fresh: bool,
    ) -> anyhow::Result<Self> {
        let latest = store.latest_rev().context("cannot read the store")?;
        let progress = Progress::new(state.office(), store.log_id().to_owned());
        let recorded = if fresh { None } else { progress.read() };
        let last = match (fresh, recorded) {
            (true, _) => 0,
            (false, Some(done)) => done.min(latest),
            (false, None) => latest,
        };
        let saved = recorded == Some(last) || progress.save(last);
        tracing::info!(
            member = %office.member(),
            from = last + 1,
            latest,
            "the back office acts as {OFFICE_HANDLE}"
        );
        Ok(Self {
            office,
            last,
            saved,
            progress: Arc::new(progress),
        })
    }

    /// The back office's member.
    #[cfg(test)]
    pub fn member(&self) -> MemberId {
        self.office.member()
    }

    /// The last revision looked at: the loop runs from the next one.
    #[cfg(test)]
    pub fn last(&self) -> u64 {
        self.last
    }

    /// Starts the loop on the current runtime. It subscribes to the store's appends at once.
    pub fn spawn(self, work: Arc<WorkService>) -> Running {
        let appended = work.store().subscribe();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(run(self, work, appended, stopped));
        Running { stop, task }
    }
}

/// The back office's loop, running.
#[derive(Debug)]
pub struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Running {
    /// A loop that does nothing but wait to be stopped, on the current runtime.
    #[cfg(test)]
    pub fn idle() -> Self {
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let _ = stopped.changed().await;
        });
        Self { stop, task }
    }

    /// Stops the loop and waits for it, at most `within`. A run in progress finishes first; then
    /// where the office got to is saved.
    pub async fn stop(self, within: Duration) {
        let _ = self.stop.send(true);
        let mut task = self.task;
        match tokio::time::timeout(within, &mut task).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "the back office's loop failed"),
            Err(_) => {
                tracing::warn!(
                    seconds = within.as_secs(),
                    "the back office is still running; stopping anyway"
                );
                task.abort();
            }
        }
    }
}

/// The loop. See the [module docs](self).
async fn run(
    office: Office,
    work: Arc<WorkService>,
    mut appended: broadcast::Receiver<RevRange>,
    mut stopped: watch::Receiver<bool>,
) {
    let Office {
        office,
        mut last,
        saved,
        progress,
    } = office;
    // What `office.json` holds, and when it was last written or tried.
    let mut saved = saved.then_some(last);
    let mut tried = Instant::now();
    // The newest revision to look at; `None` reads the log's newest. It is read after subscribing,
    // so no append falls between the two.
    let mut target: Option<u64> = None;
    let mut backoff = RETRY_FIRST;
    'run: loop {
        let to_rev = match target {
            Some(rev) => Some(rev),
            None => latest(&work).await,
        };
        target = to_rev;
        let failed = match to_rev {
            None => true,
            Some(to_rev) if to_rev > last => {
                let revs = RevRange {
                    from_rev: last + 1,
                    to_rev,
                };
                let ran = run_range(&work, &office, revs).await;
                if ran {
                    last = to_rev;
                    backoff = RETRY_FIRST;
                }
                !ran
            }
            Some(_) => false,
        };

        // Wait for a stop, or the next append; after a failure, for the time to try again, while
        // appends only extend the range. Meanwhile save progress when it is due.
        let retry_at = failed.then(|| Instant::now() + backoff);
        loop {
            let save_at = (saved != Some(last)).then(|| tried + SAVE_EVERY);
            tokio::select! {
                biased;
                _ = stopped.changed() => break 'run,
                () = sleep_until(save_at) => {
                    tried = Instant::now();
                    if save(&progress, last).await {
                        saved = Some(last);
                    }
                }
                () = sleep_until(retry_at) => {
                    backoff = backoff.saturating_mul(2).min(RETRY_MAX);
                    target = None;
                    break;
                }
                got = appended.recv() => {
                    match got {
                        Ok(revs) => target = target.map(|t| t.max(revs.to_rev)),
                        Err(RecvError::Lagged(missed)) => {
                            tracing::debug!(
                                missed,
                                "the back office fell behind the appends; reading the log's \
                                 newest revision"
                            );
                            target = None;
                        }
                        Err(RecvError::Closed) => break 'run,
                    }
                    // Whatever else is queued: one run covers it all.
                    loop {
                        match appended.try_recv() {
                            Ok(revs) => target = target.map(|t| t.max(revs.to_rev)),
                            Err(TryRecvError::Lagged(_)) => target = None,
                            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                        }
                    }
                    if retry_at.is_none() {
                        break;
                    }
                }
            }
        }
    }
    if saved != Some(last) && save(&progress, last).await {
        saved = Some(last);
    }
    tracing::info!(
        rev = last,
        saved = saved == Some(last),
        "the back office stopped"
    );
}

/// Sleeps until `at`; without it, forever.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The log's newest revision, read on the blocking pool; `None` (logged) if it cannot be read.
async fn latest(work: &Arc<WorkService>) -> Option<u64> {
    let w = Arc::clone(work);
    match tokio::task::spawn_blocking(move || w.store().latest_rev()).await {
        Ok(Ok(rev)) => Some(rev),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "the back office cannot read the log; trying again later");
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "reading the log for the back office failed");
            None
        }
    }
}

/// Runs the office over `revs` on the blocking pool; whether it succeeded. A run in which the hub
/// failed to apply an action for an internal reason ([`retry_later`]) did not: running the range
/// again applies what is missing, and skips what was applied.
async fn run_range(work: &Arc<WorkService>, office: &Arc<BackOffice>, revs: RevRange) -> bool {
    let (w, o) = (Arc::clone(work), Arc::clone(office));
    match tokio::task::spawn_blocking(move || w.run_office(&o, revs)).await {
        Ok(Ok(run)) => {
            report(&run, revs);
            let failed = run
                .refused()
                .filter(|action| retry_later(&action.result))
                .count();
            if failed > 0 {
                tracing::warn!(
                    from = revs.from_rev,
                    to = revs.to_rev,
                    failed,
                    "the hub could not apply some of the back office's actions for an internal \
                     reason; it runs these revisions again later"
                );
            }
            failed == 0
        }
        Ok(Err(e)) => {
            tracing::warn!(
                from = revs.from_rev,
                to = revs.to_rev,
                error = %e,
                "the back office could not run over these revisions; it tries them again later"
            );
            false
        }
        Err(e) => {
            tracing::error!(
                from = revs.from_rev,
                to = revs.to_rev,
                error = %e,
                "the back office failed; it tries these revisions again later"
            );
            false
        }
    }
}

/// Logs what a run did. The hub logs each action it refused as a warning itself.
fn report(run: &OfficeRun, revs: RevRange) {
    for action in run.applied() {
        tracing::info!(
            rule = %action.entry.rule,
            rev = action.entry.rev,
            "the back office acted"
        );
    }
    let replayed = run.replayed().count();
    if replayed > 0 {
        tracing::info!(
            from = revs.from_rev,
            to = revs.to_rev,
            replayed,
            "the back office had applied these actions before; nothing was appended again"
        );
    }
    tracing::debug!(
        from = revs.from_rev,
        to = revs.to_rev,
        applied = run.applied().count(),
        replayed,
        refused = run.refused().count(),
        "the back office ran"
    );
}

/// Whether the hub's answer to an action means "try again later": it failed for an internal
/// reason (its store, say a full disk) or something it needs was unavailable. A refusal by the
/// rules (a conflict, a forbidden or invalid action, an unknown task, a shape the office's guard
/// refuses) is final: running the range again would only be refused again.
///
/// (`run_office` reports both kinds as an action's result; see the crate README for the proposal
/// that it return the internal ones as its own error instead.)
fn retry_later(result: &Result<(), ApplyError<WorkError>>) -> bool {
    let Err(ApplyError::Failed(e)) = result else {
        return false;
    };
    matches!(e.code(), ErrorCode::Internal | ErrorCode::Unavailable)
}

/// Saves `done` to `office.json`, on the blocking pool; whether it was written.
async fn save(progress: &Arc<Progress>, done: u64) -> bool {
    let p = Arc::clone(progress);
    match tokio::task::spawn_blocking(move || p.save(done)).await {
        Ok(saved) => saved,
        Err(e) => {
            tracing::warn!(error = %e, "saving the back office's progress failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (tempfile::TempDir, StateDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir(&dir).unwrap();
        let state = StateDir::resolve(Some(dir)).unwrap();
        (tmp, state)
    }

    fn progress(state: &StateDir, log: &str) -> Progress {
        Progress::new(state.office(), log.to_owned())
    }

    #[test]
    fn progress_is_kept_per_log() {
        let (_tmp, state) = state();
        let progress = progress(&state, "log-a");
        assert_eq!(progress.read(), None);
        assert!(progress.save(42));
        assert_eq!(progress.read(), Some(42));
        let other = self::progress(&state, "log-b");
        assert_eq!(
            other.read(),
            None,
            "another log's revision means nothing here"
        );
        forget(&state);
        assert_eq!(progress.read(), None);
        forget(&state);
    }

    #[test]
    fn unreadable_progress_starts_at_the_end() {
        let (_tmp, state) = state();
        std::fs::write(state.office(), "{\"log\":").unwrap();
        assert_eq!(progress(&state, "log-a").read(), None);
    }

    #[test]
    fn a_save_that_cannot_write_says_so() {
        let (_tmp, state) = state();
        // A directory where the file goes: the write cannot replace it.
        std::fs::create_dir(state.office()).unwrap();
        let progress = progress(&state, "log-a");
        assert!(!progress.save(7));
        assert!(!progress.save(7));
        std::fs::remove_dir(state.office()).unwrap();
        assert!(progress.save(7));
        assert_eq!(progress.read(), Some(7));
    }

    #[test]
    fn only_internal_failures_are_tried_again() {
        use pitcrew_office::Refusal;
        let failed = |e: WorkError| Err(ApplyError::Failed(e));
        assert!(retry_later(&failed(WorkError::internal("disk full"))));
        assert!(retry_later(&failed(WorkError::unavailable(
            "the store is busy"
        ))));
        for final_refusal in [
            failed(WorkError::conflict("PAP-3 is in review now")),
            failed(WorkError::forbidden("not the office's ask")),
            failed(WorkError::not_found("no task")),
            failed(WorkError::invalid("no evidence")),
            Err(ApplyError::Refused(Refusal::MarksDone)),
            Err(ApplyError::Refused(Refusal::SendsOutward)),
            Ok(()),
        ] {
            assert!(!retry_later(&final_refusal), "{final_refusal:?}");
        }
    }
}
