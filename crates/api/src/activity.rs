//! `GET /v1/events?before=&limit=&task=&session=`: activity, paged back through the event log.
//!
//! - Events come oldest first within a page; without `before`, the newest page. `before` is an
//!   exclusive revision. `limit` defaults to 100 and is capped at 500.
//! - `from_rev` and `to_rev` are the revisions of the first and last events (both 0 for an empty
//!   page). Pass `from_rev` as `before` for the previous page. `at_start` is true when nothing
//!   older matches.
//! - **Filters.** `session` and `task` match events whose body names that id anywhere (the
//!   session a turn ran in, the task a comment is on, an ask's task, …). Events that reach the
//!   entity only through another one (a turn in a session linked to the task) are not included
//!   until the hub has projections. Filtered pages are found by scanning back at most
//!   [`SCAN_BUDGET`] events per request; if the budget runs out first, the page may hold fewer
//!   than `limit` events (even none) with `at_start` false, and `from_rev` is where the scan
//!   stopped, so the next request continues from there.
//! - `project` and `workstream` need an index the hub does not have yet (stream E), and answer
//!   `400 invalid` with [`NEEDS_INDEX`].

use crate::source::{EventSource, SourceError, StoredEvent};
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use pitcrew_auth::ErrorResponse;
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::{SessionId, TaskId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Events per page when `limit` is absent.
pub const DEFAULT_LIMIT: usize = 100;
/// The largest `limit`.
pub const MAX_LIMIT: usize = 500;
/// The most events one filtered request scans.
pub const SCAN_BUDGET: usize = 10_000;
/// The message for `project` and `workstream` filters.
pub const NEEDS_INDEX: &str = "Filtering activity by project or workstream needs the hub's \
    project index, which is not available yet. Filter by task or session, or page unfiltered.";

const SCAN_PAGE: usize = 500;

/// One page of activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsPage {
    /// The events, oldest first.
    pub events: Vec<Event>,
    /// Revision of the first event (see the module docs for a filtered page that ran out of
    /// budget).
    pub from_rev: u64,
    /// Revision of the last event.
    pub to_rev: u64,
    /// Whether nothing older matches.
    pub at_start: bool,
}

/// The activity route. Mount it as a **device** route (`RouterParts::device`).
pub fn routes(source: Arc<dyn EventSource>) -> Router {
    Router::new()
        .route("/v1/events", get(events))
        .with_state(source)
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    before: Option<u64>,
    limit: Option<usize>,
    project: Option<String>,
    workstream: Option<String>,
    task: Option<String>,
    session: Option<String>,
}

async fn events(
    State(source): State<Arc<dyn EventSource>>,
    query: Result<Query<EventsQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<EventsPage>, ErrorResponse> {
    let invalid = |m: String| ErrorResponse::new(ErrorCode::Invalid, m);
    let Query(query) = query.map_err(|e| invalid(format!("Bad query: {}", e.body_text())))?;
    if query.project.is_some() || query.workstream.is_some() {
        return Err(invalid(NEEDS_INDEX.to_owned()));
    }
    let limit = match query.limit {
        None => DEFAULT_LIMIT,
        Some(0) => return Err(invalid("`limit` must be at least 1.".to_owned())),
        Some(n) => n.min(MAX_LIMIT),
    };
    let mut needles = Vec::new();
    if let Some(session) = &query.session {
        let id: SessionId = session
            .parse()
            .map_err(|_| invalid(format!("{session:?} is not a session id.")))?;
        needles.push(id.0.to_string());
    }
    if let Some(task) = &query.task {
        let id: TaskId = task
            .parse()
            .map_err(|_| invalid(format!("{task:?} is not a task id.")))?;
        needles.push(id.0.to_string());
    }
    let before = query.before;
    let page = tokio::task::spawn_blocking(move || {
        let before = match before {
            Some(before) => before,
            None => source.latest_rev()?.saturating_add(1),
        };
        page(&*source, before, limit, &needles)
    })
    .await
    .map_err(|e| {
        ErrorResponse::new(
            ErrorCode::Internal,
            format!("Reading activity panicked: {e}"),
        )
    })?
    .map_err(|e| {
        tracing::error!(error = %e, "reading activity failed");
        ErrorResponse::new(ErrorCode::Internal, "Could not read the event log.")
    })?;
    Ok(Json(page))
}

/// The newest `limit` events below `before` whose bodies name every id in `needles`.
///
/// # Errors
/// The source cannot be read.
pub fn page(
    source: &dyn EventSource,
    before: u64,
    limit: usize,
    needles: &[String],
) -> Result<EventsPage, SourceError> {
    if needles.is_empty() {
        // One extra event tells whether older ones exist.
        let mut events = source.before(before, limit.saturating_add(1))?;
        let at_start = events.len() <= limit;
        if !at_start {
            events.remove(0);
        }
        return Ok(page_of(events, at_start, 0));
    }

    let mut found: Vec<StoredEvent> = Vec::new(); // newest first
    let mut cursor = before;
    let mut scanned = 0;
    let at_start = 'scan: loop {
        if cursor <= 1 {
            break true;
        }
        if scanned >= SCAN_BUDGET {
            break false;
        }
        let chunk = source.before(cursor, SCAN_PAGE)?;
        let Some(first) = chunk.first() else {
            break true;
        };
        cursor = first.rev;
        scanned += chunk.len();
        for event in chunk.into_iter().rev() {
            if names_all(&event.event, needles) {
                found.push(event);
                if found.len() > limit {
                    found.pop();
                    break 'scan false;
                }
            }
        }
    };
    found.reverse();
    Ok(page_of(found, at_start, cursor))
}

