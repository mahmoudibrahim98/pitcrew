//! The hub's recap index, as `pitcrew-api` takes it: `GET /v1/recaps/blocks` and
//! `GET /v1/recaps/days`.
//!
//! `pitcrew-api` does not depend on the work model, so its `BlockFilter` and `DaysScope` mirror
//! hub-work's field for field and variant for variant, and this adapter copies one into the other,
//! as [`crate::refs`] does for the activity index. The route has already applied the contract's
//! default and cap to `limit` and checked every value, so the index gets `Some(limit)`, and an
//! `invalid` from it means the two disagree about the contract: a bug, logged as a warning.

use pitcrew_api::source::SourceError;
use pitcrew_hub_work::{RecapIndex, WorkError};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::Date;
use pitcrew_protocol::recap::{BlocksPage, DaysPage};
use std::sync::Arc;

/// The hub's recap index (its one `WorkService`), answering the recap routes.
#[derive(Debug)]
pub struct WorkRecaps(pub Arc<dyn RecapIndex>);

impl pitcrew_api::RecapSource for WorkRecaps {
    fn blocks(
        &self,
        filter: &pitcrew_api::BlockFilter,
        before: Option<EventId>,
        limit: usize,
    ) -> Result<BlocksPage, SourceError> {
        let filter = pitcrew_hub_work::BlockFilter {
            session: filter.session,
            task: filter.task,
            workstream: filter.workstream,
            project: filter.project,
        };
        self.0
            .recap_blocks(&filter, before, Some(limit))
            .map_err(|e| failed("blocks", e))
    }

    fn days(
        &self,
        scope: pitcrew_api::DaysScope,
        tz_minutes: i32,
        before: Option<Date>,
        limit: usize,
    ) -> Result<DaysPage, SourceError> {
        let scope = match scope {
            pitcrew_api::DaysScope::Workstream(id) => pitcrew_hub_work::DaysScope::Workstream(id),
            pitcrew_api::DaysScope::Project(id) => pitcrew_hub_work::DaysScope::Project(id),
        };
        self.0
            .recap_days(scope, tz_minutes, before.as_ref(), Some(limit))
            .map_err(|e| failed("days", e))
    }
}

