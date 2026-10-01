//! `GET /v1/recaps/blocks` and `GET /v1/recaps/days`: activity blocks and day paragraphs, over a
//! [`RecapSource`] the daemon fills in.
//!
//! - The recap engine (`crates/recap`, stream F) computes blocks and day paragraphs from the
//!   event log; recaps are derived, never stored. This crate does not depend on the engine, so
//!   the daemon adapts it to [`RecapSource`], the way it adapts the work model's activity index to
//!   [`crate::EventRefs`] (see the `crates/api` README, "For the composition root").
//! - Both routes need a **device** token: mount them with [`Recaps::routes`] under
//!   [`crate::RouterParts::device`]. Without a source, simply do not mount them.
//! - [`BlockFilter`] mirrors the query of `GET /v1/recaps/blocks`; [`DaysScope`] the required,
//!   mutually exclusive `workstream`/`project` of `GET /v1/recaps/days`.
//! - Validation is everything `docs/build/contracts/api-v1.md` ("Recaps") calls `400 invalid`:
//!   malformed ids (bare or prefixed), malformed dates (`YYYY-MM-DD` only), a malformed or
//!   out-of-range `tz`, a `limit` of 0 or not a number, and `days` with neither or both of
//!   `workstream`/`project`. A `limit` above the route's cap counts as the cap. An unknown id is
//!   not validated here: it is simply a filter nothing matches, so the source answers an empty
//!   page with `at_start: true`, as the activity route does for an unknown session or task.

use crate::source::SourceError;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use pitcrew_auth::ErrorResponse;
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::{EventId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::Date;
use pitcrew_protocol::recap::{
    BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT, BlocksPage, DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT,
    DaysPage, MAX_TZ_MINUTES,
};
use serde::Deserialize;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// Which blocks to find: those linked to **all** of the given session, task, workstream and
/// project.
///
/// It is both what a request filters by and what the route asks the source. It mirrors the
/// `RecapBlockFilter` the daemon's `pitcrew-recap` engine (or its index) is asked with, so the
/// daemon's adapter copies it field for field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockFilter {
    /// The block's session.
    pub session: Option<SessionId>,
    /// One of the block's tasks.
    pub task: Option<TaskId>,
    /// The block's workstream.
    pub workstream: Option<WorkstreamId>,
    /// The block's project.
    pub project: Option<ProjectId>,
}

/// `GET /v1/recaps/days` needs exactly one of `workstream` and `project`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaysScope {
    /// One workstream's day paragraphs.
    Workstream(WorkstreamId),
    /// One project's: a paragraph per workstream per day, plus one per day for the project's work
    /// outside any workstream.
    Project(ProjectId),
}

/// Where the recap routes read blocks and day paragraphs from.
///
/// The recap engine computes both from the event log (`docs/build/contracts/api-v1.md`,
/// "Recaps"); this crate does not depend on it, so the daemon adapts it (or, for development, the
/// mock's fixture) to this trait.
pub trait RecapSource: Send + Sync + fmt::Debug + 'static {
    /// Blocks matching **all** of `filter`'s fields, newest first by id, below `before`
    /// (exclusive) when given.
    ///
    /// - At most `limit` blocks (never 0; already capped to [`BLOCKS_MAX_LIMIT`]).
    /// - `at_start` is true exactly when no older matching block exists; a page that is not at
    ///   the start holds at least one block.
    /// - An id nothing matches is not an error: the result is simply an empty page with
    ///   `at_start: true`.
    ///
    /// Blocking: the route calls it on the blocking pool.
    ///
    /// # Errors
    /// The recap engine could not be read.
    fn blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: usize,
    ) -> Result<BlocksPage, SourceError>;

    /// Day paragraphs of `scope`, newest date first, below `before` (exclusive) when given, at
    /// `tz_minutes` (whole minutes east of UTC, already checked to be within
    /// [`MAX_TZ_MINUTES`]).
    ///
    /// - At most `limit` **dates** (never 0; already capped to [`DAYS_MAX_LIMIT`]); a page holds
    ///   every entry of the dates it covers.
    /// - `at_start` is true exactly when no older day with matching activity exists; a page that
    ///   is not at the start holds at least one date.
    /// - An unknown workstream or project is not an error: the result is simply an empty page
    ///   with `at_start: true`.
    ///
    /// Blocking: the route calls it on the blocking pool.
    ///
    /// # Errors
    /// The recap engine could not be read, or cannot answer for `tz_minutes` (a development
    /// source may only hold one time zone's days; the real engine computes any).
    fn days(
        &self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<Date>,
        limit: usize,
    ) -> Result<DaysPage, SourceError>;
}

/// The recap routes over a [`RecapSource`].
#[derive(Clone, Debug)]
pub struct Recaps {
    source: Arc<dyn RecapSource>,
}

impl Recaps {
    /// Recaps read from `source`.
    #[must_use]
    pub fn new(source: Arc<dyn RecapSource>) -> Self {
        Self { source }
    }

