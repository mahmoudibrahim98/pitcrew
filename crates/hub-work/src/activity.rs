//! [`EventRefs`]: the activity reference index, as a small trait for the API layer.
//!
//! `GET /v1/events?project=&workstream=&task=&session=` (stream H, `crates/api`) pages back
//! through the event log. Filtering by project or workstream needs to know what each event is
//! about, which only the work model knows: an event names a session, the session is linked to a
//! task, the task belongs to a workstream. The `work.refs` projection keeps that per event, and
//! this trait answers from it, so the API layer can filter without depending on the work model's
//! tables (the daemon passes its one `Arc<WorkService>` as an `Arc<dyn EventRefs>`).
//!
//! How the route uses it, for a filtered page (`before` absent means `latest_rev + 1`):
//!
//! ```text
//! let (revs, scanned_to) = refs.revs_matching(&filter, before, limit)?;
//! events    = the events at `revs` (oldest first)
//! from_rev  = revs.first(), or scanned_to when revs is empty
//! to_rev    = revs.last(), or 0 when revs is empty
//! at_start  = scanned_to == 0
//! ```
//!
//! That is the contract's activity paging: a page may hold fewer than `limit` events (even none)
//! when the scan budget runs out, `from_rev` is where to continue, and only `at_start` ends
//! paging.

use crate::error::Result;
use crate::query::{self, REF_SCAN_BUDGET, RefFilter};
use crate::service::WorkService;

/// The activity reference index: which events are about a project, workstream, task or session.
///
/// "About" follows the links in force when each event happened: a turn in a session linked to a
/// task is about that task, its workstream and its project; `dispatch_finished` is about its
/// dispatch's task and session. See [`crate::projection::Refs`] for every event type.
pub trait EventRefs: Send + Sync + std::fmt::Debug {
    /// The newest revisions below `before_rev` (exclusive) of events about **all** of `filter`'s
    /// fields: at most `limit`, oldest first, with `scanned_to`, where to continue.
    ///
    /// - Pass `scanned_to` as the next `before_rev` to page back; it is **0 exactly when no older
    ///   event matches**.
    /// - When more than `limit` events match, `scanned_to` is the oldest revision returned.
    /// - One call examines at most [`REF_SCAN_BUDGET`] index rows. That only bounds a filter of
    ///   two or more fields (with one field every row examined matches); when it runs out, fewer
    ///   than `limit` revisions come back, maybe none, and `scanned_to` says where it stopped.
    ///
    /// # Errors
    ///
    /// `invalid` for a filter with no field or a `limit` of 0; database errors (an internal
    /// error, whose detail is logged, not returned to clients).
    fn revs_matching(
        &self,
        filter: &RefFilter,
        before_rev: u64,
        limit: usize,
    ) -> Result<(Vec<u64>, u64)>;
}

impl EventRefs for WorkService {
    fn revs_matching(
        &self,
        filter: &RefFilter,
        before_rev: u64,
        limit: usize,
    ) -> Result<(Vec<u64>, u64)> {
        self.read(|c| query::revs_matching(c, filter, before_rev, limit, REF_SCAN_BUDGET))
    }
}
