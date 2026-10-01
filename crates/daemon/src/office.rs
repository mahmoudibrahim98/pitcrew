//! The back office: its member, `@office`, and the loop that applies what it decides.
//!
//! **Its member.** The office acts as an agent of the workspace, `@office`, owned by the
//! workspace's owner (its first person, whom the device token also acts as). With `--demo` it is
//! the demo's own `@office`, which the seed adds with everything else. Otherwise the daemon reuses
//! the workspace's agent `@office`, and on the first start without one appends a `member_added`
//! for it, authored by the owner ([`member`]). The office acts inside this process, through the
//! hub's one `WorkService`, so it has no token.
//!
//! **The loop** (hub-work's "Wiring"): subscribe to the store's appends, read the newest revision
//! straight away, then run `WorkService::run_office` over every revision from the first one not
//! looked at (`last + 1`) to the newest announced one, on the blocking pool. `last` moves forward
//! only when a run succeeds; a failed range is tried again with the next append, or after a short
//! wait. On a lag, the upper bound is `latest_rev()`. What the office appends is announced like any
//! other append, so it looks at that too.
//!
//! **Restarts.** `office.json` records `last` for this store's log. The next start runs from there
//! again: `run_office` is idempotent, so what was applied is skipped (`replayed`) and what was not,
//! say after a crash between an append and its run, is applied then. It is rewritten at most once
//! a second while the office runs, and when it stops. Without it (the first start with the office
//! on, or after `--no-office`, which removes it) the office starts at the end of the log, so it
//! never acts on what was appended while it was off.

use crate::state::{StateDir, read_json, remove, write_json};
use anyhow::Context as _;
use pitcrew_fixtures::DemoWorkspace;
use pitcrew_hub_work::{BackOffice, OfficeRun, WorkService};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::MemberId;
use pitcrew_protocol::model::{Member, MemberKind, Workspace};
use pitcrew_store::{RevRange, Store};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// The back office's handle.
pub const HANDLE: &str = "@office";
/// Its display name, as in the demo.
const NAME: &str = "Back office";
/// How often, at most, `office.json` is rewritten while the office runs.
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// The first wait before a failed range is tried again without an append; it doubles from there.
const RETRY_FIRST: Duration = Duration::from_secs(1);
/// The longest wait before trying a failed range again.
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Who the back office acts as, as described in the [module docs](self): the demo's `@office`
/// with `--demo`, else the workspace's agent `@office`, added now if there is none.
///
/// The `member_added` is appended straight to `store`, before the hub's `WorkService` exists and
/// before anything is served, so it races with no other writer. (The work model has no command for
/// adding a member yet.)
///
/// `None`, logged, when the office cannot run: the workspace has no person yet to own it, or
/// `@office` is a person.
///
/// # Errors
/// The store cannot be read or appended to.
pub fn member(
    store: &Store,
    workspace: &Workspace,
    demo: Option<&DemoWorkspace>,
) -> anyhow::Result<Option<MemberId>> {
    if let Some(demo) = demo {
        let office = demo
            .members
            .iter()
            .find(|m| m.handle == HANDLE && m.kind == MemberKind::Agent);
        if office.is_none() {
            tracing::warn!("the demo workspace has no agent {HANDLE}, so the back office is off");
        }
        return Ok(office.map(|m| m.id));
    }
    let members = store
        .read(pitcrew_hub_work::query::members)
        .context("cannot list the members")?;
    if let Some(found) = members.iter().find(|m| m.handle == HANDLE) {
        if found.kind == MemberKind::Agent {
            return Ok(Some(found.id));
        }
        tracing::warn!(
            member = %found.id,
            "{HANDLE} is a person in this workspace, so the back office is off"
        );
        return Ok(None);
    }
    let Some(owner) = members.iter().find(|m| m.kind == MemberKind::Human) else {
        tracing::warn!(
            "the workspace has no person yet to own the back office's member, so the back \
             office is off; it starts on the first start after one is added"
        );
        return Ok(None);
    };
    let office = Member {
        id: MemberId::new(),
        kind: MemberKind::Agent,
        handle: HANDLE.to_owned(),
        name: NAME.to_owned(),
        owner: Some(owner.id),
        persona: None,
    };
    let id = office.id;
    let added = Event::now(
        workspace.id,
        owner.id,
        EventBody::MemberAdded { member: office },
    );
    store
        .append(&[added])
        .context("cannot add the back office's member")?;
    tracing::info!(member = %id, owner = %owner.handle, "added the back office's member {HANDLE}");
    Ok(Some(id))
}

