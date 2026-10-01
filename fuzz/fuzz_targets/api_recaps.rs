//! The recap routes (`GET /v1/recaps/blocks` and `GET /v1/recaps/days`, `pitcrew_api::Recaps`)
//! on arbitrary query parameters, over a fake `RecapSource` that keeps its contract and records
//! what it is asked. Any client of the socket writes the query (B2).
//!
//! Input: a flags byte (bit 0: days, else blocks; bit 7: raw), then
//! - raw: the query string itself, or
//! - structured: one byte per parameter choosing it from the demo workspace's ids, an unknown id,
//!   prefixed and malformed ids, dates, time zones, limits, or text from the rest of the input.
//!
//! Checks, besides "no panic":
//! - **200 or 400 `invalid`, never 500**: the source never fails, so nothing else is allowed;
//! - a 200 carries exactly the source's page, and the source was asked with a limit of 1 to the
//!   route's cap and a time zone within ±14 hours; a 400 never reaches the source;
//! - structured: 400 exactly when the contract says (a malformed id or date, a `tz` that is not
//!   whole minutes within ±840, a `limit` of 0 or not a number, `days` without exactly one of
//!   `workstream` and `project`), and otherwise the source gets exactly the parsed values, the
//!   default limit when there is none, and the cap for one above it.
#![no_main]

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use libfuzzer_sys::fuzz_target;
use pitcrew_api::source::SourceError;
use pitcrew_api::{BlockFilter, DaysScope, RecapSource, Recaps};
use pitcrew_fuzz::percent_encode;
use pitcrew_protocol::ids::{EventId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::Date;
use pitcrew_protocol::recap::{
    BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT, BlocksPage, DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT,
    DaysPage, MAX_TZ_MINUTES,
};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use tower::ServiceExt as _;

/// What the source was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Blocks(BlockFilter, Option<EventId>, usize),
    Days(DaysScope, i32, Option<Date>, usize),
}

#[derive(Debug, Default)]
struct Fake {
    asked: Mutex<Vec<Asked>>,
}

impl RecapSource for Fake {
    fn blocks(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: usize,
    ) -> Result<BlocksPage, SourceError> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Asked::Blocks(*filter, before, limit));
        Ok(BlocksPage {
            blocks: Vec::new(),
            at_start: limit % 2 == 0,
        })
    }

    fn days(
        &self,
        scope: DaysScope,
        tz_minutes: i32,
        before: Option<Date>,
        limit: usize,
    ) -> Result<DaysPage, SourceError> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Asked::Days(scope, tz_minutes, before, limit));
        Ok(DaysPage {
            days: Vec::new(),
            at_start: tz_minutes % 2 == 0,
        })
    }
}

struct World {
    runtime: tokio::runtime::Runtime,
    fake: Arc<Fake>,
    router: Router,
    sessions: Vec<String>,
    tasks: Vec<String>,
    workstreams: Vec<String>,
    projects: Vec<String>,
    events: Vec<String>,
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let demo = pitcrew_fixtures::demo_workspace().expect("the demo workspace");
        let fake = Arc::new(Fake::default());
        let source: Arc<dyn RecapSource> = Arc::clone(&fake) as Arc<dyn RecapSource>;
        let strings = |ids: Vec<String>| {
            let mut out = ids;
            out.push(format!("{:026}", 7));
            out
        };
        World {
            runtime: tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a runtime"),
            router: Recaps::new(source).routes(),
            fake,
            sessions: strings(demo.sessions.iter().map(|s| s.id.to_string()).collect()),
            tasks: strings(demo.tasks.iter().map(|t| t.id.to_string()).collect()),
            workstreams: strings(demo.workstreams.iter().map(|w| w.id.to_string()).collect()),
            projects: strings(demo.projects.iter().map(|p| p.id.to_string()).collect()),
            events: strings(demo.events.iter().map(|e| e.id.to_string()).collect()),
        }
    })
}

