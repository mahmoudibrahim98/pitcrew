//! The work model's activity index, as `pitcrew-api` takes it: the `project=` and `workstream=`
//! filters of `GET /v1/events`, and the wider `task=` and `session=` matches.
//!
//! `pitcrew-api` does not depend on the work model, so its `EventRefs` and `RefFilter` mirror
//! hub-work's field for field, and this adapter copies one into the other.

use pitcrew_api::source::SourceError;
use pitcrew_hub_work::WorkService;
use std::sync::Arc;

/// The hub's one `WorkService`, answering the activity route's index queries.
#[derive(Debug)]
pub struct WorkRefs(pub Arc<WorkService>);

impl pitcrew_api::EventRefs for WorkRefs {
    fn revs_matching(
        &self,
        f: &pitcrew_api::RefFilter,
        before_rev: u64,
        limit: usize,
    ) -> Result<(Vec<u64>, u64), SourceError> {
        let filter = pitcrew_hub_work::RefFilter {
            project: f.project,
            workstream: f.workstream,
            task: f.task,
            session: f.session,
        };
        pitcrew_hub_work::EventRefs::revs_matching(&*self.0, &filter, before_rev, limit)
            .map_err(Into::into)
    }
}
