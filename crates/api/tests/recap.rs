//! `GET /v1/recaps/blocks` and `GET /v1/recaps/days` through the whole router.
//!
//! Two fake sources:
//! - [`Recorded`] answers one canned page and records every call, for validation, auth and
//!   "do the parameters reach the source" tests.
//! - [`DemoRecapSource`] serves `crates/fixtures/data/demo-recaps.json`
//!   (`pitcrew_fixtures::demo_recaps`), replicating the mock hub's filtering so its recap test
//!   cases (`apps/mock-hub/test/recaps.test.ts`) can be ported and checked against these routes.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_api::source::SourceError;
use pitcrew_api::{BlockFilter, DaysScope, RecapSource, Recaps, RouterParts};
use pitcrew_auth::TokenStore;
use pitcrew_fixtures::{DemoRecaps, ProjectDays};
use pitcrew_protocol::ids::{EventId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{Date, Receipt};
use pitcrew_protocol::recap::{
    BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT, Block, BlockKey, BlocksPage, Counts,
    DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT, DayRecap, DaysPage, RecapBlock, Summary,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

fn app(f: &Fixture, source: Arc<dyn RecapSource>) -> axum::Router {
    let tokens: Arc<dyn TokenStore> = f.tokens.clone();
    pitcrew_api::router(
        pitcrew_api::local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().device(Recaps::new(source).routes()),
    )
}

/// The last 4 characters of a bare ULID, e.g. `"0014"` for `"...EVT0014"`, as the mock's tests
/// identify fixture rows.
fn short(id: &str) -> &str {
    &id[id.len() - 4..]
}

// ─── A fake source that records its calls ──────────────────────────────────────────────────────

fn fake_block(id: EventId) -> Block {
    Block {
        id,
        last: id,
        key: BlockKey::Session(SessionId::new()),
        start: 0,
        end: 0,
        session: None,
        workstream: None,
        project: None,
        tasks: Vec::new(),
        agent: None,
        actors: Vec::new(),
        counts: Counts::default(),
        files: Vec::new(),
        files_omitted: 0,
        facts: Vec::new(),
        facts_omitted: 0,
        tool_receipts: Vec::new(),
        turn_receipts: Vec::new(),
    }
}

fn fake_day(date: &str) -> DayRecap {
    DayRecap {
        workstream: None,
        date: Date(date.to_owned()),
        blocks: Vec::new(),
        summary: Summary::default(),
    }
}

type BlockCall = (BlockFilter, Option<EventId>, usize);
type DayCall = (DaysScope, i32, Option<Date>, usize);

/// Answers one canned page and records every call, so tests can check what reached the source.
#[derive(Debug, Default)]
struct Recorded {
    block_calls: Mutex<Vec<BlockCall>>,
    day_calls: Mutex<Vec<DayCall>>,
}

impl Recorded {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl RecapSource for Recorded {
    fn blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: usize,
    ) -> Result<BlocksPage, SourceError> {
        self.block_calls
            .lock()
            .unwrap()
            .push((*filter, before, limit));
        let id = EventId::new();
        Ok(BlocksPage {
            blocks: vec![RecapBlock {
                block: fake_block(id),
                line: Summary::default(),
            }],
            at_start: true,
        })
    }

    fn days(
        &self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<Date>,
        limit: usize,
    ) -> Result<DaysPage, SourceError> {
        self.day_calls
            .lock()
            .unwrap()
            .push((scope, tz_minutes, before, limit));
        Ok(DaysPage {
            days: vec![fake_day("2026-01-01")],
            at_start: true,
        })
    }
}

#[tokio::test]
async fn recaps_need_a_device_token() {
    let f = Fixture::new();
    for path in [
        "/v1/recaps/blocks",
        "/v1/recaps/days?project=01JB000000000000000PRJ0001",
    ] {
        let (status, body) = call(app(&f, Recorded::new()), get_request(path, None)).await;
        assert_eq!(status, 401, "{path}");
        assert_eq!(body["code"], "unauthorized", "{path}");
        let (status, body) = call(
            app(&f, Recorded::new()),
            get_request(path, Some(&f.agent_token)),
        )
        .await;
        assert_eq!(status, 403, "{path}");
        assert_eq!(body["code"], "forbidden", "{path}");
    }
}

#[tokio::test]
async fn blocks_response_is_exactly_the_protocol_page() {
    let f = Fixture::new();
    let source = Recorded::new();
    let (status, body) = call(
        app(&f, source),
        get_request("/v1/recaps/blocks", Some(&f.device_token)),
    )
    .await;
    assert_eq!(status, 200);
    let page: BlocksPage = serde_json::from_value(body).unwrap();
    assert_eq!(page.blocks.len(), 1);
    assert!(page.at_start);
}

#[tokio::test]
async fn days_response_is_exactly_the_protocol_page() {
    let f = Fixture::new();
    let source = Recorded::new();
    let (status, body) = call(
        app(&f, source),
        get_request(
            "/v1/recaps/days?project=01JB000000000000000PRJ0001",
            Some(&f.device_token),
        ),
    )
    .await;
    assert_eq!(status, 200);
    let page: DaysPage = serde_json::from_value(body).unwrap();
    assert_eq!(page.days.len(), 1);
    assert!(page.at_start);
}

#[tokio::test]
async fn blocks_validation() {
    let f = Fixture::new();
    let app = app(&f, Recorded::new());
    for query in [
        "?session=nope",
        "?task=PAP-1",
        "?workstream=wst_notaulid",
        "?project=prj_",
        "?before=notanid",
        "?before=1790761920000",
        "?limit=0",
        "?limit=-1",
        "?limit=ten",
    ] {
        let (status, body) = call(
            app.clone(),
            get_request(&format!("/v1/recaps/blocks{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert_eq!(body["code"], "invalid", "{query}");
    }
}

#[tokio::test]
async fn days_validation() {
    let f = Fixture::new();
    let app = app(&f, Recorded::new());
    let w = "01JB000000000000000WST0001";
    let p = "01JB000000000000000PRJ0001";
    for query in [
        String::new(),
        "?tz=0".to_owned(),
        format!("?workstream={w}&project={p}"),
        "?workstream=Seed%20runs".to_owned(),
        "?project=PAP".to_owned(),
        format!("?project={p}&before=2026-13-01"),
        format!("?project={p}&before=2026-9-30"),
        format!("?project={p}&before=yesterday"),
        format!("?project={p}&limit=0"),
        format!("?project={p}&limit=-1"),
        format!("?project={p}&limit=ten"),
        format!("?project={p}&tz=841"),
        format!("?project={p}&tz=-841"),
        format!("?project={p}&tz=1.5"),
        format!("?project={p}&tz=UTC"),
        format!("?project={p}&tz=%2B60"),
        format!("?project={p}&tz=60m"),
    ] {
        let (status, body) = call(
            app.clone(),
            get_request(&format!("/v1/recaps/days{query}"), Some(&f.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert_eq!(body["code"], "invalid", "{query}");
    }
    // In range, and `-0` is a whole number: these pass validation (they reach the source).
    for tz in ["-0", "0", "840", "-840"] {
        let (status, _) = call(
            app.clone(),
            get_request(
                &format!("/v1/recaps/days?project={p}&tz={tz}"),
                Some(&f.device_token),
            ),
        )
        .await;
        assert_eq!(status, 200, "tz={tz}");
    }
}

#[tokio::test]
async fn blocks_parameters_reach_the_source_combined_and_capped() {
    let f = Fixture::new();
    let source = Recorded::new();
    let app = app(&f, source.clone());
    let (session, task, workstream, project, before) = (
        SessionId::new(),
        TaskId::new(),
        WorkstreamId::new(),
        ProjectId::new(),
        EventId::new(),
    );
    let query = format!(
        "?session={}&task={}&workstream={}&project={}&before={}&limit=9999",
        session.0, task.0, workstream.0, project.0, before.0
    );
    let (status, _) = call(
        app.clone(),
        get_request(&format!("/v1/recaps/blocks{query}"), Some(&f.device_token)),
    )
    .await;
    assert_eq!(status, 200);
    {
        let calls = source.block_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (filter, got_before, limit) = calls[0];
        assert_eq!(
            filter,
            BlockFilter {
                session: Some(session),
                task: Some(task),
                workstream: Some(workstream),
                project: Some(project),
            }
        );
        assert_eq!(got_before, Some(before));
        assert_eq!(
            limit, BLOCKS_MAX_LIMIT,
            "a limit above the cap counts as the cap"
        );
    }

    // No filters, no `before`, no `limit`: the defaults reach the source.
    let (status, _) = call(
        app.clone(),
        get_request("/v1/recaps/blocks", Some(&f.device_token)),
    )
    .await;
    assert_eq!(status, 200);
    let calls = source.block_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        (BlockFilter::default(), None, BLOCKS_DEFAULT_LIMIT)
    );
}

#[tokio::test]
async fn blocks_accepts_a_prefixed_lowercase_id() {
    let f = Fixture::new();
    let source = Recorded::new();
    let app = app(&f, source.clone());
    let session = SessionId::new();
    let query = format!("?session=ses_{}", session.0.to_string().to_lowercase());
    let (status, _) = call(
        app,
        get_request(&format!("/v1/recaps/blocks{query}"), Some(&f.device_token)),
    )
    .await;
    assert_eq!(status, 200);
    let calls = source.block_calls.lock().unwrap();
    assert_eq!(calls[0].0.session, Some(session));
}

#[tokio::test]
async fn days_parameters_reach_the_source_and_default_and_cap() {
    let f = Fixture::new();
    let source = Recorded::new();
    let app = app(&f, source.clone());
    let workstream = WorkstreamId::new();
    let query = format!(
        "?workstream={}&tz=-300&before=2026-05-01&limit=9999",
        workstream.0
    );
    let (status, _) = call(
        app.clone(),
        get_request(&format!("/v1/recaps/days{query}"), Some(&f.device_token)),
    )
    .await;
    assert_eq!(status, 200);
    {
        let calls = source.day_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (scope, tz, before, limit) = &calls[0];
        assert_eq!(*scope, DaysScope::Workstream(workstream));
        assert_eq!(*tz, -300);
        assert_eq!(before.as_ref().map(|d| d.0.as_str()), Some("2026-05-01"));
        assert_eq!(
            *limit, DAYS_MAX_LIMIT,
            "a limit above the cap counts as the cap"
        );
    }

    let project = ProjectId::new();
    let (status, _) = call(
        app,
        get_request(
            &format!("/v1/recaps/days?project={}", project.0),
            Some(&f.device_token),
        ),
    )
    .await;
    assert_eq!(status, 200);
    let calls = source.day_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        (DaysScope::Project(project), 0, None, DAYS_DEFAULT_LIMIT)
    );
}

// ─── Porting the mock hub's recap test cases ───────────────────────────────────────────────────
//
// IDs copied from `apps/mock-hub/test/helpers.ts` (`ID`) and `apps/mock-hub/test/recaps.test.ts`
// (`SEED_RUNS`, `ABLATION`, `PAP5`); they name the same fixture rows.

const PAPER: &str = "01JB000000000000000PRJ0001";
const TOOLING: &str = "01JB000000000000000PRJ0002";
const SUBMISSION: &str = "01JB000000000000000WST0001";
const SEED_RUNS: &str = "01JB000000000000000WST0002";
const ABLATION: &str = "01JB000000000000000WST0004";
const PAP1: &str = "01JB000000000000000TSK0001";
const PAP5: &str = "01JB000000000000000TSK0005";
const SES2: &str = "01JB000000000000000SES0002";

/// Serves `pitcrew_fixtures::demo_recaps()`, filtering and paging the way the mock hub's
/// `apps/mock-hub/src/recaps.ts` does, so its test cases can be ported.
#[derive(Debug)]
struct DemoRecapSource {
    recaps: DemoRecaps,
}

impl DemoRecapSource {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            recaps: pitcrew_fixtures::demo_recaps().unwrap(),
        })
    }
}

impl RecapSource for DemoRecapSource {
    fn blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: usize,
    ) -> Result<BlocksPage, SourceError> {
        let mut matching: Vec<&RecapBlock> = self
            .recaps
            .blocks
            .iter()
            .filter(|b| {
                filter.session.is_none_or(|s| b.block.session == Some(s))
                    && filter.task.is_none_or(|t| b.block.tasks.contains(&t))
                    && filter
                        .workstream
                        .is_none_or(|w| b.block.workstream == Some(w))
                    && filter.project.is_none_or(|p| b.block.project == Some(p))
                    && before.is_none_or(|cutoff| b.block.id < cutoff)
            })
            .collect();
        matching.sort_by(|a, b| b.block.id.cmp(&a.block.id));
        let at_start = matching.len() <= limit;
        let blocks = matching.into_iter().take(limit).cloned().collect();
        Ok(BlocksPage { blocks, at_start })
    }

    fn days(
        &self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<Date>,
        limit: usize,
    ) -> Result<DaysPage, SourceError> {
        // The fixture, like the mock, holds days for one time zone only; the real recap engine
        // (stream F) computes days for any `tz`. See "What differs from the mock" in the report.
        if tz_minutes != self.recaps.tz {
            return Err(format!(
                "the demo recap fixture has days for tz={} only, not tz={tz_minutes}",
                self.recaps.tz
            )
            .into());
        }
        let entries: Vec<DayRecap> = match scope {
            DaysScope::Project(project) => self
                .recaps
                .projects
                .iter()
                .find(|p| p.project == project)
                .map(|p| p.days.clone())
                .unwrap_or_default(),
            DaysScope::Workstream(workstream) => self
                .recaps
                .projects
                .iter()
                .flat_map(|p| {
                    p.days
                        .iter()
                        .filter(move |d| d.workstream == Some(workstream))
                        .cloned()
                })
                .collect(),
        };
        let mut matching: Vec<DayRecap> = entries
            .into_iter()
            .filter(|d| before.as_ref().is_none_or(|cutoff| &d.date < cutoff))
            .collect();
        matching.sort_by(|a, b| b.date.cmp(&a.date)); // stable: keeps the engine's order within a date
        let mut dates: Vec<Date> = Vec::new();
        for d in &matching {
            if dates.last() != Some(&d.date) {
                dates.push(d.date.clone());
            }
        }
        let at_start = dates.len() <= limit;
        let kept: HashSet<Date> = dates.into_iter().take(limit).collect();
        let days = matching
            .into_iter()
            .filter(|d| kept.contains(&d.date))
            .collect();
        Ok(DaysPage { days, at_start })
    }
}

async fn blocks(app: &axum::Router, token: &str, query: &str) -> BlocksPage {
    let path = format!("/v1/recaps/blocks{query}");
    let (status, body) = call(app.clone(), get_request(&path, Some(token))).await;
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_value(body).unwrap()
}

async fn days(app: &axum::Router, token: &str, query: &str) -> DaysPage {
    let path = format!("/v1/recaps/days{query}");
    let (status, body) = call(app.clone(), get_request(&path, Some(token))).await;
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_value(body).unwrap()
}

fn block_ids(page: &BlocksPage) -> Vec<String> {
    page.blocks
        .iter()
        .map(|b| short(&b.block.id.0.to_string()).to_owned())
        .collect()
}

fn day_entries(page: &DaysPage) -> Vec<String> {
    page.days
        .iter()
        .map(|d| {
            let w = match &d.workstream {
                Some(w) => short(&w.0.to_string()).to_owned(),
                None => "-".to_owned(),
            };
            format!("{} {w}", d.date.0)
        })
        .collect()
}

#[tokio::test]
async fn demo_serves_every_block_newest_first_each_with_its_line() {
    let f = Fixture::new();
    let app = app(&f, DemoRecapSource::new());
    let token = &f.device_token;
    let page = blocks(&app, token, "").await;
    assert_eq!(
        block_ids(&page),
        [
            "0014", "0013", "0011", "0010", "0009", "0008", "0007", "0004", "0002", "0001"
        ]
    );
    assert!(page.at_start);
    let edit = page
        .blocks
        .iter()
        .find(|b| short(&b.block.id.0.to_string()) == "0007")
        .unwrap();
    assert_eq!(edit.line.text, "@writer edited method.tex (+84 \u{2212}12)");
    let files: Vec<(String, u64, u64)> = edit
        .block
        .files
        .iter()
        .map(|f| (f.path.clone(), f.added, f.removed))
        .collect();
    assert_eq!(files, vec![("method.tex".to_owned(), 84, 12)]);
    let span = &edit.line.spans[0];
    assert_eq!((span.range.start, span.range.end), (0, 37));
    assert_eq!(edit.line.clause(span), edit.line.text);
    assert_eq!(span.receipts, vec![Receipt::Event { id: edit.block.id }]);
    for b in &page.blocks {
        assert!(!b.line.spans.is_empty(), "{}", b.line.text);
        for span in &b.line.spans {
            assert!(!b.line.clause(span).is_empty(), "{}", b.line.text);
            assert!(!span.receipts.is_empty(), "{}", b.line.text);
        }
    }
}

#[tokio::test]
async fn demo_blocks_page_with_exclusive_before_and_limit() {
    let f = Fixture::new();
    let app = app(&f, DemoRecapSource::new());
    let token = &f.device_token;
    let first = blocks(&app, token, "?limit=4").await;
    assert_eq!(block_ids(&first), ["0014", "0013", "0011", "0010"]);
    assert!(!first.at_start);
    let last = first.blocks.last().unwrap().block.id.0.to_string();
    let second = blocks(&app, token, &format!("?limit=4&before={last}")).await;
    assert_eq!(block_ids(&second), ["0009", "0008", "0007", "0004"]);
    assert!(!second.at_start);
    let last2 = second.blocks.last().unwrap().block.id.0.to_string();
    let third = blocks(&app, token, &format!("?limit=4&before={last2}")).await;
    assert_eq!(block_ids(&third), ["0002", "0001"]);
    assert!(third.at_start);
    let exact = blocks(&app, token, &format!("?limit=2&before={last2}")).await;
    assert_eq!(block_ids(&exact), ["0002", "0001"]);
    assert!(exact.at_start);
    // Any well-formed event id works as a cursor: EVT0012 is the last event of block 0011, not a
    // block id itself.
    let cursor = blocks(&app, token, "?limit=1&before=01JB000000000000000EVT0012").await;
    assert_eq!(block_ids(&cursor), ["0011"]);
    let prefixed = blocks(
        &app,
        token,
        "?limit=1&before=evt_01jb000000000000000evt0012",
    )
    .await;
    assert_eq!(block_ids(&prefixed), ["0011"]);
    let none = blocks(&app, token, "?before=01JB000000000000000EVT0001").await;
    assert_eq!((none.blocks.len(), none.at_start), (0, true));
    // A limit over the maximum counts as the maximum; the demo only has 10 blocks.
    assert_eq!(blocks(&app, token, "?limit=100000").await.blocks.len(), 10);
}

#[tokio::test]
async fn demo_blocks_filters_by_the_links_of_each_block_combined() {
    let f = Fixture::new();
    let app = app(&f, DemoRecapSource::new());
    let token = &f.device_token;
    assert_eq!(
        block_ids(&blocks(&app, token, &format!("?session={SES2}")).await),
        ["0010", "0009", "0001"]
    );
    // PAP-1's blocks are its session's: the dispatch, move and plan, then the edit.
    assert_eq!(
        block_ids(&blocks(&app, token, &format!("?task={PAP1}")).await),
        ["0007", "0004"]
    );
    // PAP-5 is named only by the ask raised in block 0010.
    assert_eq!(
        block_ids(&blocks(&app, token, &format!("?task={PAP5}")).await),
        ["0010"]
    );
    assert_eq!(
        block_ids(&blocks(&app, token, &format!("?workstream={SEED_RUNS}")).await),
        ["0011", "0010", "0009", "0001"]
    );
    assert_eq!(
        block_ids(&blocks(&app, token, &format!("?project={TOOLING}")).await),
        ["0014", "0013", "0008"]
    );
    assert_eq!(
        block_ids(
            &blocks(
                &app,
                token,
                &format!("?project={PAPER}&session={SES2}&limit=2")
            )
            .await
        ),
        ["0010", "0009"]
    );
    let none = blocks(&app, token, &format!("?project={TOOLING}&session={SES2}")).await;
    assert_eq!((none.blocks.len(), none.at_start), (0, true));
    // Prefixed and lower-case ids are the same id.
    assert_eq!(
        block_ids(
            &blocks(
                &app,
                token,
                &format!("?session=ses_{}", SES2.to_lowercase())
            )
            .await
        ),
        ["0010", "0009", "0001"]
    );
    // An unknown id is an empty page, as in the activity route.
    let none = blocks(&app, token, &format!("?workstream={ABLATION}")).await;
    assert_eq!((none.blocks.len(), none.at_start), (0, true));
    let none = blocks(&app, token, "?task=01JB000000000000000TSK0099").await;
    assert_eq!((none.blocks.len(), none.at_start), (0, true));
}

#[tokio::test]
async fn demo_days_serves_a_projects_days_newest_first_workstreams_by_id_within_a_date() {
    let f = Fixture::new();
    let app = app(&f, DemoRecapSource::new());
    let token = &f.device_token;
    let paper = days(&app, token, &format!("?project={PAPER}")).await;
    assert_eq!(
        day_entries(&paper),
        [
            "2026-09-30 0001",
            "2026-09-30 0002",
            "2026-09-29 0001",
            "2026-09-29 0002"
        ]
    );
    assert!(paper.at_start);
    let seeds = &paper.days[1];
    assert_eq!(
        seeds
            .blocks
            .iter()
            .map(|id| short(&id.0.to_string()).to_owned())
            .collect::<Vec<_>>(),
        ["0009", "0010", "0011"]
    );
    assert!(
        seeds
            .summary
            .text
            .starts_with("3 bursts of work, 1 tool run, 1 ask raised."),
        "{}",
        seeds.summary.text
    );
    for day in &paper.days {
        for span in &day.summary.spans {
            assert!(!day.summary.clause(span).is_empty(), "{}", day.summary.text);
            assert!(!span.receipts.is_empty(), "{}", day.summary.text);
        }
    }
    // Each entry's blocks are blocks of its workstream that the blocks route serves.
    let all = blocks(&app, token, &format!("?project={PAPER}")).await;
    for day in &paper.days {
        for id in &day.blocks {
            let found = all
                .blocks
                .iter()
                .find(|b| b.block.id == *id)
                .map(|b| b.block.workstream);
            assert_eq!(found, Some(day.workstream));
        }
    }
    assert_eq!(
        day_entries(&days(&app, token, &format!("?project={TOOLING}")).await),
        ["2026-09-30 0003"]
    );
}

#[tokio::test]
async fn demo_days_serves_a_workstreams_own_entries_and_nothing_for_a_quiet_or_unknown_one() {
    let f = Fixture::new();
    let app = app(&f, DemoRecapSource::new());
    let token = &f.device_token;
    let seeds = days(&app, token, &format!("?workstream={SEED_RUNS}")).await;
    assert_eq!(day_entries(&seeds), ["2026-09-30 0002", "2026-09-29 0002"]);
    let paper = days(&app, token, &format!("?project={PAPER}")).await;
    let expected: Vec<DayRecap> = paper
        .days
        .into_iter()
        .filter(|d| d.workstream == Some(SEED_RUNS.parse().unwrap()))
        .collect();
    assert_eq!(seeds.days, expected);
    let none = days(&app, token, &format!("?workstream={ABLATION}")).await;
    assert_eq!((none.days.len(), none.at_start), (0, true));
    let none = days(&app, token, "?project=01JB000000000000000PRJ0099").await;
    assert_eq!((none.days.len(), none.at_start), (0, true));
}

#[tokio::test]
async fn demo_days_pages_whole_dates_with_exclusive_before_and_limit() {
    let f = Fixture::new();
    let app = app(&f, DemoRecapSource::new());
    let token = &f.device_token;
    let first = days(&app, token, &format!("?project={PAPER}&limit=1")).await;
    assert_eq!(day_entries(&first), ["2026-09-30 0001", "2026-09-30 0002"]);
    assert!(!first.at_start);
    let last_date = &first.days.last().unwrap().date.0;
    let second = days(
        &app,
        token,
        &format!("?project={PAPER}&limit=1&before={last_date}"),
    )
    .await;
    assert_eq!(day_entries(&second), ["2026-09-29 0001", "2026-09-29 0002"]);
    assert!(second.at_start);
    let none = days(&app, token, &format!("?project={PAPER}&before=2026-09-29")).await;
    assert_eq!((none.days.len(), none.at_start), (0, true));
    assert_eq!(
        days(
            &app,
            token,
            &format!("?project={PAPER}&before=2026-10-01&limit=31")
        )
        .await
        .days
        .len(),
        4
    );
}

#[tokio::test]
async fn demo_days_needs_a_device_token() {
    let f = Fixture::new();
    for path in [
        "/v1/recaps/blocks",
        &format!("/v1/recaps/days?project={PAPER}"),
    ] {
        let (status, body) = call(
            app(&f, DemoRecapSource::new()),
            get_request(path, Some(&f.agent_token)),
        )
        .await;
        assert_eq!(status, 403, "{path}");
        assert_eq!(body["code"], "forbidden", "{path}");
        let (status, _) = call(app(&f, DemoRecapSource::new()), get_request(path, None)).await;
        assert_eq!(status, 401, "{path}");
    }
}

/// Unlike the mock, which special-cases its one fixture time zone as `400 invalid` ("tz=0 only"),
/// a `RecapSource` that cannot answer for a valid, in-range `tz` is reported like any other
/// unreadable source: `500 internal`, without the source's detail. See "What differs from the
/// mock" in the stream H report.
#[tokio::test]
async fn demo_unsupported_tz_is_an_internal_error_unlike_the_mocks_400() {
    let f = Fixture::new();
    let app = app(&f, DemoRecapSource::new());
    for tz in ["60", "-300", "840", "-840"] {
        let (status, body) = call(
            app.clone(),
            get_request(
                &format!("/v1/recaps/days?workstream={SEED_RUNS}&tz={tz}"),
                Some(&f.device_token),
            ),
        )
        .await;
        assert_eq!(status, 500, "tz={tz}");
        assert_eq!(body["code"], "internal", "tz={tz}");
    }
}

/// The engine's within-day order (no workstream first, then by workstream id), checked directly
/// against a handcrafted `DemoRecaps`, as the mock's unit test of `daysPage` does.
#[test]
fn days_orders_the_entry_without_a_workstream_first_within_a_date() {
    let day = |date: &str, workstream: Option<&str>| DayRecap {
        workstream: workstream.map(|w| w.parse().unwrap()),
        date: Date(date.to_owned()),
        blocks: Vec::new(),
        summary: Summary::default(),
    };
    let paper: ProjectId = PAPER.parse().unwrap();
    let source = DemoRecapSource {
        recaps: DemoRecaps {
            tz: 0,
            blocks: Vec::new(),
            projects: vec![ProjectDays {
                project: paper,
                // The engine's order: by date, then the one without a workstream, then by id.
                days: vec![
                    day("2026-09-29", Some(SEED_RUNS)),
                    day("2026-09-30", None),
                    day("2026-09-30", Some(SUBMISSION)),
                    day("2026-09-30", Some(SEED_RUNS)),
                ],
            }],
        },
    };
    let page = source.days(DaysScope::Project(paper), 0, None, 30).unwrap();
    assert_eq!(
        day_entries(&page),
        [
            "2026-09-30 -",
            "2026-09-30 0001",
            "2026-09-30 0002",
            "2026-09-29 0002"
        ]
    );
}