/// One request: its status, its body as JSON, and what the source was asked.
fn get(
    world: &World,
    path: &str,
    query: &str,
) -> Option<(StatusCode, serde_json::Value, Vec<Asked>)> {
    let request = Request::get(format!("{path}?{query}"))
        .body(Body::empty())
        .ok()?;
    world
        .fake
        .asked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    let (status, body) = world.runtime.block_on(async {
        let response = world
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("infallible");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("a body");
        (status, body)
    });
    let json: serde_json::Value = serde_json::from_slice(&body)
        .unwrap_or_else(|e| panic!("status {status}, a body that is not JSON: {e}"));
    let asked = std::mem::take(
        &mut *world
            .fake
            .asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    assert!(
        status == StatusCode::OK || status == StatusCode::BAD_REQUEST,
        "status {status} for {path}?{query}: {json}"
    );
    if status == StatusCode::OK {
        assert_eq!(
            asked.len(),
            1,
            "a 200 that asked the source {} times",
            asked.len()
        );
        match &asked[0] {
            Asked::Blocks(_, _, limit) => {
                assert!((1..=BLOCKS_MAX_LIMIT).contains(limit), "limit {limit}");
                let page: BlocksPage = serde_json::from_value(json.clone()).expect("a page");
                assert_eq!(page.at_start, limit % 2 == 0, "not the source's page");
            }
            Asked::Days(_, tz, _, limit) => {
                assert!((1..=DAYS_MAX_LIMIT).contains(limit), "limit {limit}");
                assert!(tz.abs() <= MAX_TZ_MINUTES, "tz {tz}");
                let page: DaysPage = serde_json::from_value(json.clone()).expect("a page");
                assert_eq!(page.at_start, tz % 2 == 0, "not the source's page");
            }
        }
    } else {
        assert_eq!(
            json["code"], "invalid",
            "a 400 that is not `invalid`: {json}"
        );
        assert!(asked.is_empty(), "a 400 that reached the source");
    }
    Some((status, json, asked))
}

/// A parameter: absent, or a value of some kind.
fn pick(choice: u8, known: &[String], text: &str) -> Option<String> {
    let n = usize::from(choice >> 3);
    match choice % 8 {
        0 | 1 => None,
        2 | 3 => Some(known[n % known.len()].clone()),
        4 => Some(
            known[n % known.len()]
                .rsplit('_')
                .next()
                .unwrap_or("")
                .to_owned(),
        ),
        5 => Some(["", " ", "x", "01J", "ses_", "zzzzzzzzzzzzzzzzzzzzzzzzzz"][n % 6].to_owned()),
        6 => Some(format!(
            "{}{}",
            known[n % known.len()],
            ["x", "_", "%", "\u{202e}"][n % 4]
        )),
        _ => Some(text.to_owned()),
    }
}

fn pick_date(choice: u8, text: &str) -> Option<String> {
    const DATES: [&str; 10] = [
        "2026-09-30",
        "2026-02-31",
        "0000-00-00",
        "2026-13-01",
        "2026-9-30",
        "2026-09-30T00:00",
        "9999-12-31",
        "2026-09-00",
        "١٢٣٤-٠١-٠١",
        "+2026-09-30",
    ];
    match choice % 4 {
        0 | 1 => None,
        2 => Some(DATES[usize::from(choice >> 2) % DATES.len()].to_owned()),
        _ => Some(text.to_owned()),
    }
}

fn pick_number(choice: u8, text: &str) -> Option<String> {
    const NUMBERS: [&str; 14] = [
        "0",
        "1",
        "7",
        "30",
        "31",
        "200",
        "201",
        "840",
        "841",
        "-840",
        "-841",
        "+5",
        "1.5",
        "99999999999999999999",
    ];
    match choice % 4 {
        0 | 1 => None,
        2 => Some(NUMBERS[usize::from(choice >> 2) % NUMBERS.len()].to_owned()),
        _ => Some(text.to_owned()),
    }
}

fn parsed<T: FromStr>(value: &Option<String>) -> Result<Option<T>, ()> {
    value
        .as_deref()
        .map(|v| v.parse().map_err(|_| ()))
        .transpose()
}

/// `YYYY-MM-DD` with a month of 1-12 and a day of 1-31, written out.
fn date_ok(value: &str) -> bool {
    let b = value.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        b.get(r.clone())
            .filter(|d| d.iter().all(u8::is_ascii_digit))
            .map(|d| d.iter().fold(0, |n, c| n * 10 + u32::from(c - b'0')))
    };
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && num(0..4).is_some()
        && num(5..7).is_some_and(|m| (1..=12).contains(&m))
        && num(8..10).is_some_and(|d| (1..=31).contains(&d))
}