/// Forgets where the back office got to, for a start without it: the next start with it begins at
/// the end of the log, so what is appended meanwhile is never acted on.
pub fn forget(state: &StateDir) {
    let path = state.office();
    if let Err(e) = remove(&path) {
        tracing::warn!(path = %path.display(), error = %e, "cannot remove the back office's progress");
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

    fn save(&self, done: u64) {
        let record = Record {
            log: self.log.clone(),
            done,
        };
        if let Err(e) = write_json(&self.path, &record) {
            tracing::warn!(path = %self.path.display(), error = %e, "cannot save the back office's progress");
        }
    }
}

/// The back office of this hub, ready to run.
#[derive(Debug)]
pub struct Office {
    office: Arc<BackOffice>,
    /// The last revision looked at: the loop runs from the next one.
    last: u64,
    progress: Arc<Progress>,
}

impl Office {
    /// The back office over `store`, which must be open with its run log
    /// (`projections_with_office`). A `fresh` store (just seeded with `--demo`) is looked at from
    /// its first revision, so the office sees the seed. Otherwise from where `office.json` says
    /// it got to, which a restart runs again; without it, from the end of the log.
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
        let progress = Progress {
            path: state.office(),
            log: store.log_id().to_owned(),
        };
        let last = if fresh {
            0
        } else {
            progress.read().map_or(latest, |done| done.min(latest))
        };
        tracing::info!(
            member = %office.member(),
            from = last + 1,
            latest,
            "the back office acts as {HANDLE}"
        );
        Ok(Self {
            office,
            last,
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
        progress,
    } = office;
    // Where this start begins: a crash before the first run then runs from here again.
    save(&progress, last).await;
    let mut saved = last;
    let mut saved_at = Instant::now();
    // The newest revision to look at; `None` reads the log's newest. It is read after subscribing,
    // so no append falls between the two.
    let mut target: Option<u64> = None;
    let mut backoff = RETRY_FIRST;
    loop {
        let to_rev = match target {
            Some(rev) => Some(rev),
            None => latest(&work).await,
        };
        target = to_rev;
        let failing = match to_rev {
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
                    if saved_at.elapsed() >= SAVE_EVERY {
                        save(&progress, last).await;
                        saved = last;
                        saved_at = Instant::now();
                    }
                }
                !ran
            }
            Some(_) => false,
        };

        // Wait for a stop, the next append, or the time to try again.
        let retry = async {
            if failing {
                tokio::time::sleep(backoff).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            () = retry => {
                backoff = backoff.saturating_mul(2).min(RETRY_MAX);
                target = None;
            }
            got = appended.recv() => match got {
                Ok(revs) => target = target.map(|t| t.max(revs.to_rev)),
                Err(RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "the back office fell behind the appends; reading the log's newest revision");
                    target = None;
                }
                Err(RecvError::Closed) => break,
            },
        }
        // Whatever else is queued: one run covers it all.
        loop {
            match appended.try_recv() {
                Ok(revs) => target = target.map(|t| t.max(revs.to_rev)),
                Err(TryRecvError::Lagged(_)) => target = None,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
    }
    if saved != last {
        save(&progress, last).await;
    }
    tracing::info!(rev = last, "the back office stopped");
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

/// Runs the office over `revs` on the blocking pool; whether it succeeded.
async fn run_range(work: &Arc<WorkService>, office: &Arc<BackOffice>, revs: RevRange) -> bool {
    let (w, o) = (Arc::clone(work), Arc::clone(office));
    match tokio::task::spawn_blocking(move || w.run_office(&o, revs)).await {
        Ok(Ok(run)) => {
            report(&run, revs);
            true
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

/// Saves `done` to `office.json`, on the blocking pool.
async fn save(progress: &Arc<Progress>, done: u64) {
    let p = Arc::clone(progress);
    if let Err(e) = tokio::task::spawn_blocking(move || p.save(done)).await {
        tracing::warn!(error = %e, "saving the back office's progress failed");
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

    #[test]
    fn progress_is_kept_per_log() {
        let (_tmp, state) = state();
        let progress = Progress {
            path: state.office(),
            log: "log-a".to_owned(),
        };
        assert_eq!(progress.read(), None);
        progress.save(42);
        assert_eq!(progress.read(), Some(42));
        let other = Progress {
            path: state.office(),
            log: "log-b".to_owned(),
        };
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
        let progress = Progress {
            path: state.office(),
            log: "log-a".to_owned(),
        };
        assert_eq!(progress.read(), None);
    }
}