/// The index's error as the route takes it. The route answers `500` for any; an `invalid` is
/// also logged here, as the bug it is.
fn failed(what: &str, error: WorkError) -> SourceError {
    if error.code() == ErrorCode::Invalid {
        tracing::warn!(
            what,
            error = %error,
            "the recap index refused a query the route had validated; the two disagree about \
             the contract"
        );
    }
    error.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_api::RecapSource as _;
    use pitcrew_protocol::ids::{ProjectId, SessionId, TaskId, WorkstreamId};
    use std::sync::Mutex;

    /// What the index was asked.
    #[derive(Debug, PartialEq, Eq)]
    enum Asked {
        Blocks(
            pitcrew_hub_work::BlockFilter,
            Option<EventId>,
            Option<usize>,
        ),
        Days(
            pitcrew_hub_work::DaysScope,
            i32,
            Option<Date>,
            Option<usize>,
        ),
    }

    /// A recap index that records what it is asked and answers `fail`, or an empty page.
    #[derive(Debug, Default)]
    struct Recorder {
        asked: Mutex<Vec<Asked>>,
        fail: Option<WorkError>,
    }

    impl Recorder {
        fn answer<T>(&self, page: T) -> pitcrew_hub_work::Result<T> {
            self.fail.clone().map_or(Ok(page), Err)
        }
    }

    impl RecapIndex for Recorder {
        fn recap_blocks(
            &self,
            filter: &pitcrew_hub_work::BlockFilter,
            before: Option<EventId>,
            limit: Option<usize>,
        ) -> pitcrew_hub_work::Result<BlocksPage> {
            self.asked
                .lock()
                .unwrap()
                .push(Asked::Blocks(*filter, before, limit));
            self.answer(BlocksPage {
                blocks: Vec::new(),
                at_start: true,
            })
        }

        fn recap_days(
            &self,
            scope: pitcrew_hub_work::DaysScope,
            tz_minutes: i32,
            before: Option<&Date>,
            limit: Option<usize>,
        ) -> pitcrew_hub_work::Result<DaysPage> {
            self.asked
                .lock()
                .unwrap()
                .push(Asked::Days(scope, tz_minutes, before.cloned(), limit));
            self.answer(DaysPage {
                days: Vec::new(),
                at_start: true,
            })
        }
    }

    #[test]
    fn copies_every_field_and_variant() {
        let index = Arc::new(Recorder::default());
        let recaps = WorkRecaps(Arc::clone(&index) as Arc<dyn RecapIndex>);
        let (session, task) = (SessionId::new(), TaskId::new());
        let (workstream, project) = (WorkstreamId::new(), ProjectId::new());
        let before = EventId::new();
        let date = Date("2026-09-30".to_owned());

        let page = recaps
            .blocks(
                &pitcrew_api::BlockFilter {
                    session: Some(session),
                    task: Some(task),
                    workstream: Some(workstream),
                    project: Some(project),
                },
                Some(before),
                4,
            )
            .unwrap();
        assert!(page.at_start);
        recaps
            .blocks(&pitcrew_api::BlockFilter::default(), None, 200)
            .unwrap();
        recaps
            .days(
                pitcrew_api::DaysScope::Workstream(workstream),
                -300,
                Some(date.clone()),
                1,
            )
            .unwrap();
        recaps
            .days(pitcrew_api::DaysScope::Project(project), 840, None, 30)
            .unwrap();

        assert_eq!(
            *index.asked.lock().unwrap(),
            [
                Asked::Blocks(
                    pitcrew_hub_work::BlockFilter {
                        session: Some(session),
                        task: Some(task),
                        workstream: Some(workstream),
                        project: Some(project),
                    },
                    Some(before),
                    Some(4),
                ),
                Asked::Blocks(pitcrew_hub_work::BlockFilter::default(), None, Some(200)),
                Asked::Days(
                    pitcrew_hub_work::DaysScope::Workstream(workstream),
                    -300,
                    Some(date),
                    Some(1),
                ),
                Asked::Days(
                    pitcrew_hub_work::DaysScope::Project(project),
                    840,
                    None,
                    Some(30),
                ),
            ]
        );
    }

    /// What the code under it logs, without colours.
    #[derive(Clone, Debug, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl Captured {
        fn during(&self, f: impl FnOnce()) -> String {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_ansi(false)
                .finish();
            tracing::subscriber::with_default(subscriber, f);
            String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
        }
    }

    /// Every error reaches the route (which answers 500 and logs it); an `invalid` is also a
    /// warning here, since the route validated the query first.
    #[test]
    fn errors_reach_the_route_and_invalid_is_a_warning() {
        let captured = Captured::default();
        for (fail, warned) in [
            (
                WorkError::new(ErrorCode::Internal, "the store is gone"),
                false,
            ),
            (WorkError::invalid("tz is out of range"), true),
        ] {
            let recaps = WorkRecaps(Arc::new(Recorder {
                fail: Some(fail.clone()),
                ..Recorder::default()
            }));
            let logs = captured.during(|| {
                let blocks = recaps
                    .blocks(&pitcrew_api::BlockFilter::default(), None, 50)
                    .unwrap_err();
                assert_eq!(blocks.to_string(), fail.message());
                let days = recaps
                    .days(
                        pitcrew_api::DaysScope::Project(ProjectId::new()),
                        0,
                        None,
                        7,
                    )
                    .unwrap_err();
                assert_eq!(days.to_string(), fail.message());
            });
            let warnings: Vec<&str> = logs
                .lines()
                .filter(|l| l.contains("WARN") && l.contains("disagree about the contract"))
                .collect();
            if warned {
                assert_eq!(warnings.len(), 2, "{logs}");
                assert!(warnings[0].contains("blocks"), "{logs}");
                assert!(warnings[1].contains("days"), "{logs}");
            } else {
                assert!(warnings.is_empty(), "{logs}");
            }
        }
    }
}