/// `-?[0-9]{1,4}` within ±840.
fn tz_of(value: Option<&str>) -> Result<i32, ()> {
    let Some(value) = value else { return Ok(0) };
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    let n: i32 = digits.parse().map_err(|_| ())?;
    let tz = if value.starts_with('-') { -n } else { n };
    if tz.abs() > MAX_TZ_MINUTES {
        Err(())
    } else {
        Ok(tz)
    }
}

fn limit_of(value: Option<&str>, default: usize, cap: usize) -> Result<usize, ()> {
    match value {
        None => Ok(default),
        Some(v) => match v.parse::<usize>() {
            Ok(0) | Err(_) => Err(()),
            Ok(n) => Ok(n.min(cap)),
        },
    }
}

fn query(params: &[(&str, &Option<String>)]) -> String {
    params
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_ref()
                .map(|v| format!("{name}={}", percent_encode(v)))
        })
        .collect::<Vec<_>>()
        .join("&")
}

fuzz_target!(|input: &[u8]| {
    let Some((&flags, rest)) = input.split_first() else {
        return;
    };
    let world = world();
    let days = flags & 1 == 1;
    let path = if days {
        "/v1/recaps/days"
    } else {
        "/v1/recaps/blocks"
    };
    if flags & 0x80 != 0 {
        let _ = get(world, path, &String::from_utf8_lossy(rest));
        return;
    }
    let Some((choices, text)) = rest.split_first_chunk::<6>() else {
        return;
    };
    let text = String::from_utf8_lossy(text).into_owned();
    if days {
        let workstream = pick(choices[0], &world.workstreams, &text);
        let project = pick(choices[1], &world.projects, &text);
        let tz = pick_number(choices[2], &text);
        let before = pick_date(choices[3], &text);
        let limit = pick_number(choices[4], &text);
        let q = query(&[
            ("workstream", &workstream),
            ("project", &project),
            ("tz", &tz),
            ("before", &before),
            ("limit", &limit),
        ]);
        let want = (|| -> Result<Asked, ()> {
            let w: Option<WorkstreamId> = parsed(&workstream)?;
            let p: Option<ProjectId> = parsed(&project)?;
            let scope = match (w, p) {
                (Some(w), None) => DaysScope::Workstream(w),
                (None, Some(p)) => DaysScope::Project(p),
                _ => return Err(()),
            };
            let tz = tz_of(tz.as_deref())?;
            let before = match &before {
                Some(d) if date_ok(d) => Some(Date(d.clone())),
                Some(_) => return Err(()),
                None => None,
            };
            let limit = limit_of(limit.as_deref(), DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT)?;
            Ok(Asked::Days(scope, tz, before, limit))
        })();
        expect(world, path, &q, want);
    } else {
        let session = pick(choices[0], &world.sessions, &text);
        let task = pick(choices[1], &world.tasks, &text);
        let workstream = pick(choices[2], &world.workstreams, &text);
        let project = pick(choices[3], &world.projects, &text);
        let before = pick(choices[4], &world.events, &text);
        let limit = pick_number(choices[5], &text);
        let q = query(&[
            ("session", &session),
            ("task", &task),
            ("workstream", &workstream),
            ("project", &project),
            ("before", &before),
            ("limit", &limit),
        ]);
        let want = (|| -> Result<Asked, ()> {
            let filter = BlockFilter {
                session: parsed::<SessionId>(&session)?,
                task: parsed::<TaskId>(&task)?,
                workstream: parsed::<WorkstreamId>(&workstream)?,
                project: parsed::<ProjectId>(&project)?,
            };
            let before: Option<EventId> = parsed(&before)?;
            let limit = limit_of(limit.as_deref(), BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT)?;
            Ok(Asked::Blocks(filter, before, limit))
        })();
        expect(world, path, &q, want);
    }
});

fn expect(world: &World, path: &str, query: &str, want: Result<Asked, ()>) {
    let Some((status, json, asked)) = get(world, path, query) else {
        return;
    };
    match want {
        Ok(want) => {
            assert_eq!(status, StatusCode::OK, "refused {path}?{query}: {json}");
            assert_eq!(asked, [want], "{path}?{query}");
        }
        Err(()) => assert_eq!(status, StatusCode::BAD_REQUEST, "accepted {path}?{query}"),
    }
}