    /// The routes. Mount as a **device** route ([`crate::RouterParts::device`]).
    pub fn routes(self) -> Router {
        Router::new()
            .route("/v1/recaps/blocks", get(blocks))
            .route("/v1/recaps/days", get(days))
            .with_state(self)
    }
}

fn invalid(message: impl Into<String>) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::Invalid, message)
}

fn failed() -> ErrorResponse {
    ErrorResponse::new(ErrorCode::Internal, "Could not read recaps.")
}

/// Parses an optional id from the query: `400 invalid` if it is present but not one.
fn parse_id<T: FromStr>(value: Option<&str>, kind: &str) -> Result<Option<T>, ErrorResponse> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid(format!("{value:?} is not a {kind} id.")))
        })
        .transpose()
}

/// Parses an optional `YYYY-MM-DD` date from the query: `400 invalid` otherwise.
fn parse_date(value: Option<&str>) -> Result<Option<Date>, ErrorResponse> {
    value
        .map(|value| {
            let date = Date(value.to_owned());
            if date.is_well_formed() {
                Ok(date)
            } else {
                Err(invalid(format!(
                    "{value:?} is not a date written YYYY-MM-DD."
                )))
            }
        })
        .transpose()
}

/// Parses `tz`: whole minutes east of UTC, `-MAX_TZ_MINUTES..=MAX_TZ_MINUTES`; absent is 0.
///
/// Only ASCII digits, with an optional leading `-`, are accepted (so `1.5`, `+60` and `60m` are
/// all rejected), matching the mock's `tz` parsing.
fn parse_tz(value: Option<&str>) -> Result<i32, ErrorResponse> {
    let out_of_range = || {
        invalid(format!(
            "tz must be whole minutes east of UTC, from -{MAX_TZ_MINUTES} to {MAX_TZ_MINUTES}."
        ))
    };
    let Some(raw) = value else {
        return Ok(0);
    };
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(out_of_range());
    }
    let magnitude: i32 = digits.parse().map_err(|_| out_of_range())?;
    let tz = if raw.starts_with('-') {
        -magnitude
    } else {
        magnitude
    };
    if tz.abs() > MAX_TZ_MINUTES {
        return Err(out_of_range());
    }
    Ok(tz)
}

#[derive(Debug, Deserialize)]
struct BlocksQuery {
    session: Option<String>,
    task: Option<String>,
    workstream: Option<String>,
    project: Option<String>,
    before: Option<String>,
    limit: Option<usize>,
}

async fn blocks(
    State(recaps): State<Recaps>,
    query: Result<Query<BlocksQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<BlocksPage>, ErrorResponse> {
    let Query(query) = query.map_err(|e| invalid(format!("Bad query: {}", e.body_text())))?;
    let limit = match query.limit {
        None => BLOCKS_DEFAULT_LIMIT,
        Some(0) => return Err(invalid("`limit` must be at least 1.")),
        Some(n) => n.min(BLOCKS_MAX_LIMIT),
    };
    let filter = BlockFilter {
        session: parse_id(query.session.as_deref(), "session")?,
        task: parse_id(query.task.as_deref(), "task")?,
        workstream: parse_id(query.workstream.as_deref(), "workstream")?,
        project: parse_id(query.project.as_deref(), "project")?,
    };
    let before: Option<EventId> = parse_id(query.before.as_deref(), "event")?;
    let source = Arc::clone(&recaps.source);
    let page = tokio::task::spawn_blocking(move || source.blocks(&filter, before, limit))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "reading recap blocks panicked");
            failed()
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "reading recap blocks failed");
            failed()
        })?;
    Ok(Json(page))
}

#[derive(Debug, Deserialize)]
struct DaysQuery {
    workstream: Option<String>,
    project: Option<String>,
    tz: Option<String>,
    before: Option<String>,
    limit: Option<usize>,
}

async fn days(
    State(recaps): State<Recaps>,
    query: Result<Query<DaysQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<DaysPage>, ErrorResponse> {
    let Query(query) = query.map_err(|e| invalid(format!("Bad query: {}", e.body_text())))?;
    let workstream: Option<WorkstreamId> = parse_id(query.workstream.as_deref(), "workstream")?;
    let project: Option<ProjectId> = parse_id(query.project.as_deref(), "project")?;
    let scope = match (workstream, project) {
        (Some(workstream), None) => DaysScope::Workstream(workstream),
        (None, Some(project)) => DaysScope::Project(project),
        _ => return Err(invalid("Give exactly one of `workstream` and `project`.")),
    };
    let tz = parse_tz(query.tz.as_deref())?;
    let before = parse_date(query.before.as_deref())?;
    let limit = match query.limit {
        None => DAYS_DEFAULT_LIMIT,
        Some(0) => return Err(invalid("`limit` must be at least 1.")),
        Some(n) => n.min(DAYS_MAX_LIMIT),
    };
    let source = Arc::clone(&recaps.source);
    let page = tokio::task::spawn_blocking(move || source.days(scope, tz, before, limit))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "reading recap days panicked");
            failed()
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "reading recap days failed");
            failed()
        })?;
    Ok(Json(page))
}