fn page_of(events: Vec<StoredEvent>, at_start: bool, scanned_to: u64) -> EventsPage {
    let (from_rev, to_rev) = match (events.first(), events.last()) {
        (Some(first), Some(last)) => (first.rev, last.rev),
        // An empty page that ran out of scan budget: continue from where it stopped.
        _ if !at_start => (scanned_to, 0),
        _ => (0, 0),
    };
    EventsPage {
        events: events.into_iter().map(|e| e.event).collect(),
        from_rev,
        to_rev,
        at_start,
    }
}

/// Whether the event's body holds each needle as a whole JSON string.
fn names_all(event: &Event, needles: &[String]) -> bool {
    let Ok(body) = serde_json::to_string(&event.body) else {
        return false;
    };
    needles
        .iter()
        .all(|needle| body.contains(&format!("\"{needle}\"")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::model::Liveness;
    use pitcrew_protocol::{MachineId, MemberId, WorkspaceId};

    fn filler(n: usize) -> Vec<Event> {
        (0..n)
            .map(|_| {
                Event::now(
                    WorkspaceId::new(),
                    MemberId::new(),
                    EventBody::MachineLiveness {
                        machine: MachineId::new(),
                        liveness: Liveness::Live,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn unfiltered_pages_join_up() {
        let source = MemorySource::new("log", 4);
        source.append(filler(1200));
        for limit in [100, 500, 7] {
            let mut revs = Vec::new();
            let mut before = 1201;
            loop {
                let page = page(&source, before, limit, &[]).unwrap();
                assert!(page.events.len() <= limit);
                let expected_first = page.to_rev + 1 - page.events.len() as u64;
                assert_eq!(page.from_rev, expected_first);
                revs.splice(0..0, page.from_rev..=page.to_rev);
                if page.at_start {
                    assert_eq!(page.from_rev, 1);
                    break;
                }
                before = page.from_rev;
            }
            assert_eq!(revs, (1..=1200).collect::<Vec<_>>(), "limit {limit}");
        }
    }

    #[test]
    fn before_is_exclusive_and_at_start_is_exact() {
        let source = MemorySource::new("log", 4);
        source.append(filler(10));
        let page = page(&source, 11, 5, &[]).unwrap();
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (6, 10, false));
        let page = super::page(&source, 6, 5, &[]).unwrap();
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (1, 5, true));
        let page = super::page(&source, 1, 5, &[]).unwrap();
        assert_eq!((page.from_rev, page.to_rev, page.at_start), (0, 0, true));
    }

    #[test]
    fn a_filtered_scan_that_runs_out_of_budget_says_where_it_stopped() {
        let source = MemorySource::new("log", 4);
        source.append(filler(SCAN_BUDGET + 1500));
        let needle = [MachineId::new().0.to_string()];
        let first = page(&source, u64::MAX, 10, &needle).unwrap();
        assert!(first.events.is_empty());
        assert!(!first.at_start);
        assert_eq!(first.from_rev, 1501);
        let rest = page(&source, first.from_rev, 10, &needle).unwrap();
        assert!(rest.events.is_empty());
        assert!(rest.at_start);
    }
}
